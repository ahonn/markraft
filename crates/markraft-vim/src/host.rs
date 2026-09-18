//! The editor as a vim command needs it.
//!
//! [`EditorCx`] is the real one. Tests implement the same trait over a bare
//! [`EditorState`], so every command in `command.rs` runs unchanged in both and there is
//! no second implementation of vim to drift.

use markraft_doc::commands::Command;
use markraft_doc::projection::Projection;
use markraft_doc::{EditorState, Selection, Slice, TransactionSpec};
use markraft_gpui::EditorCx;
use std::sync::Arc;

pub(crate) trait Host {
    fn state(&self) -> &EditorState;
    fn projection(&self) -> Arc<Projection>;
    /// See [`EditorCx::select`].
    fn select(&mut self, selection: Selection, keep_column: bool);
    /// One command's whole edit, as one transaction and one undo step.
    fn dispatch(&mut self, specs: Vec<TransactionSpec>) -> bool;
    /// Run a command from the catalogue against the current state.
    fn run(&mut self, command: &Command) -> bool;
    /// Move the caret by visual rows, keeping the column. Only a laid-out editor knows
    /// where a wrapped row breaks, so this is the one command input a test cannot
    /// reproduce; it moves by line instead.
    fn rows(&mut self, rows: isize, extend: bool);
    fn write_clipboard(&mut self, slice: &Slice);
    fn read_clipboard(&mut self) -> Option<Slice>;
    /// Undo or redo one entry, reporting whether there was one.
    fn history(&mut self, undo: bool) -> bool;
    /// Bracket an insert session so that it undoes as one step.
    fn begin_undo_group(&mut self);
    fn end_undo_group(&mut self);
}

/// The moving end of the selection.
pub(crate) fn head(host: &impl Host) -> usize {
    let state = host.state();
    state.selection().head(state.doc())
}

/// The fixed end of the selection.
pub(crate) fn anchor(host: &impl Host) -> usize {
    let state = host.state();
    state.selection().anchor(state.doc())
}

impl Host for EditorCx<'_> {
    fn state(&self) -> &EditorState {
        EditorCx::state(self)
    }
    fn projection(&self) -> Arc<Projection> {
        EditorCx::projection(self)
    }
    fn select(&mut self, selection: Selection, keep_column: bool) {
        EditorCx::select(self, selection, keep_column);
    }
    fn dispatch(&mut self, specs: Vec<TransactionSpec>) -> bool {
        EditorCx::dispatch(self, specs).is_some()
    }
    fn run(&mut self, command: &Command) -> bool {
        EditorCx::run(self, command)
    }
    fn rows(&mut self, rows: isize, extend: bool) {
        self.move_visual_rows(rows, extend);
    }
    fn write_clipboard(&mut self, slice: &Slice) {
        EditorCx::write_clipboard(self, slice);
    }
    fn read_clipboard(&mut self) -> Option<Slice> {
        EditorCx::read_clipboard(self)
    }
    fn history(&mut self, undo: bool) -> bool {
        if undo { self.undo() } else { self.redo() }.is_some()
    }
    fn begin_undo_group(&mut self) {
        EditorCx::begin_undo_group(self);
    }
    fn end_undo_group(&mut self) {
        EditorCx::end_undo_group(self);
    }
}
