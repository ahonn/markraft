//! Input rules: automatic conversions triggered by what was just typed.
//!
//! A rule looks at the text before the caret in the current textblock after a
//! transaction whose user event is `input.type`, and answers with the edit to
//! perform — turning `"- "` at the start of a paragraph into a bullet list,
//! `"# "` into a heading, `"```"` into a code block.
//!
//! # Why a transaction filter and not an appender
//!
//! Rules are registered as a [`transaction_filter`]. A filter runs for every
//! transaction, including ones built with plain
//! [`EditorState::update`](crate::EditorState::update); a
//! [`transaction_appender`](crate::transaction_appender) only runs through
//! `update_with_appended`, which would make input rules depend on how the host
//! happens to dispatch. Folding the rule into the typing transaction also means
//! a view never observes the intermediate document in which the marker text is
//! still there.
//!
//! The combined transaction is annotated
//! [`isolate_history(Both)`](crate::protocol::isolate_history), so an automatic
//! conversion is always an undo step of its own — the behaviour the old core's
//! block conversions relied on.
//!
//! # Undoing a rule without undoing the text
//!
//! The rule's own changes are recovered from the transaction — the typing part
//! is inverted and composed with the whole — and their inverse is kept in
//! [`input_rule_field`]. [`undo_input_rule`] applies that inverse, which
//! restores the document to what it was *after* the character was typed: the
//! `"- "` is back and no list was made. Any later transaction that changes the
//! document or moves the selection clears the field.

use std::sync::{Arc, LazyLock};

use crate::change::ChangeSet;
use crate::node::Node;
use crate::projection::atom_filler;
use crate::schema::Schema;
use crate::selection::Selection;
use crate::state::protocol::{IsolateHistory, isolate, remote};
use crate::state::{
    AnnotationType, EditorState, Extension, Facet, StateField, StateFieldConfig, Transaction,
    TransactionFilterFn, TransactionSpec, transaction_filter,
};

use super::{Command, command};
use crate::protocol::event;

/// Decides whether a rule fires, given the text before the caret.
///
/// Returns how many `char`s at the end of that text the rule consumes.
pub type InputRuleMatcher = Arc<dyn Fn(&str) -> Option<usize> + Send + Sync>;

/// Turns a match into the edit to perform.
///
/// Positions in the returned spec refer to
/// [`InputRuleMatch::doc`] — the document the typing transaction produces.
pub type InputRuleHandler =
    Arc<dyn Fn(&InputRuleMatch<'_>) -> Option<TransactionSpec> + Send + Sync>;

/// What a rule is told about the text it matched.
pub struct InputRuleMatch<'a> {
    /// The state the typing transaction started from.
    pub start_state: &'a EditorState,
    /// The typing transaction.
    pub tr: &'a Transaction,
    /// The document the typing transaction produces. Every position here, and
    /// every position the handler returns, refers to it.
    pub doc: &'a Node,
    /// The schema.
    pub schema: &'a Schema,
    /// Position of the first `char` the rule matched.
    pub from: usize,
    /// The caret, just after the last matched `char`.
    pub to: usize,
    /// The matched text.
    pub text: &'a str,
    /// The whole text before the caret in the textblock.
    pub before: &'a str,
    /// The textblock the match sits in.
    pub block: &'a Node,
    /// Position of the first token inside [`InputRuleMatch::block`].
    pub block_start: usize,
}

/// One automatic conversion.
#[derive(Clone)]
pub struct InputRule {
    matcher: InputRuleMatcher,
    handler: InputRuleHandler,
}

impl std::fmt::Debug for InputRule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("InputRule")
    }
}

impl InputRule {
    /// A rule with an explicit matcher.
    pub fn new(
        matcher: impl Fn(&str) -> Option<usize> + Send + Sync + 'static,
        handler: impl Fn(&InputRuleMatch<'_>) -> Option<TransactionSpec> + Send + Sync + 'static,
    ) -> InputRule {
        InputRule {
            matcher: Arc::new(matcher),
            handler: Arc::new(handler),
        }
    }

    /// A rule that fires when the text before the caret ends with `pattern`.
    pub fn suffix(
        pattern: &str,
        handler: impl Fn(&InputRuleMatch<'_>) -> Option<TransactionSpec> + Send + Sync + 'static,
    ) -> InputRule {
        let pattern = pattern.to_string();
        let len = pattern.chars().count();
        InputRule::new(
            move |before| before.ends_with(&pattern).then_some(len),
            handler,
        )
    }

    /// A rule that fires when the text before the caret is exactly `pattern`,
    /// so it only applies at the start of a block.
    pub fn block_start(
        pattern: &str,
        handler: impl Fn(&InputRuleMatch<'_>) -> Option<TransactionSpec> + Send + Sync + 'static,
    ) -> InputRule {
        let pattern = pattern.to_string();
        let len = pattern.chars().count();
        InputRule::new(move |before| (before == pattern).then_some(len), handler)
    }

    /// This rule, matching only while `enabled` answers true.
    ///
    /// `enabled` is asked on every typing transaction, so a host can switch a
    /// set of rules on and off — a "Markdown shortcuts" preference — without
    /// rebuilding the state's extensions.
    pub fn when(self, enabled: impl Fn() -> bool + Send + Sync + 'static) -> InputRule {
        let matcher = self.matcher;
        InputRule {
            matcher: Arc::new(move |before| if enabled() { matcher(before) } else { None }),
            handler: self.handler,
        }
    }
}

/// What [`undo_input_rule`] needs to take a rule back.
#[derive(Debug, Clone)]
pub struct InputRuleUndo {
    /// The inverse of the rule's own changes, over the current document.
    pub changes: ChangeSet,
    /// The selection to restore.
    pub selection: Selection,
}

#[derive(Clone)]
struct Applied {
    typed: ChangeSet,
    typed_doc: Node,
    typed_selection: Selection,
}

static INPUT_RULE: LazyLock<Facet<InputRule>> = LazyLock::new(Facet::list);
static APPLIED: LazyLock<AnnotationType<Applied>> = LazyLock::new(AnnotationType::define);
static UNDO_FIELD: LazyLock<StateField<Option<InputRuleUndo>>> =
    LazyLock::new(|| StateField::define(StateFieldConfig::new(|_| None, update_undo)));
static RUNNER: LazyLock<Extension> = LazyLock::new(|| {
    let run: TransactionFilterFn = Arc::new(run_input_rules);
    Extension::all([transaction_filter().of(run), UNDO_FIELD.extension()])
});

/// The facet input rules are registered in.
pub fn input_rule() -> &'static Facet<InputRule> {
    &INPUT_RULE
}

/// The field holding what [`undo_input_rule`] would take back.
pub fn input_rule_field() -> &'static StateField<Option<InputRuleUndo>> {
    &UNDO_FIELD
}

/// The extension that runs `rules` after every typing transaction.
pub fn input_rules(rules: impl IntoIterator<Item = InputRule>) -> Extension {
    let mut items: Vec<Extension> = rules.into_iter().map(|rule| INPUT_RULE.of(rule)).collect();
    items.push(RUNNER.clone());
    Extension::all(items)
}

/// Take back the last automatic conversion, keeping the text that triggered it.
pub fn undo_input_rule() -> Command {
    command(|state| {
        let undo = state.field(input_rule_field())?.clone()?;
        if undo.changes.length_before() != state.doc().content_size() {
            return None;
        }
        Some(
            TransactionSpec::new()
                .change_set(undo.changes)
                .selection(undo.selection)
                .user_event(event::DELETE)
                .annotate(isolate(IsolateHistory::Both))
                .scroll_into_view(),
        )
    })
}

fn update_undo(value: &Option<InputRuleUndo>, tr: &Transaction) -> Option<InputRuleUndo> {
    if let Some(applied) = tr.annotation(&APPLIED) {
        let rule = applied
            .typed
            .invert(tr.start_state().doc())
            .ok()?
            .compose(tr.changes())
            .ok()?;
        return Some(InputRuleUndo {
            changes: rule.invert(&applied.typed_doc).ok()?,
            selection: applied.typed_selection.clone(),
        });
    }
    if tr.doc_changed() || tr.selection().is_some() {
        return None;
    }
    value.clone()
}

fn run_input_rules(tr: &Transaction) -> Option<Vec<TransactionSpec>> {
    if !tr.is_user_event(event::INPUT_TYPE)
        || tr.annotation(&APPLIED).is_some()
        || tr.annotation(remote()) == Some(&true)
    {
        return None;
    }
    let state = tr.start_state();
    let rules = state.facet(input_rule()).clone();
    if rules.is_empty() {
        return None;
    }
    let schema = state.schema();
    let doc = tr.new_doc();
    let selection = tr.new_selection();
    if !selection.is_cursor() {
        return None;
    }
    let pos = selection.head(doc);
    let resolved = doc.resolve(pos).ok()?;
    let block = resolved.parent();
    if !block.is_textblock(schema) || schema.node_type(block.type_id()).is_code() {
        return None;
    }
    let block_start = resolved.start(resolved.depth());
    let before = block_text(schema, block, pos - block_start);
    let total = pos - block_start;

    for rule in &rules {
        let Some(len) = (rule.matcher)(&before) else {
            continue;
        };
        if len > total {
            continue;
        }
        let start_byte = char_to_byte(&before, total - len);
        let matched = InputRuleMatch {
            start_state: state,
            tr,
            doc,
            schema,
            from: pos - len,
            to: pos,
            text: &before[start_byte..],
            before: &before,
            block,
            block_start,
        };
        let Some(spec) = (rule.handler)(&matched) else {
            continue;
        };
        let applied = Applied {
            typed: tr.changes().clone(),
            typed_doc: doc.clone(),
            typed_selection: selection.clone(),
        };
        return Some(vec![
            tr.as_spec(),
            spec.sequential()
                .annotate(APPLIED.of(applied))
                .annotate(isolate(IsolateHistory::Both)),
        ]);
    }
    None
}

/// The first `upto` tokens of `block`'s content as text, one `char` per token.
///
/// Atoms become [`OBJECT_REPLACEMENT`](crate::projection::OBJECT_REPLACEMENT),
/// hard breaks `'\n'` and soft breaks a space, exactly as
/// [`Projection`](crate::projection::Projection) renders them, so a matched
/// `char` count is also a token count.
fn block_text(schema: &Schema, block: &Node, upto: usize) -> String {
    let mut out = String::new();
    let mut pos = 0usize;
    for child in block.children() {
        if pos >= upto {
            break;
        }
        let size = child.node_size();
        let take = (upto - pos).min(size);
        match child.text() {
            Some(text) => out.extend(text.chars().take(take)),
            None => {
                let filler = atom_filler(schema, child.type_id());
                for _ in 0..take {
                    out.push(filler);
                }
            }
        }
        pos += size;
    }
    out
}

fn char_to_byte(text: &str, n: usize) -> usize {
    text.char_indices()
        .nth(n)
        .map(|(i, _)| i)
        .unwrap_or(text.len())
}
