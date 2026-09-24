//! Caret motion by grapheme cluster and by word, and the deletions that follow
//! the same boundaries.
//!
//! Vertical motion and "to the start of the visual line" are deliberately
//! absent: where a line wraps is a layout decision, and this crate has no
//! layout. A view implements those on top of
//! [`Projection`](crate::projection::Projection).

use crate::projection::{Projection, projection_of};
use crate::selection::Selection;
use crate::state::{EditorState, TransactionSpec};

use super::text::delete_range_changes;
use super::{Command, command};

/// Which way a motion command moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Towards the end of the document.
    Forward,
    /// Towards the start of the document.
    Backward,
}

/// Move the caret one grapheme cluster.
///
/// With `extend`, the anchor stays put and the head moves, which is what a
/// shifted arrow key does. A non-empty selection that is *not* being extended
/// collapses to the edge the motion points at rather than moving past it.
pub fn move_by_grapheme(dir: Direction, extend: bool) -> Command {
    command(move |state| {
        move_selection(state, extend, |projection, pos| match dir {
            Direction::Forward => projection.next_grapheme_boundary(pos),
            Direction::Backward => projection.prev_grapheme_boundary(pos),
        })
        .or_else(|| collapse(state, dir, extend))
    })
}

/// Move the caret one word. See [`move_by_grapheme`].
pub fn move_by_word(dir: Direction, extend: bool) -> Command {
    move_by(dir, extend, move |projection, pos| match dir {
        Direction::Forward => projection.next_word_boundary(pos),
        Direction::Backward => projection.prev_word_boundary(pos),
    })
}

/// Move the caret to where `step` says the next boundary in `dir` lies. See
/// [`move_by_grapheme`].
///
/// For a view whose boundaries are not the projection's own — one that hides
/// some of the text, say, and must not stop inside what it hides.
pub fn move_by(
    dir: Direction,
    extend: bool,
    step: impl Fn(&Projection, usize) -> Option<usize> + Send + Sync + 'static,
) -> Command {
    command(move |state| {
        move_selection(state, extend, &step).or_else(|| collapse(state, dir, extend))
    })
}

/// Delete one grapheme cluster in `dir`, or the selection when there is one.
///
/// Deliberately stops at a block boundary: the position before a textblock's
/// first token is a boundary between two nodes, not a character, and joining
/// those two blocks is [`join_backward`](super::join_backward)'s job. A chain
/// that puts this before the join commands therefore deletes text inside a
/// block and leaves structure to them.
pub fn delete_by_grapheme(dir: Direction) -> Command {
    command(move |state| {
        delete_to(state, |projection, pos| match dir {
            Direction::Forward => projection.next_grapheme_boundary(pos),
            Direction::Backward => projection.prev_grapheme_boundary(pos),
        })
    })
}

/// Delete one word in `dir`. See [`delete_by_grapheme`].
pub fn delete_by_word(dir: Direction) -> Command {
    delete_by(move |projection, pos| match dir {
        Direction::Forward => projection.next_word_boundary(pos),
        Direction::Backward => projection.prev_word_boundary(pos),
    })
}

/// Delete from the caret to where `step` says the next boundary lies, or the
/// selection when there is one. See [`delete_by_grapheme`] and [`move_by`].
pub fn delete_by(
    step: impl Fn(&Projection, usize) -> Option<usize> + Send + Sync + 'static,
) -> Command {
    command(move |state| delete_to(state, &step))
}

fn delete_to(
    state: &EditorState,
    step: impl Fn(&Projection, usize) -> Option<usize>,
) -> Option<TransactionSpec> {
    let doc = state.doc();
    let selection = state.selection();
    let (from, to) = if selection.is_empty(doc) {
        let projection = projection_of(state);
        let head = selection.head(doc);
        let target = step(&projection, head)?;
        // A step that leaves the line has crossed a block boundary.
        if projection.line_at(target) != projection.line_at(head) {
            return None;
        }
        (target.min(head), target.max(head))
    } else {
        let range = selection.replacement_range(doc);
        (range.from, range.to)
    };
    if from == to {
        return None;
    }
    super::changes_spec(
        state,
        delete_range_changes(state.schema(), doc, from, to),
        "delete",
    )
}

fn move_selection(
    state: &EditorState,
    extend: bool,
    step: impl Fn(&Projection, usize) -> Option<usize>,
) -> Option<TransactionSpec> {
    let doc = state.doc();
    let selection = state.selection();
    if !extend && !selection.is_empty(doc) {
        return None;
    }
    let projection = projection_of(state);
    let head = selection.head(doc);
    let target = step(&projection, head)?;
    if target == head {
        return None;
    }
    let next = if projection.is_caret_position(target) {
        if extend {
            Selection::text(selection.anchor(doc), target)
        } else {
            Selection::cursor(target)
        }
    } else if !extend && Selection::is_selectable(state.schema(), doc, target) {
        // Stepping onto a leaf block's line, a divider's, selects the leaf, as
        // arriving there by line does; no caret can sit beside it.
        Selection::node(target)
    } else if extend {
        // A selection grows over the leaf to the text beyond it.
        let beyond = step(&projection, target).filter(|&at| projection.is_caret_position(at))?;
        Selection::text(selection.anchor(doc), beyond)
    } else {
        return None;
    };
    if next.check(doc, state.schema()).is_err() {
        return None;
    }
    Some(
        TransactionSpec::new()
            .selection(next)
            .user_event(if extend { "select" } else { "move" })
            .scroll_into_view(),
    )
}

/// Collapse a non-empty selection to the edge the motion points at.
fn collapse(state: &EditorState, dir: Direction, extend: bool) -> Option<TransactionSpec> {
    let doc = state.doc();
    let selection = state.selection();
    if extend || selection.is_empty(doc) {
        return None;
    }
    // A text range collapses onto its own edge, so the search looks back into
    // it. A selected node has no inside to land in: looking back from its edge
    // finds the node again — its start is the very position that selects it —
    // and the arrow would never leave it. It looks the way the arrow points,
    // from past the node, instead.
    let node = matches!(selection, Selection::Node { .. });
    let (target, bias) = match dir {
        Direction::Forward => (selection.to(doc), if node { 1 } else { -1 }),
        Direction::Backward if node => (selection.from(doc).saturating_sub(1), -1),
        Direction::Backward => (selection.from(doc), 1),
    };
    // Nowhere else to go — a node with nothing past it — is not a motion, so a
    // chain can offer something after this.
    let next = Selection::near(state.schema(), doc, target, bias);
    if next == *selection {
        return None;
    }
    Some(
        TransactionSpec::new()
            .selection(next)
            .user_event("move")
            .scroll_into_view(),
    )
}
