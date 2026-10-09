//! State shared by editing controls. Execution still belongs to the editor's
//! actions and transaction guards; querying a control never applies an edit.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum EditCommand {
    Cut,
    Copy,
    Paste,
    PastePlain,
    PasteMarkdown,
}

impl EditCommand {
    pub(super) fn action(self) -> Box<dyn Action> {
        match self {
            Self::Cut => Box::new(markraft_gpui::Cut),
            Self::Copy => Box::new(markraft_gpui::Copy),
            Self::Paste => Box::new(markraft_gpui::Paste),
            Self::PastePlain => Box::new(markraft_gpui::PastePlain),
            Self::PasteMarkdown => Box::new(markraft_gpui::PasteMarkdown),
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Cut => "menu.cut",
            Self::Copy => "menu.copy",
            Self::Paste => "menu.paste",
            Self::PastePlain => "command.paste-as-plain-text",
            Self::PasteMarkdown => "command.paste-as-markdown",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct CommandState {
    pub enabled: bool,
    pub checked: Option<bool>,
}

impl CommandState {
    fn enabled(enabled: bool) -> Self {
        Self {
            enabled,
            checked: None,
        }
    }
}

impl WorkspaceView {
    /// Only editing intents participate. Other application actions retain their
    /// existing availability and lifecycle rules.
    pub(super) fn editing_state(&self, intent: &Intent, cx: &App) -> Option<CommandState> {
        if !matches!(
            intent,
            Intent::Edit(_)
                | Intent::PastePlain
                | Intent::PasteMarkdown
                | Intent::Mark(_)
                | Intent::Block(_)
                | Intent::Undo
                | Intent::Redo
                | Intent::EditLink
                | Intent::Link
                | Intent::InsertTable
                | Intent::Unlink
                | Intent::CopyLink
                | Intent::OpenLink
                | Intent::Table(_)
                | Intent::ChooseCodeLanguage
        ) {
            return None;
        }
        let editor = self.editor().read(cx);
        let writable = !self.is_reloading()
            && self.notes.library.active_note().read_only.is_none()
            && !editor.is_composing();
        let state = match intent {
            Intent::Edit(command) => {
                let caps = editor.edit_capabilities(cx);
                CommandState::enabled(match command {
                    EditCommand::Copy => caps.copy,
                    EditCommand::Cut => writable && caps.cut,
                    EditCommand::Paste => writable && caps.paste,
                    EditCommand::PastePlain => writable && caps.paste_plain,
                    EditCommand::PasteMarkdown => writable && caps.paste_markdown,
                })
            }
            Intent::PastePlain => {
                return self.editing_state(&Intent::Edit(EditCommand::PastePlain), cx);
            }
            Intent::PasteMarkdown => {
                return self.editing_state(&Intent::Edit(EditCommand::PasteMarkdown), cx);
            }
            Intent::Mark(mark) => CommandState {
                enabled: writable && editor.can_toggle_mark(mark.mark()),
                checked: Some(mark.is_active(&editor.active_marks())),
            },
            Intent::Block(block) => CommandState {
                enabled: writable && (block.command())(editor.state()).is_some(),
                checked: Some(
                    doc::Block::active(editor.state(), &editor.projection()) == Some(*block),
                ),
            },
            Intent::Undo => CommandState::enabled(
                writable && markraft_core::history::undo_depth(editor.state()) > 0,
            ),
            Intent::Redo => CommandState::enabled(
                writable && markraft_core::history::redo_depth(editor.state()) > 0,
            ),
            Intent::EditLink | Intent::Unlink => {
                CommandState::enabled(writable && editor.active_link().is_some())
            }
            Intent::Link => CommandState::enabled(
                writable
                    && !doc::types().in_verbatim_block_at(editor.state())
                    && markraft_commonmark::Formatter::new(self.house.clone())
                        .set_link("https://example.com", "")(editor.state())
                    .is_ok_and(|spec| spec.is_some()),
            ),
            Intent::InsertTable => CommandState::enabled(
                writable
                    && editor.can_table(markraft_gpui::TableOp::Insert {
                        rows: 2,
                        columns: 3,
                    }),
            ),
            Intent::CopyLink | Intent::OpenLink => {
                CommandState::enabled(editor.active_link().is_some())
            }
            Intent::Table(edit) => {
                CommandState::enabled(writable && editor.can_table((*edit).into()))
            }
            Intent::ChooseCodeLanguage => CommandState::enabled(
                writable
                    && doc::Block::active(editor.state(), &editor.projection())
                        == Some(doc::Block::Code),
            ),
            _ => return None,
        };
        Some(state)
    }

    pub(super) fn editing_enabled(&self, intent: &Intent, cx: &App) -> bool {
        self.editing_state(intent, cx)
            .is_none_or(|state| state.enabled)
    }

    pub(super) fn edit_selection(
        &mut self,
        command: EditCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.editing_enabled(&Intent::Edit(command), cx) {
            return;
        }
        self.close_popover(cx);
        self.set_panel(Panel::Editor, cx);
        self.focus_editor(window, cx);
        // Explicitly dispatch to this editor. A native popup temporarily owns
        // keyboard focus, so the global active responder is not the target.
        let focus = self.editor().focus_handle(cx);
        focus.dispatch_action(command.action().as_ref(), window, cx);
    }
}
