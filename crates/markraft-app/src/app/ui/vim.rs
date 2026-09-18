//! The app's side of modal editing: the preference, the extension handle every open
//! note editor holds while it is on, and the small label that says which mode it is in.

use super::*;
use markraft_vim::Mode;

impl NotesApp {
    /// Register vim on `editor`. The handle is what keeps it registered: dropping it
    /// unregisters the extension before the editor's next update.
    pub(in crate::app) fn attach_vim(
        editor: &Entity<EditorView>,
        cx: &mut Context<Self>,
    ) -> ExtensionHandle {
        editor.update(cx, |editor, cx| {
            editor.add_extension(markraft_vim::vim(), cx)
        })
    }

    /// Turn modal editing on or off for every open note editor at once, rather than on
    /// the next launch. The query field never gets it: it is a single-line host control.
    pub(in crate::app) fn toggle_vim(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let enabled = !self.library.preferences.vim_mode;
        self.library.preferences.vim_mode = enabled;
        let editors: Vec<_> = self
            .sessions
            .iter()
            .map(|(id, session)| (id.clone(), session.editor.clone()))
            .collect();
        for (id, editor) in editors {
            let handle = enabled.then(|| Self::attach_vim(&editor, cx));
            if let Some(session) = self.sessions.get_mut(&id) {
                // Assigning drops the previous handle, which unregisters it.
                session.vim = handle;
                session.vim_mode = Mode::default();
            }
        }
        self.panel = Panel::Editor;
        self.focus_editor(window, cx);
        self.changed(cx);
    }

    /// The mode a note's editor reported through `EditorEvent::Extension`. It is kept per
    /// note, so switching notes shows the mode that note's editor is actually in.
    pub(in crate::app) fn vim_effect(
        &mut self,
        note: &str,
        payload: &markraft_gpui::ExtensionPayload,
        cx: &mut Context<Self>,
    ) {
        let Some(mode) = payload.downcast_ref::<Mode>() else {
            return;
        };
        if let Some(session) = self.sessions.get_mut(note) {
            session.vim_mode = *mode;
        }
        cx.notify();
    }

    /// A quiet `NORMAL` / `INSERT` / `VISUAL` label, first in the footer's left group
    /// and ahead of the count. Nothing is drawn while vim is off.
    pub(in crate::app) fn vim_badge(&self) -> Option<Div> {
        if !self.library.preferences.vim_mode {
            return None;
        }
        let mode = self.sessions.get(&self.library.active_id)?.vim_mode;
        Some(
            div()
                .flex_shrink_0()
                .h(px(18.))
                .px(px(6.))
                .flex()
                .items_center()
                .rounded(px(4.))
                .bg(self.hover_color())
                .text_size(px(10.))
                .text_color(self.muted())
                .child(mode.label()),
        )
    }
}
