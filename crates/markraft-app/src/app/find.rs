//! Finding in the note on screen.
//!
//! The bar is a client of the editor. The query lives in the note's find
//! field, the other hits are decorations, and the current one is the
//! selection. This module only decides when that query is set, stepped, or
//! cleared. Browse and Actions replace the note, so the bar is not one of
//! them: the note stays where it is.

use super::*;

/// How far the first line moves down while the bar is open: the bar's own
/// height, and a gap under it.
pub(super) const CLEARANCE: Pixels = px(44.);

impl MarkraftApp {
    /// ⌘F. A panel gives the note back first. A bar that is already open
    /// selects its query so the next keystroke replaces it.
    pub(super) fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.persistence.is_none() {
            return;
        }
        if self.interaction.panel() != Panel::Editor {
            self.set_panel(Panel::Editor, cx);
        }
        self.close_popover(cx);
        self.ring.release();
        if self.find_open {
            self.find_editor
                .update(cx, |editor, cx| editor.select_all(cx));
            window.focus(&self.find_editor.focus_handle(cx), cx);
            cx.notify();
            return;
        }
        let seed = self.editor().read(cx).find_selection_query();
        self.find_open = true;
        self.restyle_editors(cx);
        if let Some(seed) = seed {
            self.find_editor.update(cx, |editor, cx| {
                editor.set_value(&seed, cx);
                editor.select_all(cx);
            });
        } else {
            self.find_editor
                .update(cx, |editor, cx| editor.select_all(cx));
        }
        self.sync_find(cx);
        window.focus(&self.find_editor.focus_handle(cx), cx);
        cx.notify();
    }

    /// Close the bar. The last hit stays selected; the query stays in the
    /// field for the next time. `false` when it was already closed.
    pub(super) fn close_find(
        &mut self,
        focus_note: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.find_open {
            return false;
        }
        self.find_open = false;
        if self.persistence.is_some() && self.sessions.get(&self.library.active_id).is_some() {
            self.editor()
                .update(cx, |editor, cx| editor.set_find_query(String::new(), cx));
        }
        self.restyle_editors(cx);
        if focus_note && self.persistence.is_some() {
            self.focus_editor(window, cx);
        }
        cx.notify();
        true
    }

    /// The next hit. An empty field opens the bar instead of searching. A
    /// query the note does not have yet is applied, which selects its first
    /// hit, and is not also stepped.
    pub(super) fn find_next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.step_find(true, window, cx);
    }

    /// The previous hit. See [`MarkraftApp::find_next`].
    pub(super) fn find_previous(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.step_find(false, window, cx);
    }

    fn step_find(&mut self, next: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.persistence.is_none() {
            return;
        }
        let query = self.find_editor.read(cx).text().to_owned();
        if query.is_empty() {
            self.open_find(window, cx);
            return;
        }
        if self.sessions.get(&self.library.active_id).is_none() {
            return;
        }
        let applied = self.editor().read(cx).find_status().query;
        self.editor().update(cx, |note, cx| {
            if applied != query {
                note.set_find_query(query, cx);
            } else if next {
                note.find_next(cx);
            } else {
                note.find_previous(cx);
            }
        });
        cx.notify();
    }

    /// Give the note on screen the field's query. A newly opened note has an
    /// empty find field of its own, and a document replaced under it does too.
    pub(super) fn sync_find(&mut self, cx: &mut Context<Self>) {
        if !self.find_open || self.persistence.is_none() {
            return;
        }
        if self.sessions.get(&self.library.active_id).is_none() {
            return;
        }
        let query = self.find_editor.read(cx).text().to_owned();
        self.editor()
            .update(cx, |note, cx| note.set_find_query(query, cx));
        cx.notify();
    }

    pub(super) fn find_focused(&self, window: &Window, cx: &App) -> bool {
        self.find_open && self.find_editor.focus_handle(cx).is_focused(window)
    }
}
