//! Caret motion by grapheme cluster and by word.
//!
//! Vertical motion and "to the start of the visual line" are deliberately
//! absent: where a line wraps is a layout decision, and this crate has no
//! layout. A view implements those on top of
//! [`Projection`](crate::projection::Projection).

use crate::projection::projection_of;
use crate::selection::Selection;
use crate::state::{EditorState, TransactionSpec};

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
    command(move |state| {
        move_selection(state, extend, |projection, pos| match dir {
            Direction::Forward => projection.next_word_boundary(pos),
            Direction::Backward => projection.prev_word_boundary(pos),
        })
        .or_else(|| collapse(state, dir, extend))
    })
}

fn move_selection(
    state: &EditorState,
    extend: bool,
    step: impl Fn(&crate::projection::Projection, usize) -> Option<usize>,
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
    let next = if extend {
        Selection::text(selection.anchor(doc), target)
    } else {
        Selection::cursor(target)
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
    let target = match dir {
        Direction::Forward => selection.to(doc),
        Direction::Backward => selection.from(doc),
    };
    Some(
        TransactionSpec::new()
            .selection(Selection::near(
                state.schema(),
                doc,
                target,
                match dir {
                    Direction::Forward => -1,
                    Direction::Backward => 1,
                },
            ))
            .user_event("move")
            .scroll_into_view(),
    )
}
