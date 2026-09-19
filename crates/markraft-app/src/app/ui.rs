mod code;
mod focus;
mod formatting;
pub(super) mod html;
mod icons;
mod link;
pub(in crate::app) mod slash;
mod table;
mod tokens;
mod vim;

use super::*;
use focus::Surface;
use icons::{Icon, icon, sized_icon};
use slash::{Command, SlashEffect};
use table::TableEdit;
pub(in crate::app) use tokens::playback;
use tokens::{POPOVER_RADIUS, ROW_HEIGHT, ROW_RADIUS, keycaps, popover_shadow};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone)]
enum Intent {
    SaveHtml,
    CancelHtml,
    EditHtml(usize),
    New,
    Browse,
    Trash,
    Actions,
    ToggleFormatToolbar,
    ToggleCount,
    FormatMenu(FormatMenu),
    Settings,
    Link,
    EditLink,
    ApplyLink,
    Unlink,
    CopyLink,
    OpenLink,
    Back,
    Save,
    Undo,
    Redo,
    Delete,
    UndoDelete,
    Pin,
    PinNote(String),
    TrashNote(String),
    Copy,
    PastePlain,
    PasteMarkdown,
    Export,
    Import,
    Select(String),
    Restore(String),
    /// Asked twice: the first one turns the row's button into the question.
    PurgeNote(String),
    EmptyTrash,
    CodeLanguage(&'static str),
    Theme(Option<bool>),
    Login,
    AutoHeight,
    VimMode,
    Shortcut,
    Reveal,
    ChooseFolder,
    DefaultFolder,
    Retry,
    SaveCopy,
    Reload,
    Mark(doc::Inline),
    Block(doc::Block),
    InsertTable,
    Table(TableEdit),
}

/// Shared ordering and separators for the command menu, including filtered results.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ActionGroup {
    Notes,
    Editing,
    Formatting,
    Context,
    Files,
    View,
    Recovery,
}

impl Intent {
    fn action_group(&self) -> ActionGroup {
        match self {
            Self::New | Self::Browse | Self::Pin => ActionGroup::Notes,
            Self::Undo | Self::Redo | Self::Copy | Self::PastePlain | Self::PasteMarkdown => {
                ActionGroup::Editing
            }
            Self::Mark(_) | Self::Block(_) | Self::Link | Self::InsertTable => {
                ActionGroup::Formatting
            }
            Self::Table(_) | Self::EditLink | Self::CopyLink | Self::OpenLink | Self::Unlink => {
                ActionGroup::Context
            }
            Self::Save | Self::Export | Self::Import | Self::Reveal | Self::SaveCopy => {
                ActionGroup::Files
            }
            Self::Trash | Self::Delete | Self::EmptyTrash => ActionGroup::Recovery,
            _ => ActionGroup::View,
        }
    }
}

/// Where the caret is, for the commands that only apply in one place. The editor's `/`
/// menu is built before there is an editor to ask, so it takes the default: every
/// command that does not depend on where the caret is.
#[derive(Clone, Copy, Default)]
struct Caret {
    in_link: bool,
    /// The caret's table, as the last frame painted it.
    table: Option<TableInfo>,
    /// A code block keeps its text literal, so nothing is inserted into one.
    in_code: bool,
}
/// The settings rows drawn as a switch rather than as a labelled button.
fn is_switch(id: &str) -> bool {
    matches!(id, "auto-height" | "launch-at-login" | "vim-mode")
}

/// Critically damped, settling in about 150 ms: the switch knob eases into its new
/// end and reverses from wherever it is when the row is flipped back.
const SWITCH_SPRING: SpringConfig = SpringConfig::new(3700., 121.7, 1.);
const ACTION_ROW_HEIGHT: Pixels = px(36.);

/// A stored timestamp read on this Mac's clock. Notes carry UTC; a date label has to be
/// the one on the user's calendar, so both ends of a comparison are shifted before they
/// are cut into days.
fn local(milliseconds: u64) -> u64 {
    milliseconds.saturating_add_signed(crate::platform::local_utc_offset() * 1000)
}

/// When a note was last written, as the user would say it. Past a week it is a date,
/// and past this year the date carries it.
fn relative_day(updated: u64, now: u64) -> String {
    const DAY: u64 = 86_400_000;
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let (updated, now) = (local(updated), local(now));
    // A note written with the clock ahead of this one still reads as today.
    match (now / DAY).saturating_sub(updated / DAY) {
        0 => "Today".to_owned(),
        1 => "Yesterday".to_owned(),
        days @ 2..=6 => format!("{days} days ago"),
        _ => {
            let (year, month, date, ..) = crate::vault::civil(updated);
            let month = MONTHS[(month.clamp(1, 12) - 1) as usize];
            let (this_year, ..) = crate::vault::civil(now);
            if year == this_year {
                format!("{date} {month}")
            } else {
                format!("{date} {month} {year}")
            }
        }
    }
}

/// The icon a command shows in the ⌘K panel and in the `/` menu.
fn intent_icon(intent: &Intent) -> Icon {
    match intent {
        Intent::New => Icon::Plus,
        Intent::Browse => Icon::Notes,
        Intent::Pin => Icon::Pin,
        Intent::Trash | Intent::Delete | Intent::PurgeNote(_) | Intent::EmptyTrash => Icon::Trash,
        Intent::Copy | Intent::SaveCopy | Intent::CopyLink => Icon::Copy,
        Intent::Export | Intent::Import => Icon::Export,
        Intent::Settings => Icon::Settings,
        Intent::Save => Icon::Check,
        Intent::Undo | Intent::Redo | Intent::UndoDelete => Icon::Restore,
        Intent::ToggleFormatToolbar | Intent::ToggleCount => Icon::Text,
        Intent::OpenLink | Intent::Reveal => Icon::Open,
        Intent::Unlink => Icon::Link,
        Intent::Mark(doc::Inline::Bold) => Icon::Bold,
        Intent::Mark(doc::Inline::Italic) => Icon::Italic,
        Intent::Mark(doc::Inline::Code) => Icon::Code,
        Intent::Mark(doc::Inline::Strikethrough) => Icon::Strikethrough,
        Intent::Mark(doc::Inline::Underline) => Icon::Underline,
        Intent::Link => Icon::Link,
        Intent::Block(doc::Block::Heading(_)) => Icon::Heading,
        Intent::Block(doc::Block::Quote) => Icon::Quote,
        Intent::Block(doc::Block::Code) => Icon::CodeBlock,
        Intent::Block(doc::Block::Ordered) => Icon::Ordered,
        Intent::Block(doc::Block::Bullet) => Icon::Bullet,
        Intent::Block(doc::Block::Task) => Icon::Task,
        Intent::Block(doc::Block::Divider) => Icon::Divider,
        Intent::InsertTable => Icon::Table,
        Intent::Table(edit) => edit.icon(),
        _ => Icon::Paragraph,
    }
}

impl NotesApp {
    fn intent(&mut self, intent: Intent, window: &mut Window, cx: &mut Context<Self>) {
        if self.html_editor.is_some() && !matches!(intent, Intent::SaveHtml | Intent::CancelHtml) {
            return;
        }
        match intent {
            Intent::SaveHtml => self.save_html_source(window, cx),
            Intent::CancelHtml => {
                self.cancel_html_source(window, cx);
            }
            Intent::EditHtml(pos) => self.open_html_source(pos, window, cx),
            Intent::New => self.new_note(window, cx),
            Intent::Browse => self.open_panel(Panel::Browse, window, cx),
            Intent::Trash => self.open_panel(Panel::Trash, window, cx),
            Intent::Actions => self.open_panel(Panel::Actions, window, cx),
            Intent::ToggleFormatToolbar => {
                self.code_language_block = None;
                self.link_popover = None;
                self.format_toolbar = !self.format_toolbar;
                self.format_menu = None;
                self.panel = Panel::Editor;
                self.focus_editor(window, cx);
                cx.notify();
            }
            Intent::ToggleCount => {
                self.show_words = !self.show_words;
                self.panel = Panel::Editor;
                self.focus_editor(window, cx);
                cx.notify();
            }
            Intent::FormatMenu(menu) => self.open_format_menu(menu, window, cx),
            Intent::Settings => self.open_panel(Panel::Settings, window, cx),
            Intent::Link => {
                self.panel = Panel::Editor;
                self.open_link_popover(window, cx);
            }
            Intent::EditLink => self.edit_link(window, cx),
            Intent::ApplyLink => self.apply_link(window, cx),
            Intent::Unlink => self.unlink(window, cx),
            Intent::CopyLink | Intent::OpenLink => {
                if let Some(url) = self.editor().read(cx).active_link() {
                    if matches!(intent, Intent::CopyLink) {
                        cx.write_to_clipboard(ClipboardItem::new_string(url));
                        self.inform("Copied link", cx);
                    } else {
                        EditorView::open_link(&url, cx);
                    }
                }
                self.link_popover = None;
                self.focus_editor(window, cx);
                cx.notify();
            }
            Intent::Back => {
                self.code_language_block = None;
                self.link_popover = None;
                self.query.update(cx, |e, cx| e.cancel_composition(cx));
                self.panel = Panel::Editor;
                self.focus_editor(window, cx);
                cx.notify();
            }
            Intent::Save => {
                self.intent(Intent::Back, window, cx);
                self.flush(cx);
            }
            // The editor owns the note's history, so these reach it as its own actions.
            Intent::Undo | Intent::Redo => {
                self.intent(Intent::Back, window, cx);
                let action: Box<dyn Action> = if matches!(intent, Intent::Undo) {
                    Box::new(markraft_gpui::Undo)
                } else {
                    Box::new(markraft_gpui::Redo)
                };
                window.dispatch_action(action, cx);
            }
            Intent::Delete => self.delete_note(window, cx),
            Intent::UndoDelete => self.undo_delete(window, cx),
            Intent::Pin => {
                let id = self.library.active_id.clone();
                self.toggle_pin(&id, cx);
                self.intent(Intent::Back, window, cx);
            }
            Intent::PinNote(id) => self.toggle_pin(&id, cx),
            Intent::TrashNote(id) => self.trash_note(&id, window, cx),
            Intent::Select(id) => self.select_note(&id, window, cx),
            Intent::Restore(id) => self.restore_note(&id, window, cx),
            Intent::PurgeNote(id) => {
                if self.confirm_purge.as_deref() == Some(id.as_str()) {
                    self.purge_notes(vec![id], window, cx);
                } else {
                    self.confirm_purge = Some(id);
                    cx.notify();
                }
            }
            Intent::EmptyTrash => {
                self.intent(Intent::Back, window, cx);
                self.empty_trash(window, cx);
            }
            Intent::CodeLanguage(language) => self.apply_code_language(language, window, cx),
            Intent::Copy => {
                self.copy_markdown(cx);
                self.intent(Intent::Back, window, cx);
            }
            Intent::PastePlain | Intent::PasteMarkdown => {
                self.intent(Intent::Back, window, cx);
                let action: Box<dyn Action> = if matches!(intent, Intent::PastePlain) {
                    Box::new(markraft_gpui::PastePlain)
                } else {
                    Box::new(markraft_gpui::PasteMarkdown)
                };
                window.dispatch_action(action, cx);
            }
            Intent::Export => {
                self.intent(Intent::Back, window, cx);
                self.export(cx);
            }
            Intent::Import => self.import(window, cx),
            Intent::Theme(mode) => {
                self.library.preferences.dark_mode = mode;
                self.apply_theme(window, cx);
                self.changed(cx);
            }
            Intent::Login => {
                if let Some(platform) = &mut self.platform {
                    let enabled = platform.launch_at_login_enabled();
                    match platform.set_launch_at_login(!enabled) {
                        Ok(()) => self.inform("Updated login setting", cx),
                        Err(e) => {
                            self.platform_error = Some(e);
                            cx.notify();
                        }
                    }
                }
            }
            Intent::AutoHeight => {
                self.library.preferences.auto_height = !self.library.preferences.auto_height;
                self.changed(cx);
            }
            Intent::VimMode => self.toggle_vim(window, cx),
            Intent::Shortcut => self.apply_shortcut(cx),
            Intent::Reveal => {
                if let Some(path) = &self.path {
                    cx.reveal_path(path);
                }
            }
            Intent::ChooseFolder => self.choose_folder(window, cx),
            Intent::DefaultFolder => {
                if let Some(directory) = Self::default_folder() {
                    self.open_folder(directory, window, cx);
                }
            }
            Intent::Retry => self.recover(window, cx),
            Intent::SaveCopy => self.save_copy(cx),
            Intent::Reload => self.reload(window, cx),
            Intent::Mark(mark) => {
                self.format_menu = None;
                self.editor()
                    .update(cx, |e, cx| e.run_command(&mark.command(), cx));
                self.panel = Panel::Editor;
                self.focus_editor(window, cx);
            }
            Intent::Block(block) => {
                self.format_menu = None;
                self.editor()
                    .update(cx, |e, cx| e.run_command(&block.command(), cx));
                self.panel = Panel::Editor;
                self.focus_editor(window, cx);
            }
            Intent::InsertTable => {
                self.format_menu = None;
                self.link_popover = None;
                self.editor().update(cx, |e, cx| e.insert_table(2, 3, cx));
                self.panel = Panel::Editor;
                self.focus_editor(window, cx);
            }
            // The table commands keep the caret where it is, so the note takes the
            // keyboard back and the toolbar re-anchors from the next paint.
            Intent::Table(edit) => {
                self.format_menu = None;
                self.editor().update(cx, |e, cx| edit.run(e, cx));
                self.panel = Panel::Editor;
                self.focus_editor(window, cx);
            }
        }
    }
    /// What a destructive control is written in: deleting a note, emptying the trash,
    /// taking a table away.
    fn danger(&self) -> Hsla {
        if self.dark {
            rgb(0xf18a8a)
        } else {
            rgb(0xc44d4d)
        }
        .into()
    }
    fn muted(&self) -> Hsla {
        tokens::muted(self.dark)
    }
    fn hover_color(&self) -> Hsla {
        if self.dark {
            rgb(0x36373a)
        } else {
            rgb(0xeaeaea)
        }
        .into()
    }
    fn selected_color(&self) -> Hsla {
        if self.dark {
            rgb(0x414246)
        } else {
            rgb(0xe2e2e2)
        }
        .into()
    }
    fn pressed_color(&self) -> Hsla {
        if self.dark {
            rgb(0x505156)
        } else {
            rgb(0xd5d5d5)
        }
        .into()
    }
    fn control_text(&self) -> Hsla {
        if self.dark {
            rgb(0xe6e6e8)
        } else {
            rgb(0x272727)
        }
        .into()
    }
    fn border_color(&self) -> Hsla {
        tokens::border_color(self.dark)
    }
    fn button(
        &self,
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
        intent: Intent,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        self.button_with_hover(id, label, intent, self.hover_color(), cx)
    }
    /// GPUI accepts a single hover style per element, so callers that need a
    /// different hover fill choose it here instead of chaining another `.hover`.
    fn button_with_hover(
        &self,
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
        intent: Intent,
        hover: Hsla,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let id: SharedString = id.into();
        let label: SharedString = label.into();
        let accessible_label = match id.as_ref() {
            "auto-height" => "Automatic height",
            "launch-at-login" => "Launch at login",
            "vim-mode" => "Vim mode",
            _ => label.as_ref(),
        }
        .to_string();
        let switch = is_switch(&id);
        self.ring(
            &id.clone(),
            px(6.),
            div()
                .id(id.clone())
                .role(Role::Button)
                .aria_label(accessible_label)
                .when(switch, |s| {
                    s.role(Role::Switch)
                        .aria_toggled(if label.as_ref() == "On" {
                            accesskit::Toggled::True
                        } else {
                            accesskit::Toggled::False
                        })
                })
                .px_2()
                .h(px(28.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.))
                .cursor_pointer()
                .text_size(px(13.))
                .text_color(self.muted())
                .hover(move |s| s.bg(hover))
                .active(|s| s.bg(self.pressed_color()).text_color(self.control_text()))
                .on_click(
                    cx.listener(move |this, _, window, cx| this.intent(intent.clone(), window, cx)),
                )
                .child(if switch {
                    self.switch(&id, label.as_ref() == "On", cx)
                        .into_any_element()
                } else {
                    label.into_any_element()
                }),
        )
    }
    /// A settings switch: its track, and the knob that slides between the two ends.
    /// Reduced motion keeps both ends and drops the slide. The spring is keyed by the
    /// row, so each switch carries its own position.
    fn switch(&self, id: &str, enabled: bool, cx: &App) -> SpringAnimationElement<Div> {
        let off: Hsla = if self.dark {
            rgb(0x595b62)
        } else {
            rgb(0xc5c6cb)
        }
        .into();
        let on = notes_style(self.dark).marker;
        div()
            .w(px(28.))
            .h(px(17.))
            .p(px(2.))
            .rounded_full()
            .with_spring(
                SharedString::from(format!("{id}-switch")),
                SpringAnimation::new(SWITCH_SPRING)
                    .to(enabled)
                    .playback(playback(cx.reduce_motion())),
                move |track, phase| {
                    track.bg(phase.interpolate_clamped(off, on)).child(
                        div()
                            .size(px(13.))
                            .rounded_full()
                            .bg(rgb(0xffffff))
                            .ml(phase.interpolate_clamped(px(0.), px(11.))),
                    )
                },
            )
    }
    fn surface_color(&self) -> Hsla {
        if self.dark {
            rgb(0x2e2f33)
        } else {
            rgb(0xf8f8f8)
        }
        .into()
    }
    fn shortcut(&self, hint: &str) -> Div {
        keycaps(hint, self.dark)
    }
    fn icon_button(
        &self,
        id: impl Into<SharedString>,
        label: &'static str,
        kind: Icon,
        intent: Intent,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let id: SharedString = id.into();
        let expanded = match intent {
            Intent::Browse => Some(self.panel == Panel::Browse || self.panel == Panel::Trash),
            Intent::Actions => Some(self.panel == Panel::Actions),
            _ => None,
        };
        let chrome = matches!(
            intent,
            Intent::Actions | Intent::Browse | Intent::New | Intent::ToggleFormatToolbar
        );
        let hover = if chrome {
            self.chrome_fill(0.05)
        } else {
            self.hover_color()
        };
        let pressed = if chrome {
            self.chrome_fill(0.10)
        } else {
            self.pressed_color()
        };
        let selected = if chrome {
            pressed
        } else {
            self.selected_color()
        };
        let icon_size = if chrome && !matches!(intent, Intent::ToggleFormatToolbar) {
            20.
        } else {
            16.
        };
        self.ring(
            &id.clone(),
            px(if chrome { 16. } else { 6. }),
            div()
                .id(id)
                .role(Role::Button)
                .aria_label(label)
                .size(px(28.))
                .opacity(0.7)
                .when_some(expanded, |s, expanded| s.aria_expanded(expanded))
                .when(expanded == Some(true), |s| s.bg(selected).opacity(1.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.))
                .cursor_pointer()
                .hover(|s| {
                    s.bg(if expanded == Some(true) {
                        selected
                    } else {
                        hover
                    })
                    .opacity(1.)
                })
                .active(|s| s.bg(pressed).opacity(1.))
                .tooltip(self.hint(label))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.intent(intent.clone(), window, cx);
                }))
                .child(sized_icon(kind, self.chrome_icon_color(), icon_size)),
        )
    }
    /// Toolbar icons recede while another application is active.
    pub(super) fn chrome_icon_color(&self) -> Hsla {
        if self.window_active {
            self.control_text()
        } else {
            self.muted()
        }
    }
    /// Tooltip builder; a label may carry a shortcut after " · ".
    pub(super) fn hint(
        &self,
        label: &'static str,
    ) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
        let dark = self.dark;
        move |_, cx| cx.new(|_| Hint { label, dark }).into()
    }
    fn row(
        &self,
        id: &'static str,
        label: &'static str,
        hint: &'static str,
        intent: Intent,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let kind = intent_icon(&intent);
        let destructive = matches!(
            intent,
            Intent::Delete | Intent::EmptyTrash | Intent::Table(TableEdit::DeleteTable)
        );
        let ink = if destructive {
            self.danger()
        } else {
            self.control_text()
        };
        self.ring(
            id,
            ROW_RADIUS,
            div()
                .id(id)
                .role(Role::Button)
                .aria_label(label)
                .flex()
                .items_center()
                .gap(px(8.))
                .h(ROW_HEIGHT)
                .px_2()
                .rounded(ROW_RADIUS)
                .cursor_pointer()
                .text_size(px(13.))
                .text_color(ink)
                .hover(|s| s.bg(self.selected_color()))
                .active(|s| s.bg(self.pressed_color()))
                .on_click(
                    cx.listener(move |this, _, window, cx| this.intent(intent.clone(), window, cx)),
                )
                .child(icon(kind, ink))
                .child(div().flex_1().min_w_0().truncate().child(label))
                .child(self.shortcut(hint)),
        )
    }
    fn search_field(&self, cx: &mut Context<Self>) -> Div {
        div()
            .h(px(44.))
            .flex_shrink_0()
            .px_4()
            .pt(px(12.))
            .child(self.query_field(cx))
    }
    /// The one query editor, borrowed by whichever surface is open. It carries the name
    /// of the surface it is serving, set with its text in [`NotesApp::set_query`], so
    /// this is only the box the ring is drawn around.
    fn query_field(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        self.ring(
            focus::QUERY,
            px(6.),
            div()
                .id(focus::QUERY)
                .size_full()
                // Clicking into the field hands the keyboard back to the caret.
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, _| this.chrome_focus = None),
                )
                .child(self.query.clone()),
        )
    }
    fn picker(&self, heading: bool, cx: &mut Context<Self>) -> Div {
        let deleted = self.panel == Panel::Trash;
        let query = self.query.read(cx).text().to_owned();
        let notes = self.matching_notes(query.trim(), deleted);
        let total = notes.len();
        let now = crate::storage::timestamp();
        let mut list = div()
            .id("note-results")
            // Rows keep `Role::Button` inside the list: a `ListBoxOption` reaches VoiceOver as
            // static text and its AXPress lands outside the row, closing the panel instead.
            .role(Role::ListBox)
            .aria_label(if deleted { "Deleted notes" } else { "Notes" })
            .track_scroll(&self.picker_scroll)
            .overflow_y_scroll()
            .px_2()
            .pb_2();
        if notes.is_empty() {
            list = list.child(
                div()
                    .py_6()
                    .text_center()
                    .text_size(px(13.))
                    .text_color(self.muted())
                    .child(if deleted {
                        "No deleted notes"
                    } else {
                        "No matching notes"
                    }),
            );
        }
        for (index, note) in notes.iter().enumerate() {
            let id = note.id.clone();
            let current = note.id == self.library.active_id;
            let selected = index == self.selected;
            let status = if current && !deleted {
                "Current".to_owned()
            } else {
                let date = relative_day(note.deleted_at.unwrap_or(note.updated_at), now);
                let date = match date.as_str() {
                    "Today" | "Yesterday" => date.to_lowercase(),
                    _ => date,
                };
                format!("{} {date}", if deleted { "Deleted" } else { "Edited" })
            };
            let meta = format!(
                "{status} · {}",
                self.count_label(&doc::plain_text(&note.document))
            );
            // A deleted note's buttons are spelled out, so they sit on the row's second
            // line and leave its title the full width.
            let mut controls = div()
                .absolute()
                .right(px(6.))
                .top(px(if deleted { 27. } else { 15. }))
                .flex()
                .gap(px(4.));
            if !deleted && selected {
                controls = controls
                    .child(
                        self.icon_button(
                            SharedString::from(format!("pin-{id}")),
                            if note.pinned {
                                "Unpin Note"
                            } else {
                                "Pin Note"
                            },
                            Icon::Pin,
                            Intent::PinNote(id.clone()),
                            cx,
                        )
                        .size(px(24.))
                        .opacity(1.)
                        .aria_toggled(if note.pinned {
                            accesskit::Toggled::True
                        } else {
                            accesskit::Toggled::False
                        })
                        .when(note.pinned, |s| s.bg(self.hover_color())),
                    )
                    .child(
                        self.icon_button(
                            SharedString::from(format!("trash-{id}")),
                            "Move to Recently Deleted",
                            Icon::Trash,
                            Intent::TrashNote(id.clone()),
                            cx,
                        )
                        .size(px(24.)),
                    );
            } else if deleted && selected {
                // Putting a note back is spelled out; the row itself only selects.
                // Deleting for good is asked twice: the button becomes its own
                // confirmation, and Escape puts the question away.
                let confirming = self.confirm_purge.as_deref() == Some(id.as_str());
                let danger = self.danger();
                controls = controls
                    .child(
                        self.button(
                            SharedString::from(format!("purge-{id}")),
                            if confirming {
                                "Delete permanently?"
                            } else {
                                "Delete Permanently"
                            },
                            Intent::PurgeNote(id.clone()),
                            cx,
                        )
                        .h(px(24.))
                        .text_color(danger)
                        .when(confirming, |s| s.bg(danger.opacity(0.14))),
                    )
                    .child(
                        self.button(
                            SharedString::from(format!("restore-{id}")),
                            "Restore",
                            Intent::Restore(id.clone()),
                            cx,
                        )
                        .h(px(24.))
                        .text_color(self.control_text()),
                    );
            } else if note.pinned && !deleted {
                controls = controls.child(
                    div()
                        .size(px(24.))
                        .opacity(1.)
                        .flex()
                        .items_center()
                        .justify_center()
                        // Decorative: the row's own text already says "Pinned". Neither
                        // this wrapper nor the glyph carries an id or a role, so nothing
                        // of it reaches the accessibility tree.
                        .child(icon(Icon::Pin, self.muted())),
                );
            }
            let row_id = SharedString::from(id.clone());
            let select = id.clone();
            list = list.child(
                self.ring(
                    &row_id,
                    px(9.),
                    div()
                        .id(row_id.clone())
                        .role(Role::Button)
                        .aria_label(format!(
                            "{}, {meta}{}",
                            note.title(),
                            if note.pinned { ", Pinned" } else { "" }
                        ))
                        .aria_selected(selected)
                        .aria_position_in_set(index + 1)
                        .aria_size_of_set(total)
                        .relative()
                        .h(px(58.))
                        .flex_shrink_0()
                        .rounded(px(9.))
                        .px_2()
                        .flex()
                        .items_center()
                        .cursor_pointer()
                        .when(selected, |s| s.bg(self.selected_color()))
                        .hover(|s| s.bg(self.selected_color()))
                        .active(|s| s.bg(self.pressed_color()))
                        // The pointer picks a row without scrolling it: the list must
                        // not move out from under the cursor that is aiming at it.
                        .on_mouse_move(cx.listener(move |this, _, _, cx| {
                            if this.selected != index {
                                this.selected = index;
                                this.confirm_purge = None;
                                cx.notify();
                            }
                        }))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if deleted {
                                if this.selected != index {
                                    this.confirm_purge = None;
                                }
                                this.selected = index;
                                this.chrome_focus = None;
                                cx.notify();
                            } else {
                                this.intent(Intent::Select(select.clone()), window, cx);
                            }
                        }))
                        .child(
                            div()
                                .w_full()
                                // Clear of the row's controls.
                                .pr(px(if deleted { 8. } else { 60. }))
                                .child(
                                    div()
                                        .text_size(px(13.))
                                        .line_height(px(18.))
                                        .text_color(self.control_text())
                                        .truncate()
                                        .child(note.title()),
                                )
                                .child(
                                    div()
                                        .mt(px(2.))
                                        // The two labelled buttons of a selected
                                        // deleted note share this line.
                                        .pr(px(if deleted && selected { 202. } else { 0. }))
                                        .flex()
                                        .items_center()
                                        .gap(px(4.))
                                        .when(current && !deleted, |s| {
                                            s.child(
                                                div()
                                                    .size(px(4.))
                                                    .rounded_full()
                                                    .bg(notes_style(self.dark).marker),
                                            )
                                        })
                                        .child(
                                            div()
                                                .text_size(px(12.))
                                                .line_height(px(18.))
                                                .text_color(self.muted())
                                                .truncate()
                                                .child(meta),
                                        ),
                                ),
                        )
                        .child(controls),
                ),
            );
        }
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(self.search_field(cx))
            .when(heading, |s| {
                s.child(
                    div()
                        .flex_shrink_0()
                        .h(px(if deleted { 32. } else { 26. }))
                        .px_3()
                        .flex()
                        .items_center()
                        .justify_between()
                        .text_size(px(11.))
                        .text_color(self.muted())
                        .child(if deleted { "Recently Deleted" } else { "Notes" })
                        .when(deleted, |s| {
                            s.child(self.button("trash-back", "All Notes", Intent::Browse, cx))
                        }),
                )
            })
            .child(self.scroll_area(list, &self.picker_scroll, cx))
    }
    fn settings(&self, cx: &mut Context<Self>) -> Div {
        let mut themes = div()
            .flex()
            .p(px(2.))
            .rounded(px(7.))
            .bg(self.hover_color());
        for (id, label, mode) in [
            ("theme-system", "Auto", None),
            ("theme-light", "Light", Some(false)),
            ("theme-dark", "Dark", Some(true)),
        ] {
            // The selected segment is raised above its track in both themes, and keeps
            // that fill while hovered (the track itself is the ordinary hover color).
            let selected = self.library.preferences.dark_mode == mode;
            let raised: Hsla = if self.dark {
                rgb(0x4a4b50).into()
            } else {
                self.surface_color()
            };
            let hover = if selected {
                raised
            } else {
                self.selected_color()
            };
            themes = themes.child(
                self.button_with_hover(id, label, Intent::Theme(mode), hover, cx)
                    .aria_label(format!("{label} appearance"))
                    .when(selected, |s| {
                        s.bg(raised).text_color(notes_style(self.dark).text)
                    }),
            );
        }
        let login = self
            .platform
            .as_ref()
            .is_some_and(|p| p.launch_at_login_enabled());
        let content = div()
            .id("settings-content")
            .track_scroll(&self.settings_scroll)
            .overflow_y_scroll()
            .px_4()
            .pb_4()
            .text_size(px(13.))
            .child(
                div()
                    .id("appearance-group")
                    .role(Role::RadioGroup)
                    .aria_label("Appearance")
                    .py_2()
                    .flex()
                    .justify_between()
                    .items_center()
                    .child("Appearance")
                    .child(themes),
            )
            .child(
                div()
                    .py_2()
                    .flex()
                    .justify_between()
                    .items_center()
                    .child("Grow with content")
                    .child(self.button(
                        "auto-height",
                        if self.library.preferences.auto_height {
                            "On"
                        } else {
                            "Off"
                        },
                        Intent::AutoHeight,
                        cx,
                    )),
            )
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(self.muted())
                    .child("Manual resizing keeps your chosen height."),
            )
            .child(
                div()
                    .py_3()
                    .flex()
                    .justify_between()
                    .items_center()
                    .child("Vim mode")
                    .child(self.button(
                        "vim-mode",
                        if self.library.preferences.vim_mode {
                            "On"
                        } else {
                            "Off"
                        },
                        Intent::VimMode,
                        cx,
                    )),
            )
            .child(
                div()
                    .py_3()
                    .flex()
                    .justify_between()
                    .items_center()
                    .child("Launch at login")
                    .child(self.button(
                        "launch-at-login",
                        if login { "On" } else { "Off" },
                        Intent::Login,
                        cx,
                    )),
            )
            .child(
                div()
                    .mt_2()
                    .pt_3()
                    .border_t_1()
                    .border_color(self.border_color())
                    .text_size(px(11.))
                    .text_color(self.muted())
                    .child("Notes Folder"),
            )
            .child(
                div()
                    .id("folder-group")
                    .role(Role::Group)
                    .aria_label("Notes folder")
                    .mt_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .w_0()
                            .truncate()
                            .text_size(px(12.))
                            .child(
                                self.path
                                    .as_ref()
                                    .map(|path| path.display().to_string())
                                    .unwrap_or_default(),
                            ),
                    )
                    .child(self.button("change-folder", "Change…", Intent::ChooseFolder, cx)),
            )
            .child(
                div()
                    .mt_2()
                    .pt_3()
                    .border_t_1()
                    .border_color(self.border_color())
                    .text_size(px(11.))
                    .text_color(self.muted())
                    .child("Global Shortcut"),
            )
            .child(
                div()
                    .id("shortcut-group")
                    .role(Role::Group)
                    .aria_label("Global shortcut")
                    .mt_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .w_0()
                            .h(px(34.))
                            .px_2()
                            .pt(px(6.))
                            .rounded(px(6.))
                            .border_1()
                            .border_color(self.border_color())
                            .child(self.query_field(cx)),
                    )
                    .child(self.button("apply-shortcut", "Apply", Intent::Shortcut, cx)),
            )
            .child(
                div()
                    .mt_2()
                    .text_size(px(11.))
                    .text_color(self.muted())
                    .child("Alt+N or Ctrl+Shift+Space. Leave empty to disable."),
            )
            .child(
                div()
                    .mt_4()
                    .pt_2()
                    .border_t_1()
                    .border_color(self.border_color())
                    .child(self.row("import-notes", "Import Notes…", "⌘O", Intent::Import, cx))
                    .child(self.row(
                        "export-library",
                        "Export Library Backup…",
                        "",
                        Intent::SaveCopy,
                        cx,
                    ))
                    .child(self.row(
                        "show-storage",
                        "Show Notes Folder in Finder",
                        "",
                        Intent::Reveal,
                        cx,
                    )),
            )
            .child(
                div()
                    .mt_3()
                    .text_size(px(11.))
                    .text_color(self.muted())
                    .child("Saved on this Mac. Closing the window keeps Markraft running."),
            );
        self.scroll_area(content, &self.settings_scroll, cx)
    }
    fn panel_key(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.html_editor.is_some() {
            return self.html_key(key, window, cx);
        }
        // A ringed control answers first, whichever surface it belongs to.
        if key == "enter" && self.focus_activate(window, cx) {
            return true;
        }
        if self.surface() == Surface::Chooser {
            // The chooser has no list; Enter takes its primary action.
            if key == "enter" {
                self.choose_folder(window, cx);
                return true;
            }
            return false;
        }
        if self.format_menu.is_some() {
            return self.format_key(key, window, cx);
        }
        if self.query.read(cx).is_composing() {
            return false;
        }
        if self.code_language_block.is_some() {
            return self.code_language_key(key, window, cx);
        }
        if self.link_popover == Some(LinkPopover::Edit) {
            if key == "enter" {
                self.apply_link(window, cx);
            }
            // Up and down have nowhere to go in a one-line field.
            return true;
        }
        if self.panel == Panel::Editor {
            return false;
        }
        if self.panel == Panel::Settings {
            if key == "enter" {
                self.apply_shortcut(cx);
                return true;
            }
            return false;
        }
        if self.panel == Panel::Actions {
            let items = self.filtered_actions(cx);
            match key {
                "up" => self.selected = self.selected.saturating_sub(1),
                "down" => self.selected = (self.selected + 1).min(items.len().saturating_sub(1)),
                "enter" => {
                    if let Some(intent) = items.get(self.selected).and_then(|c| c.intent.clone()) {
                        self.intent(intent, window, cx);
                    }
                }
                _ => return false,
            }
            self.actions_scroll.scroll_to_item(self.selected);
        } else {
            let query = self.query.read(cx).text().to_owned();
            let notes = self.matching_notes(query.trim(), self.panel == Panel::Trash);
            match key {
                "up" => self.select_row(self.selected.saturating_sub(1)),
                "down" => self.select_row((self.selected + 1).min(notes.len().saturating_sub(1))),
                "enter" => {
                    if let Some(note) = notes.get(self.selected) {
                        let id = note.id.clone();
                        if self.panel == Panel::Trash {
                            self.restore_note(&id, window, cx);
                        } else {
                            self.select_note(&id, window, cx);
                        }
                    }
                }
                _ => return false,
            }
            self.picker_scroll.scroll_to_item(self.selected);
        }
        cx.notify();
        true
    }
    /// Every command the app offers, once: the ⌘K panel lists the ones with an intent
    /// and the editor's `/` menu the ones with a slash effect, so both keep the same
    /// labels and shortcut hints. The link and table commands only apply where the
    /// caret already is, so `caret` keeps them out of the panel everywhere else.
    fn action_items(&self, caret: Caret) -> Vec<Command> {
        let mut items = vec![
            Command::new("new-action", "New Note", "⌘N", Intent::New),
            Command::new(
                "pin-note",
                if self.library.active_note().pinned {
                    "Unpin Note"
                } else {
                    "Pin Note"
                },
                "",
                Intent::Pin,
            ),
            Command::new("browse-action", "Browse Notes", "⌘P", Intent::Browse),
            Command::new("undo-edit", "Undo", "⌘Z", Intent::Undo),
            Command::new("redo-edit", "Redo", "⇧⌘Z", Intent::Redo),
            Command::new("save-now", "Save Now", "⌘S", Intent::Save),
            Command::new("copy-markdown", "Copy as Markdown", "⇧⌘C", Intent::Copy),
            Command::new(
                "paste-plain",
                "Paste as Plain Text",
                "⇧⌘V",
                Intent::PastePlain,
            ),
            Command::new(
                "paste-markdown",
                "Paste as Markdown",
                "⌥⇧⌘V",
                Intent::PasteMarkdown,
            ),
            Command::new("export-note", "Export Markdown…", "⇧⌘E", Intent::Export),
            Command::new("import-action", "Import Notes…", "⌘O", Intent::Import),
            Command::new(
                "reveal-folder",
                "Show Notes Folder in Finder",
                "",
                Intent::Reveal,
            ),
            Command::new("format-bold", "Bold", "⌘B", Intent::Mark(doc::Inline::Bold)),
            Command::new(
                "format-italic",
                "Italic",
                "⌘I",
                Intent::Mark(doc::Inline::Italic),
            ),
            Command::new(
                "format-strikethrough",
                "Strikethrough",
                "⇧⌘S",
                Intent::Mark(doc::Inline::Strikethrough),
            ),
            Command::new(
                "format-underline",
                "Underline",
                "⌘U",
                Intent::Mark(doc::Inline::Underline),
            ),
            Command::new(
                "format-code",
                "Inline Code",
                "⌘E",
                Intent::Mark(doc::Inline::Code),
            ),
            Command::new("format-link", "Link", "⌘L", Intent::Link).slash(13, SlashEffect::Host),
            Command::new(
                "format-heading",
                "Heading 1",
                "⌥⌘1",
                Intent::Block(doc::Block::Heading(1)),
            )
            .slash(1, SlashEffect::Block(doc::Block::Heading(1))),
            Command::new(
                "format-heading-2",
                "Heading 2",
                "⌥⌘2",
                Intent::Block(doc::Block::Heading(2)),
            )
            .slash(2, SlashEffect::Block(doc::Block::Heading(2))),
            Command::new(
                "format-heading-3",
                "Heading 3",
                "⌥⌘3",
                Intent::Block(doc::Block::Heading(3)),
            )
            .slash(3, SlashEffect::Block(doc::Block::Heading(3))),
            Command::new(
                "format-heading-4",
                "Heading 4",
                "⌥⌘4",
                Intent::Block(doc::Block::Heading(4)),
            )
            .slash(4, SlashEffect::Block(doc::Block::Heading(4))),
            Command::new(
                "format-heading-5",
                "Heading 5",
                "⌥⌘5",
                Intent::Block(doc::Block::Heading(5)),
            )
            .slash(5, SlashEffect::Block(doc::Block::Heading(5))),
            Command::new(
                "format-heading-6",
                "Heading 6",
                "⌥⌘6",
                Intent::Block(doc::Block::Heading(6)),
            )
            .slash(6, SlashEffect::Block(doc::Block::Heading(6))),
            Command::new(
                "format-quote",
                "Quote",
                "⇧⌘B",
                Intent::Block(doc::Block::Quote),
            )
            .slash(10, SlashEffect::Block(doc::Block::Quote)),
            Command::new(
                "format-code-block",
                "Code Block",
                "⌥⌘C",
                Intent::Block(doc::Block::Code),
            )
            .slash(11, SlashEffect::Block(doc::Block::Code)),
            Command::new(
                "format-paragraph",
                "Paragraph",
                "⌥⌘0",
                Intent::Block(doc::Block::Paragraph),
            )
            .slash(0, SlashEffect::Block(doc::Block::Paragraph)),
            Command::new(
                "format-ordered",
                "Ordered List",
                "⇧⌘7",
                Intent::Block(doc::Block::Ordered),
            )
            .slash(8, SlashEffect::Block(doc::Block::Ordered)),
            Command::new(
                "format-bullet",
                "Bullet List",
                "⇧⌘8",
                Intent::Block(doc::Block::Bullet),
            )
            .slash(7, SlashEffect::Block(doc::Block::Bullet)),
            Command::new(
                "format-task",
                "Task List",
                "⇧⌘9",
                Intent::Block(doc::Block::Task),
            )
            .slash(9, SlashEffect::Block(doc::Block::Task)),
            // A rule replaces the line it is on, so only the `/` menu offers it.
            Command::editor(
                "insert-divider",
                "Divider",
                12,
                SlashEffect::Block(doc::Block::Divider),
            ),
        ];
        // A table cannot nest in another one, and a code block keeps its pipes literal.
        if caret.table.is_none() && !caret.in_code {
            items.push(
                Command::new("insert-table", "Table", "", Intent::InsertTable)
                    .slash(14, SlashEffect::Host),
            );
        }
        items.extend([
            Command::new(
                "show-trash",
                "Show Recently Deleted Notes",
                "",
                Intent::Trash,
            ),
            Command::new(
                "delete-note",
                "Move to Recently Deleted",
                "",
                Intent::Delete,
            ),
            Command::new(
                "toggle-format-toolbar",
                if self.format_toolbar {
                    "Hide Formatting Toolbar"
                } else {
                    "Show Formatting Toolbar"
                },
                "",
                Intent::ToggleFormatToolbar,
            ),
            Command::new(
                "toggle-count",
                if self.show_words {
                    "Show Character Count"
                } else {
                    "Show Word Count"
                },
                "",
                Intent::ToggleCount,
            ),
            Command::new(
                "vim-mode",
                if self.library.preferences.vim_mode {
                    "Disable Vim Mode"
                } else {
                    "Enable Vim Mode"
                },
                "",
                Intent::VimMode,
            ),
            Command::new(
                "open-settings",
                "Open Markraft Settings",
                "⌘,",
                Intent::Settings,
            ),
        ]);
        // Only offered while there is something to empty.
        if !self.library.search("", true).is_empty() {
            items.push(Command::new(
                "empty-trash",
                "Empty Recently Deleted",
                "",
                Intent::EmptyTrash,
            ));
        }
        if caret.in_link {
            items.extend([
                Command::new("copy-link", "Copy Link", "", Intent::CopyLink),
                Command::new("open-link", "Open Link", "", Intent::OpenLink),
                Command::new("remove-link", "Remove Link", "", Intent::Unlink),
            ]);
        }
        if let Some(table) = caret.table {
            items.extend([
                Command::new(
                    "table-row-above",
                    "Add Row Above",
                    "",
                    Intent::Table(TableEdit::RowBefore),
                ),
                Command::new(
                    "table-row-below",
                    "Add Row Below",
                    "⌘↩",
                    Intent::Table(TableEdit::RowAfter),
                ),
                Command::new(
                    "table-column-left",
                    "Add Column Left",
                    "",
                    Intent::Table(TableEdit::ColumnBefore),
                ),
                Command::new(
                    "table-column-right",
                    "Add Column Right",
                    "",
                    Intent::Table(TableEdit::ColumnAfter),
                ),
                Command::new(
                    "table-delete-row",
                    "Delete Row",
                    "",
                    Intent::Table(TableEdit::DeleteRow),
                ),
                Command::new(
                    "table-delete-column",
                    "Delete Column",
                    "",
                    Intent::Table(TableEdit::DeleteColumn),
                ),
            ]);
            // The column's own alignment is the one the panel marks.
            for (id, label, alignment) in [
                (
                    "table-align-left",
                    "Align Column Left",
                    ColumnAlignment::Left,
                ),
                (
                    "table-align-center",
                    "Align Column Center",
                    ColumnAlignment::Center,
                ),
                (
                    "table-align-right",
                    "Align Column Right",
                    ColumnAlignment::Right,
                ),
            ] {
                items.push(
                    Command::new(id, label, "", Intent::Table(TableEdit::Align(alignment)))
                        .checked(table.alignment == alignment),
                );
            }
            items.push(Command::new(
                "table-delete",
                "Delete Table",
                "",
                Intent::Table(TableEdit::DeleteTable),
            ));
        }
        items
    }
    /// How much text there is, in the unit the footer is set to. The Browse rows use it
    /// too, so a note's size is described the same way wherever it is shown.
    pub(super) fn count_label(&self, text: &str) -> String {
        self.count_of(self.count_units(text))
    }
    fn count_units(&self, text: &str) -> usize {
        if self.show_words {
            text.unicode_words().count()
        } else {
            text.graphemes(true).count()
        }
    }
    fn count_of(&self, units: usize) -> String {
        if self.show_words {
            format!("{units} {}", if units == 1 { "word" } else { "words" })
        } else {
            format!(
                "{units} {}",
                if units == 1 {
                    "character"
                } else {
                    "characters"
                }
            )
        }
    }
    /// What the footer counts: the note as the editor lays it out. A table puts each of
    /// its cells on a line of its own, and those breaks are the grid rather than
    /// anything anyone typed, so they are not characters — they still part words, as the
    /// break between two blocks does.
    fn note_count(&self, cx: &App) -> String {
        let projection = self.editor().read(cx).projection();
        let mut units = 0;
        let mut previous: Option<Option<usize>> = None;
        for (index, line) in projection.lines().iter().enumerate() {
            units += self.count_units(projection.line_text(index).unwrap_or_default());
            let table = doc::table_of(line);
            // The break this line opened with, unless it fell between two cells of one
            // table, or the count is of words, which no break adds to.
            if !self.show_words && previous.is_some_and(|before| table.is_none() || before != table)
            {
                units += 1;
            }
            previous = Some(table);
        }
        self.count_of(units)
    }
    fn filtered_actions(&self, cx: &App) -> Vec<Command> {
        let query = self.query.read(cx).text().to_owned().trim().to_lowercase();
        let editor = self.editor();
        let editor = editor.read(cx);
        let caret = Caret {
            in_link: editor.active_link().is_some(),
            // The note goes on painting behind the panel, so this is the table the
            // caret is in even while the panel has the keyboard.
            table: editor.table_at_caret(),
            in_code: doc::Block::active(editor.state(), &editor.projection())
                == Some(doc::Block::Code),
        };
        let mut items: Vec<_> = self
            .action_items(caret)
            .into_iter()
            .filter(|command| {
                command.intent.is_some() && command.label.to_lowercase().contains(&query)
            })
            .collect();
        items.sort_by_key(|command| command.intent.as_ref().map(Intent::action_group));
        items
    }
    fn actions_panel(&self, cx: &mut Context<Self>) -> Div {
        let items = self.filtered_actions(cx);
        let total = items.len();
        let mut list = div()
            .id("actions-list")
            .role(Role::ListBox)
            .aria_label("Actions")
            .track_scroll(&self.actions_scroll)
            .overflow_y_scroll()
            .px_2()
            .pb_2();
        if items.is_empty() {
            list = list.child(
                div()
                    .py_6()
                    .text_center()
                    .text_size(px(13.))
                    .text_color(self.muted())
                    .child("No matching actions"),
            );
        }
        let mut previous_group = None;
        for (index, command) in items.into_iter().enumerate() {
            let Command {
                id,
                label,
                shortcut,
                intent,
                checked,
                ..
            } = command;
            let Some(intent) = intent else { continue };
            let group = intent.action_group();
            let separator = previous_group.is_some_and(|previous| previous != group);
            previous_group = Some(group);
            list = list.child(
                div()
                    .when(separator, |s| {
                        s.child(
                            div()
                                .h(px(1.))
                                .my(px(8.))
                                .mx(px(8.))
                                .bg(self.border_color()),
                        )
                    })
                    .child(
                        self.row(id, label, shortcut, intent, cx)
                            .h(ACTION_ROW_HEIGHT)
                            .text_size(px(14.))
                            .role(Role::Button)
                            .aria_selected(index == self.selected)
                            .aria_position_in_set(index + 1)
                            .aria_size_of_set(total)
                            // The one alignment the caret's column already has wears a
                            // check where the other rows carry their shortcut.
                            .when_some(checked, |s, checked| {
                                s.aria_toggled(if checked {
                                    accesskit::Toggled::True
                                } else {
                                    accesskit::Toggled::False
                                })
                                .when(checked, |s| s.child(icon(Icon::Check, self.control_text())))
                            })
                            .when(index == self.selected, |s| s.bg(self.selected_color()))
                            .on_mouse_move(cx.listener(move |this, _, _, cx| {
                                if this.selected != index {
                                    this.selected = index;
                                    cx.notify();
                                }
                            })),
                    ),
            );
        }
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(self.search_field(cx))
            .child(self.scroll_area(list, &self.actions_scroll, cx))
    }
    fn overlay(&self, window: &Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let viewport = window.bounds().size;
        // The card hangs from the top of the window and always ends clear of its lower
        // edge, so a short window shows a shorter card rather than one running off it.
        let top = if viewport.height < px(400.) {
            px(44.)
        } else if self.panel == Panel::Settings {
            px(72.)
        } else {
            px(100.)
        };
        let width = px(if self.panel == Panel::Settings {
            360.
        } else {
            320.
        })
        .min(viewport.width - px(32.));
        let available = (viewport.height - top - px(16.)).max(px(0.));
        let desired = match self.panel {
            Panel::Browse | Panel::Trash => {
                let n = self
                    .library
                    .search(
                        self.query.read(cx).text().trim(),
                        self.panel == Panel::Trash,
                    )
                    .len();
                let heading = if self.panel == Panel::Trash { 32. } else { 26. };
                let rows = if n == 0 { 72. } else { 58. * n as f32 };
                px(44. + heading + rows + 8.)
            }
            Panel::Actions => {
                let items = self.filtered_actions(cx);
                let count = items.len();
                let separators = items
                    .windows(2)
                    .filter(|pair| {
                        pair[0].intent.as_ref().map(Intent::action_group)
                            != pair[1].intent.as_ref().map(Intent::action_group)
                    })
                    .count();
                // The search field, bottom inset, rows and group separators.
                if count == 0 {
                    px(124.)
                } else {
                    px(52.) + ACTION_ROW_HEIGHT * count as f32 + px(17. * separators as f32)
                }
            }
            _ => px(470.),
        };
        // Include both border pixels so a fully visible short list does not scroll.
        let height = (desired + px(2.))
            .min(px(if self.panel == Panel::Settings {
                440.
            } else {
                420.
            }))
            .min(available);
        let contents = match self.panel {
            // A short card gives what room it has to the rows rather than to a heading.
            Panel::Browse | Panel::Trash => self.picker(height >= px(136.), cx),
            Panel::Actions => self.actions_panel(cx),
            _ => div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .child(
                    div()
                        .h(px(44.))
                        .flex_shrink_0()
                        .px_4()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_size(px(13.))
                                .font_weight(FontWeight::MEDIUM)
                                .child("Settings"),
                        )
                        .child(self.button("settings-done", "Done", Intent::Back, cx)),
                )
                .child(self.settings(cx)),
        };
        div()
            .id("notes-overlay")
            .absolute()
            .top(top)
            .left((viewport.width - width) / 2.)
            .w(width)
            .h(height)
            .flex()
            .flex_col()
            .rounded(POPOVER_RADIUS)
            .bg(self.surface_color())
            .border_1()
            .border_color(self.border_color())
            .shadow(popover_shadow())
            .occlude()
            .overflow_hidden()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                this.dismiss(window, cx);
                cx.stop_propagation();
            }))
            .child(contents)
    }
}
impl Render for NotesApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.code_language_block.is_some() && self.code_language_focus_pending {
            self.code_language_focus_pending = false;
            window.focus(&self.query.focus_handle(cx), cx);
            let block = self.code_language_block;
            let weak = cx.entity().downgrade();
            window.on_next_frame(move |_, cx| {
                let _ = weak.update(cx, |this, cx| {
                    if this.code_language_block == block {
                        this.code_language_scroll
                            .scroll_to_item(this.code_language_selected);
                        cx.notify();
                    }
                });
            });
        }
        let title = self.library.active_note().title();
        window.set_window_title(&title);
        let style = notes_style(self.dark);
        let reduce_motion = cx.reduce_motion();
        let chrome = Self::chrome_spring(self.chrome_visible(), reduce_motion);
        let root = div()
            .key_context("MarkraftApp")
            .track_focus(&self.panel_focus)
            .capture_action(cx.listener(|this, _: &markraft_gpui::Up, w, cx| {
                if this.panel_key("up", w, cx) {
                    cx.stop_propagation();
                } else {
                    cx.propagate();
                }
            }))
            .capture_action(cx.listener(|this, _: &markraft_gpui::Down, w, cx| {
                if this.panel_key("down", w, cx) {
                    cx.stop_propagation();
                } else {
                    cx.propagate();
                }
            }))
            .capture_action(cx.listener(|this, _: &markraft_gpui::Enter, w, cx| {
                if this.panel_key("enter", w, cx) {
                    cx.stop_propagation();
                } else {
                    cx.propagate();
                }
            }))
            // Tab walks the open surface's controls. With nothing open it falls through,
            // so the note still indents.
            .capture_action(cx.listener(|this, _: &markraft_gpui::Indent, w, cx| {
                if this.html_focus_step(true, w, cx) || this.focus_step(true, w, cx) {
                    cx.stop_propagation();
                } else {
                    cx.propagate();
                }
            }))
            .capture_action(cx.listener(|this, _: &markraft_gpui::Outdent, w, cx| {
                if this.html_focus_step(false, w, cx) || this.focus_step(false, w, cx) {
                    cx.stop_propagation();
                } else {
                    cx.propagate();
                }
            }))
            // Every keystroke passes here on its way down, which is where the chrome
            // learns that someone is at the window. Space additionally runs the ringed
            // control: it is bound to nothing, so it only ever reaches here when no text
            // field owns the keyboard.
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, w, cx| {
                this.note_key_press(cx);
                if this.html_editor.is_some()
                    && event.keystroke.key == "enter"
                    && event.keystroke.modifiers.platform
                {
                    this.save_html_source(w, cx);
                    cx.stop_propagation();
                    return;
                }
                if event.keystroke.key == "space"
                    && !event.keystroke.modifiers.modified()
                    && (this.html_key("space", w, cx) || this.focus_activate(w, cx))
                {
                    cx.stop_propagation();
                }
            }))
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(style.background)
            .text_color(style.text)
            .font_family(".SystemUIFont")
            .on_action(cx.listener(|this, _: &Save, w, cx| {
                if this.html_editor.is_some() {
                    this.save_html_source(w, cx);
                } else {
                    this.flush(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &CopyMarkdown, _, cx| this.copy_markdown(cx)))
            .on_action(cx.listener(|this, _: &Quit, _, cx| this.quit(cx)))
            .on_action(cx.listener(|this, _: &Hide, w, cx| this.dismiss(w, cx)))
            .on_action(cx.listener(|this, _: &NewNote, w, cx| this.intent(Intent::New, w, cx)))
            .on_action(cx.listener(|this, _: &Browse, w, cx| this.intent(Intent::Browse, w, cx)))
            .on_action(cx.listener(|this, _: &Actions, w, cx| this.intent(Intent::Actions, w, cx)))
            .on_action(
                cx.listener(|this, _: &Settings, w, cx| this.intent(Intent::Settings, w, cx)),
            )
            .on_action(cx.listener(|this, _: &Link, w, cx| this.intent(Intent::Link, w, cx)))
            .on_action(cx.listener(|this, _: &Export, w, cx| this.intent(Intent::Export, w, cx)))
            .on_action(cx.listener(|this, _: &Import, w, cx| this.intent(Intent::Import, w, cx)));
        let actions = self
            .chrome_capsule()
            .p(px(4.))
            .child(
                self.icon_button(
                    "actions",
                    "Actions · ⌘K",
                    Icon::Command,
                    Intent::Actions,
                    cx,
                )
                .size(px(28.))
                .rounded_full()
                .opacity(1.),
            )
            .child(
                self.icon_button(
                    "browse",
                    "Browse Notes · ⌘P",
                    Icon::Notes,
                    Intent::Browse,
                    cx,
                )
                .size(px(28.))
                .rounded_full()
                .opacity(1.),
            )
            .child(
                self.icon_button("new-note", "New Note · ⌘N", Icon::Plus, Intent::New, cx)
                    .size(px(28.))
                    .rounded_full()
                    .opacity(1.),
            );
        let toolbar = div()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(TOOLBAR_HEIGHT)
            .child(
                div()
                    .absolute()
                    .left(px(112.))
                    .right(px(112.))
                    .top(px(17.))
                    .text_center()
                    .text_size(px(12.))
                    .text_color(self.muted())
                    .truncate()
                    .child(title)
                    .with_spring("title-fade", chrome.clone(), |s, phase| {
                        s.opacity(phase.interpolate_clamped(0.6, 1.))
                    }),
            )
            .child(
                div()
                    .absolute()
                    .right(px(8.))
                    .top(px(8.))
                    .child(actions)
                    .with_spring(
                        "capsule-fade",
                        Self::chrome_spring(self.pointer_inside, reduce_motion),
                        |s, phase| s.opacity(phase.interpolate_clamped(0., 1.)),
                    ),
            );
        if self.persistence.is_none() {
            // Either no folder has been chosen yet, or the chosen one could not be opened.
            let first_launch = self.path.is_none();
            let (title, explanation) = if first_launch {
                (
                    "Choose where to keep your notes",
                    "Each note is a Markdown file in this folder, so other apps can read, \
                     sync and back them up. You can change the folder later in Settings.",
                )
            } else {
                (
                    "Your notes could not be opened",
                    "Nothing in the folder was changed. Check that it exists and that no \
                     other Markraft is using it, then retry or choose another folder.",
                )
            };
            // Choosing a folder is the way forward on both screens, so it is the one
            // filled button and the one Enter takes.
            let accent = notes_style(self.dark).marker;
            let primary = self
                .button_with_hover(
                    "choose-folder",
                    "Choose Folder…",
                    Intent::ChooseFolder,
                    accent.opacity(0.85),
                    cx,
                )
                .px_3()
                .bg(accent)
                .text_color(rgb(0xffffff))
                .aria_keyshortcuts("Enter");
            let actions = div()
                .mt_5()
                .flex()
                .flex_wrap()
                .gap_2()
                .when(!first_launch, |s| {
                    s.child(self.button("retry-open", "Retry", Intent::Retry, cx))
                })
                .child(primary)
                .when(first_launch && Self::default_folder().is_some(), |s| {
                    s.child(self.button(
                        "default-folder",
                        "Use Documents/Markraft",
                        Intent::DefaultFolder,
                        cx,
                    ))
                })
                .when(!first_launch, |s| {
                    s.child(self.button("reveal-library", "Show Folder", Intent::Reveal, cx))
                });
            let detail = div()
                .mt_3()
                .text_size(px(12.))
                .text_color(self.muted())
                .child(self.error.clone().unwrap_or_default());
            return root.child(toolbar).child(
                div()
                    .flex_1()
                    .p_6()
                    .pt(TOOLBAR_HEIGHT + px(24.))
                    .child(
                        div()
                            .text_size(px(19.))
                            .line_height(px(25.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .when(self.error.is_some(), |s| s.child(detail))
                    .child(
                        div()
                            .mt_3()
                            .text_size(px(13.))
                            .line_height(px(19.))
                            .text_color(self.muted())
                            .child(explanation),
                    )
                    .child(actions),
            );
        }
        let count = self.note_count(cx);
        // The toolbar and footer float over the note. Each pair of fades reaches the
        // background color only at its window edge, so no opaque band forms; the short
        // one keeps the title or count legible. The top pair is shallower so the first
        // line is not dimmed while the note rests at its start.
        let mut clear = style.background;
        clear.a = 0.;
        let fade = |height: Pixels, top: bool| {
            let edge = div().absolute().left_0().right_0().h(height);
            let (edge, angle) = if top {
                (edge.top_0(), 0.)
            } else {
                (edge.bottom_0(), 180.)
            };
            edge.bg(linear_gradient(
                angle,
                linear_color_stop(clear, 0.),
                linear_color_stop(style.background, 1.),
            ))
        };
        let body = div()
            .flex_1()
            .min_h_0()
            .relative()
            .child(self.editor())
            .child(fade(px(80.), true))
            .child(fade(px(56.), true))
            .child(fade(px(128.), false))
            .child(fade(px(64.), false))
            .child(toolbar)
            .child(
                div()
                    .absolute()
                    .bottom_0()
                    .left_0()
                    .right_0()
                    .child(self.footer(count, window.bounds().size.width, cx)),
            );
        root.child(body)
            // Both banners are live regions, so a failure is spoken rather than only shown.
            .when_some(self.platform_error.clone(), |s, error| {
                let message = format!("{error} · Change the shortcut in Settings.");
                s.child(
                    div()
                        .id("shortcut-error")
                        .role(Role::Status)
                        .aria_label(message.clone())
                        .px_6()
                        .py_2()
                        .text_size(px(11.))
                        .text_color(self.muted())
                        .child(message),
                )
            })
            .when_some(self.error.clone(), |s, error| {
                let message = format!("{error}  ⌘S retry · ⇧⌘C copy");
                s.child(
                    div()
                        .id("save-error")
                        .role(Role::Alert)
                        .aria_label(message.clone())
                        .px_6()
                        .py_2()
                        .text_size(px(11.))
                        .text_color(if self.dark {
                            rgb(0xffa4a4)
                        } else {
                            rgb(0xa53232)
                        })
                        .child(message),
                )
            })
            .when(self.error.is_some(), |s| {
                s.child(
                    div()
                        .flex()
                        .px_4()
                        .gap_2()
                        .child(self.button(
                            "save-library-copy",
                            "Save Library Copy…",
                            Intent::SaveCopy,
                            cx,
                        ))
                        .child(self.button(
                            "reload-library",
                            "Reload from Disk…",
                            Intent::Reload,
                            cx,
                        )),
                )
            })
            .when_some(self.notice.clone(), |s, notice| {
                let announced = match &notice.undo {
                    Some(_) => format!("{} · Undo available", notice.text),
                    None => notice.text.to_string(),
                };
                s.child(
                    div()
                        .id("notice")
                        .role(Role::Status)
                        .aria_label(announced)
                        .absolute()
                        .bottom(px(52.))
                        .right(px(12.))
                        // A folder's notices are sentences rather than acknowledgments,
                        // so the strip wraps instead of running off the window.
                        .max_w(window.bounds().size.width - px(24.))
                        .flex()
                        .items_center()
                        .gap_1()
                        .px_3()
                        .py_1()
                        .rounded(ROW_RADIUS)
                        .bg(self.surface_color())
                        .border_1()
                        .border_color(self.border_color())
                        .text_size(px(12.))
                        .text_color(self.control_text())
                        .child(notice.text.clone())
                        .when_some(notice.undo.as_ref(), |s, _| {
                            s.child(div().text_color(self.muted()).child("·")).child(
                                self.button("notice-undo", "Undo", Intent::UndoDelete, cx)
                                    .h(px(22.))
                                    .text_color(self.control_text()),
                            )
                        }),
                )
            })
            .when(
                self.format_menu.is_some() && self.panel == Panel::Editor,
                |s| {
                    s.child(popover_enter(
                        "format-enter",
                        self.format_popover(window, cx),
                        true,
                        reduce_motion,
                    ))
                },
            )
            .when_some(self.link_pill(window, cx), |s, pill| {
                s.child(popover_enter("link-enter", pill, true, reduce_motion))
            })
            .when_some(self.code_language_popover(window, cx), |s, popover| {
                s.child(popover_enter(
                    "code-language-enter",
                    popover,
                    false,
                    reduce_motion,
                ))
            })
            .when_some(self.table_toolbar(window, cx), |s, toolbar| {
                s.child(popover_enter("table-enter", toolbar, true, reduce_motion))
            })
            .when_some(self.html_source_entry(window, cx), |s, entry| {
                s.child(entry)
            })
            .when(self.panel != Panel::Editor, |s| {
                s.child(popover_enter(
                    "overlay-enter",
                    self.overlay(window, cx),
                    false,
                    reduce_motion,
                ))
            })
            .when_some(self.html_source_popover(window, cx), |s, popover| {
                s.child(popover_enter("html-enter", popover, false, reduce_motion))
            })
    }
}

/// Popovers fade in over a short travel. Spring state is dropped once a popover stops
/// rendering, so each opening replays it while switching panels does not. Reduced
/// motion opens them at their end state instead.
fn popover_enter(
    id: &'static str,
    popover: Stateful<Div>,
    anchored_bottom: bool,
    reduce_motion: bool,
) -> SpringAnimationElement<Stateful<Div>> {
    popover.with_spring(
        id,
        SpringAnimation::new(SpringConfig::new(900., 60., 1.))
            .to(true)
            .from(false)
            .playback(playback(reduce_motion)),
        move |s, phase| {
            // Travel away from the anchored edge.
            let offset = phase.interpolate_clamped(px(-4.), px(0.));
            let s = s.opacity(phase.interpolate_clamped(0., 1.));
            if anchored_bottom {
                s.mb(offset)
            } else {
                s.mt(offset)
            }
        },
    )
}

struct Hint {
    label: &'static str,
    dark: bool,
}
impl Render for Hint {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let (text, keys) = self.label.split_once(" · ").unwrap_or((self.label, ""));
        let (surface, ink) = if self.dark {
            (rgba(0x3a3b40f2), rgb(0xf2f2f3))
        } else {
            (rgba(0xf6f6f6f2), rgb(0x272727))
        };
        div()
            .flex()
            .items_center()
            .gap(px(6.))
            .px_2()
            .h(px(26.))
            .rounded(ROW_RADIUS)
            .bg(surface)
            .border_1()
            .border_color(tokens::border_color(self.dark))
            .text_color(ink)
            .text_size(px(11.))
            .child(text)
            .when(!keys.is_empty(), |s| s.child(keycaps(keys, self.dark)))
    }
}
