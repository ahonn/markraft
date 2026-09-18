//! Keyboard reach for the chrome. Real focus stays on the one panel handle (or on the
//! query editor), and this module keeps a ring on the control Tab has walked to, so a
//! panel, a popover or the folder chooser can be driven without a pointer.

use super::*;

/// The stop id of the shared query editor. It is the same wherever the field appears,
/// because only one surface borrows it at a time.
pub(super) const QUERY: &str = "query-field";

/// The surface Tab walks. At most one is open, and the order mirrors the precedence
/// `panel_key` already uses for the arrow keys.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Surface {
    /// Nothing is open; Tab belongs to the note.
    Editor,
    /// The first-run and folder-missing screen.
    Chooser,
    Format,
    CodeLanguage,
    LinkView,
    LinkEdit,
    /// The pill over the table the caret is in. Unlike the others it is not modal: the
    /// note keeps the keyboard, and Tab belongs to the grid's cells until the ring has
    /// stepped up onto the pill.
    Table,
    Picker,
    Actions,
    Settings,
}

/// What Enter or Space does at a stop.
enum Act {
    /// Hand the keyboard back to the query editor.
    Query,
    /// Only move the list selection; the row's own controls carry the actions.
    Select,
    Run(Intent),
}

/// One keyboard stop, in Tab order.
struct Stop {
    /// Matches the [`NotesApp::ring`] call that decorates the control.
    id: SharedString,
    act: Act,
    /// The list row the stop belongs to, so reaching it also selects that row.
    row: Option<usize>,
}

impl Stop {
    fn run(id: impl Into<SharedString>, intent: Intent) -> Self {
        Self {
            id: id.into(),
            act: Act::Run(intent),
            row: None,
        }
    }
    fn select(id: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            act: Act::Select,
            row: None,
        }
    }
    fn query() -> Self {
        Self {
            id: QUERY.into(),
            act: Act::Query,
            row: None,
        }
    }
    fn in_row(mut self, row: usize) -> Self {
        self.row = Some(row);
        self
    }
    fn is_query(&self) -> bool {
        matches!(self.act, Act::Query)
    }
}

impl NotesApp {
    pub(super) fn surface(&self) -> Surface {
        if self.persistence.is_none() {
            return Surface::Chooser;
        }
        if self.format_menu.is_some() && self.panel == Panel::Editor {
            return Surface::Format;
        }
        if self.code_language_block.is_some() {
            return Surface::CodeLanguage;
        }
        match self.link_popover {
            Some(LinkPopover::Edit) => return Surface::LinkEdit,
            Some(LinkPopover::View) => return Surface::LinkView,
            None => {}
        }
        if self.table.is_some() {
            return Surface::Table;
        }
        match self.panel {
            Panel::Editor => Surface::Editor,
            Panel::Browse | Panel::Trash => Surface::Picker,
            Panel::Actions => Surface::Actions,
            Panel::Settings => Surface::Settings,
        }
    }

    /// Every stop of the open surface, in the order it is drawn. Row controls are listed
    /// for each row rather than only the selected one, so the walk is symmetric: reaching
    /// any of them selects its row, which is also what reveals them.
    fn focus_stops(&self, cx: &App) -> Vec<Stop> {
        let surface = self.surface();
        if surface == Surface::Editor {
            return Vec::new();
        }
        let mut stops = Vec::new();
        // A notice offering an action is reachable from whatever else is open.
        if self
            .notice
            .as_ref()
            .is_some_and(|notice| notice.undo.is_some())
        {
            stops.push(Stop::run("notice-undo", Intent::UndoDelete));
        }
        match surface {
            Surface::Editor => {}
            Surface::Chooser => {
                let first_launch = self.path.is_none();
                if !first_launch {
                    stops.push(Stop::run("retry-open", Intent::Retry));
                }
                stops.push(Stop::run("choose-folder", Intent::ChooseFolder));
                if first_launch && Self::default_folder().is_some() {
                    stops.push(Stop::run("default-folder", Intent::DefaultFolder));
                }
                if !first_launch {
                    stops.push(Stop::run("reveal-library", Intent::Reveal));
                }
            }
            Surface::Format => {
                for (index, (_, _, intent, _)) in self.format_items(cx).into_iter().enumerate() {
                    stops.push(Stop::run(format!("format-choice-{index}"), intent).in_row(index));
                }
            }
            Surface::CodeLanguage => {
                stops.push(Stop::query());
                for (index, (language, _)) in
                    self.matching_code_languages(cx).into_iter().enumerate()
                {
                    stops.push(
                        Stop::run(
                            format!("code-language-{index}"),
                            Intent::CodeLanguage(language),
                        )
                        .in_row(index),
                    );
                }
            }
            Surface::LinkView => {
                stops.push(Stop::run("link-edit", Intent::EditLink));
                stops.push(Stop::run("link-copy", Intent::CopyLink));
                stops.push(Stop::run("link-open", Intent::OpenLink));
                stops.push(Stop::run("link-unlink", Intent::Unlink));
            }
            Surface::LinkEdit => {
                stops.push(Stop::query());
                stops.push(Stop::run("link-apply", Intent::ApplyLink));
                stops.push(Stop::run("link-remove", Intent::Unlink));
            }
            Surface::Table => {
                for (id, intent) in self.table_stops() {
                    stops.push(Stop::run(id, intent));
                }
            }
            Surface::Picker => {
                let deleted = self.panel == Panel::Trash;
                stops.push(Stop::query());
                let query = self.query.read(cx).text().to_owned();
                for (index, note) in self
                    .matching_notes(query.trim(), deleted)
                    .iter()
                    .enumerate()
                {
                    let id = note.id.clone();
                    if deleted {
                        // Selecting a deleted note reveals its buttons rather than
                        // putting the note back, or throwing it away, on the spot.
                        stops.push(Stop::select(id.clone()).in_row(index));
                        stops.push(
                            Stop::run(format!("purge-{id}"), Intent::PurgeNote(id.clone()))
                                .in_row(index),
                        );
                        stops.push(
                            Stop::run(format!("restore-{id}"), Intent::Restore(id)).in_row(index),
                        );
                    } else {
                        stops.push(Stop::run(id.clone(), Intent::Select(id.clone())).in_row(index));
                        stops.push(
                            Stop::run(format!("pin-{id}"), Intent::PinNote(id.clone()))
                                .in_row(index),
                        );
                        stops.push(
                            Stop::run(format!("trash-{id}"), Intent::TrashNote(id)).in_row(index),
                        );
                    }
                }
                stops.push(Stop::run(
                    "browse-trash",
                    if deleted {
                        Intent::Browse
                    } else {
                        Intent::Trash
                    },
                ));
            }
            Surface::Actions => {
                stops.push(Stop::query());
                for (index, command) in self.filtered_actions(cx).into_iter().enumerate() {
                    if let Some(intent) = command.intent {
                        stops.push(Stop::run(command.id, intent).in_row(index));
                    }
                }
            }
            Surface::Settings => {
                stops.push(Stop::run("settings-done", Intent::Back));
                for (id, mode) in [
                    ("theme-system", None),
                    ("theme-light", Some(false)),
                    ("theme-dark", Some(true)),
                ] {
                    stops.push(Stop::run(id, Intent::Theme(mode)));
                }
                stops.push(Stop::run("auto-height", Intent::AutoHeight));
                stops.push(Stop::run("vim-mode", Intent::VimMode));
                stops.push(Stop::run("launch-at-login", Intent::Login));
                stops.push(Stop::run("change-folder", Intent::ChooseFolder));
                stops.push(Stop::query());
                stops.push(Stop::run("apply-shortcut", Intent::Shortcut));
                stops.push(Stop::run("import-notes", Intent::Import));
                stops.push(Stop::run("export-library", Intent::SaveCopy));
                stops.push(Stop::run("show-storage", Intent::Reveal));
            }
        }
        stops
    }

    /// Move the ring to the next or previous stop, wrapping at both ends. Returns false
    /// when no surface is open, which leaves Tab to the note as Indent.
    pub(super) fn focus_step(
        &mut self,
        forward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let stops = self.focus_stops(cx);
        if stops.is_empty() {
            return false;
        }
        if self.surface() == Surface::Table {
            return self.table_step(forward, &stops, window, cx);
        }
        let current = match &self.chrome_focus {
            Some(id) => stops.iter().position(|stop| &stop.id == id),
            // A caret in the query field is already standing on that stop.
            None => self
                .query
                .focus_handle(cx)
                .is_focused(window)
                .then(|| stops.iter().position(Stop::is_query))
                .flatten(),
        };
        let next = match (current, forward) {
            (Some(index), true) => (index + 1) % stops.len(),
            (Some(index), false) => (index + stops.len() - 1) % stops.len(),
            // Nothing is ringed yet: start where the arrow keys left the list, so Tab
            // continues the walk rather than restarting it.
            (None, true) => self.selected_stop(&stops).unwrap_or(0),
            (None, false) => self
                .selected_stop(&stops)
                .unwrap_or_else(|| stops.len() - 1),
        };
        let stop = &stops[next];
        let (id, query, row) = (stop.id.clone(), stop.is_query(), stop.row);
        if let Some(row) = row {
            self.select_row(row);
        }
        self.chrome_focus = Some(id);
        if query {
            window.focus(&self.query.focus_handle(cx), cx);
        } else {
            window.focus(&self.panel_focus, cx);
        }
        cx.notify();
        true
    }

    /// Tab and ⇧Tab inside a table belong to its cells, which is where the keyboard
    /// already moves. ⇧Tab in the first cell has nowhere to go inside the grid, so it
    /// steps up onto the toolbar; from there both keys walk the pill, and stepping off
    /// either end hands the note its keyboard back.
    fn table_step(
        &mut self,
        forward: bool,
        stops: &[Stop],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let current = self
            .chrome_focus
            .as_ref()
            .and_then(|id| stops.iter().position(|stop| &stop.id == id));
        let next = match current {
            Some(index) if forward => (index + 1 < stops.len()).then_some(index + 1),
            Some(index) => index.checked_sub(1),
            None if !forward
                && self
                    .table
                    .is_some_and(|table| table.row == 0 && table.column == 0) =>
            {
                Some(0)
            }
            // Everywhere else in the grid, both keys are the cells'.
            None => return false,
        };
        match next {
            Some(index) => {
                self.chrome_focus = Some(stops[index].id.clone());
                window.focus(&self.panel_focus, cx);
            }
            None => self.focus_editor(window, cx),
        }
        cx.notify();
        true
    }

    /// The first stop of the row the open surface has selected, if it has a list.
    fn selected_stop(&self, stops: &[Stop]) -> Option<usize> {
        let row = match self.surface() {
            Surface::Format => self.format_selected,
            Surface::CodeLanguage => self.code_language_selected,
            Surface::Picker | Surface::Actions => self.selected,
            _ => return None,
        };
        stops.iter().position(|stop| stop.row == Some(row))
    }

    /// Move the open surface's list to `row`, keeping it in view. Every path that
    /// changes a selection goes through here — the ring, the arrow keys and the
    /// pointer — so a question standing on the row being left is taken back with it.
    pub(super) fn select_row(&mut self, row: usize) {
        self.confirm_purge = None;
        match self.surface() {
            Surface::Format => {
                self.format_selected = row;
                self.format_scroll.scroll_to_item(row);
            }
            Surface::CodeLanguage => {
                self.code_language_selected = row;
                self.code_language_scroll.scroll_to_item(row);
            }
            Surface::Picker => {
                self.selected = row;
                self.picker_scroll.scroll_to_item(row);
            }
            Surface::Actions => {
                self.selected = row;
                self.actions_scroll.scroll_to_item(row);
            }
            _ => {}
        }
    }

    /// Run the ringed control. Returns false when the ring is on the query field, on a
    /// row that only selects, or nowhere, so Enter keeps its ordinary panel meaning.
    pub(super) fn focus_activate(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(id) = self.chrome_focus.clone() else {
            return false;
        };
        let intent = self
            .focus_stops(cx)
            .into_iter()
            .find(|stop| stop.id == id)
            .and_then(|stop| match stop.act {
                Act::Run(intent) => Some(intent),
                Act::Query | Act::Select => None,
            });
        let Some(intent) = intent else {
            return false;
        };
        self.intent(intent, window, cx);
        true
    }

    /// Draw the keyboard focus ring around `element` while the ring rests on `id`. It
    /// sits outside the control's own box, so showing it moves nothing.
    pub(super) fn ring(&self, id: &str, radius: Pixels, element: Stateful<Div>) -> Stateful<Div> {
        if self.chrome_focus.as_deref() != Some(id) {
            return element;
        }
        element.child(
            div()
                .absolute()
                .inset(px(-4.))
                .rounded(radius + px(4.))
                .border_2()
                .border_color(notes_style(self.dark).marker),
        )
    }
}
