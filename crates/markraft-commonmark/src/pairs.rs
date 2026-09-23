//! Brackets and quotes that close themselves, as a writer's editor offers.
//!
//! [`commonmark_auto_pairs`] answers to a flag a host can flip at any time.
//! While it is set, typing in a paragraph, a heading or a table cell does
//! this:
//!
//! * An opener — `(`, `[`, `{`, `"`, or one of the CJK brackets `（`, `「`,
//!   `《`, `【` — at a caret writes its closer after the caret too: `(|)`.
//!   Only where the closer cannot be mistaken for part of what follows: at
//!   the end of the block, or before whitespace, a closing bracket or
//!   punctuation. `"` pairs only where the character before the caret is not
//!   a letter or a digit, so `5"` stays an inch and `say"` a typo rather than
//!   a quotation. An opener escaped by a backslash, `\(`, is a literal and
//!   pairs nothing.
//! * The closer of a pair written this way, typed while the caret stands
//!   right before it, steps the caret over it instead of writing another:
//!   `(ab|)` then `)` is `(ab)|`.
//! * Backspace in such a pair while it is empty, `(|)`, deletes both.
//! * An opener typed over a selection inside one block wraps it, `(ab)`, and
//!   keeps it selected.
//!
//! A closer is followed only while the caret stays in its block and the
//! character is still there; after that it is text like any other, which is
//! what CodeMirror and Typora do too. Undo takes an opener back with its
//! closer, since both are one edit.
//!
//! # What does not pair
//!
//! * `'`, which prose uses far more often as an apostrophe than as a quote,
//!   and the curly quotes an input method types: it alternates them itself.
//! * The Markdown delimiters `` ` ``, `*`, `_` and `~`. A formatting toggle
//!   writes their pairs, and the `pending` module settles them; pairing them
//!   as they are typed would fight both, and a lone `*` is as often a bullet
//!   or a multiplication.
//! * Anything in a code span or a code block, whose text is verbatim — as is
//!   a raw block's.
//! * Anything an input method composes: a transaction that belongs to a
//!   composition, or is made while one is live, is left alone, so a candidate
//!   window never sees the document change under it. A bracket the input
//!   method commits without composing — `（` from a Chinese keyboard's `(` —
//!   is ordinary typing and pairs.
//!
//! # `[[`
//!
//! `[` pairs like any other opener, but a second `[` typed straight into the
//! empty pair the first one wrote takes that pair's `]` back: `[[` is left as
//! typed, `[[|`. It opens a wiki link, and the menu a host opens on it
//! replaces the `[[query` before the caret with the whole link it chose —
//! closers after the caret would be left behind that link, and deleting them
//! with it would change a transaction the menu builds in two steps, each
//! computed on what the one before leaves. Typed out by hand, `[[Note]]`
//! comes out as typed. `【【`, which a Chinese keyboard types for `[[`, does
//! the same.
//!
//! A footnote's `[^1]` and a task's `[ ]` type through: the `]` steps over the
//! one the `[` wrote, and the input rules see the text they always did.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};

use markraft_core::protocol::{COMPOSE_USER_EVENT, add_to_history, remote};
use markraft_core::{
    AnnotationType, Change, ChangeRange, Extension, Fragment, Node, Schema, Selection, Slice,
    StateField, StateFieldConfig, TrackMode, Transaction, TransactionFilterFn, TransactionSpec,
    transaction_filter,
};

use crate::derive::{DeriveContext, Style, derive};
use crate::textblock::{Items, block_kind};

/// Every pair, opener first.
const PAIRS: [(char, char); 8] = [
    ('(', ')'),
    ('[', ']'),
    ('{', '}'),
    ('"', '"'),
    ('（', '）'),
    ('「', '」'),
    ('《', '》'),
    ('【', '】'),
];

/// What may follow the caret for an opener to pair there, besides whitespace
/// and the end of the block: a closing bracket or punctuation, before which
/// a closer cannot be read as the start of the next word.
const BEFORE_PAIRING: &str = ")]}>）」》】,.;:!?，。；：！？、";

fn closer_of(opener: char) -> Option<char> {
    PAIRS
        .iter()
        .find(|(open, _)| *open == opener)
        .map(|(_, close)| *close)
}

/// A pair auto-pairing wrote, as the positions before its two characters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Tracked {
    open: usize,
    close: usize,
}

/// Set on a transaction that wrote a pair: where it stands in the document
/// the transaction produces.
static PAIRED: LazyLock<AnnotationType<Tracked>> = LazyLock::new(AnnotationType::define);
/// Set on a transaction that stepped over a tracked closer: the position
/// before that closer, in the document it started from.
static STEPPED: LazyLock<AnnotationType<usize>> = LazyLock::new(AnnotationType::define);
static FIELD: LazyLock<StateField<Vec<Tracked>>> =
    LazyLock::new(|| StateField::define(StateFieldConfig::new(|_| Vec::new(), update)));

/// Brackets and quotes that close themselves while `enabled` is set. See the
/// module documentation for what is paired and where.
///
/// The flag is read on every keystroke, so a host can switch auto-pairing
/// off and on without rebuilding its states. Turned off, typing is plain
/// typing, and the closers already written are left alone.
pub fn commonmark_auto_pairs(enabled: Arc<AtomicBool>) -> Extension {
    let filter: TransactionFilterFn = Arc::new(move |tr: &Transaction| {
        if !enabled.load(Ordering::Relaxed) {
            return None;
        }
        auto_pair(tr)
    });
    Extension::all([transaction_filter().of(filter), FIELD.extension()])
}

// -- the field -----------------------------------------------------------------

// A field's update takes the field's own type, which is the `Vec`.
#[allow(clippy::ptr_arg)]
fn update(value: &Vec<Tracked>, tr: &Transaction) -> Vec<Tracked> {
    let stepped = tr.annotation(&STEPPED).copied();
    let mut pairs: Vec<Tracked> = value
        .iter()
        .filter(|pair| Some(pair.close) != stepped)
        .copied()
        .collect();
    if pairs.is_empty() && tr.annotation(&PAIRED).is_none() {
        return pairs;
    }
    let schema = tr.start_state().schema();
    let doc = tr.new_doc();
    if tr.doc_changed() {
        let changes = tr.changes();
        // A character is followed while it is there: an insertion at its
        // position goes before it, and deleting it ends the pair.
        let map = |pos: usize| changes.map_pos(pos, 1, TrackMode::After);
        pairs = pairs
            .into_iter()
            .filter_map(|pair| {
                Some(Tracked {
                    open: map(pair.open)?,
                    close: map(pair.close)?,
                })
            })
            .collect();
    }
    if let Some(paired) = tr.annotation(&PAIRED) {
        pairs.push(*paired);
    }
    if !tr.doc_changed() && tr.selection().is_none() {
        return pairs;
    }
    // Only the caret's own block keeps its pairs: a closer the caret has
    // gone away from is text the writer has moved on from.
    let head = tr.new_selection().head(doc);
    let Some(block) = Block::at(schema, doc, head) else {
        return Vec::new();
    };
    pairs.retain(|pair| block.spells(pair));
    pairs
}

// -- the filter ----------------------------------------------------------------

fn auto_pair(tr: &Transaction) -> Option<Vec<TransactionSpec>> {
    let state = tr.start_state();
    if !tr.doc_changed()
        || tr.annotation(remote()) == Some(&true)
        || tr.annotation(add_to_history()) == Some(&false)
        || tr.is_user_event("undo")
        || tr.is_user_event("redo")
        || tr.is_user_event(COMPOSE_USER_EVENT)
        || markraft_core::composition::is_composing(state)
    {
        return None;
    }
    if tr.is_user_event("input.type")
        && let Some(typed) = Typed::read(tr)
    {
        return typed.respond(tr);
    }
    take_closers(tr)
}

/// One character typed over the selection — nothing else in the transaction.
struct Typed {
    character: char,
    /// The typed character's own node, whose marks a closer takes too.
    node: Node,
    /// The selection it replaced, in the starting document.
    from: usize,
    to: usize,
}

impl Typed {
    fn read(tr: &Transaction) -> Option<Typed> {
        let state = tr.start_state();
        let doc = state.doc();
        let Selection::Text { .. } = state.selection() else {
            return None;
        };
        let range = state.selection().replacement_range(doc);
        let changes = tr.changes().iter_changes();
        let [
            ChangeRange::Replaced {
                from_a,
                to_a,
                inserted,
                ..
            },
        ] = changes.as_slice()
        else {
            return None;
        };
        if (*from_a, *to_a) != (range.from, range.to)
            || inserted.open_start() != 0
            || inserted.open_end() != 0
            || inserted.content().child_count() != 1
        {
            return None;
        }
        let node = inserted.content().child(0).clone();
        let mut chars = node.text()?.chars();
        let character = chars.next()?;
        if chars.next().is_some() {
            return None;
        }
        Some(Typed {
            character,
            node,
            from: range.from,
            to: range.to,
        })
    }

    fn respond(&self, tr: &Transaction) -> Option<Vec<TransactionSpec>> {
        let state = tr.start_state();
        let schema = state.schema();
        let block = Block::at(schema, state.doc(), self.from)?;
        if self.to > block.end() {
            return None;
        }
        if self.from == self.to {
            return self.step_over(tr, &block).or_else(|| self.pair(tr, &block));
        }
        self.wrap(tr, &block)
    }

    /// Typing a tracked closer right before it moves the caret past it.
    fn step_over(&self, tr: &Transaction, block: &Block) -> Option<Vec<TransactionSpec>> {
        let tracked = tr.start_state().field(&FIELD)?;
        let before_closer = tracked.iter().any(|pair| pair.close == self.from)
            && block.char_at(self.from) == Some(self.character);
        if !before_closer {
            return None;
        }
        Some(vec![
            TransactionSpec::new()
                .selection(Selection::cursor(self.from + 1))
                .user_event("input.type")
                .annotate(STEPPED.of(self.from))
                .scroll_into_view(),
        ])
    }

    /// An opener at the caret writes its closer after it.
    fn pair(&self, tr: &Transaction, block: &Block) -> Option<Vec<TransactionSpec>> {
        let closer = closer_of(self.character)?;
        if let Some(unpaired) = self.open_wiki_link(tr, block) {
            return Some(unpaired);
        }
        let index = self.from - block.start;
        if !block.pairs_at(index, self.character)
            || block.in_code_with(index, self.character, index)
        {
            return None;
        }
        let doc = tr.new_doc();
        let caret = tr.new_selection().head(doc);
        if caret != self.from + 1 {
            return None;
        }
        let closing = self.node.with_text(&closer.to_string());
        Some(vec![
            tr.as_spec(),
            TransactionSpec::new()
                .changes([Change::insert(
                    caret,
                    Slice::from_fragment(Fragment::from_node(closing)),
                )])
                .selection(Selection::cursor(caret))
                .sequential()
                .annotate(PAIRED.of(Tracked {
                    open: self.from,
                    close: caret,
                })),
        ])
    }

    /// A `[` typed into the empty pair a `[` just wrote takes its `]` back,
    /// leaving the `[[` a wiki link opens with. See the module documentation.
    fn open_wiki_link(&self, tr: &Transaction, block: &Block) -> Option<Vec<TransactionSpec>> {
        if !matches!(self.character, '[' | '【')
            || block.char_at(self.from.checked_sub(1)?) != Some(self.character)
        {
            return None;
        }
        let tracked = tr.start_state().field(&FIELD)?;
        tracked.iter().find(|pair| {
            pair.open + 1 == self.from && pair.close == self.from && block.spells(pair)
        })?;
        let caret = self.from + 1;
        Some(vec![
            tr.as_spec(),
            TransactionSpec::new()
                .changes([Change::delete(caret, caret + 1)])
                .selection(Selection::cursor(caret))
                .sequential(),
        ])
    }

    /// An opener typed over a selection in one block wraps it, and the
    /// selection stays on what it wrapped.
    fn wrap(&self, tr: &Transaction, block: &Block) -> Option<Vec<TransactionSpec>> {
        let closer = closer_of(self.character)?;
        let (from, to) = (self.from - block.start, self.to - block.start);
        if block.in_code_with(from, self.character, to) {
            return None;
        }
        let state = tr.start_state();
        let doc = state.doc();
        let text = |c: char| {
            Slice::from_fragment(Fragment::from_node(self.node.with_text(&c.to_string())))
        };
        let (anchor, head) = (state.selection().anchor(doc), state.selection().head(doc));
        let inner = |pos: usize| pos + 1;
        Some(vec![
            TransactionSpec::new()
                .changes([
                    Change::insert(self.from, text(self.character)),
                    Change::insert(self.to, text(closer)),
                ])
                .selection(Selection::text(inner(anchor), inner(head)))
                .user_event("input.type")
                .scroll_into_view(),
        ])
    }
}

/// Backspace in an empty tracked pair: a transaction that deletes its opener
/// and nothing else deletes its closer too.
///
/// Only that shape. A wider deletion that reaches a closer — a menu taking
/// back the text it was opened on — may be the first step of an edit whose
/// next step is computed on what it leaves, and one more deletion there would
/// move everything that step says.
fn take_closers(tr: &Transaction) -> Option<Vec<TransactionSpec>> {
    let tracked = tr.start_state().field(&FIELD)?;
    let changes = tr.changes().iter_changes();
    let [
        ChangeRange::Replaced {
            from_a,
            to_a,
            inserted,
            ..
        },
    ] = changes.as_slice()
    else {
        return None;
    };
    if !inserted.is_empty() || *to_a != from_a + 1 {
        return None;
    }
    tracked
        .iter()
        .find(|pair| pair.open == *from_a && pair.close == *to_a)?;
    // The opener is gone, so the closer now stands where it started.
    Some(vec![
        tr.as_spec(),
        TransactionSpec::new()
            .changes([Change::delete(*from_a, from_a + 1)])
            .sequential(),
    ])
}

// -- reading a block -------------------------------------------------------------

/// A textblock of inline source: where its content starts and its text, one
/// `char` per position.
struct Block {
    kind: crate::derive::BlockKind,
    start: usize,
    chars: Vec<char>,
}

impl Block {
    /// The block of inline source `pos` stands in. Code blocks and raw
    /// blocks are verbatim, and are none.
    fn at(schema: &Schema, doc: &Node, pos: usize) -> Option<Block> {
        let resolved = doc.resolve(pos).ok()?;
        let block = resolved.parent();
        let kind = block_kind(schema, block.type_id())?;
        let chars = Items::from_nodes(schema, block.children())
            .text()
            .chars()
            .collect();
        Some(Block {
            kind,
            start: resolved.start(resolved.depth()),
            chars,
        })
    }

    fn end(&self) -> usize {
        self.start + self.chars.len()
    }

    fn char_at(&self, pos: usize) -> Option<char> {
        self.chars.get(pos.checked_sub(self.start)?).copied()
    }

    /// Whether `pair` still spells a pair in this block.
    fn spells(&self, pair: &Tracked) -> bool {
        pair.open < pair.close
            && self
                .char_at(pair.open)
                .and_then(closer_of)
                .is_some_and(|closer| self.char_at(pair.close) == Some(closer))
    }

    /// Whether `opener`, typed at `index`, pairs there.
    fn pairs_at(&self, index: usize, opener: char) -> bool {
        let before = index.checked_sub(1).map(|at| self.chars[at]);
        let escapes = self.chars[..index]
            .iter()
            .rev()
            .take_while(|c| **c == '\\')
            .count();
        if escapes % 2 == 1 {
            return false;
        }
        if opener == '"' && before.is_some_and(char::is_alphanumeric) {
            return false;
        }
        match self.chars.get(index) {
            None => true,
            Some(next) => next.is_whitespace() || BEFORE_PAIRING.contains(*next),
        }
    }

    /// Whether the text, with `typed` put over `from..to`, reads the typed
    /// character as part of a code span.
    fn in_code_with(&self, from: usize, typed: char, to: usize) -> bool {
        let text: String = self.chars[..from]
            .iter()
            .chain(std::iter::once(&typed))
            .chain(&self.chars[to..])
            .collect();
        derive(self.kind, &text, &DeriveContext::new())
            .styles
            .iter()
            .any(|span| span.style == Style::Code && span.range.contains(&from))
    }
}
