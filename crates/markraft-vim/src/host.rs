//! The editor as a vim command needs it.
//!
//! [`EditorCx`] is the real one. Tests implement the same trait over a bare
//! `markraft_core::Editor`, so every command in `command.rs` runs unchanged in both and
//! there is no second implementation of vim to drift.

use markraft_core::{Document, Selection, Transaction};
use markraft_gpui::EditorCx;

pub(crate) trait Host {
    fn document(&self) -> &Document;
    fn selection(&self) -> Selection;
    /// See [`EditorCx::select`].
    fn select(&mut self, selection: Selection, keep_column: bool);
    /// One command's whole edit, as one undo step.
    fn edit(&mut self, action: &mut dyn FnMut(&mut Transaction<'_>));
    /// Move the caret by visual rows, keeping the column. Only a laid-out editor knows
    /// where a wrapped row breaks, so this is the one command input a test cannot
    /// reproduce; it moves by block instead.
    fn rows(&mut self, rows: isize, extend: bool);
    fn write_clipboard(&mut self, fragment: Document, text: String);
    fn read_clipboard(&mut self) -> Option<Document>;
    /// Undo or redo one entry, reporting whether there was one.
    fn history(&mut self, undo: bool) -> bool;
    /// Bracket an insert session so that it undoes as one step; see
    /// [`markraft_core::Editor::begin_undo_group`].
    fn begin_undo_group(&mut self);
    fn end_undo_group(&mut self);
}

impl Host for EditorCx<'_> {
    fn document(&self) -> &Document {
        EditorCx::document(self)
    }
    fn selection(&self) -> Selection {
        EditorCx::selection(self)
    }
    fn select(&mut self, selection: Selection, keep_column: bool) {
        EditorCx::select(self, selection, keep_column);
    }
    fn edit(&mut self, action: &mut dyn FnMut(&mut Transaction<'_>)) {
        self.transact(|tx| action(tx));
    }
    fn rows(&mut self, rows: isize, extend: bool) {
        self.move_visual_rows(rows, extend);
    }
    fn write_clipboard(&mut self, fragment: Document, text: String) {
        EditorCx::write_clipboard(self, fragment, text);
    }
    fn read_clipboard(&mut self) -> Option<Document> {
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
