//! The editing behaviour that belongs to Markdown rather than to the model.
//!
//! [`commonmark_extensions`] bundles four things:
//!
//! * **Input rules** — the conversions a Markdown writer expects while typing:
//!   `# ` through `###### `, `- `/`* `/`+ `, `1. `, `> `, `---`,
//!   `[ ] `/`[x] ` at the start of a bullet item, and a fence. The fence waits
//!   for the space that ends its info string — ```` ``` ```` opens a code block
//!   with no language and ```` ```rust ```` opens one with `rust` — because the
//!   two cannot be told apart before it. `[!note] ` at the start of a block
//!   quote turns it into a callout on the same space a check box waits for,
//!   and `[^label]: ` at the start of a block makes a footnote definition.
//! * **Corrections** — a list merge, the repair that puts a required child
//!   back into an emptied container, and the canonicalising correction that
//!   keeps every textblock's marks what its source says. Two lists of the same
//!   type and attributes sitting next to each other are one list as far as
//!   CommonMark is concerned, so the model is brought in line rather than left
//!   describing something the source cannot express.
//! * **Pending pairs** — the empty delimiter pair a cursor toggle writes is
//!   deleted again when the caret leaves it with nothing typed in it, so it
//!   never reaches the file as literal `****`. See the `pending` module.
//! * **The table invariant** — every row of a table as wide as the rest,
//!   held by refusing any edit that would leave a table ragged; see
//!   [`markraft_core::commands::table_invariant`]. The key
//!   chains a view binds need know nothing of cells.
//!
//! # The canonicalising correction
//!
//! A paragraph's, a heading's and a table cell's text is its inline source,
//! and every style mark on it is [derived](crate::derive::derive) from that
//! text. Whenever a transaction changes the characters of such a block, one
//! correction brings the block back to what a reader of its source would read:
//!
//! 1. Its lines: a blank line splits the block in two, a line break at either
//!    end of it goes — with the `\` that spells it a hard break —, whitespace starting a line or ending the block goes,
//!    and in a heading of level 3 or more — which has no way to hold one — or
//!    a table cell a break becomes a space.
//! 2. Its atoms: text a reader takes for an image, a wiki link or a raw HTML
//!    tag becomes that atom, every such spelling in the same round. This is
//!    how a typed `[[Note]]` becomes a link.
//! 3. Its block syntax: the backslashes [`guard`](crate::derive::guard) names
//!    go in, so no line of it opens another block.
//! 4. Its marks: each run whose marks differ from what the derivation says
//!    gets its whole mark set in one change. Mark changes only: replacing the
//!    content would collapse every caret inside it.
//!
//! Each round of the correction loop does the first of these that has
//! anything to do, so each works on the text the one before it left. Each
//! settles the whole block in the round it runs in: every blank line splits
//! at once, and the marks go in as one non-overlapping change per run. Steps 1 and 3 leave alone the
//! line a caret stands on: `-` is a list item a writer may still be typing
//! into `- `, and `*` the start of `*em*`. The transaction that moves the
//! caret off that line settles it — the correction is registered with
//! [`Correction::when_selection_leaves`] — so apart from the caret's line the
//! tree always holds what the file will.
//!
//! A block is read against the document's link reference definitions, which
//! stand in raw blocks of their own. A transaction that changes them reaches
//! no textblock, so a second correction, on the document, re-derives the
//! marks of every block a reference link could be in — see
//! [`follow_definitions`].
//!
//! Nothing else: key bindings, clipboard handling and the rest belong to a
//! host, which composes them with this.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use markraft_core::commands::structure::markup_of;
use markraft_core::commands::{
    InputRule, InputRuleMatch, TableTypes, input_rules, table_invariant,
};
use markraft_core::kind::TABLE_ALIGNMENTS_ATTR;
use markraft_core::{
    Attrs, Change, ChangeRange, Extension, Fragment, MarkSet, Markup, Node, NodeTypeId, Schema,
    Slice, Token, TrackMode, attrs,
    corrections::{Correction, CorrectionContext, corrections, fill_required_content},
};

use crate::derive::{BlockKind, Derived, derive};
use crate::schema as md;
use crate::textblock::{
    Item, Items, block_kind, definition_candidates, definitions_context, derived_mark_types,
    derived_marks, item_marker,
};

/// Input rules and corrections for the CommonMark preset, and the settling of
/// the delimiter pair a cursor toggle leaves pending.
pub fn commonmark_extensions(schema: &Schema) -> Extension {
    with_rules(schema, commonmark_input_rules())
}

/// [`commonmark_extensions`], with every input rule answering to `shortcuts`.
///
/// The rules match only while the flag is set; the corrections, pending pairs
/// and atom unfolding stay on regardless, since they keep the tree what its
/// source says rather than convert anything a writer typed. The flag is read
/// on every keystroke, so a host can turn Markdown shortcuts off and on
/// without rebuilding its states.
pub fn commonmark_extensions_with_shortcuts(
    schema: &Schema,
    shortcuts: Arc<AtomicBool>,
) -> Extension {
    let rules = commonmark_input_rules().into_iter().map(|rule| {
        let shortcuts = shortcuts.clone();
        rule.when(move || shortcuts.load(Ordering::Relaxed))
    });
    with_rules(schema, rules)
}

fn with_rules(schema: &Schema, rules: impl IntoIterator<Item = InputRule>) -> Extension {
    Extension::all([
        input_rules(rules),
        corrections(commonmark_corrections(schema)),
        crate::pending::pending_pairs(),
        crate::unfold::unfold_atoms(),
        table_invariant_for(schema),
    ])
}

/// [`table_invariant`] over this schema's table types, or nothing for a
/// schema a consumer built without tables.
fn table_invariant_for(schema: &Schema) -> Extension {
    match (
        schema.node_id(md::TABLE),
        schema.node_id(md::TABLE_ROW),
        schema.node_id(md::TABLE_CELL),
    ) {
        (Some(table), Some(row), Some(cell)) => {
            table_invariant(TableTypes::new(table, row, cell, TABLE_ALIGNMENTS_ATTR))
        }
        _ => Extension::none(),
    }
}

/// The corrections the preset needs: merge adjacent identical lists, fill in
/// the block a container's content rule requires, and keep every textblock
/// what its source says — see the module documentation.
pub fn commonmark_corrections(schema: &Schema) -> Vec<Correction> {
    let containers = [
        md::DOC,
        md::BLOCKQUOTE,
        md::LIST_ITEM,
        md::TASK_ITEM,
        md::BULLET_LIST,
        md::ORDERED_LIST,
        md::TABLE,
        md::TABLE_ROW,
    ];
    let mut out = Vec::new();
    for name in containers {
        let Some(ty) = schema.node_id(name) else {
            continue;
        };
        out.push(fill_required_content(ty));
        out.push(Correction::on_child_list(ty, merge_adjacent_lists));
    }
    if let Some(ordered) = schema.node_id(md::ORDERED_LIST) {
        out.push(Correction::on_child_list(ordered, keep_numbers));
    }
    if let Some(paragraph) = schema.node_id(md::PARAGRAPH) {
        out.push(Correction::on_content(paragraph, define_on_leaving).when_selection_leaves());
    }
    for name in [md::PARAGRAPH, md::HEADING, md::TABLE_CELL] {
        if let Some(ty) = schema.node_id(name) {
            out.push(Correction::on_content(ty, canonicalise).when_selection_leaves());
        }
    }
    if let Some(doc) = schema.node_id(md::DOC) {
        out.push(Correction::on_content(doc, follow_definitions));
    }
    out
}

/// Join two sibling lists that a reader would read as one.
///
/// Only one pair per round: two merges of neighbouring pairs would overlap, and
/// the correction loop runs to a fixed point anyway.
fn merge_adjacent_lists(cx: &markraft_core::corrections::CorrectionContext<'_>) -> Vec<Change> {
    let list_types = [md::BULLET_LIST, md::ORDERED_LIST];
    let schema = cx.start_state.schema();
    let ids: Vec<NodeTypeId> = list_types
        .iter()
        .filter_map(|name| schema.node_id(name))
        .collect();
    let mut pos = cx.content_start;
    let mut previous: Option<&Node> = None;
    for child in cx.node.children() {
        if let Some(previous) = previous
            && ids.contains(&child.type_id())
            && previous.type_id() == child.type_id()
            && previous.attrs() == child.attrs()
        {
            return vec![Change::delete(pos - 1, pos + 1)];
        }
        pos += child.node_size();
        previous = Some(child);
    }
    Vec::new()
}

/// Keep the numbers of an ordered list's items when an edit takes the items
/// before them, text and all: selecting a paragraph and the
/// first item of `1. a` / `2. b` and deleting leaves `2. b`. Lifting the
/// first item out takes the item but keeps its text, so the rest are
/// numbered from the list's start as before.
///
/// The list's first item is matched to the item of a list the transaction
/// started with that it was, and the list starts at that item's number when
/// every item before it went with what it held. An undo or a remote change
/// puts a document back rather than editing one, and is left as it is.
fn keep_numbers(cx: &CorrectionContext<'_>) -> Vec<Change> {
    if !cx.tr.recorded_in_history() {
        return Vec::new();
    }
    let Some(before) = cx.before else {
        return Vec::new();
    };
    let changes = cx.tr.changes();
    let start = |node: &Node| {
        node.attrs()
            .get("start")
            .and_then(|value| value.as_int())
            .unwrap_or(1)
    };
    let ordered = cx.node.type_id();
    let mut number = None;
    cx.start_state.doc().descendants(&mut |old, pos, _, _| {
        if number.is_some() {
            return false;
        }
        if old.type_id() != ordered {
            return true;
        }
        let mut item = pos + 1;
        for (index, child) in old.children().enumerate() {
            // The item's open token and the first of what it held both
            // deleted: an item lifted out loses the one and keeps the other.
            let gone = changes.map_pos(item, 1, TrackMode::After).is_none()
                && changes.map_pos(item + 2, 1, TrackMode::After).is_none();
            if !gone {
                if changes.map_pos(item, 1, TrackMode::Simple) == Some(before + 1) {
                    number = Some(start(old) + index as i64);
                }
                break;
            }
            item += child.node_size();
        }
        true
    });
    // Already numbered so, or not a list the transaction started with.
    let Some(number) = number.filter(|&number| number != start(cx.node)) else {
        return Vec::new();
    };
    let attrs = cx.node.attrs().with("start", number);
    let markup = Markup::with_attrs(ordered, attrs);
    let after = before + cx.node.node_size() - 1;
    vec![
        Change::replace(
            before,
            before + 1,
            Slice::from_tokens(&[Token::Open(markup.clone())]),
        ),
        Change::replace(
            after,
            after + 1,
            Slice::from_tokens(&[Token::Close(markup)]),
        ),
    ]
}

/// The conversions a Markdown writer expects while typing.
pub fn commonmark_input_rules() -> Vec<InputRule> {
    vec![
        heading_rule(),
        bullet_rule(),
        ordered_rule(),
        quote_rule(),
        code_rule(),
        divider_rule(),
        task_rule(),
        callout_rule(),
        footnote_rule(),
    ]
}

/// Replace the block's own open and close tokens and drop the marker text,
/// which is what a block conversion is at the token level.
fn retype_block(m: &InputRuleMatch<'_>, markup: Markup) -> Vec<Change> {
    let before = m.block_start - 1;
    let after = m.block_start + m.block.content_size();
    vec![
        Change::replace(
            before,
            before + 1,
            Slice::from_tokens(&[Token::Open(markup.clone())]),
        ),
        Change::delete(m.from, m.to),
        Change::replace(
            after,
            after + 1,
            Slice::from_tokens(&[Token::Close(markup)]),
        ),
    ]
}

/// Wrap the block in `markups`, outermost first, and drop the marker text.
fn wrap_block(m: &InputRuleMatch<'_>, markups: &[Markup]) -> Vec<Change> {
    let before = m.block_start - 1;
    let after = m.block_start + m.block.content_size() + 1;
    let opens: Vec<Token> = markups.iter().cloned().map(Token::Open).collect();
    let closes: Vec<Token> = markups.iter().rev().cloned().map(Token::Close).collect();
    vec![
        Change::insert(before, Slice::from_tokens(&opens)),
        Change::delete(m.from, m.to),
        Change::insert(after, Slice::from_tokens(&closes)),
    ]
}

fn heading_rule() -> InputRule {
    InputRule::new(
        |before| {
            let hashes = before.chars().take_while(|c| *c == '#').count();
            ((1..=6).contains(&hashes) && before.chars().count() == hashes + 1)
                .then_some(hashes + 1)
                .filter(|_| before.ends_with(' '))
        },
        |m| {
            let level = m.text.chars().take_while(|c| *c == '#').count() as i64;
            let heading = m.schema.node_id(md::HEADING)?;
            let markup = markup_of(m.schema, heading, &attrs! {"level" => level});
            Some(spec(retype_block(m, markup)))
        },
    )
}

fn bullet_rule() -> InputRule {
    InputRule::new(
        |before| matches!(before, "- " | "* " | "+ ").then_some(2),
        |m| {
            let bullet = m.text.get(..1).unwrap_or("-");
            let list = m.schema.node_id(md::BULLET_LIST)?;
            let item = m.schema.node_id(md::LIST_ITEM)?;
            let markups = [
                markup_of(m.schema, list, &attrs! {"bullet_char" => bullet}),
                markup_of(m.schema, item, &Attrs::empty()),
            ];
            Some(spec(wrap_block(m, &markups)))
        },
    )
}

fn ordered_rule() -> InputRule {
    InputRule::new(
        |before| {
            let digits = before.chars().take_while(|c| c.is_ascii_digit()).count();
            let delimited = before[digits..].starts_with(['.', ')']);
            ((1..=9).contains(&digits) && delimited && before.chars().count() == digits + 2)
                .then_some(digits + 2)
                .filter(|_| before.ends_with(' '))
        },
        |m| {
            let digits = m.text.chars().take_while(|c| c.is_ascii_digit()).count();
            let start: i64 = m.text[..digits].parse().ok()?;
            let delimiter = m.text.get(digits..digits + 1).unwrap_or(".");
            let list = m.schema.node_id(md::ORDERED_LIST)?;
            let item = m.schema.node_id(md::LIST_ITEM)?;
            let markups = [
                markup_of(
                    m.schema,
                    list,
                    &attrs! {"start" => start, "delimiter" => delimiter},
                ),
                markup_of(m.schema, item, &Attrs::empty()),
            ];
            Some(spec(wrap_block(m, &markups)))
        },
    )
}

fn quote_rule() -> InputRule {
    InputRule::block_start("> ", |m| {
        let quote = m.schema.node_id(md::BLOCKQUOTE)?;
        let markups = [markup_of(m.schema, quote, &Attrs::empty())];
        Some(spec(wrap_block(m, &markups)))
    })
}

fn code_rule() -> InputRule {
    InputRule::new(
        |before| {
            let ticks = before.chars().take_while(|c| *c == '`').count();
            let info = &before[ticks.min(before.len())..];
            (ticks == 3 && before.ends_with(' ') && !info.contains('`'))
                .then(|| before.chars().count())
        },
        |m| {
            let language = m.text[3..].trim().to_string();
            let code = m.schema.node_id(md::CODE_BLOCK)?;
            let markup = markup_of(m.schema, code, &attrs! {"language" => language});
            Some(spec(retype_block(m, markup)))
        },
    )
}

fn divider_rule() -> InputRule {
    InputRule::block_start("---", |m| {
        let divider = m.schema.node_id(md::HORIZONTAL_RULE)?;
        let paragraph = m.schema.node_id(md::PARAGRAPH)?;
        let before = m.block_start - 1;
        let after = m.block_start + m.block.content_size();
        // The rule replaces the whole block with the break and an empty
        // paragraph to carry on typing in.
        let rule_node = m
            .schema
            .create(divider, Attrs::empty(), MarkSet::empty(), Fragment::empty())
            .ok()?;
        let next = m
            .schema
            .create(
                paragraph,
                Attrs::empty(),
                MarkSet::empty(),
                Fragment::empty(),
            )
            .ok()?;
        Some(spec(vec![Change::replace(
            before,
            after + 1,
            Slice::from_fragment(Fragment::from_nodes([rule_node, next])),
        )]))
    })
}

fn footnote_rule() -> InputRule {
    InputRule::new(
        |before| footnote_label(before).map(|_| before.chars().count()),
        |m| {
            let label = footnote_label(m.text)?;
            let definition = m.schema.node_id(md::FOOTNOTE_DEFINITION)?;
            let markups = [markup_of(
                m.schema,
                definition,
                &attrs! {md::FOOTNOTE_LABEL_ATTR => label},
            )];
            Some(spec(wrap_block(m, &markups)))
        },
    )
}

/// The label of a footnote definition's marker, `[^label]: `, when that is
/// all `before` is.
fn footnote_label(before: &str) -> Option<&str> {
    let label = before.strip_prefix("[^")?.strip_suffix("]: ")?;
    (!label.is_empty()
        && !label.contains(|c: char| c.is_whitespace() || matches!(c, '[' | ']' | '^')))
    .then_some(label)
}

fn task_rule() -> InputRule {
    InputRule::new(
        |before| matches!(before, "[ ] " | "[x] " | "[X] ").then_some(4),
        |m| {
            let item_type = m.schema.node_id(md::LIST_ITEM)?;
            let task_type = m.schema.node_id(md::TASK_ITEM)?;
            let resolved = m.doc.resolve(m.block_start).ok()?;
            let depth = resolved.depth();
            if depth < 1 || resolved.index(depth - 1) != 0 {
                return None;
            }
            let item = resolved.node(depth - 1);
            if item.type_id() != item_type {
                return None;
            }
            let checked = m.text.contains(['x', 'X']);
            let markup = markup_of(m.schema, task_type, &attrs! {"checked" => checked});
            let before = resolved.before(depth - 1);
            let after = before + item.node_size() - 1;
            Some(spec(vec![
                Change::replace(
                    before,
                    before + 1,
                    Slice::from_tokens(&[Token::Open(markup.clone())]),
                ),
                Change::delete(m.from, m.to),
                Change::replace(
                    after,
                    after + 1,
                    Slice::from_tokens(&[Token::Close(markup)]),
                ),
            ]))
        },
    )
}

/// `[!note] ` at the start of a block quote turns it into a callout.
///
/// The space is what completes the marker, the way it completes a check box:
/// before it, `[!note]-` may still be growing a fold marker. Only an ordinary
/// quote converts, and only from its own first block, which is where a reader
/// looks for the marker too. A title is not typed here — it is not text once
/// the quote is a callout, and v1 has no way to edit one.
fn callout_rule() -> InputRule {
    InputRule::new(
        |before| {
            let marker = before.strip_suffix(' ')?;
            crate::callout::read_callout(marker)
                .filter(|callout| callout.title.is_empty())
                .map(|_| before.chars().count())
        },
        |m| {
            let quote_type = m.schema.node_id(md::BLOCKQUOTE)?;
            let callout = crate::callout::read_callout(m.text.trim_end_matches(' '))?;
            let resolved = m.doc.resolve(m.block_start).ok()?;
            let depth = resolved.depth();
            // The marker opens the quote, so it sits in its first block.
            if depth < 1 || resolved.index(depth - 1) != 0 {
                return None;
            }
            let quote = resolved.node(depth - 1);
            if quote.type_id() != quote_type {
                return None;
            }
            // A quote that is already a callout has its marker in hand.
            let named = quote
                .attrs()
                .get("callout")
                .and_then(|value| value.as_str())
                .is_some_and(|kind| !kind.is_empty());
            if named {
                return None;
            }
            let in_code = m.schema.mark_id(md::CODE).is_some_and(|code| {
                m.doc
                    .resolve(m.from)
                    .is_ok_and(|resolved| resolved.marks(m.schema).contains_type(code))
            });
            if in_code {
                return None;
            }
            let markup = markup_of(
                m.schema,
                quote_type,
                &attrs! {
                    "callout" => callout.kind,
                    "fold" => callout.fold,
                    "title" => "",
                },
            );
            let before = resolved.before(depth - 1);
            let after = before + quote.node_size() - 1;
            Some(spec(vec![
                Change::replace(
                    before,
                    before + 1,
                    Slice::from_tokens(&[Token::Open(markup.clone())]),
                ),
                Change::delete(m.from, m.to),
                Change::replace(
                    after,
                    after + 1,
                    Slice::from_tokens(&[Token::Close(markup)]),
                ),
            ]))
        },
    )
}

fn spec(changes: Vec<Change>) -> markraft_core::TransactionSpec {
    markraft_core::TransactionSpec::new().changes(changes)
}

/// A paragraph typed as link reference definitions becomes the definitions
/// once the caret leaves it: `[ref]: /url` on a line of
/// its own tells `[a][ref]` where to go rather than standing as text. While
/// the caret is in it the line is still being typed, and a definition only
/// half written would read as something else.
///
/// Registered before [`canonicalise`], whose guard would otherwise escape the
/// `[` to keep the text reading as a paragraph.
fn define_on_leaving(cx: &CorrectionContext<'_>) -> Vec<Change> {
    let schema = cx.start_state.schema();
    let (Some(before), Some(text)) = (cx.before, spelled_definitions(cx)) else {
        return Vec::new();
    };
    let Ok(raw) = schema.node(md::RAW_BLOCK, [schema.text(&text)]) else {
        return Vec::new();
    };
    vec![Change::replace(
        before,
        before + cx.node.node_size(),
        Slice::from_fragment(Fragment::from_node(raw)),
    )]
}

/// The text of a paragraph the caret has just left, when it reads as nothing
/// but link reference definitions.
fn spelled_definitions(cx: &CorrectionContext<'_>) -> Option<String> {
    let schema = cx.start_state.schema();
    if !cx.selection_left || Some(cx.node.type_id()) != schema.node_id(md::PARAGRAPH) {
        return None;
    }
    let items = Items::from_nodes(schema, cx.node.children());
    if items.0.iter().any(|item| matches!(item, Item::Atom(_))) {
        return None;
    }
    let text = items.text();
    crate::textblock::reads_as_definitions(&text).then_some(text)
}

// -- the canonicalising correction -------------------------------------------

/// Bring one textblock back to what its source says. See the module
/// documentation for the steps and their order.
fn canonicalise(cx: &CorrectionContext<'_>) -> Vec<Change> {
    let schema = cx.start_state.schema();
    let Some(kind) = block_kind(schema, cx.node.type_id()) else {
        return Vec::new();
    };
    // A paragraph becoming definitions is replaced whole by
    // [`define_on_leaving`]; a change of its own here would overlap that.
    if spelled_definitions(cx).is_some() {
        return Vec::new();
    }
    // In the first round only a change to this block's characters can have
    // made it wrong. In a later one the document is the corrections' own
    // output, and what touched this block was one of them.
    let first_round = cx.doc.ptr_eq(cx.tr.new_doc());
    let items = Items::from_nodes(schema, cx.node.children());
    let carets = caret_lines(cx, &items);
    let candidates = definition_candidates(schema, cx.doc);
    // A caret leaving the spelling of an atom, even along its own line, is
    // what folds it again.
    let left = cx.selection_left && (left_a_line(cx, &items, &carets) || may_spell_an_atom(&items));
    if first_round
        && !edits_characters(cx)
        && !left
        && candidates == definition_candidates(schema, cx.start_state.doc())
    {
        return Vec::new();
    }
    let lines = settle_lines(cx, kind, &items, &carets);
    if !lines.is_empty() {
        return lines;
    }
    let ctx = definitions_context(&candidates);
    let text = items.text();
    let derived = derive(kind, &text, &ctx);
    let atoms = fold_atoms(cx, &derived);
    if !atoms.is_empty() {
        return atoms;
    }
    let guards = guard_backslashes(cx, kind, &items, &carets);
    if !guards.is_empty() {
        return guards;
    }
    mark_changes(schema, cx.node, cx.content_start, &items, &derived)
}

/// Re-derive the marks of every textblock a change to the document's link
/// reference definitions may have changed the meaning of.
///
/// A definition is a raw block of its own, so editing, adding or deleting one
/// touches no textblock, and [`canonicalise`] is not called for the blocks
/// whose `[a][ref]` it resolves. This is: it runs in the first round of a
/// transaction whose definitions differ from the ones it started with, and
/// sets the marks of each textblock that holds a `]` — no reference link can
/// do without one — to what it reads as against the new definitions. A block
/// the round reaches anyway is [`canonicalise`]'s, which reads it against
/// the same definitions; leaving it alone keeps the two from asking for the
/// same range.
fn follow_definitions(cx: &CorrectionContext<'_>) -> Vec<Change> {
    let schema = cx.start_state.schema();
    if !cx.doc.ptr_eq(cx.tr.new_doc()) {
        return Vec::new();
    }
    let candidates = definition_candidates(schema, cx.doc);
    if candidates == definition_candidates(schema, cx.start_state.doc()) {
        return Vec::new();
    }
    let ctx = definitions_context(&candidates);
    let mut out = Vec::new();
    cx.doc.descendants(&mut |node, pos, _, _| {
        let Some(kind) = block_kind(schema, node.type_id()) else {
            return !node.is_textblock(schema);
        };
        let content_start = pos + 1;
        if cx.touches(content_start, content_start + node.content_size()) {
            return false;
        }
        let items = Items::from_nodes(schema, node.children());
        let text = items.text();
        if text.contains(']') {
            let derived = derive(kind, &text, &ctx);
            out.extend(mark_changes(schema, node, content_start, &items, &derived));
        }
        false
    });
    out
}

/// Whether this transaction changed characters *in this block*.
///
/// Only characters can change what the text says, so a transaction that
/// only moved marks around has nothing here to re-derive. It also keeps the
/// derivation off the blocks an edit never reached.
fn edits_characters(cx: &CorrectionContext<'_>) -> bool {
    let schema = cx.start_state.schema();
    let start = cx.content_start;
    let end = start + cx.node.content_size();
    let text_of = |slice: &Slice| markraft_core::projection::slice_to_plain_text(schema, slice);
    cx.tr
        .changes()
        .iter_changes()
        .iter()
        .any(|change| match change {
            ChangeRange::Marked { .. } => false,
            ChangeRange::Replaced {
                from_a,
                to_a,
                from_b,
                to_b,
                inserted,
            } => {
                if *from_b > end || *to_b < start {
                    return false;
                }
                // A command may rewrite the nodes it covers without moving a
                // character, so a replacement alone proves nothing. Compare
                // the text.
                match cx.start_state.doc().slice(*from_a, *to_a) {
                    Ok(replaced) => text_of(&replaced) != text_of(inserted),
                    Err(_) => true,
                }
            }
        })
}

/// The ends of the selection after the transaction that stand in this block,
/// as offsets into its items.
fn selection_ends(cx: &CorrectionContext<'_>) -> Vec<usize> {
    let start = cx.content_start;
    let end = start + cx.node.content_size();
    cx.selection
        .ranges(cx.doc)
        .iter()
        .flat_map(|range| [range.from, range.to])
        .filter(|pos| (start..=end).contains(pos))
        .map(|pos| pos - start)
        .collect()
}

/// Whether the block's text could hold the spelling of an atom — a picture,
/// a wiki link, a raw HTML tag, an emoji shortcode — that a caret may have been let into. A
/// cheap test, so a caret moving along a line of plain prose derives
/// nothing.
fn may_spell_an_atom(items: &Items) -> bool {
    let text = items.text();
    text.contains("![") || text.contains("[[") || text.contains('<') || text.contains(':')
}

/// The lines of the block a caret or either end of a selection stands on
/// after the transaction, counted from 0 by the line breaks before them.
fn caret_lines(cx: &CorrectionContext<'_>, items: &Items) -> Vec<usize> {
    let ends = cx
        .selection
        .ranges(cx.doc)
        .iter()
        .flat_map(|range| [range.from, range.to])
        .collect::<Vec<_>>();
    lines_at(cx, items, &ends)
}

/// Whether a line of the block that an end of the selection stood on before
/// the transaction has none on it now: the line the caret left, which steps
/// 1 and 3 left alone while it was there and settle now.
fn left_a_line(cx: &CorrectionContext<'_>, items: &Items, carets: &[usize]) -> bool {
    let start = cx.tr.start_state();
    let before = start
        .selection()
        .map(start.schema(), cx.tr.new_doc(), cx.tr.changes().desc());
    let ends = before
        .ranges(cx.tr.new_doc())
        .iter()
        .flat_map(|range| [range.from, range.to])
        .collect::<Vec<_>>();
    lines_at(cx, items, &ends)
        .iter()
        .any(|line| !carets.contains(line))
}

/// The lines of the block the positions among `ends` inside it stand on.
fn lines_at(cx: &CorrectionContext<'_>, items: &Items, ends: &[usize]) -> Vec<usize> {
    let start = cx.content_start;
    let end = start + cx.node.content_size();
    ends.iter()
        .copied()
        .filter(|head| (start..=end).contains(head))
        .map(|head| {
            items.0[..(head - start).min(items.len())]
                .iter()
                .filter(|item| **item == Item::Break)
                .count()
        })
        .collect()
}

/// The document position of the item at `index`.
fn at(cx: &CorrectionContext<'_>, index: usize) -> usize {
    cx.content_start + index
}

/// Whether the break at `index` is spelled by the `\\` before it: an odd run
/// of backslashes, since each pair of them is an escaped backslash.
fn spells_hard_break(items: &[Item], index: usize) -> bool {
    items[..index]
        .iter()
        .rev()
        .take_while(|item| **item == Item::Char('\\'))
        .count()
        % 2
        == 1
}

/// Step 1: a blank line splits the block, a break at either end goes,
/// whitespace starting a line or ending the block goes, and a break a heading
/// of level 3 or more or a table cell cannot hold becomes a space.
fn settle_lines(
    cx: &CorrectionContext<'_>,
    kind: BlockKind,
    items: &Items,
    carets: &[usize],
) -> Vec<Change> {
    let schema = cx.start_state.schema();
    let items = &items.0;
    // A blank line ends a paragraph, so the block becomes as many blocks as
    // it has blank-line runs plus one, all in one round; each new block is
    // this correction's again in the next.
    let mut splits = Vec::new();
    let mut line_start: Option<usize> = None;
    let mut last_split = 0;
    for (index, item) in items.iter().enumerate() {
        match item {
            Item::Break => {
                // Only between two lines that hold something: a break at
                // either end of the block is an edge, settled below.
                if let Some(first) = line_start
                    && first > last_split + 1
                    && index + 1 < items.len()
                {
                    splits.extend(split_block(cx, first - 1, index + 1, splits.is_empty()));
                    last_split = index + 1;
                    line_start = None;
                    continue;
                }
                line_start = Some(index + 1);
            }
            Item::Char(' ' | '\t') => {}
            _ => line_start = None,
        }
    }
    if !splits.is_empty() {
        return splits;
    }
    let flatten = match kind {
        BlockKind::TableCell => true,
        BlockKind::Heading => {
            cx.node
                .attrs()
                .get("level")
                .and_then(|value| value.as_int())
                .unwrap_or(1)
                > 2
        }
        BlockKind::Paragraph => false,
    };
    let last_line = items.iter().filter(|item| **item == Item::Break).count();
    let mut out = Vec::new();
    let mut line = 0;
    let mut at_line_start = true;
    for (index, item) in items.iter().enumerate() {
        match item {
            Item::Break => {
                let edge = (index == 0 && !carets.contains(&0))
                    || (index + 1 == items.len() && !carets.contains(&last_line));
                // The `\` spelling a hard break goes with it: without the
                // break it is a backslash a reader sees.
                let from = if spells_hard_break(items, index) {
                    index - 1
                } else {
                    index
                };
                if flatten {
                    out.push(Change::replace(
                        at(cx, from),
                        at(cx, index + 1),
                        Slice::from_fragment(Fragment::from_node(schema.text(" "))),
                    ));
                } else if edge {
                    out.push(Change::delete(at(cx, from), at(cx, index + 1)));
                }
                line += 1;
                at_line_start = !flatten;
            }
            Item::Char(' ' | '\t') if at_line_start && !carets.contains(&line) => {
                out.push(Change::delete(at(cx, index), at(cx, index + 1)));
            }
            _ => at_line_start = false,
        }
    }
    // Whitespace ending the block is not part of what a reader reads — `**ab** `
    // left by an Enter reads back as `**ab**`. A line holding nothing else was
    // cleared above, and a break still ending the block is settled first:
    // until it goes, the whitespace before it spells a hard break.
    if !at_line_start && !carets.contains(&last_line) && items.last() != Some(&Item::Break) {
        let kept = items
            .iter()
            .rposition(|item| !matches!(item, Item::Char(' ' | '\t')))
            .map_or(0, |index| index + 1);
        if kept < items.len() {
            out.push(Change::delete(at(cx, kept), at(cx, items.len())));
        }
    }
    out
}

/// Replace the items `from..to` — a blank line and the breaks around it —
/// with the end of a block and the start of a paragraph. The first split
/// ends this block; every later one ends the paragraph an earlier one opened.
fn split_block(cx: &CorrectionContext<'_>, from: usize, to: usize, first: bool) -> Vec<Change> {
    let schema = cx.start_state.schema();
    let paragraph = schema.node_id(md::PARAGRAPH).unwrap_or(cx.node.type_id());
    let open = markup_of(schema, paragraph, &Attrs::empty());
    let close = if first {
        markup_of(schema, cx.node.type_id(), cx.node.attrs())
    } else {
        open.clone()
    };
    vec![Change::replace(
        at(cx, from),
        at(cx, to),
        Slice::from_tokens(&[Token::Close(close), Token::Open(open)]),
    )]
}

/// Step 2: text a reader takes for an atom becomes the atom — except where an
/// end of the selection touches it in a transaction that lets the caret in,
/// moves it or types: that is a spelling the caret was let into (see the
/// `unfold` module) or is typing, and it folds once the caret has gone.
///
/// Every spelling folds in the same round. The derivation reports them in
/// order and never one inside another, but a change set cannot hold two
/// overlapping changes, so a spelling that overlaps one already folded waits
/// for the next round — as does one that only a fold exposes.
fn fold_atoms(cx: &CorrectionContext<'_>, derived: &Derived) -> Vec<Change> {
    let schema = cx.start_state.schema();
    let ends = if crate::unfold::keeps_spelling_at_caret(cx.tr) {
        selection_ends(cx)
    } else {
        Vec::new()
    };
    let mut folded_to = 0;
    derived
        .atoms
        .iter()
        .filter(|atom| {
            !ends
                .iter()
                .any(|end| (atom.range.start..=atom.range.end).contains(end))
        })
        .filter(|atom| {
            let disjoint = atom.range.start >= folded_to;
            if disjoint {
                folded_to = atom.range.end;
            }
            disjoint
        })
        .filter_map(|atom| {
            let ty = schema.node_id(atom.node_type)?;
            let node = schema
                .create(ty, atom.attrs.clone(), MarkSet::empty(), Fragment::empty())
                .ok()?;
            Some(Change::replace(
                at(cx, atom.range.start),
                at(cx, atom.range.end),
                Slice::from_fragment(Fragment::from_node(node)),
            ))
        })
        .collect()
}

/// Step 3: the backslashes that keep each line from opening a block, except
/// on a line a caret stands on.
fn guard_backslashes(
    cx: &CorrectionContext<'_>,
    kind: BlockKind,
    items: &Items,
    carets: &[usize],
) -> Vec<Change> {
    let schema = cx.start_state.schema();
    let marker = item_marker(schema, cx.doc, cx.content_start);
    items
        .guard_insertions(schema, kind, marker.as_deref())
        .into_iter()
        .filter(|at| {
            let line = items.0[..*at]
                .iter()
                .filter(|item| **item == Item::Break)
                .count();
            !carets.contains(&line)
        })
        .map(|index| {
            Change::insert(
                at(cx, index),
                Slice::from_fragment(Fragment::from_node(schema.text("\\"))),
            )
        })
        .collect()
}

/// Step 4: the mark changes that make every derived mark type over `node`,
/// whose content starts at `content_start`, what the derivation says.
///
/// One [`Change::set_marks`] per run of positions whose marks differ from
/// their target and share one target: the target keeps every mark type this
/// kind does not derive and takes the derived ones from the derivation. The
/// runs never overlap, so the whole block settles in one round.
fn mark_changes(
    schema: &Schema,
    node: &Node,
    content_start: usize,
    items: &Items,
    derived: &Derived,
) -> Vec<Change> {
    let wanted = derived_marks(schema, derived, items.len());
    let mut current: Vec<&MarkSet> = Vec::with_capacity(items.len());
    for child in node.children() {
        let count = child.text().map_or(1, |text| text.chars().count());
        current.extend(std::iter::repeat_n(child.marks(), count));
    }
    if current.len() != wanted.len() {
        return Vec::new();
    }
    let types = derived_mark_types(schema);
    // `None` where a position already carries its target.
    let targets = current.iter().zip(&wanted).map(|(current, wanted)| {
        let kept = current.filter(|mark| !types.contains(&mark.ty));
        let target = wanted
            .iter()
            .fold(kept, |set, mark| set.add(schema, mark.clone()));
        (target != **current).then_some(target)
    });
    let mut out = Vec::new();
    let mut run: Option<(usize, MarkSet)> = None;
    for (index, next) in targets.chain([None]).enumerate() {
        if let Some((_, open)) = &run
            && next.as_ref() == Some(open)
        {
            continue;
        }
        if let Some((from, marks)) = run.take() {
            out.push(Change::set_marks(
                content_start + from,
                content_start + index,
                marks,
            ));
        }
        run = next.map(|set| (index, set));
    }
    out
}
