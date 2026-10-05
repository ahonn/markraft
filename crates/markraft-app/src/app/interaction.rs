//! Interaction ownership and transitions. A surface owns its input session.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum InputKind {
    Notes,
    Actions,
    Link,
    Language,
    Rename,
}

impl InputKind {
    fn messages(self) -> (&'static str, &'static str) {
        match self {
            Self::Notes => ("input.search-notes", "input.notes-label"),
            Self::Actions => ("input.search-actions", "input.actions-label"),
            Self::Link => ("input.link", "input.link-label"),
            Self::Language => ("input.search-languages", "input.languages-label"),
            Self::Rename => ("input.name", "input.name-label"),
        }
    }
}

pub(super) enum Popover {
    Format(FormatMenu),
    Link(LinkPopover),
    CodeLanguage(usize),
    Rename(rename::Rename),
    FileStatus,
}

struct NotePopover {
    note: String,
    content: Popover,
}

pub(super) struct Interaction {
    panel: Panel,
    popover: Option<NotePopover>,
}

impl Default for Interaction {
    fn default() -> Self {
        Self {
            panel: Panel::Editor,
            popover: None,
        }
    }
}

impl Interaction {
    pub fn panel(&self) -> Panel {
        self.panel
    }
    pub fn popover(&self) -> Option<&Popover> {
        self.popover.as_ref().map(|popover| &popover.content)
    }
    pub fn input_kind(&self) -> Option<InputKind> {
        match self.popover() {
            Some(Popover::Link(LinkPopover::Edit)) => return Some(InputKind::Link),
            Some(Popover::CodeLanguage(_)) => return Some(InputKind::Language),
            Some(Popover::Rename(_)) => return Some(InputKind::Rename),
            _ => {}
        }
        match self.panel {
            Panel::Editor => None,
            Panel::Browse => Some(InputKind::Notes),
            Panel::Actions => Some(InputKind::Actions),
        }
    }
    pub fn switch_panel(&mut self, panel: Panel) -> bool {
        self.panel = panel;
        self.popover = None;
        true
    }
    pub fn open(&mut self, note: &str, popover: Popover) -> bool {
        self.panel = Panel::Editor;
        self.popover = Some(NotePopover {
            note: note.to_owned(),
            content: popover,
        });
        true
    }
    /// A cached editor still represents a different note. Its predecessor's
    /// controls must never apply to the new note's selection or block positions.
    pub fn note_changed(&mut self, note: &str) -> bool {
        if self
            .popover
            .as_ref()
            .is_some_and(|popover| popover.note != note)
        {
            self.close()
        } else {
            false
        }
    }
    pub fn close(&mut self) -> bool {
        self.popover.take().is_some()
    }
    pub fn format_menu(&self) -> Option<FormatMenu> {
        match self.popover() {
            Some(Popover::Format(menu)) => Some(*menu),
            _ => None,
        }
    }
    pub fn link(&self) -> Option<LinkPopover> {
        match self.popover() {
            Some(Popover::Link(mode)) => Some(*mode),
            _ => None,
        }
    }
    pub fn code_language(&self) -> Option<usize> {
        match self.popover() {
            Some(Popover::CodeLanguage(pos)) => Some(*pos),
            _ => None,
        }
    }
    pub fn rename(&self) -> Option<&rename::Rename> {
        match self.popover() {
            Some(Popover::Rename(rename)) => Some(rename),
            _ => None,
        }
    }
    pub fn rename_mut(&mut self) -> Option<&mut rename::Rename> {
        match self.popover.as_mut().map(|popover| &mut popover.content) {
            Some(Popover::Rename(rename)) => Some(rename),
            _ => None,
        }
    }
    pub fn file_status(&self) -> bool {
        matches!(self.popover(), Some(Popover::FileStatus))
    }
}

pub(super) struct InputSession {
    kind: InputKind,
    editor: Entity<EditorView>,
    text: String,
    _changes: Subscription,
}

impl MarkraftApp {
    /// Only a surface with an input may read it. Each opening creates a fresh editor,
    /// so selection, undo and IME state cannot leak into the next surface.
    pub(super) fn query(&self) -> &Entity<EditorView> {
        let input = self
            .input
            .as_ref()
            .expect("the surface has an input session");
        debug_assert_eq!(Some(input.kind), self.interaction.input_kind());
        &input.editor
    }

    pub(super) fn search_text(&self, cx: &App) -> String {
        self.input
            .as_ref()
            .filter(|input| matches!(input.kind, InputKind::Notes))
            .map(|input| input.editor.read(cx).text().to_owned())
            .unwrap_or_default()
    }
    pub(super) fn query_focused(&self, window: &Window, cx: &App) -> bool {
        self.input
            .as_ref()
            .is_some_and(|input| input.editor.focus_handle(cx).is_focused(window))
    }
    pub(super) fn input_composing(&self, cx: &App) -> bool {
        self.input
            .as_ref()
            .is_some_and(|input| input.editor.read(cx).is_composing())
    }
    pub(super) fn style_input(&mut self, cx: &mut Context<Self>) {
        if let Some(input) = &self.input {
            input.editor.update(cx, |editor, cx| {
                editor.set_style(query_style(self.dark), cx)
            });
        }
    }
    pub(super) fn reconcile_interaction(&mut self, window: &Window, cx: &mut Context<Self>) {
        let invalid = match self.interaction.popover() {
            Some(Popover::Link(LinkPopover::View)) => {
                self.editor().read(cx).active_link().is_none()
            }
            Some(
                Popover::Link(LinkPopover::Edit) | Popover::Rename(_) | Popover::CodeLanguage(_),
            ) => {
                !self.code_language.focus_pending()
                    && !self.query_focused(window, cx)
                    && !self.ring.panel().is_focused(window)
            }
            Some(Popover::FileStatus) => !self.has_file_status(),
            _ => false,
        };
        if invalid {
            self.close_popover(cx);
            cx.notify();
        }
    }

    pub(super) fn cancel_input(&mut self, cx: &mut Context<Self>) {
        if let Some(input) = &self.input {
            input
                .editor
                .update(cx, |editor, cx| editor.cancel_composition(cx));
        }
    }

    pub(super) fn leave_input(&mut self, cx: &mut Context<Self>) {
        self.cancel_input(cx);
        self.input = None;
        self.ring.release();
        self.code_language.released();
    }

    pub(super) fn set_panel(&mut self, panel: Panel, cx: &mut Context<Self>) {
        self.context_menus.dismiss();
        // A command that keeps the editor in front leaves its checking panel
        // open; the panel belongs to the editor and goes when the editor does.
        if self.interaction.panel() != panel {
            self.cancel_checking_panel();
        }
        self.leave_input(cx);
        self.interaction.switch_panel(panel);
    }

    pub(super) fn show_popover(&mut self, popover: Popover, cx: &mut Context<Self>) {
        self.leave_input(cx);
        self.interaction.open(&self.library.active_id, popover);
    }

    pub(super) fn close_popover(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.interaction.close() {
            return false;
        }
        self.leave_input(cx);
        true
    }

    pub(super) fn refresh_input_language(&self, cx: &mut Context<Self>) {
        if let Some(input) = &self.input {
            let (placeholder, label) = input.kind.messages();
            input.editor.update(cx, |editor, cx| {
                editor.set_messages(self.i18n.editor_messages(), cx);
                editor.set_placeholder(self.i18n.text(placeholder), cx);
                editor.set_aria_label(self.i18n.text(label), cx);
            });
        }
    }

    pub(super) fn set_query(&mut self, text: String, cx: &mut Context<Self>) {
        let Some(kind) = self.interaction.input_kind() else {
            return;
        };
        let (placeholder, label) = kind.messages();
        self.cancel_input(cx);
        let editor = cx.new(|cx| {
            EditorView::single_line(cx)
                .with_style(query_style(self.dark))
                .with_messages(self.i18n.editor_messages())
        });
        editor.update(cx, |editor, cx| {
            editor.set_value(&text, cx);
            editor.set_placeholder(self.i18n.text(placeholder), cx);
            editor.set_aria_label(self.i18n.text(label), cx);
        });
        let changes = cx.subscribe(&editor, move |this, editor, event: &EditorEvent, cx| {
            if !matches!(event, EditorEvent::Changed { .. }) {
                return;
            }
            let Some(input) = this.input.as_mut() else {
                return;
            };
            if input.editor != editor || input.kind != kind {
                return;
            }
            let text = editor.read(cx).text().to_owned();
            if input.text == text {
                return;
            }
            input.text = text;
            match kind {
                InputKind::Language => this.code_language.reopen(),
                InputKind::Notes => this.picker.reopen_browse(),
                InputKind::Actions => this.picker.reopen_actions(),
                _ => {}
            }
            cx.notify();
        });
        self.input = Some(InputSession {
            kind,
            editor,
            text,
            _changes: changes,
        });
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    #[::core::prelude::v1::test]
    fn opening_a_surface_replaces_the_previous_input_owner() {
        let mut state = Interaction::default();
        state.switch_panel(Panel::Browse);
        assert_eq!(state.input_kind(), Some(InputKind::Notes));
        state.open("note", Popover::Link(LinkPopover::Edit));
        assert_eq!(state.panel(), Panel::Editor);
        assert_eq!(state.input_kind(), Some(InputKind::Link));
        state.open("note", Popover::CodeLanguage(42));
        assert_eq!(state.link(), None);
        assert_eq!(state.input_kind(), Some(InputKind::Language));
        assert!(state.close());
        assert_eq!(state.input_kind(), None);
        assert!(!state.close());
    }
    #[::core::prelude::v1::test]
    fn panels_clear_popovers_and_have_distinct_input_lifetimes() {
        let mut state = Interaction::default();
        state.open("note", Popover::Format(FormatMenu::Inline));
        state.switch_panel(Panel::Browse);
        assert!(state.popover().is_none());
        assert_eq!(state.input_kind(), Some(InputKind::Notes));
        state.switch_panel(Panel::Actions);
        assert_eq!(state.input_kind(), Some(InputKind::Actions));
        state.switch_panel(Panel::Editor);
        assert_eq!(state.input_kind(), None);
    }

    #[::core::prelude::v1::test]
    fn observing_the_same_note_preserves_its_popover_and_input() {
        let mut state = Interaction::default();
        state.open("active", Popover::Link(LinkPopover::Edit));
        assert!(!state.note_changed("active"));
        assert_eq!(state.link(), Some(LinkPopover::Edit));
        assert_eq!(state.input_kind(), Some(InputKind::Link));
    }

    #[::core::prelude::v1::test]
    fn switching_notes_closes_controls_even_when_the_next_editor_is_cached() {
        let mut state = Interaction::default();
        state.open("removed", Popover::CodeLanguage(42));
        assert!(state.note_changed("cached"));
        assert_eq!(state.code_language(), None);
        assert_eq!(state.input_kind(), None);
        assert!(!state.note_changed("cached"));
    }

    #[::core::prelude::v1::test]
    fn replacing_a_popover_updates_its_note_ownership() {
        let mut state = Interaction::default();
        state.open("first", Popover::Link(LinkPopover::Edit));
        state.open("second", Popover::CodeLanguage(8));
        assert!(!state.note_changed("second"));
        assert_eq!(state.code_language(), Some(8));
        assert!(state.note_changed("first"));
        assert!(state.popover().is_none());
    }

    #[::core::prelude::v1::test]
    fn note_changes_preserve_workspace_panels_without_note_owned_controls() {
        let mut state = Interaction::default();
        state.switch_panel(Panel::Browse);
        assert!(!state.note_changed("other"));
        assert_eq!(state.panel(), Panel::Browse);
        assert_eq!(state.input_kind(), Some(InputKind::Notes));
    }
}
