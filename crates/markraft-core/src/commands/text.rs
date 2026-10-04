//! Inserting content and deleting ranges.

use crate::change::{Change, TrackMode};
use crate::fit::Fit;
use crate::fragment::Fragment;
use crate::mark::MarkSet;
use crate::node::Node;
use crate::pos::ResolvedPos;
use crate::schema::{NodeTypeId, Schema};
use crate::selection::{ReplacementStyle, Selection};
use crate::slice::{Slice, Token};
use crate::state::{EditorState, TransactionSpec};

use super::structure::can_replace;
use super::{Command, command, resolve_changes};
use crate::protocol::event;

/// Type `text` over the selection.
///
/// The inserted text carries the state's stored marks when it has any and
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
    let (mut from, mut to) = (range.from, range.to);
    let resolved = doc.resolve(from).ok()?;
    let marks = marks_for_insertion(state, &resolved);
    let inherited = resolved.inherited_marks(schema);
    if matches!(state.selection(), Selection::Custom(_)) {
        let local_marks = resolved.local_marks(schema);
        let local = marks.filter(|mark| !inherited.contains(mark) || local_marks.contains(mark));
        let slice = Slice::from_fragment(Fragment::from_node(schema.text_marked(text, local)));
        let mut spec =
            custom_replacement_spec(state, slice, ReplacementStyle::Receiving, event::INPUT_TYPE)?;
        if state.stored_marks().is_some() {
            spec = spec.stored_marks(Some(marks));
        }
        return Some(spec);
    }
    let leave_scope = state.stored_marks().is_some()
        && inherited.iter().any(|mark| !marks.contains(mark))
        && from == to;
    let (slice, explicit_caret) = if leave_scope {
        // A stored-mark toggle can turn off a mark inherited from an inline
        // container. Type between split scopes so the ancestor cannot reapply it.
        let scopes: Vec<_> = (1..=resolved.depth())
            .filter(|&d| schema.node_type(resolved.node(d).type_id()).is_inline())
            .map(|d| resolved.node(d).markup().clone())
            .collect();
        let mut left_empty = 0;
        let mut right_empty = 0;
        for d in (1..=resolved.depth()).rev() {
            if !schema.node_type(resolved.node(d).type_id()).is_inline()
                || from != resolved.start(d)
            {
                break;
            }
            from = resolved.before(d);
            left_empty += 1;
        }
        for d in (1..=resolved.depth()).rev() {
            if !schema.node_type(resolved.node(d).type_id()).is_inline() || to != resolved.end(d) {
                break;
            }
            to = resolved.after(d);
            right_empty += 1;
        }
        let mut tokens: Vec<_> = scopes[..scopes.len() - left_empty]
            .iter()
            .rev()
            .cloned()
            .map(Token::Close)
            .collect();
        tokens.push(Token::Node(schema.text_marked(text, marks.clone())));
        let caret = from + scopes.len() - left_empty + text.chars().count();
        tokens.extend(
            scopes[..scopes.len() - right_empty]
                .iter()
                .cloned()
                .map(Token::Open),
        );
        (Slice::from_tokens(&tokens), Some(caret))
    } else {
        let local_marks = resolved.local_marks(schema);
        let local = marks.filter(|mark| !inherited.contains(mark) || local_marks.contains(mark));
        (
            Slice::from_fragment(Fragment::from_node(schema.text_marked(text, local))),
            None,
        )
    };
    let (set, new_doc) = resolve_changes(
        state,
        vec![Change::replace(from, to, slice).with_fit(Fit::Auto)],
    )?;
    let caret = explicit_caret.unwrap_or_else(|| {
        set.map_pos(to, 1, TrackMode::Simple)
            .unwrap_or_else(|| new_doc.content_size())
    });
    // Fitting may wrap text in a paragraph. Its closing token is not a text
    // caret position, so find the end of the inserted inline content.
    let caret = Selection::find_from(schema, &new_doc, caret, -1, true)?.head(&new_doc);
    let selection = Selection::cursor(caret);
    selection.check(&new_doc, schema).ok()?;
    let mut spec = TransactionSpec::new()
        .change_set(set)
        .selection(selection)
        .user_event(event::INPUT_TYPE)
        .scroll_into_view();
    // Stored marks survive typing, so several characters in a row share them.
    if state.stored_marks().is_some() {
        spec = spec.stored_marks(Some(marks));
    }
    Some(spec)
}

/// The marks content inserted at `resolved` should carry.
pub(crate) fn marks_for_insertion(state: &EditorState, resolved: &ResolvedPos) -> MarkSet {
    let schema = state.schema();
    let parent_ty = schema.node_type(resolved.parent().type_id());
    state
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
    if matches!(state.selection(), Selection::Custom(_)) {
        return custom_replacement_spec(state, slice, ReplacementStyle::Own, event);
    }
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
/// The range is widened to whole nodes where deleting their content alone
/// would leave a node the schema rejects.
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
    replace_selection_as(slice, ReplacementStyle::Own, event::INPUT_PASTE)
}

/// Replace through the same selection contract, saying where the content gets
/// its styles and what the edit is. Typing and literal-text insertion take the
/// receiving style, so a visible selection keeps it; a rich paste brings its own.
pub fn replace_selection_as(slice: Slice, style: ReplacementStyle, event: &str) -> Command {
    let event = event.to_owned();
    command(move |state| {
        if matches!(state.selection(), Selection::Custom(_)) {
            return custom_replacement_spec(state, slice.clone(), style, &event);
        }
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
                .user_event(&event)
                .scroll_into_view(),
        )
    })
}

/// Resolve a custom selection's replacement contract without running extension
/// filters during command construction. The hook owns its changes and may own
/// the resulting caret; ordinary command metadata is applied at this boundary.
pub(super) fn custom_replacement_spec(
    state: &EditorState,
    slice: Slice,
    style: ReplacementStyle,
    event: &str,
) -> Option<TransactionSpec> {
    let Selection::Custom(kind) = state.selection() else {
        return None;
    };
    let range = kind.replacement_range(state.doc());
    let spec = kind.replace_with_schema(
        TransactionSpec::new().user_event(event),
        state.doc(),
        state.schema(),
        slice,
        style,
    );
    let (changes, doc, explicit_selection) = spec.resolve_edit(state).ok()?;
    if changes.is_empty() {
        return None;
    }
    let selection = explicit_selection.unwrap_or_else(|| {
        let end = changes
            .map_pos(range.to, 1, TrackMode::Simple)
            .unwrap_or(doc.content_size());
        Selection::near(state.schema(), &doc, end, -1)
    });
    selection.check(&doc, state.schema()).ok()?;
    Some(
        spec.change_set(changes)
            .selection(selection)
            .scroll_into_view(),
    )
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
    // An empty textblock has no text for a slice's open start to carry on: a
    // cut from inside a list pasted there is the list again, its first item
    // included, rather than that item's text in the empty block.
    let empty_block = from == to
        && resolved_from.depth() > 0
        && resolved_from.parent().content_size() == 0
        && schema
            .node_type(resolved_from.parent().type_id())
            .has_inline_content();
    let opens_container = slice.open_start() > 0
        && slice
            .content()
            .first_child()
            .is_some_and(|node| !schema.node_type(node.type_id()).is_textblock());
    let closed;
    let slice = if empty_block && opens_container {
        closed = Slice::new(slice.content().clone(), 0, slice.open_end());
        &closed
    } else {
        slice
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
    if let Some(change) = items_for_empty_item(schema, &resolved_from, &resolved_to, slice) {
        return vec![change];
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

/// A list pasted into an empty item of a list: the pasted items take that
/// item's place, beside the items around it, rather than nesting a list of
/// their own inside it — where the item before would read as holding them.
///
/// The empty textblock must be all the item holds, and every pasted item one
/// the surrounding list can hold, so an ordered list pasted into a bullet list
/// gives it more items rather than a list inside one.
fn items_for_empty_item(
    schema: &Schema,
    from: &ResolvedPos,
    to: &ResolvedPos,
    slice: &Slice,
) -> Option<Change> {
    let depth = from.depth();
    let block = from.parent();
    if depth < 2
        || !from.same_parent(to)
        || block.content_size() != 0
        || slice.open_end() != 0
        || slice.content().child_count() != 1
    {
        return None;
    }
    let item = from.node(depth - 1);
    let list = from.node(depth - 2);
    let pasted = slice.content().first_child()?;
    let fits = |node: &Node| {
        schema.can_contain(list.type_id(), node.type_id())
            && schema.can_contain(list.type_id(), item.type_id())
    };
    if item.child_count() != 1
        || pasted.type_id() == item.type_id()
        || pasted.child_count() == 0
        || !pasted.children().all(fits)
    {
        return None;
    }
    let items = Fragment::from_nodes(pasted.children().cloned());
    Some(
        Change::replace(
            from.before(depth - 1),
            from.after(depth - 1),
            Slice::from_fragment(items),
        )
        .with_fit(Fit::Auto),
    )
}
