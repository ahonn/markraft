//! Finding in the note on screen.
//!
//! The bar is a client of the editor. The query lives in the note's find
//! field. Ordinary find selects a hit; Vim find moves a caret and keeps all
//! hits decorated. The editor owns preview position mapping, while this
//! module owns the input, its saved query, and which note the preview belongs to.

use super::*;

/// How far the first line moves down while the bar is open: the bar's own
/// height, and a gap under it.
pub(super) const CLEARANCE: Pixels = px(44.);

pub(super) struct VimFind {
    pub(super) note: String,
    query: String,
}

impl MarkraftApp {
    /// `/` borrows the ordinary input without giving it modal key bindings.
    pub(super) fn open_vim_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.persistence.is_none() || self.editor().read(cx).is_composing() {
            return;
        }
        self.close_popover(cx);
        self.ring.release();
        if self.find_vim.is_none() {
            self.find_vim = Some(VimFind {
                note: self.library.active_id.clone(),
                query: self.find_editor.read(cx).text().to_owned(),
            });
            self.editor()
                .update(cx, |editor, cx| editor.begin_find_preview(cx));
        }
        self.find_open = true;
        self.restyle_editors(cx);
        self.sync_find(cx);
        self.find_editor
            .update(cx, |editor, cx| editor.select_all(cx));
        window.focus(&self.find_editor.focus_handle(cx), cx);
        cx.notify();
    }

    /// Accept only the already previewed hit: Return must not skip another one.
    pub(super) fn submit_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.find_editor.read(cx).is_composing() {
            return;
        }
        let Some(preview) = self.find_vim.as_ref() else {
            self.find_next(window, cx);
            return;
        };
        if self.find_editor.read(cx).text().is_empty() && !preview.query.is_empty() {
            let query = preview.query.clone();
            self.find_editor
                .update(cx, |editor, cx| editor.set_value(&query, cx));
            self.sync_find(cx);
        }
        self.find_vim = None;
        self.find_open = false;
        self.editor()
            .update(cx, |editor, cx| editor.accept_find_preview(cx));
        self.restyle_editors(cx);
        self.focus_editor(window, cx);
        self.report_vim_find(cx);
        cx.notify();
    }

    /// Cancel on the editor that opened the preview, never on a newly active note.
    pub(super) fn cancel_vim_find(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(preview) = self.find_vim.take() else {
            return false;
        };
        self.find_open = false;
        if let Some(session) = self.sessions.get(&preview.note) {
            session
                .editor()
                .clone()
                .update(cx, |editor, cx| editor.cancel_find_preview(cx));
        }
        self.find_editor
            .update(cx, |editor, cx| editor.set_value(&preview.query, cx));
        self.restyle_editors(cx);
        cx.notify();
        true
    }

    /// A document replacement discards the editor's mapped preview bookmark.
    /// End the input session too, rather than accepting an obsolete position.
    pub(super) fn reconcile_vim_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.find_vim.as_ref().is_some_and(|preview| {
            preview.note != self.library.active_id
                || self
                    .sessions
                    .get(&preview.note)
                    .is_none_or(|session| !session.editor().read(cx).has_find_preview())
        }) {
            let focus_note = self.find_focused(window, cx);
            self.cancel_vim_find(cx);
            if focus_note {
                self.focus_editor(window, cx);
            }
        }
    }

    /// `n` and `N` use the current caret, including after an unrelated motion.
    pub(super) fn repeat_vim_find(&mut self, forward: bool, cx: &mut Context<Self>) {
        let query = self.find_editor.read(cx).text().to_owned();
        if query.is_empty() {
            self.inform("No previous search.", cx);
            return;
        }
        self.editor()
            .update(cx, |editor, cx| editor.find_from_cursor(query, forward, cx));
        self.report_vim_find(cx);
        cx.notify();
    }

    fn report_vim_find(&mut self, cx: &mut Context<Self>) {
        let status = self.editor().read(cx).find_status();
        if status.query.is_empty() {
            self.inform("No previous search.", cx);
        } else if status.total == 0 {
            self.inform("No results", cx);
        }
    }

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
        // ⌘F explicitly switches back to the ordinary selection-based workflow.
        if self.find_vim.take().is_some() {
            self.editor()
                .update(cx, |editor, cx| editor.accept_find_preview(cx));
            self.sync_find(cx);
        }
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
        if self.cancel_vim_find(cx) {
            if focus_note {
                self.focus_editor(window, cx);
            }
            return true;
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
        if self.find_editor.read(cx).is_composing() {
            return;
        }
        if self.find_vim.is_some() {
            self.repeat_vim_find(next, cx);
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
        if let Some(preview) = &self.find_vim {
            if preview.note != self.library.active_id || !self.editor().read(cx).has_find_preview()
            {
                // The editor's change subscription has a Window, so it can end
                // this invalidated session and return keyboard focus together.
                return;
            }
            self.editor()
                .update(cx, |note, cx| note.preview_find_query(query, cx));
        } else {
            self.editor()
                .update(cx, |note, cx| note.set_find_query(query, cx));
        }
        cx.notify();
    }

    pub(super) fn find_focused(&self, window: &Window, cx: &App) -> bool {
        self.find_open && self.find_editor.focus_handle(cx).is_focused(window)
    }
}
