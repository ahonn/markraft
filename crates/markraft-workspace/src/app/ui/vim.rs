//! The app's side of modal editing: the preference, the extension handle every open
//! note editor holds while it is on, and the small label that says which mode it is in.

use super::*;
use markraft_vim::Mode;

impl MarkraftApp {
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
    pub(in crate::app) fn apply_vim(
        &mut self,
        enabled: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let focus_note = self.find_focused(window, cx);
        if !enabled && self.cancel_vim_find(cx) && focus_note {
            self.focus_editor(window, cx);
        }
        let editors: Vec<_> = self
            .sessions
            .iter()
            .map(|(id, session)| (id.clone(), session.editor().clone()))
            .collect();
        for (id, editor) in editors {
            let handle = enabled.then(|| Self::attach_vim(&editor, cx));
            if let Some(session) = self.sessions.get_mut(&id) {
                session.set_vim(handle);
            }
        }
        cx.notify();
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
            session.report_mode(*mode);
        }
        cx.notify();
    }

    /// The left-hand mode indicator stays visible beside the centered format bar.
    /// Abbreviate only when the full label would overlap it in a narrow window.
    pub(in crate::app) fn vim_badge(&self, compact: bool) -> Option<Stateful<Div>> {
        if !self.preferences.vim_mode {
            return None;
        }
        let mode = self.sessions.get(&self.library.active_id)?.vim_mode();
        let full_label = self.i18n.text(match mode {
            Mode::Normal => "surfaces.vim.normal",
            Mode::Insert => "surfaces.vim.insert",
            Mode::Visual => "surfaces.vim.visual",
            Mode::VisualLine => "surfaces.vim.visual-line",
        });
        let label = if compact {
            match mode {
                Mode::Normal => "N",
                Mode::Insert => "I",
                Mode::Visual => "V",
                Mode::VisualLine => "V-L",
            }
        } else {
            &full_label
        };
        Some(
            div()
                .id("vim-mode-indicator")
                .role(Role::Status)
                .aria_label(
                    self.i18n
                        .text_with("surfaces.vim.announced", &[("mode", &full_label)]),
                )
                // The full mode name is already the badge's text. Only the
                // abbreviation needs the hover label.
                .when(compact, |s| s.tooltip(self.hint(full_label.clone())))
                .flex_shrink_0()
                .h(px(18.))
                .px(px(6.))
                .flex()
                .items_center()
                .rounded(px(4.))
                .bg(self.hover_color())
                .text_size(px(10.))
                .text_color(self.muted())
                .child(label.to_owned()),
        )
    }
}
