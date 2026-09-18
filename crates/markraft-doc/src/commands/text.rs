//! Inserting content and deleting ranges.

use crate::change::{Change, TrackMode};
use crate::fit::Fit;
use crate::fragment::Fragment;
use crate::mark::MarkSet;
use crate::node::Node;
use crate::pos::ResolvedPos;
use crate::schema::{NodeTypeId, Schema};
use crate::selection::Selection;
use crate::slice::{Slice, Token};
use crate::state::{EditorState, TransactionSpec};

use super::structure::can_replace;
use super::{Command, command, resolve_changes};

/// Type `text` over the selection.
///
/// The inserted text carries the selection's stored marks when it has any and
/// the marks at the insertion point otherwise, in both cases filtered to the
/// marks the destination allows — which is why typing into a code block whose
/// type declares no marks never carries emphasis in.
pub fn insert_text(text: &str) -> Command {
    let text = text.to_string();
    command(move |state| insert_text_spec(state, &text))
}

pub(crate) fn insert_text_spec(state: &EditorState, text: &str) -> Option<TransactionSpec> {
    if text.is_empty() {
        return None;
    }
    let doc = state.doc();
    let schema = state.schema();
    schema.text_type()?;
    let range = state.selection().replacement_range(doc);
    let (from, to) = (range.from, range.to);
    let resolved = doc.resolve(from).ok()?;
    let marks = marks_for_insertion(state, &resolved);
    let slice = Slice::from_fragment(Fragment::from_node(schema.text_marked(text, marks.clone())));
    let (set, new_doc) = resolve_changes(
        state,
        vec![Change::replace(from, to, slice).with_fit(Fit::Auto)],
    )?;
    let caret = set
        .map_pos(to, 1, TrackMode::Simple)
        .unwrap_or_else(|| new_doc.content_size());
    // Stored marks survive typing, so several characters in a row share them.
    let selection = if state.selection().stored_marks().is_some() {
        Selection::cursor_with_marks(caret, marks)
    } else {
        Selection::cursor(caret)
    };
    selection.check(&new_doc, schema).ok()?;
    Some(
        TransactionSpec::new()
            .change_set(set)
            .selection(selection)
            .user_event("input.type")
            .scroll_into_view(),
    )
}

/// The marks content inserted at `resolved` should carry.
pub(crate) fn marks_for_insertion(state: &EditorState, resolved: &ResolvedPos) -> MarkSet {
    let schema = state.schema();
    let parent_ty = schema.node_type(resolved.parent().type_id());
    state
        .selection()
        .stored_marks()
        .cloned()
        .unwrap_or_else(|| resolved.marks(schema))
        .filter(|mark| parent_ty.allows_mark_in_content(mark.ty))
}

/// Insert a leaf of `node_type` — a hard break — over the selection.
pub fn insert_hard_break(node_type: NodeTypeId) -> Command {
    command(move |state| {
        let schema = state.schema();
        if !schema.node_type(node_type).is_leaf() || !schema.node_type(node_type).is_inline() {
            return None;
        }
        let doc = state.doc();
        let range = state.selection().replacement_range(doc);
        let resolved = doc.resolve(range.from).ok()?;
        let marks = marks_for_insertion(state, &resolved);
        let node = schema
            .create(
                node_type,
                schema.node_type(node_type).default_attrs().clone(),
                marks,
                Fragment::empty(),
            )
            .ok()?;
        insert_node_spec(state, node, "insert")
    })
}

/// Insert a complete node — typically an atom — over the selection.
pub fn insert_node(node: Node) -> Command {
    command(move |state| insert_node_spec(state, node.clone(), "insert"))
}

fn insert_node_spec(state: &EditorState, node: Node, event: &str) -> Option<TransactionSpec> {
    let doc = state.doc();
    let schema = state.schema();
    let range = state.selection().replacement_range(doc);
    let slice = Slice::from_fragment(Fragment::from_node(node));
    let (set, new_doc) = resolve_changes(
        state,
        vec![Change::replace(range.from, range.to, slice).with_fit(Fit::Auto)],
    )?;
    let end = set
        .map_pos(range.to, 1, TrackMode::Simple)
        .unwrap_or_else(|| new_doc.content_size());
    Some(
        TransactionSpec::new()
            .change_set(set)
            .selection(Selection::near(schema, &new_doc, end, -1))
            .user_event(event)
            .scroll_into_view(),
    )
}

/// Delete `from..to`, removing containers the deletion would empty.
///
/// Mirrors ProseMirror's `deleteRange`: the range is widened to whole nodes
/// where deleting their content alone would leave a node the schema rejects.
pub fn delete_range(from: usize, to: usize) -> Command {
    command(move |state| {
        let changes = delete_range_changes(state.schema(), state.doc(), from, to);
        super::changes_spec(state, changes, "delete")
    })
}

/// The changes [`delete_range`] performs, for callers that build their own
/// transaction.
pub fn delete_range_changes(schema: &Schema, doc: &Node, from: usize, to: usize) -> Vec<Change> {
    let plain = || vec![Change::delete(from, to).with_fit(Fit::Auto)];
    let (Ok(resolved_from), Ok(resolved_to)) = (doc.resolve(from), doc.resolve(to)) else {
        return plain();
    };
    let covered = covered_depths(schema, &resolved_from, &resolved_to);
    for (i, depth) in covered.iter().copied().enumerate() {
        let last = i == covered.len() - 1;
        if (last && depth == 0)
            || schema
                .content_expr(resolved_from.node(depth).type_id())
                .start()
                .valid_end()
        {
            return vec![
                Change::delete(resolved_from.start(depth), resolved_to.end(depth))
                    .with_fit(Fit::Auto),
            ];
        }
        if depth > 0
            && (last
                || can_replace(
                    schema,
                    resolved_from.node(depth - 1),
                    resolved_from.index(depth - 1),
                    resolved_to.index_after(depth - 1),
                ))
        {
            return vec![
                Change::delete(resolved_from.before(depth), resolved_to.after(depth))
                    .with_fit(Fit::Auto),
            ];
        }
    }
    for depth in 1..=resolved_from.depth().min(resolved_to.depth()) {
        if from - resolved_from.start(depth) == resolved_from.depth() - depth
            && to > resolved_from.end(depth)
            && resolved_to.end(depth) - to != resolved_to.depth() - depth
            && resolved_from.start(depth - 1) == resolved_to.start(depth - 1)
            && can_replace(
                schema,
                resolved_from.node(depth - 1),
                resolved_from.index(depth - 1),
                resolved_to.index(depth - 1),
            )
        {
            return vec![Change::delete(resolved_from.before(depth), to).with_fit(Fit::Auto)];
        }
    }
    plain()
}

/// The depths at which both positions sit exactly at the edge of the same
/// node, deepest first.
fn covered_depths(schema: &Schema, from: &ResolvedPos, to: &ResolvedPos) -> Vec<usize> {
    let mut out = Vec::new();
    let min_depth = from.depth().min(to.depth());
    for depth in (0..=min_depth).rev() {
        let start = from.start(depth);
        if start + (from.depth() - depth) < from.pos()
            || to.end(depth) > to.pos() + (to.depth() - depth)
            || schema.node_type(from.node(depth).type_id()).is_isolating()
            || schema.node_type(to.node(depth).type_id()).is_isolating()
        {
            break;
        }
        let inline_pair = depth == from.depth()
            && depth == to.depth()
            && schema
                .node_type(from.parent().type_id())
                .has_inline_content()
            && schema.node_type(to.parent().type_id()).has_inline_content()
            && depth > 0
            && to.start(depth - 1) + 1 == start;
        if start == to.start(depth) || inline_pair {
            out.push(depth);
        }
    }
    out
}

/// Replace the selection with `slice`, merging its open edges with the
/// surrounding content.
///
/// A slice with open sides is exactly what a copy produces: its first and last
/// nodes are cut open, so their inline content merges with the textblock the
/// caret sits in while the closed nodes in between stay whole. A fully closed
/// slice of block content splits the textblock around it instead.
pub fn replace_selection(slice: Slice) -> Command {
    command(move |state| {
        let doc = state.doc();
        let schema = state.schema();
        let range = state.selection().replacement_range(doc);
        let changes = replace_selection_changes(schema, doc, range.from, range.to, &slice);
        let (set, new_doc) = resolve_changes(state, changes)?;
        let end = set
            .map_pos(range.to, 1, TrackMode::Simple)
            .unwrap_or_else(|| new_doc.content_size());
        Some(
            TransactionSpec::new()
                .change_set(set)
                .selection(Selection::near(schema, &new_doc, end, -1))
                .user_event("input.paste")
                .scroll_into_view(),
        )
    })
}

/// The changes [`replace_selection`] performs.
///
/// Exposed so a paste handler can combine them with its own edits.
pub fn replace_selection_changes(
    schema: &Schema,
    doc: &Node,
    from: usize,
    to: usize,
    slice: &Slice,
) -> Vec<Change> {
    if slice.is_empty() {
        return delete_range_changes(schema, doc, from, to);
    }
    let (Ok(resolved_from), Ok(resolved_to)) = (doc.resolve(from), doc.resolve(to)) else {
        return vec![Change::replace(from, to, slice.clone()).with_fit(Fit::Auto)];
    };
    let into_inline = schema
        .node_type(resolved_from.parent().type_id())
        .has_inline_content();
    let block_first = slice.open_start() == 0
        && slice
            .content()
            .first_child()
            .is_some_and(|node| schema.node_type(node.type_id()).is_block());
    if !into_inline || !block_first || resolved_from.depth() == 0 {
        return vec![Change::replace(from, to, slice.clone()).with_fit(Fit::Auto)];
    }

    // Closed block content cannot merge into a textblock, so split it open.
    let mut start = from;
    let mut end = to;
    let mut tokens: Vec<Token> = Vec::new();
    if resolved_from.parent_offset() == 0 {
        start = resolved_from.before(resolved_from.depth());
    } else {
        tokens.push(Token::Close(resolved_from.parent().markup().clone()));
    }
    tokens.extend(slice.tokens());
    if resolved_to.depth() > 0 && resolved_to.parent_offset() == resolved_to.parent().content_size()
    {
        end = resolved_to.after(resolved_to.depth());
    } else {
        tokens.push(Token::Open(resolved_to.parent().markup().clone()));
    }
    vec![Change::replace(start, end, Slice::from_tokens(&tokens)).with_fit(Fit::Auto)]
}
