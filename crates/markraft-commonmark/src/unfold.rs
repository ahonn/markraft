//! An atom the caret reaches, spelled out as the source it was read from.
//!
//! A picture, a wiki link, a raw HTML tag and an emoji shortcode are atoms:
//! the canonicalising correction folds the text that spells one into a single
//! node, which is one position wide, so a caret could only stand before or
//! after it. As in
//! Typora, a caret that reaches one — an arrow key onto it, a click beside it
//! — now finds its source instead: a transaction that moves the selection,
//! and changes nothing else, and puts an end of it against an atom replaces
//! the atom with its spelling, and the
//! caret can walk into `![alt](src)` and edit it like any other text. The
//! correction leaves a spelling alone while a caret touches it — when the
//! caret was let into it, moves about in it or types it — and folds it again
//! once the caret has gone. An edit that only happens to leave the caret
//! against an atom — a paste ending in a picture, a document replaced — does
//! neither: what it brings in is folded, and what was an atom stays one.
//!
//! Neither step changes what is written to the file: the atom and its
//! spelling save as the same characters. So the unfolding is annotated
//! [`fold_into_previous`], as the correction's folding on a caret move is:
//! neither is an edit of its own to undo.
//!
//! Nothing is unfolded in a transaction the history does not record — an
//! undo, a redo — or one from another peer: they restore or relay a
//! selection rather than move it.
//!
//! # Why a filter
//!
//! The selection reaching a block changes nothing in it, so no correction
//! runs for it. A [`transaction_filter`] sees every transaction, and what it
//! adds is seen by the corrections that follow, which derive the unfolded
//! text's marks in the same transaction.

use std::sync::{Arc, LazyLock};

use markraft_core::protocol::fold_into_previous;
use markraft_core::{
    AnnotationType, Change, Extension, Fragment, Node, Schema, Selection, Slice, Transaction,
    TransactionFilterFn, TransactionSpec, transaction_filter,
};

use crate::schema as md;
use crate::textblock::{atom_spelling, block_kind};

/// Set on a transaction the filter extended to spell out an atom.
static UNFOLDS: LazyLock<AnnotationType<bool>> = LazyLock::new(AnnotationType::define);

static EXTENSION: LazyLock<Extension> = LazyLock::new(|| {
    let unfold: TransactionFilterFn = Arc::new(unfold);
    transaction_filter().of(unfold)
});

/// The filter that spells out an atom the selection reaches.
pub(crate) fn unfold_atoms() -> Extension {
    EXTENSION.clone()
}

/// Whether the correction should leave a spelling the caret touches as text in
/// `tr`: one that lets the caret into an atom, one that only moves the caret,
/// one that types, or an undo or a redo.
///
/// An undo puts back the caret the undone edit started from, so a spelling it
/// restores is one the caret was in, which was text then. Folding it would
/// append a change the history never recorded, and the next undo, written
/// against the text, would no longer apply.
pub(crate) fn keeps_spelling_at_caret(tr: &Transaction) -> bool {
    tr.annotation(&UNFOLDS) == Some(&true)
        || !tr.doc_changed()
        || tr.is_user_event("input.type")
        || tr.replays_history()
}

/// Whether `node` is an atom a reader wrote as text: one the caret should find
/// spelled out.
pub(crate) fn is_spelled_atom(schema: &Schema, node: &Node) -> bool {
    matches!(
        schema.node_type(node.type_id()).name(),
        md::IMAGE | md::WIKI_LINK | md::RAW_INLINE | md::EMOJI
    )
}

/// Replace every atom an end of the selection stands against with its
/// spelling. See the module documentation.
fn unfold(tr: &Transaction) -> Option<Vec<TransactionSpec>> {
    if tr.doc_changed() || !tr.recorded_in_history() {
        return None;
    }
    let schema = tr.start_state().schema();
    let doc = tr.new_doc();
    let Selection::Text { anchor, head } = tr.new_selection() else {
        return None;
    };
    // The atoms as the positions before them, each with its spelling.
    let mut atoms: Vec<(usize, String)> = Vec::new();
    for end in [anchor, head] {
        for (pos, node) in atoms_against(schema, doc, end) {
            if !atoms.iter().any(|(at, _)| *at == pos) {
                atoms.push((pos, atom_spelling(schema, &node)));
            }
        }
    }
    if atoms.is_empty() {
        return None;
    }
    atoms.sort_by_key(|(pos, _)| *pos);
    // An end after an atom goes to the end of its spelling, one before it
    // stays where the spelling starts.
    let map = |end: usize| {
        end + atoms
            .iter()
            .filter(|(pos, _)| *pos < end)
            .map(|(_, spelling)| spelling.chars().count() - 1)
            .sum::<usize>()
    };
    let changes = atoms.iter().map(|(pos, spelling)| {
        Change::replace(
            *pos,
            pos + 1,
            Slice::from_fragment(Fragment::from_node(schema.text(spelling))),
        )
    });
    let spelled = TransactionSpec::new()
        .changes(changes)
        .sequential()
        .selection(Selection::text(map(anchor), map(head)))
        .annotate(UNFOLDS.of(true))
        .annotate(fold_into_previous().of(true));
    Some(vec![tr.as_spec(), spelled])
}

/// The spelled atoms directly before and after `pos`, as the positions before
/// them, when `pos` stands in a textblock of inline source.
///
/// A `<br>` in a table cell is left folded: it is the line break the cell is
/// drawn with, as Typora draws it, not markup to walk into.
fn atoms_against(schema: &Schema, doc: &Node, pos: usize) -> Vec<(usize, Node)> {
    let Ok(resolved) = doc.resolve(pos) else {
        return Vec::new();
    };
    if block_kind(schema, resolved.parent().type_id()).is_none() {
        return Vec::new();
    }
    let in_cell = schema.node_id(md::TABLE_CELL) == Some(resolved.parent().type_id());
    let cell_break = |node: &Node| {
        in_cell
            && schema.node_id(md::RAW_INLINE) == Some(node.type_id())
            && node
                .attrs()
                .get("source")
                .and_then(|value| value.as_str())
                .is_some_and(crate::parse::is_break_tag)
    };
    let spelled =
        |node: &Node| node.text().is_none() && is_spelled_atom(schema, node) && !cell_break(node);
    let mut out = Vec::new();
    if let Some(before) = resolved.node_before().filter(spelled) {
        out.push((pos - 1, before));
    }
    if let Some(after) = resolved.node_after().filter(spelled) {
        out.push((pos, after));
    }
    out
}
