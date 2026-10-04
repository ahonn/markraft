//! Deleting, joining and selecting.
//!
//! These are the commands a backspace, delete or navigation key binds to.
//! Everything is expressed as token-level changes in one change set, so a
//! command that both deletes and re-shapes the tree stays a single, invertible
//! edit.

use crate::change::Change;
use crate::fit::Fit;
use crate::node::Node;
use crate::pos::ResolvedPos;
use crate::schema::{NodeTypeId, Schema};
use crate::selection::{ReplacementStyle, Selection};
use crate::slice::{Slice, Token};
use crate::state::{EditorState, TransactionSpec};

use super::structure::{
    can_join, can_replace, content_match_at, join_changes, join_point, lift_changes, lift_target,
    markup_of,
};
use super::{
    Command, at_block_end, at_block_start, changes_spec, command, find_cut_after, find_cut_before,
    resolve_changes,
};
use crate::protocol::event;

/// Delete everything the selection covers, leaving a caret where it was.
///
/// The caret is set rather than mapped: a selection of the whole document
/// maps to one again, and what is typed next would then replace a range
/// instead of going in at a caret.
pub fn delete_selection() -> Command {
    command(|state| {
        let doc = state.doc();
        if state.selection().is_empty(doc) {
            return None;
        }
        if matches!(state.selection(), Selection::Custom(_)) {
            return super::text::custom_replacement_spec(
                state,
                Slice::empty(),
                ReplacementStyle::Own,
                event::DELETE_SELECTION,
            );
        }
        let range = state.selection().replacement_range(doc);
        let (set, new_doc) = resolve_changes(
            state,
            vec![Change::delete(range.from, range.to).with_fit(Fit::Auto)],
        )?;
        let at = set
            .map_pos(range.from, -1, crate::change::TrackMode::Simple)
            .unwrap_or(0);
        Some(
            TransactionSpec::new()
                .change_set(set)
                .selection(Selection::near(state.schema(), &new_doc, at, 1))
                .user_event(event::DELETE_SELECTION)
                .scroll_into_view(),
        )
    })
}

/// Delete backwards over the boundary before the cursor.
///
/// Joins the current textblock with what comes before it, lifting or wrapping
/// when a plain join would not produce valid content. Applies only to a cursor
/// at the start of a textblock.
pub fn join_backward() -> Command {
    command(|state| {
        let cursor = at_block_start(state)?;
        let schema = state.schema();
        match find_cut_before(schema, &cursor) {
            Some(cut) => delete_barrier(state, cut, -1, "delete.backward"),
            None => {
                // Nothing before the block inside its ancestors: lift it out.
                let range = cursor.block_range(schema, &cursor, None)?;
                let target = lift_target(schema, &range)?;
                changes_spec(state, lift_changes(&range, target), "delete.backward")
            }
        }
    })
}

/// Delete forwards over the boundary after the cursor.
pub fn join_forward() -> Command {
    command(|state| {
        let cursor = at_block_end(state)?;
        let cut = find_cut_after(state.schema(), &cursor)?;
        delete_barrier(state, cut, 1, "delete.forward")
    })
}

/// Select the node before the cursor, when there is a selectable one.
pub fn select_node_backward() -> Command {
    command(|state| select_node_at_cut(state, -1))
}

/// Select the node after the cursor, when there is a selectable one.
pub fn select_node_forward() -> Command {
    command(|state| select_node_at_cut(state, 1))
}

fn select_node_at_cut(state: &EditorState, dir: i32) -> Option<TransactionSpec> {
    let doc = state.doc();
    let schema = state.schema();
    let selection = state.selection();
    if !selection.is_empty(doc) {
        return None;
    }
    let head = doc.resolve(selection.head(doc)).ok()?;
    let cut = if head.parent().is_textblock(schema) {
        if dir < 0 {
            if head.parent_offset() > 0 {
                return None;
            }
            find_cut_before(schema, &head)?
        } else {
            if head.parent_offset() < head.parent().content_size() {
                return None;
            }
            find_cut_after(schema, &head)?
        }
    } else {
        head.pos()
    };
    let resolved = doc.resolve(cut).ok()?;
    let pos = if dir < 0 {
        cut.checked_sub(resolved.node_before()?.node_size())?
    } else {
        cut
    };
    if !Selection::is_selectable(schema, doc, pos) {
        return None;
    }
    Some(
        TransactionSpec::new()
            .selection(Selection::node(pos))
            .user_event(event::SELECT)
            .scroll_into_view(),
    )
}

/// Merge the textblock before the cursor with the one the cursor is in.
///
/// Unlike [`join_backward`] this always merges the two textblocks' content
/// rather than re-shaping the tree, which is what a host that wants
/// "backspace always deletes a character or a boundary" binds.
pub fn join_textblock_backward() -> Command {
    command(|state| {
        let cursor = at_block_start(state)?;
        let cut = find_cut_before(state.schema(), &cursor)?;
        join_textblocks_around(state, cut, "delete.backward")
    })
}

/// The forward counterpart of [`join_textblock_backward`].
pub fn join_textblock_forward() -> Command {
    command(|state| {
        let cursor = at_block_end(state)?;
        let cut = find_cut_after(state.schema(), &cursor)?;
        join_textblocks_around(state, cut, "delete.forward")
    })
}

/// Delete from the end of the last textblock before `cut` to the start of the
/// first textblock after it, which merges the two.
fn join_textblocks_around(state: &EditorState, cut: usize, event: &str) -> Option<TransactionSpec> {
    let doc = state.doc();
    let schema = state.schema();
    let resolved = doc.resolve(cut).ok()?;
    let mut before = resolved.node_before()?;
    let mut before_pos = cut.checked_sub(1)?;
    while !before.is_textblock(schema) {
        if schema.node_type(before.type_id()).is_isolating() {
            return None;
        }
        before = before.last_child()?.clone();
        before_pos = before_pos.checked_sub(1)?;
    }
    let mut after = resolved.node_after()?;
    let mut after_pos = cut + 1;
    while !after.is_textblock(schema) {
        if schema.node_type(after.type_id()).is_isolating() {
            return None;
        }
        after = after.first_child()?.clone();
        after_pos += 1;
    }
    let (set, _) = resolve_changes(
        state,
        vec![Change::delete(before_pos, after_pos).with_fit(Fit::Auto)],
    )?;
    Some(
        TransactionSpec::new()
            .change_set(set)
            .selection(Selection::cursor(before_pos))
            .user_event(event)
            .scroll_into_view(),
    )
}

/// The heart of [`join_backward`]/[`join_forward`]: remove the boundary at
/// `cut`, choosing between removing an empty container, joining, wrapping the
/// node after into the one before, lifting and merging textblocks, trying them
/// in that order.
fn delete_barrier(
    state: &EditorState,
    cut: usize,
    dir: i32,
    event: &str,
) -> Option<TransactionSpec> {
    let doc = state.doc();
    let schema = state.schema();
    let resolved = doc.resolve(cut).ok()?;
    let before = resolved.node_before()?;
    let after = resolved.node_after()?;
    let isolated = schema.node_type(before.type_id()).is_isolating()
        || schema.node_type(after.type_id()).is_isolating();
    let index = resolved.index(resolved.depth());
    let parent = resolved.parent();

    if !isolated {
        // An empty *container* before the cut is simply removed. A leaf is not:
        // it is content in its own right, and the chain's `select_node_backward`
        // is what offers it up for deletion.
        if !before.is_leaf()
            && before.content_size() == 0
            && index > 0
            && can_replace(schema, parent, index - 1, index)
            && let Some(spec) = changes_spec(
                state,
                vec![Change::delete(cut - before.node_size(), cut)],
                event,
            )
        {
            return Some(spec);
        }
        if can_join(schema, doc, cut) {
            if dir < 0
                && after.content_size() == 0
                && let Some(spec) = delete_emptied_block(state, cut, event)
            {
                return Some(spec);
            }
            if let Some(spec) = changes_spec(state, join_changes(cut, 1), event) {
                return Some(spec);
            }
        }
    }

    let can_delete_after = !isolated && can_replace(schema, parent, index, index + 1);
    if can_delete_after
        && let Some(spec) = wrap_after_into_before(state, cut, &before, &after, event)
    {
        return Some(spec);
    }

    // Lift whatever follows the cut one level out, so a later press can join.
    let barred = schema.node_type(after.type_id()).is_isolating() || (dir > 0 && isolated);
    if !barred && let Some(spec) = lift_after_cut(state, cut, resolved.depth(), event) {
        return Some(spec);
    }

    if can_delete_after
        && textblock_at(schema, &after, true, true)
        && textblock_at(schema, &before, false, false)
    {
        return join_textblocks_around(state, cut, event);
    }
    None
}

/// Join the empty block after the cut — the one the caret is in — onto the
/// node before it, and put the caret at the end of that node's content.
///
/// Appending nothing to a container is a join only in name: it deletes the
/// block. Left to be mapped, a caret inside the deleted block would land on
/// the next text position forwards, in whatever follows — past a list's end
/// and into the list after it. Backspace belongs at the end of what came
/// before instead.
fn delete_emptied_block(state: &EditorState, cut: usize, event: &str) -> Option<TransactionSpec> {
    let (set, doc) = resolve_changes(state, join_changes(cut, 1))?;
    // The join deletes the tokens around the cut, so `cut - 1` is still the
    // end of the node before it.
    let end = Selection::find_from(state.schema(), &doc, cut - 1, -1, true)?;
    Some(
        TransactionSpec::new()
            .change_set(set)
            .selection(end)
            .user_event(event)
            .scroll_into_view(),
    )
}

/// Move the node after the cut inside the node before it, wrapping it in
/// whatever the destination's content rule asks for.
fn wrap_after_into_before(
    state: &EditorState,
    cut: usize,
    before: &Node,
    after: &Node,
    event: &str,
) -> Option<TransactionSpec> {
    let schema = state.schema();
    if before.is_leaf() {
        return None;
    }
    let m = content_match_at(schema, before, before.child_count())?;
    let conn = schema.find_wrapping(m, after.type_id())?;
    let first = conn.first().copied().unwrap_or(after.type_id());
    if !m.match_type(first)?.valid_end() {
        return None;
    }
    let end = cut + after.node_size();
    let markups: Vec<_> = conn
        .iter()
        .map(|ty| markup_of(schema, *ty, &crate::attr::Attrs::empty()))
        .collect();
    let opens: Vec<Token> = markups.iter().cloned().map(Token::Open).collect();
    let mut closes: Vec<Token> = markups.iter().rev().cloned().map(Token::Close).collect();
    closes.push(Token::Close(before.markup().clone()));
    changes_spec(
        state,
        vec![
            Change::replace(cut - 1, cut, Slice::from_tokens(&opens)),
            Change::insert(end, Slice::from_tokens(&closes)),
        ],
        event,
    )
}

/// Lift the block after the cut, when doing so brings it out to at least the
/// cut's own depth.
fn lift_after_cut(
    state: &EditorState,
    cut: usize,
    cut_depth: usize,
    event: &str,
) -> Option<TransactionSpec> {
    let doc = state.doc();
    let schema = state.schema();
    let selection = Selection::find_from(schema, doc, cut, 1, false)?;
    let from = doc.resolve(selection.from(doc)).ok()?;
    let to = doc.resolve(selection.to(doc)).ok()?;
    let range = from.block_range(schema, &to, None)?;
    let target = lift_target(schema, &range)?;
    if target < cut_depth {
        return None;
    }
    changes_spec(state, lift_changes(&range, target), event)
}

/// Whether descending into `node` on the given side reaches a textblock.
///
/// With `only`, every node on the way has to be the sole child, which is what
/// tells "this node is really just a textblock in a wrapper" from "this node
/// happens to start with one".
fn textblock_at(schema: &Schema, node: &Node, start: bool, only: bool) -> bool {
    let mut scan = node.clone();
    loop {
        if scan.is_textblock(schema) {
            return true;
        }
        if only && scan.child_count() != 1 {
            return false;
        }
        let next = if start {
            scan.first_child().cloned()
        } else {
            scan.last_child().cloned()
        };
        match next {
            Some(next) => scan = next,
            None => return false,
        }
    }
}

/// Join the node before the selection with the one before that.
pub fn join_up() -> Command {
    command(|state| join_in_direction(state, -1))
}

/// Join the node after the selection with the one after that.
pub fn join_down() -> Command {
    command(|state| join_in_direction(state, 1))
}

fn join_in_direction(state: &EditorState, dir: i32) -> Option<TransactionSpec> {
    let doc = state.doc();
    let schema = state.schema();
    let selection = state.selection();
    let point = match selection {
        Selection::Node { pos } => {
            let node = doc.node_at(*pos)?;
            let at = if dir < 0 {
                *pos
            } else {
                pos + node.node_size()
            };
            if node.is_textblock(schema) || !can_join(schema, doc, at) {
                return None;
            }
            at
        }
        _ => {
            let at = if dir < 0 {
                selection.from(doc)
            } else {
                selection.to(doc)
            };
            join_point(schema, doc, at, dir)?
        }
    };
    let (set, new_doc) = resolve_changes(state, join_changes(point, 1))?;
    let mut spec = TransactionSpec::new()
        .change_set(set)
        .user_event(event::DELETE)
        .scroll_into_view();
    if let Selection::Node { .. } = selection {
        let resolved = new_doc.resolve(point).ok()?;
        let node = resolved.node_before()?;
        spec = spec.selection(Selection::node(point - node.node_size()));
    }
    Some(spec)
}

/// Select the whole document.
pub fn select_all() -> Command {
    command(|state| {
        if *state.selection() == Selection::All {
            return None;
        }
        Some(
            TransactionSpec::new()
                .selection(Selection::All)
                .user_event(event::SELECT_ALL),
        )
    })
}

/// Select the node that holds the current selection.
pub fn select_parent_node() -> Command {
    command(|state| {
        let doc = state.doc();
        let selection = state.selection();
        let from = doc.resolve(selection.from(doc)).ok()?;
        let shared = from.shared_depth(selection.to(doc));
        if shared == 0 {
            return None;
        }
        Some(
            TransactionSpec::new()
                .selection(Selection::node(from.before(shared)))
                .user_event(event::SELECT)
                .scroll_into_view(),
        )
    })
}

/// Move the cursor to the start of its textblock.
pub fn select_textblock_start() -> Command {
    command(|state| select_textblock_edge(state, true))
}

/// Move the cursor to the end of its textblock.
pub fn select_textblock_end() -> Command {
    command(|state| select_textblock_edge(state, false))
}

fn select_textblock_edge(state: &EditorState, start: bool) -> Option<TransactionSpec> {
    let cursor = super::cursor(state)?;
    if !cursor.parent().is_textblock(state.schema()) {
        return None;
    }
    let depth = cursor.depth();
    let target = if start {
        cursor.start(depth)
    } else {
        cursor.end(depth)
    };
    if target == cursor.pos() {
        return None;
    }
    Some(
        TransactionSpec::new()
            .selection(Selection::cursor(target))
            .user_event(event::MOVE)
            .scroll_into_view(),
    )
}

/// The default block type a new sibling after `resolved`'s block would get.
pub(crate) fn default_type_after(schema: &Schema, resolved: &ResolvedPos) -> Option<NodeTypeId> {
    let depth = resolved.depth().checked_sub(1)?;
    content_match_at(schema, resolved.node(depth), resolved.index_after(depth))
        .and_then(|m| schema.default_type(m))
}
