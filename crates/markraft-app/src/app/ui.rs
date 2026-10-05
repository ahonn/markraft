use crate::locale::Message;
mod code;
pub(super) mod context_menu;
mod editing;
mod focus;
mod formatting;
mod icons;
mod link;
mod rename;
pub(in crate::app) mod settings;
pub(in crate::app) mod slash;
pub(super) mod table;
mod text_checking;
mod tokens;
mod vim;
pub(in crate::app) mod wiki;

use super::*;
use crate::storage::Pref;
use focus::Surface;
use icons::{Icon, icon, sized_icon};
use slash::{Command, SlashEffect};
use table::TableEdit;
pub(in crate::app) use text_checking::CheckTrigger;
pub(in crate::app) use tokens::playback;
use tokens::{POPOVER_RADIUS, ROW_HEIGHT, ROW_RADIUS, keycaps, popover_shadow};

#[derive(Clone)]
enum Intent {
    New,
    /// Today's daily note, and the ones either side of the daily note on screen.
    DailyToday,
    DailyPrevious,
    DailyNext,
    Browse,
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
    Delete,
    Pin,
    PinNote(String),
    TrashNote(String),
    CopyMarkdown,
    Edit(editing::EditCommand),
    CopyRichText,
    PastePlain,
    PasteMarkdown,
    Export,
    ExportHtml,
    ExportPdf,
    Print,
    SendToObsidian(PathBuf),
    /// The pill under the title, and the two things done inside it.
    Rename,
    ApplyRename,
    RenameLinks,
    OpenMarkdown,
    Select(String),
    CodeLanguage(&'static str),
    Reveal,
    ChooseFolder,
    /// Error-recovery shortcut back to Documents/Markraft when another folder failed.
    UseDefaultFolder,
    Retry,
    RevealNote,
    SaveAs,
    Reload,
    /// The lower-left file status indicator: the card of ways out for a read-only file.
    FileStatus,
    OpenExternally,
    /// Commands the editor owns and the panel only forwards, because each one
    /// acts on the thing the caret is already in.
    ToggleTask,
    ChooseCodeLanguage,
    CopyCodeBlock,
    Mark(doc::Inline),
    Block(doc::Block),
    InsertTable,
    Table(TableEdit),
    /// Getting a problem to someone who can fix it.
    ReportIssue,
    CopyDebugInfo,
    RevealLogs,
    /// vim's `:q`, `:qa`, `:u` and `:red`: the window's and the editor's own
    /// commands, which the panel otherwise leaves to their keys.
    Hide,
    Quit,
    Undo,
    Redo,
    /// Several intents in turn, as `:wq` saves and then hides.
    Then(Vec<Intent>),
}

/// The shortcut shown beside an intent, wherever it is offered: the ⌘K panel,
/// the toolbar's menus and the `/` menu all read it here, so a rebinding is
/// one edit and the three cannot disagree. Empty for an intent without one.
///
/// The labels are written here rather than derived from the keymap: the editor's
/// bindings live in gpui's own binding table, keyed by action, and are not
/// exposed per intent.
fn shortcut_label(intent: &Intent) -> &'static str {
    match intent {
        Intent::New => "⌘N",
        Intent::Browse => "⌘P",
        Intent::Save => "⌘S",
        Intent::Hide => "⌘W",
        Intent::CopyMarkdown => "⇧⌘C",
        Intent::PastePlain => "⇧⌘V",
        Intent::PasteMarkdown => "⌥⇧⌘V",
        Intent::Export => "⇧⌘E",
        Intent::OpenMarkdown => "⌘O",
        Intent::Link => "⌘L",
        Intent::Mark(doc::Inline::Bold) => "⌘B",
        Intent::Mark(doc::Inline::Italic) => "⌘I",
        Intent::Mark(doc::Inline::Strikethrough) => "⇧⌘S",
        Intent::Mark(doc::Inline::Code) => "⌘E",
        Intent::Block(doc::Block::Paragraph) => "⌘0",
        Intent::Block(doc::Block::Heading(1)) => "⌘1",
        Intent::Block(doc::Block::Heading(2)) => "⌘2",
        Intent::Block(doc::Block::Heading(3)) => "⌘3",
        Intent::Block(doc::Block::Heading(4)) => "⌘4",
        Intent::Block(doc::Block::Heading(5)) => "⌘5",
        Intent::Block(doc::Block::Heading(6)) => "⌘6",
        Intent::Block(doc::Block::Quote) => "⇧⌘B",
        Intent::Block(doc::Block::Code) => "⌥⌘C",
        Intent::Block(doc::Block::Ordered) => "⇧⌘7",
        Intent::Block(doc::Block::Bullet) => "⇧⌘8",
        Intent::Block(doc::Block::Task) => "⇧⌘9",
        Intent::ToggleTask => "⌘↩",
        Intent::ChooseCodeLanguage => "⌥⌘L",
        Intent::CopyCodeBlock => "⌥⇧⌘C",
        Intent::Table(TableEdit::RowAfter) => "⌘↩",
        Intent::Quit => "⌘Q",
        Intent::Undo => "⌘Z",
        Intent::Redo => "⇧⌘Z",
        _ => "",
    }
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
            Self::New
            | Self::DailyToday
            | Self::DailyPrevious
            | Self::DailyNext
            | Self::Browse
            | Self::Pin => ActionGroup::Notes,
            Self::Edit(_)
            | Self::CopyMarkdown
            | Self::CopyRichText
            | Self::PastePlain
            | Self::PasteMarkdown
            | Self::Undo
            | Self::Redo => ActionGroup::Editing,
            Self::Mark(_) | Self::Block(_) | Self::Link | Self::InsertTable => {
                ActionGroup::Formatting
            }
            Self::Table(_)
            | Self::EditLink
            | Self::CopyLink
            | Self::OpenLink
            | Self::Unlink
            | Self::ToggleTask
            | Self::ChooseCodeLanguage
            | Self::CopyCodeBlock => ActionGroup::Context,
            Self::Save
            | Self::Export
            | Self::ExportHtml
            | Self::ExportPdf
            | Self::Print
            | Self::SendToObsidian(_)
            | Self::Rename
            | Self::OpenMarkdown
            | Self::Reveal
            | Self::RevealNote
            | Self::SaveAs
            | Self::OpenExternally => ActionGroup::Files,
            Self::Delete | Self::TrashNote(_) | Self::ChooseFolder | Self::UseDefaultFolder => {
                ActionGroup::Recovery
            }
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
    in_task: bool,
}
/// Soft fade for the format toolbar and its toggle glyphs — a touch slower than the
/// title chrome, still under a beat.
const FORMAT_SPRING: SpringConfig = SpringConfig::new(800., 55., 1.);
const ACTION_ROW_HEIGHT: Pixels = px(36.);

/// A stored timestamp read on this Mac's clock. Notes carry UTC; a date label has to be
/// the one on the user's calendar, so both ends of a comparison are shifted before they
/// are cut into days.
fn local(milliseconds: u64) -> u64 {
    milliseconds.saturating_add_signed(crate::platform::local_utc_offset() * 1000)
}

/// When a note was last written, as the user would say it. Past a week it is a date,
/// and past this year the date carries it.
fn relative_day(updated: u64, now: u64, i18n: &crate::locale::I18n) -> String {
    const DAY: u64 = 86_400_000;
    let (updated, now) = (local(updated), local(now));
    // A note written with the clock ahead of this one still reads as today.
    match (now / DAY).saturating_sub(updated / DAY) {
        0 => i18n.text("notes.edited-today"),
        1 => i18n.text("notes.edited-yesterday"),
        days @ 2..=6 => i18n.text_with("notes.edited-days-ago", &[("days", &days.to_string())]),
        _ => {
            let (year, month, date, ..) = crate::vault::civil(updated);
            let month = i18n.text(&format!("date.month-{}", month.clamp(1, 12)));
            let (this_year, ..) = crate::vault::civil(now);
            let date = i18n.text_with(
                if year == this_year {
                    "date.day-month"
                } else {
                    "date.day-month-year"
                },
                &[
                    ("day", &date.to_string()),
                    ("month", &month),
                    ("year", &year.to_string()),
                ],
            );
            i18n.text_with("notes.edited-date", &[("date", &date)])
        }
    }
}

/// The vim `:` commands that are not already panel commands of their own, each named
/// as vim spells it. They answer a `:` query only.
fn ex_commands() -> Vec<Command> {
    vec![
        Command::new("ex-quit", "command.hide-window", Intent::Hide).ex_only(&[("quit", 1)]),
        Command::new(
            "ex-write-quit",
            "command.save-and-hide-window",
            Intent::Then(vec![Intent::Save, Intent::Hide]),
        )
        .ex_only(&[("wq", 2), ("xit", 1), ("exit", 3)]),
        Command::new("ex-quit-all", "command.quit-markraft", Intent::Quit)
            .ex_only(&[("qall", 2), ("quitall", 5)]),
        Command::new(
            "ex-write-quit-all",
            "command.save-and-quit-markraft",
            Intent::Then(vec![Intent::Save, Intent::Quit]),
        )
        .ex_only(&[("wqall", 3), ("xall", 2)]),
        Command::new(
            "ex-edit",
            "command.reload-from-disk",
            Intent::Then(vec![Intent::Back, Intent::Reload]),
        )
        .ex_only(&[("edit", 1)]),
        Command::new("ex-undo", "command.undo", Intent::Undo).ex_only(&[("undo", 1)]),
        Command::new("ex-redo", "command.redo", Intent::Redo).ex_only(&[("redo", 3)]),
    ]
}

/// The icon a command shows in the ⌘K panel and in the `/` menu.
fn intent_icon(intent: &Intent) -> Icon {
    match intent {
        Intent::New => Icon::Plus,
        Intent::DailyToday => Icon::Calendar,
        Intent::DailyPrevious => Icon::PreviousDay,
        Intent::DailyNext => Icon::NextDay,
        Intent::Browse => Icon::Notes,
        Intent::Pin => Icon::Pin,
        Intent::Delete | Intent::TrashNote(_) => Icon::Trash,
        Intent::CopyMarkdown | Intent::CopyRichText | Intent::CopyLink => Icon::Copy,
        Intent::Export | Intent::ExportHtml | Intent::ExportPdf => Icon::Export,
        Intent::Print => Icon::Print,
        Intent::SendToObsidian(_) => Icon::Send,
        Intent::Rename => Icon::Edit,
        Intent::OpenMarkdown => Icon::Document,
        Intent::Settings => Icon::Settings,
        Intent::ReportIssue => Icon::External,
        Intent::CopyDebugInfo => Icon::Copy,
        Intent::RevealLogs => Icon::Open,
        Intent::Save | Intent::SaveAs => Icon::Save,
        Intent::ToggleTask => Icon::Task,
        Intent::ChooseCodeLanguage => Icon::CodeBlock,
        Intent::CopyCodeBlock => Icon::Copy,
        Intent::ToggleFormatToolbar => Icon::Text,
        Intent::ToggleCount => Icon::Count,
        // Anything that hands the note to something outside Markraft.
        Intent::OpenLink | Intent::OpenExternally => Icon::External,
        Intent::Reveal | Intent::RevealNote | Intent::ChooseFolder | Intent::UseDefaultFolder => {
            Icon::Open
        }
        Intent::FileStatus => Icon::Conflict,
        Intent::Retry | Intent::Reload => Icon::Reset,
        // Removing a link is a removal, as the pill's own button says.
        Intent::Unlink => Icon::Trash,
        Intent::Mark(doc::Inline::Bold) => Icon::Bold,
        Intent::Mark(doc::Inline::Italic) => Icon::Italic,
        Intent::Mark(doc::Inline::Code) => Icon::Code,
        Intent::Mark(doc::Inline::Strikethrough) => Icon::Strikethrough,
        Intent::Mark(doc::Inline::Underline) => Icon::Underline,
        Intent::Link => Icon::Link,
        Intent::Block(doc::Block::Heading(_)) => Icon::Heading,
        Intent::Block(doc::Block::Quote) | Intent::Block(doc::Block::Callout) => Icon::Quote,
        Intent::Block(doc::Block::Code) => Icon::CodeBlock,
        Intent::Block(doc::Block::Ordered) => Icon::Ordered,
        Intent::Block(doc::Block::Bullet) => Icon::Bullet,
        Intent::Block(doc::Block::Task) => Icon::Task,
        Intent::Block(doc::Block::Divider) => Icon::Divider,
        Intent::InsertTable => Icon::Table,
        Intent::Table(edit) => edit.icon(),
        Intent::Undo => Icon::Undo,
        Intent::Redo => Icon::Redo,
        Intent::Hide => Icon::Hide,
        Intent::Quit => Icon::Quit,
        Intent::Then(intents) => intents.last().map_or(Icon::Paragraph, intent_icon),
        _ => Icon::Paragraph,
    }
}

impl MarkraftApp {
    fn intent(&mut self, intent: Intent, window: &mut Window, cx: &mut Context<Self>) {
        // The reload replaces the note within moments; an action taken meanwhile
        // would land on content about to be discarded.
        if self.is_reloading() {
            return;
        }
        if !self.editing_enabled(&intent, cx) {
            return;
        }
        match intent {
            Intent::Edit(command) => self.edit_selection(command, window, cx),
            Intent::New => self.new_note(window, cx),
            Intent::DailyToday => {
                self.open_daily_note(daily_notes::today(), window, cx);
            }
            Intent::DailyPrevious | Intent::DailyNext => {
                match self.adjacent_daily_note(matches!(intent, Intent::DailyNext)) {
                    Some(id) => self.select_note(&id, window, cx),
                    None => self.intent(Intent::Back, window, cx),
                }
            }
            Intent::Browse => self.open_panel(Panel::Browse, window, cx),
            Intent::Actions => self.open_panel(Panel::Actions, window, cx),
            Intent::ToggleFormatToolbar => {
                self.close_popover(cx);
                self.toolbar.toggle();
                self.set_panel(Panel::Editor, cx);
                self.focus_editor(window, cx);
                cx.notify();
            }
            Intent::ToggleCount => {
                self.toolbar.toggle_counting();
                self.set_panel(Panel::Editor, cx);
                self.focus_editor(window, cx);
                cx.notify();
            }
            Intent::FormatMenu(menu) => self.open_format_menu(menu, window, cx),
            Intent::Settings => {
                self.intent(Intent::Back, window, cx);
                self.open_settings(window, cx);
            }
            Intent::ReportIssue => {
                self.intent(Intent::Back, window, cx);
                self.report_issue(cx);
            }
            Intent::CopyDebugInfo => {
                self.intent(Intent::Back, window, cx);
                self.copy_debug_info(cx);
            }
            Intent::RevealLogs => {
                self.intent(Intent::Back, window, cx);
                self.reveal_logs(cx);
            }
            Intent::Link => {
                self.set_panel(Panel::Editor, cx);
                self.open_link_popover(window, cx);
            }
            Intent::EditLink => self.edit_link(window, cx),
            Intent::ApplyLink => self.apply_link(window, cx),
            Intent::Unlink => self.unlink(window, cx),
            Intent::CopyLink | Intent::OpenLink => {
                if let Some(url) = self.editor().read(cx).active_link() {
                    if matches!(intent, Intent::CopyLink) {
                        cx.write_to_clipboard(ClipboardItem::new_string(url));
                        self.inform(Message::new("notice.copied-link"), cx);
                    } else {
                        EditorView::open_link(&url, cx);
                    }
                }
                self.close_popover(cx);
                self.focus_editor(window, cx);
                cx.notify();
            }
            Intent::Back => {
                self.close_popover(cx);
                self.cancel_input(cx);
                self.set_panel(Panel::Editor, cx);
                self.focus_editor(window, cx);
                cx.notify();
            }
            Intent::Save => {
                self.intent(Intent::Back, window, cx);
                self.save_now(window, cx);
            }
            Intent::Hide => {
                self.intent(Intent::Back, window, cx);
                self.hide(window, cx);
            }
            Intent::Quit => {
                self.intent(Intent::Back, window, cx);
                self.quit(window, cx);
            }
            Intent::Undo | Intent::Redo => {
                self.intent(Intent::Back, window, cx);
                let action: Box<dyn Action> = if matches!(intent, Intent::Undo) {
                    Box::new(markraft_gpui::Undo)
                } else {
                    Box::new(markraft_gpui::Redo)
                };
                window.dispatch_action(action, cx);
            }
            Intent::Then(intents) => {
                for intent in intents {
                    self.intent(intent, window, cx);
                }
            }
            // The caret is already where these act, so the panel closes and the
            // editor's own action does the work.
            Intent::ToggleTask | Intent::ChooseCodeLanguage | Intent::CopyCodeBlock => {
                self.intent(Intent::Back, window, cx);
                let action: Box<dyn Action> = match intent {
                    Intent::ToggleTask => Box::new(markraft_gpui::ToggleTask),
                    Intent::ChooseCodeLanguage => Box::new(markraft_gpui::ChooseCodeLanguage),
                    _ => Box::new(markraft_gpui::CopyCodeBlock),
                };
                window.dispatch_action(action, cx);
            }
            Intent::Delete => {
                let id = self.library.active_id.clone();
                self.confirm_trash(id, false, window, cx)
            }
            Intent::Pin => {
                let id = self.library.active_id.clone();
                self.toggle_pin(&id, cx);
                self.intent(Intent::Back, window, cx);
            }
            Intent::PinNote(id) => self.toggle_pin(&id, cx),
            Intent::TrashNote(id) => self.confirm_trash(id, true, window, cx),
            Intent::Select(id) => self.select_note(&id, window, cx),
            Intent::CodeLanguage(language) => self.apply_code_language(language, window, cx),
            Intent::CopyMarkdown => {
                self.copy_markdown(cx);
                self.intent(Intent::Back, window, cx);
            }
            Intent::CopyRichText => {
                self.intent(Intent::Back, window, cx);
                self.copy_rich_text(window, cx);
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
                self.export(exports::ExportFormat::Markdown, window, cx);
            }
            Intent::ExportHtml => {
                self.intent(Intent::Back, window, cx);
                self.export(exports::ExportFormat::Html, window, cx);
            }
            Intent::ExportPdf => {
                self.intent(Intent::Back, window, cx);
                self.print_note(true, window, cx);
            }
            Intent::Print => {
                self.intent(Intent::Back, window, cx);
                self.print_note(false, window, cx);
            }
            Intent::SendToObsidian(vault) => {
                self.intent(Intent::Back, window, cx);
                self.send_to_obsidian(vault, window, cx);
            }
            Intent::Rename => self.open_rename(window, cx),
            Intent::ApplyRename => self.apply_rename(window, cx),
            Intent::RenameLinks => self.toggle_rename_links(cx),
            Intent::OpenMarkdown => self.open_markdown(window, cx),
            // Two destinations, so two commands: the folder the notes live in, and
            // the one file this note is.
            Intent::Reveal => {
                if let Some(path) = &self.path {
                    cx.reveal_path(path);
                }
            }
            Intent::RevealNote => {
                if let Some(path) = &self.library.active_note().path {
                    cx.reveal_path(path);
                }
                self.close_popover(cx);
                cx.notify();
            }
            // Every capsule opens the one card of file states (read-only, not saved).
            Intent::FileStatus => {
                if self.interaction.file_status() {
                    self.close_popover(cx);
                } else {
                    self.show_popover(Popover::FileStatus, cx);
                }
                cx.notify();
            }
            Intent::OpenExternally => {
                if let Some(path) = self.library.active_note().path.clone() {
                    cx.open_with_system(&path);
                }
                self.close_popover(cx);
                cx.notify();
            }
            Intent::ChooseFolder => self.choose_folder(window, cx),
            Intent::UseDefaultFolder => match crate::storage::default_notes_folder() {
                Some(path) => self.open_folder(path, window, cx),
                None => {
                    self.feedback
                        .set_error(Message::new("notice.home-unavailable"));
                    cx.notify();
                }
            },
            Intent::Retry => self.recover(window, cx),
            Intent::SaveAs => self.save_as(window, cx),
            Intent::Reload => self.reload(window, cx),
            Intent::Mark(mark) => {
                self.close_popover(cx);
                self.editor().update(cx, |e, cx| {
                    e.toggle_mark(mark.mark(), markraft_core::Attrs::empty(), cx)
                });
                self.set_panel(Panel::Editor, cx);
                self.focus_editor(window, cx);
            }
            Intent::Block(block) => {
                self.close_popover(cx);
                self.editor()
                    .update(cx, |e, cx| e.run_command(&block.command(), cx));
                self.set_panel(Panel::Editor, cx);
                self.focus_editor(window, cx);
            }
            Intent::InsertTable => {
                self.close_popover(cx);
                self.editor().update(cx, |e, cx| {
                    e.table(
                        markraft_gpui::TableOp::Insert {
                            rows: 2,
                            columns: 3,
                        },
                        cx,
                    )
                });
                self.set_panel(Panel::Editor, cx);
                self.focus_editor(window, cx);
            }
            // The table commands keep the caret where it is, so the note takes the
            // keyboard back and the toolbar re-anchors from the next paint.
            Intent::Table(edit) => {
                self.close_popover(cx);
                self.editor().update(cx, |e, cx| edit.run(e, cx));
                self.set_panel(Panel::Editor, cx);
                self.focus_editor(window, cx);
            }
        }
    }
    /// What a destructive control is written in: deleting a note, taking a table away,
    /// and a file status that needs attention.
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
        div()
            .id(id)
            .role(Role::Button)
            .aria_label(label.clone())
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
            .child(label)
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
        label: impl Into<SharedString>,
        kind: Icon,
        intent: Intent,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let id: SharedString = id.into();
        let label = label.into();
        let expanded = match intent {
            Intent::Browse => Some(self.interaction.panel() == Panel::Browse),
            Intent::Actions => Some(self.interaction.panel() == Panel::Actions),
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
        let ink = self.chrome_icon_color();
        let glyph = if matches!(intent, Intent::ToggleFormatToolbar) {
            let shown = self.toolbar.shown();
            let reduce_motion = cx.reduce_motion();
            div()
                .relative()
                .size(px(icon_size))
                .child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .with_spring(
                            "format-toggle-brush",
                            SpringAnimation::new(FORMAT_SPRING)
                                .to(!shown)
                                .playback(playback(reduce_motion)),
                            |s, phase| s.opacity(phase.interpolate_clamped(0., 1.)),
                        )
                        .child(sized_icon(Icon::Text, ink, icon_size)),
                )
                .child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .with_spring(
                            "format-toggle-close",
                            SpringAnimation::new(FORMAT_SPRING)
                                .to(shown)
                                .playback(playback(reduce_motion)),
                            |s, phase| s.opacity(phase.interpolate_clamped(0., 1.)),
                        )
                        .child(sized_icon(Icon::Close, ink, icon_size)),
                )
                .into_any_element()
        } else {
            sized_icon(kind, ink, icon_size).into_any_element()
        };
        // Whatever this button opens sits directly under it, so a tooltip describing
        // the button is both noise and drawn over the thing the user just asked for.
        // Dropping it also keeps the label honest: a pointer that has not moved keeps
        // the tooltip it opened with, which by then names the opposite action.
        let showing = expanded == Some(true)
            || (matches!(intent, Intent::ToggleFormatToolbar) && self.toolbar.shown());
        div()
            .id(id)
            .role(Role::Button)
            .aria_label(label.clone())
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
            .when(!showing && shows_tooltip(&intent), |s| {
                s.tooltip(self.hint(label))
            })
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.intent(intent.clone(), window, cx);
            }))
            .child(glyph)
    }
    /// Toolbar icons recede while another application is active.
    pub(super) fn chrome_icon_color(&self) -> Hsla {
        if self.presence.window_active() {
            self.control_text()
        } else {
            self.muted()
        }
    }
    /// Tooltip builder; a label may carry a shortcut after " · ".
    pub(super) fn hint(
        &self,
        label: impl Into<SharedString>,
    ) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
        let dark = self.dark;
        let label = label.into();
        move |_, cx| {
            cx.new(|_| Hint {
                label: label.clone(),
                dark,
            })
            .into()
        }
    }
    fn row(
        &self,
        id: &'static str,
        label: impl Into<SharedString>,
        hint: &'static str,
        ex: Option<String>,
        intent: Intent,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let label: SharedString = label.into();
        let kind = intent_icon(&intent);
        let destructive = matches!(
            intent,
            Intent::Delete
                | Intent::TrashNote(_)
                | Intent::Table(
                    TableEdit::DeleteTable | TableEdit::DeleteRow | TableEdit::DeleteColumn
                )
        );
        let ink = if destructive {
            self.danger()
        } else {
            self.control_text()
        };
        div()
            .id(id)
            .role(Role::Button)
            .aria_label(label.clone())
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
            // The `:` command vim users know it by, quieter than the shortcut beside it.
            .when_some(ex, |s, ex| {
                s.child(div().text_size(px(12.)).text_color(self.muted()).child(ex))
            })
            .child(self.shortcut(hint))
    }
    fn search_field(&self, cx: &mut Context<Self>) -> Div {
        div()
            .h(px(44.))
            .flex_shrink_0()
            .px_4()
            .pt(px(12.))
            .child(self.query_field(cx))
    }
    /// The input editor owned by the current surface. It carries the name
    /// of the surface it is serving, set with its text in [`MarkraftApp::set_query`], so
    /// this is only the box around it.
    fn query_field(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        div()
            .id(focus::QUERY)
            .size_full()
            // Clicking into the field hands the keyboard back to the caret.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.ring.release()),
            )
            .child(self.query().clone())
    }
    /// The bar under the title. Return steps forward and Shift-Return back,
    /// the same keys the field would otherwise hand to the note.
    fn find_bar(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let status = self.editor().read(cx).find_status();
        let query_style = self.find_editor.read(cx).style();
        let query_height = query_style.body_size * query_style.line_height_ratio;
        let label = if status.query.is_empty() {
            String::new()
        } else if status.total == 0 {
            self.i18n.text("chrome.find-none").to_owned()
        } else {
            let current = status.current.map(|index| index + 1).unwrap_or(0);
            self.i18n.text_with(
                "chrome.find-count",
                &[
                    ("current", &current.to_string()),
                    ("total", &status.total.to_string()),
                ],
            )
        };
        div()
            .id("find-bar")
            .absolute()
            .top(TOOLBAR_HEIGHT + px(6.))
            .left(px(16.))
            .right(px(16.))
            .h(px(32.))
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .rounded(px(10.))
            .bg(self.surface_color())
            .border_1()
            .border_color(self.border_color())
            .shadow(popover_shadow())
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.ring.release();
                    window.focus(&this.find_editor.focus_handle(cx), cx);
                    cx.stop_propagation();
                }),
            )
            .on_action(cx.listener(|this, _: &markraft_gpui::Enter, window, cx| {
                this.submit_find(window, cx);
                cx.stop_propagation();
            }))
            .on_action(
                cx.listener(|this, _: &markraft_gpui::LineBreak, window, cx| {
                    this.find_previous(window, cx);
                    cx.stop_propagation();
                }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .w_0()
                    // Center the text line, rather than a full-height editor viewport.
                    .h(query_height)
                    .child(self.find_editor.clone()),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .min_w(px(72.))
                    .text_size(px(12.))
                    .text_color(self.muted())
                    .child(label),
            )
            .child(self.find_step(
                "find-previous",
                "↑",
                self.i18n.text("chrome.find-previous"),
                false,
                cx,
            ))
            .child(self.find_step(
                "find-next",
                "↓",
                self.i18n.text("chrome.find-next"),
                true,
                cx,
            ))
    }
    fn find_step(
        &self,
        id: &'static str,
        label: &'static str,
        aria: String,
        next: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        div()
            .id(id)
            .role(Role::Button)
            .aria_label(aria)
            .w(px(22.))
            .h(px(22.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(4.))
            .cursor_pointer()
            .text_size(px(12.))
            .text_color(self.muted())
            .hover(|style| style.bg(self.hover_color()).text_color(self.control_text()))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, window, cx| {
                if next {
                    this.find_next(window, cx);
                } else {
                    this.find_previous(window, cx);
                }
                window.focus(&this.find_editor.focus_handle(cx), cx);
            }))
            .child(label)
    }
    /// What stands in the document area for a file Markraft could not read.
    ///
    /// Its text never became a document, so the editor below would offer "Start
    /// writing…" for a file that takes no writing. The explanation and the two ways
    /// out take the gate's own shape, scaled to a note.
    fn unreadable_file(&self, cx: &mut Context<Self>) -> Option<Div> {
        let note = self.library.active_note();
        let reason = note.read_only.as_ref()?.render(&self.i18n);
        if !note.document_is_empty() || self.interaction.panel() != Panel::Editor {
            return None;
        }
        let openable = note.path.is_some();
        let heading = self.i18n.text("chrome.unreadable-file");
        let rest = reason;
        Some(
            div()
                .absolute()
                .inset_0()
                // Opaque: the editor keeps the keyboard behind this, and its own
                // "Start writing…" would otherwise read through the explanation.
                .bg(notes_style(self.dark).background)
                .pt(TOOLBAR_HEIGHT + px(24.))
                .px_6()
                .flex()
                .flex_col()
                .gap_3()
                .child(
                    div()
                        .text_size(px(19.))
                        .line_height(px(25.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(heading),
                )
                .when(!rest.is_empty(), |s| {
                    s.child(
                        div()
                            .text_size(px(13.))
                            .line_height(px(19.))
                            .text_color(self.muted())
                            .child(rest),
                    )
                })
                .when(openable, |s| {
                    s.child(
                        div()
                            .mt_2()
                            .flex()
                            .flex_wrap()
                            .gap_2()
                            .child(self.button(
                                "unreadable-open",
                                self.i18n.text("chrome.open-default"),
                                Intent::OpenExternally,
                                cx,
                            ))
                            .child(self.button(
                                "unreadable-reveal",
                                self.i18n.text("chrome.reveal"),
                                Intent::RevealNote,
                                cx,
                            )),
                    )
                }),
        )
    }
    fn picker(&self, heading: bool, cx: &mut Context<Self>) -> Div {
        let query = self.query().read(cx).text().to_owned();
        let notes = self.matching_notes(query.trim());
        let total = notes.len();
        let now = crate::storage::timestamp();
        let home = std::env::var("HOME").ok();
        let mut list = div()
            .id("note-results")
            // Rows keep `Role::Button` inside the list: a `ListBoxOption` reaches VoiceOver as
            // static text and its AXPress lands outside the row, closing the panel instead.
            .role(Role::ListBox)
            .aria_label(self.i18n.text("notes.title"))
            .track_scroll(self.picker.browse_scroll())
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
                    .child(self.i18n.text("notes.no-matches")),
            );
        }
        for (index, note) in notes.iter().enumerate() {
            let id = note.id.clone();
            let current = note.id == self.library.active_id;
            let selected = index == self.picker.row();
            let status = if current {
                self.i18n.text("notes.current")
            } else {
                relative_day(note.updated_at, now, &self.i18n)
            };
            let location = note.path.as_ref().map(|path| {
                shorten_location(
                    &note_location(path, self.path.as_deref(), home.as_deref()),
                    location_budget(&status, current),
                )
            });
            // A pathless note has nowhere on disk yet, which the accent says without
            // adding a badge of its own: the row already speaks in dots and muted text.
            let location_color = match &location {
                Some(_) => self.muted(),
                None => notes_style(self.dark).marker,
            };
            let location = location.unwrap_or_else(|| self.i18n.text("notes.no-file"));
            let meta = format!("{status} · {location}");
            let mut controls = div()
                .absolute()
                .right(px(6.))
                .top(px(15.))
                .flex()
                .gap(px(4.));
            if selected {
                controls = controls
                    .child(
                        self.icon_button(
                            SharedString::from(format!("pin-{id}")),
                            self.i18n.text(if note.pinned {
                                "command.unpin-note"
                            } else {
                                "command.pin-note"
                            }),
                            if note.pinned { Icon::Pinned } else { Icon::Pin },
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
                            self.i18n.text("command.move-to-trash"),
                            Icon::Trash,
                            Intent::TrashNote(id.clone()),
                            cx,
                        )
                        .size(px(24.)),
                    );
            } else if note.pinned {
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
                        .child(icon(Icon::Pinned, self.muted())),
                );
            }
            let row_id = SharedString::from(id.clone());
            let select = id.clone();
            list = list.child(
                div()
                    .id(row_id.clone())
                    .role(Role::Button)
                    .aria_label(self.i18n.text_with(
                        if note.pinned {
                            "notes.pinned-row-label"
                        } else {
                            "notes.row-label"
                        },
                        &[("title", &note.display_title(&self.i18n)), ("meta", &meta)],
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
                        if this.picker.row() != index {
                            this.picker.point_at(index);
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.intent(Intent::Select(select.clone()), window, cx);
                    }))
                    .child(
                        div()
                            .w_full()
                            // Clear of the row's controls.
                            .pr(px(60.))
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .line_height(px(18.))
                                    .text_color(self.control_text())
                                    .truncate()
                                    .child(note.display_title(&self.i18n)),
                            )
                            .child(
                                div()
                                    .mt(px(2.))
                                    .flex()
                                    .items_center()
                                    .gap(px(4.))
                                    .when(current, |s| {
                                        s.child(
                                            div()
                                                .size(px(4.))
                                                .rounded_full()
                                                .bg(notes_style(self.dark).marker),
                                        )
                                    })
                                    // Two parts, so the location can be shortened
                                    // on its own while the status stays whole.
                                    .child(
                                        div()
                                            .flex_shrink_0()
                                            .text_size(px(12.))
                                            .line_height(px(18.))
                                            .text_color(self.muted())
                                            .child(format!("{status} ·")),
                                    )
                                    .child(
                                        div()
                                            .min_w_0()
                                            .text_size(px(12.))
                                            .line_height(px(18.))
                                            .text_color(location_color)
                                            .truncate()
                                            .child(location),
                                    ),
                            ),
                    )
                    .child(controls),
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
                        .h(px(26.))
                        .px_3()
                        .flex()
                        .items_center()
                        .justify_between()
                        .text_size(px(11.))
                        .text_color(self.muted())
                        .child(self.i18n.text("notes.title")),
                )
            })
            .child(self.scroll_area(list, self.picker.browse_scroll(), cx))
    }

    fn panel_key(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
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
        if self.interaction.format_menu().is_some() {
            return self.format_key(key, window, cx);
        }
        if self.input_composing(cx) {
            return false;
        }
        if self.interaction.code_language().is_some() {
            return self.code_language_key(key, window, cx);
        }
        if self.interaction.link() == Some(LinkPopover::Edit) {
            if key == "enter" {
                self.apply_link(window, cx);
            }
            // Up and down have nowhere to go in a one-line field.
            return true;
        }
        if self.interaction.rename().is_some() {
            if key == "enter" {
                self.apply_rename(window, cx);
            }
            return true;
        }
        if self.interaction.panel() == Panel::Editor {
            return false;
        }
        if self.interaction.panel() == Panel::Actions {
            let items = self.filtered_actions(cx);
            match key {
                "up" => self
                    .picker
                    .select_in_actions(self.picker.row().saturating_sub(1)),
                "down" => self
                    .picker
                    .select_in_actions((self.picker.row() + 1).min(items.len().saturating_sub(1))),
                "enter" => {
                    if let Some(intent) =
                        items.get(self.picker.row()).and_then(|c| c.intent.clone())
                    {
                        self.intent(intent, window, cx);
                    }
                }
                _ => return false,
            }
        } else {
            let query = self.query().read(cx).text().to_owned();
            let notes = self.matching_notes(query.trim());
            match key {
                "up" => self.select_row(self.picker.row().saturating_sub(1)),
                "down" => {
                    self.select_row((self.picker.row() + 1).min(notes.len().saturating_sub(1)))
                }
                "enter" => {
                    if let Some(note) = notes.get(self.picker.row()) {
                        let id = note.id.clone();
                        self.select_note(&id, window, cx);
                    }
                }
                _ => return false,
            }
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
            Command::new("new-action", "command.new-note", Intent::New).ex(&[("enew", 3)]),
            Command::new(
                "pin-note",
                if self.library.active_note().pinned {
                    "command.unpin-note"
                } else {
                    "command.pin-note"
                },
                Intent::Pin,
            ),
            Command::new("browse-action", "command.browse-notes", Intent::Browse).ex(&[
                ("ls", 2),
                ("buffers", 7),
                ("files", 5),
            ]),
            Command::new("daily-today", "command.daily-today", Intent::DailyToday)
                .ex(&[("today", 5)]),
            Command::new("save-now", "command.save-now", Intent::Save).ex(&[("write", 1)]),
            Command::new(
                "copy-markdown",
                "command.copy-as-markdown",
                Intent::CopyMarkdown,
            ),
            Command::new(
                "copy-rich-text",
                "command.copy-as-rich-text",
                Intent::CopyRichText,
            ),
            Command::new(
                "paste-plain",
                "command.paste-as-plain-text",
                Intent::PastePlain,
            ),
            Command::new(
                "paste-markdown",
                "command.paste-as-markdown",
                Intent::PasteMarkdown,
            ),
            Command::new("export-note", "command.export-markdown", Intent::Export),
            Command::new("export-html", "command.export-html", Intent::ExportHtml),
            Command::new("export-pdf", "command.export-pdf", Intent::ExportPdf),
            Command::new("print-note", "command.print", Intent::Print),
            Command::new("rename-note", "command.rename", Intent::Rename),
            Command::new(
                "open-markdown-action",
                "command.open-markdown",
                Intent::OpenMarkdown,
            ),
            Command::new(
                "report-issue",
                "command.report-an-issue",
                Intent::ReportIssue,
            ),
            Command::new(
                "copy-debug-info",
                "command.copy-debug-info",
                Intent::CopyDebugInfo,
            ),
            Command::new(
                "reveal-logs",
                "command.show-logs-in-finder",
                Intent::RevealLogs,
            ),
            Command::new(
                "format-bold",
                "command.bold",
                Intent::Mark(doc::Inline::Bold),
            ),
            Command::new(
                "format-italic",
                "command.italic",
                Intent::Mark(doc::Inline::Italic),
            ),
            Command::new(
                "format-strikethrough",
                "command.strikethrough",
                Intent::Mark(doc::Inline::Strikethrough),
            ),
            Command::new(
                "format-code",
                "command.inline-code",
                Intent::Mark(doc::Inline::Code),
            ),
            Command::new("format-link", "command.link", Intent::Link).slash(13, SlashEffect::Host),
            Command::new(
                "format-heading",
                "command.heading-1",
                Intent::Block(doc::Block::Heading(1)),
            )
            .slash(1, SlashEffect::Block(doc::Block::Heading(1))),
            Command::new(
                "format-heading-2",
                "command.heading-2",
                Intent::Block(doc::Block::Heading(2)),
            )
            .slash(2, SlashEffect::Block(doc::Block::Heading(2))),
            Command::new(
                "format-heading-3",
                "command.heading-3",
                Intent::Block(doc::Block::Heading(3)),
            )
            .slash(3, SlashEffect::Block(doc::Block::Heading(3))),
            Command::new(
                "format-heading-4",
                "command.heading-4",
                Intent::Block(doc::Block::Heading(4)),
            )
            .slash(4, SlashEffect::Block(doc::Block::Heading(4))),
            Command::new(
                "format-heading-5",
                "command.heading-5",
                Intent::Block(doc::Block::Heading(5)),
            )
            .slash(5, SlashEffect::Block(doc::Block::Heading(5))),
            Command::new(
                "format-heading-6",
                "command.heading-6",
                Intent::Block(doc::Block::Heading(6)),
            )
            .slash(6, SlashEffect::Block(doc::Block::Heading(6))),
            Command::new(
                "format-quote",
                "command.quote",
                Intent::Block(doc::Block::Quote),
            )
            .slash(10, SlashEffect::Block(doc::Block::Quote)),
            Command::new(
                "format-callout",
                "command.callout",
                Intent::Block(doc::Block::Callout),
            )
            .slash(10, SlashEffect::Block(doc::Block::Callout)),
            Command::new(
                "format-code-block",
                "command.code-block",
                Intent::Block(doc::Block::Code),
            )
            .slash(11, SlashEffect::Block(doc::Block::Code)),
            Command::new(
                "format-paragraph",
                "command.paragraph",
                Intent::Block(doc::Block::Paragraph),
            )
            .slash(0, SlashEffect::Block(doc::Block::Paragraph)),
            Command::new(
                "format-ordered",
                "command.ordered-list",
                Intent::Block(doc::Block::Ordered),
            )
            .slash(8, SlashEffect::Block(doc::Block::Ordered)),
            Command::new(
                "format-bullet",
                "command.bullet-list",
                Intent::Block(doc::Block::Bullet),
            )
            .slash(7, SlashEffect::Block(doc::Block::Bullet)),
            Command::new(
                "format-task",
                "command.task-list",
                Intent::Block(doc::Block::Task),
            )
            .slash(9, SlashEffect::Block(doc::Block::Task)),
            // A rule replaces the line it is on, so only the `/` menu offers it.
            Command::editor(
                "insert-divider",
                "command.divider",
                12,
                SlashEffect::Block(doc::Block::Divider),
            ),
        ];
        let vaults = crate::send::obsidian::vaults();
        // Without a notes folder there is no file for pictures to be found
        // beside, and a note already in a vault has nowhere new to go.
        let sendable = self
            .path
            .as_deref()
            .is_some_and(|folder| !crate::send::obsidian::within(folder, &vaults));
        if sendable {
            const IDS: [&str; 8] = [
                "send-to-obsidian-0",
                "send-to-obsidian-1",
                "send-to-obsidian-2",
                "send-to-obsidian-3",
                "send-to-obsidian-4",
                "send-to-obsidian-5",
                "send-to-obsidian-6",
                "send-to-obsidian-7",
            ];
            if let [vault] = vaults.as_slice() {
                items.push(Command::new(
                    IDS[0],
                    "command.send-to-obsidian",
                    Intent::SendToObsidian(vault.path.clone()),
                ));
            } else {
                // Several vaults: one command each, most recently used first.
                for (id, vault) in IDS.iter().zip(&vaults) {
                    items.push(
                        Command::new(
                            id,
                            "command.send-to-obsidian-vault",
                            Intent::SendToObsidian(vault.path.clone()),
                        )
                        .arg("vault", vault.name.clone()),
                    );
                }
            }
        }
        // Stepping between daily notes is offered from a daily note, towards a day
        // that has one.
        if self.adjacent_daily_note(false).is_some() {
            items.push(
                Command::new(
                    "daily-previous",
                    "command.daily-previous",
                    Intent::DailyPrevious,
                )
                .ex(&[("dprev", 5)]),
            );
        }
        if self.adjacent_daily_note(true).is_some() {
            items.push(
                Command::new("daily-next", "command.daily-next", Intent::DailyNext)
                    .ex(&[("dnext", 5)]),
            );
        }
        // A table cannot nest in another one, and a code block keeps its pipes literal.
        if caret.table.is_none() && !caret.in_code {
            items.push(
                Command::new("insert-table", "command.table", Intent::InsertTable)
                    .slash(14, SlashEffect::Host),
            );
        }
        items.extend([Command::new(
            "delete-note",
            "command.move-to-trash",
            Intent::Delete,
        )]);
        // Each of these reveals a different thing, and only while there is one.
        if self.library.active_note().path.is_some() {
            items.push(Command::new(
                "reveal-note",
                "command.reveal-note-in-finder",
                Intent::RevealNote,
            ));
        }
        if self.path.is_some() {
            items.push(Command::new(
                "reveal-folder",
                "command.show-folder-in-finder",
                Intent::Reveal,
            ));
        }
        if caret.in_task {
            items.push(Command::new(
                "toggle-task",
                "command.toggle-task",
                Intent::ToggleTask,
            ));
        }
        if caret.in_code {
            items.extend([
                Command::new(
                    "code-language",
                    "command.choose-code-language",
                    Intent::ChooseCodeLanguage,
                ),
                Command::new(
                    "copy-code-block",
                    "command.copy-code-block",
                    Intent::CopyCodeBlock,
                ),
            ]);
        }
        if caret.in_link {
            items.extend([
                Command::new("copy-link", "command.copy-link", Intent::CopyLink),
                Command::new("open-link", "command.open-link", Intent::OpenLink),
                Command::new("remove-link", "command.remove-link", Intent::Unlink),
            ]);
        }
        if let Some(table) = caret.table {
            items.extend([
                Command::new(
                    "table-row-above",
                    "command.add-row-above",
                    Intent::Table(TableEdit::RowBefore),
                ),
                Command::new(
                    "table-row-below",
                    "command.add-row-below",
                    Intent::Table(TableEdit::RowAfter),
                ),
                Command::new(
                    "table-column-left",
                    "command.add-column-left",
                    Intent::Table(TableEdit::ColumnBefore),
                ),
                Command::new(
                    "table-column-right",
                    "command.add-column-right",
                    Intent::Table(TableEdit::ColumnAfter),
                ),
                Command::new(
                    "table-delete-row",
                    "command.delete-row",
                    Intent::Table(TableEdit::DeleteRow),
                ),
                Command::new(
                    "table-delete-column",
                    "command.delete-column",
                    Intent::Table(TableEdit::DeleteColumn),
                ),
            ]);
            // The column's own alignment is the one the panel marks.
            for (id, label, alignment) in [
                (
                    "table-align-left",
                    "command.align-column-left",
                    ColumnAlignment::Left,
                ),
                (
                    "table-align-center",
                    "command.align-column-center",
                    ColumnAlignment::Center,
                ),
                (
                    "table-align-right",
                    "command.align-column-right",
                    ColumnAlignment::Right,
                ),
            ] {
                items.push(
                    Command::new(id, label, Intent::Table(TableEdit::Align(alignment)))
                        .checked(table.alignment == alignment),
                );
            }
            items.push(Command::new(
                "table-delete",
                "command.delete-table",
                Intent::Table(TableEdit::DeleteTable),
            ));
        }
        // Low-frequency, high-stakes: last so it is not hit by accident.
        items.push(Command::new(
            "folder-action",
            if self.path.is_some() {
                "command.switch-folder"
            } else {
                "command.open-folder"
            },
            Intent::ChooseFolder,
        ));
        items.extend(ex_commands());
        items
    }
    fn count_of(&self, units: usize) -> String {
        let key = match (self.toolbar.counts_words(), units == 1) {
            (true, true) => "chrome.word-one",
            (true, false) => "chrome.word-many",
            (false, true) => "chrome.character-one",
            (false, false) => "chrome.character-many",
        };
        self.i18n.text_with(key, &[("count", &units.to_string())])
    }
    /// What the footer counts: the note as a reader sees it. See [`doc::Counter`].
    fn note_count(&self, cx: &App) -> String {
        let editor = self.editor().read(cx);
        let units = self
            .counted
            .borrow_mut()
            .count(editor.state().doc(), self.toolbar.counts_words());
        self.count_of(units)
    }
    /// The labels the actions panel lists, in order, while it is open.
    #[cfg(test)]
    #[cfg_attr(coverage_nightly, coverage(off))]
    pub(crate) fn test_action_labels(&self, cx: &App) -> Option<Vec<String>> {
        (self.interaction.panel() == Panel::Actions).then(|| {
            self.filtered_actions(cx)
                .into_iter()
                .map(|command| command.text(&self.i18n))
                .collect()
        })
    }
    fn filtered_actions(&self, cx: &App) -> Vec<Command> {
        let query = self
            .query()
            .read(cx)
            .text()
            .to_owned()
            .trim()
            .to_lowercase();
        let editor = self.editor();
        let editor = editor.read(cx);
        let block = doc::Block::active(editor.state(), &editor.projection());
        let caret = Caret {
            in_link: editor.active_link().is_some(),
            // The note goes on painting behind the panel, so this is the table the
            // caret is in even while the panel has the keyboard.
            table: editor.table_at_caret(),
            in_code: block == Some(doc::Block::Code),
            in_task: block == Some(doc::Block::Task),
        };
        let items = self.action_items(caret).into_iter();
        // A `:` query is a vim command line: it matches the commands' `:` names, and
        // one it already names in full or as vim abbreviates it goes first, so Return
        // runs `:w` rather than a longer command it is also the start of. The fullwidth
        // colon a Chinese input method types counts as the same.
        if let Some(typed) = query.strip_prefix(':').or_else(|| query.strip_prefix('：')) {
            let typed = typed.trim();
            let mut items: Vec<_> = items
                .filter(|command| command.intent.is_some())
                .filter_map(|command| Some((!command.ex_match(typed)?, command)))
                .collect();
            items.sort_by_key(|(partial, command)| {
                (*partial, command.intent.as_ref().map(Intent::action_group))
            });
            return items.into_iter().map(|(_, command)| command).collect();
        }
        let english = crate::locale::I18n::english();
        let mut items: Vec<_> = items
            .filter(|command| {
                command.intent.is_some()
                    && !command.ex_only
                    && (command.text(&self.i18n).to_lowercase().contains(&query)
                        || command.text(&english).to_lowercase().contains(&query))
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
            .aria_label(self.i18n.text("actions.title"))
            .track_scroll(self.picker.actions_scroll())
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
                    .child(self.i18n.text("actions.no-matches")),
            );
        }
        let mut previous_group = None;
        let vim = self.preferences.vim_mode;
        for (index, command) in items.into_iter().enumerate() {
            let ex = vim.then(|| command.ex_label()).flatten();
            let label = command.text(&self.i18n);
            let Command {
                id,
                shortcut,
                intent,
                checked,
                ..
            } = command;
            let Some(intent) = intent else { continue };
            let enabled = self.editing_enabled(&intent, cx);
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
                        self.row(id, label, shortcut, ex, intent, cx)
                            .when(!enabled, |s| s.opacity(0.45))
                            .h(ACTION_ROW_HEIGHT)
                            .text_size(px(14.))
                            .role(Role::Button)
                            .aria_selected(index == self.picker.row())
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
                            .when(index == self.picker.row(), |s| s.bg(self.selected_color()))
                            .on_mouse_move(cx.listener(move |this, _, _, cx| {
                                if this.picker.row() != index {
                                    this.picker.point_at(index);
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
            .child(self.scroll_area(list, self.picker.actions_scroll(), cx))
    }
    fn overlay(&self, window: &Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let viewport = window.bounds().size;
        // The card hangs from the top of the window and always ends clear of its lower
        // edge, so a short window shows a shorter card rather than one running off it.
        let top = if viewport.height < px(400.) {
            px(44.)
        } else {
            px(100.)
        };
        let width = px(320.).min(viewport.width - px(32.));
        let available = (viewport.height - top - px(16.)).max(px(0.));
        let desired = match self.interaction.panel() {
            Panel::Browse => {
                let n = self
                    .matching_notes(self.query().read(cx).text().trim())
                    .len();
                let heading = 26.;
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
            // The overlay is only drawn over a panel.
            Panel::Editor => px(0.),
        };
        // Include both border pixels so a fully visible short list does not scroll.
        let height = (desired + px(2.)).min(px(420.)).min(available);
        let contents = match self.interaction.panel() {
            // A short card gives what room it has to the rows rather than to a heading.
            Panel::Browse => self.picker(height >= px(136.), cx),
            Panel::Actions => self.actions_panel(cx),
            Panel::Editor => div(),
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
impl Render for MarkraftApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.interaction.code_language().is_some() && self.code_language.take_focus() {
            window.focus(&self.query().focus_handle(cx), cx);
            let block = self.interaction.code_language();
            let weak = cx.entity().downgrade();
            window.on_next_frame(move |_, cx| {
                let _ = weak.update(cx, |this, cx| {
                    if this.interaction.code_language() == block {
                        this.code_language.select(this.code_language.row());
                        cx.notify();
                    }
                });
            });
        }
        // The title bar shows the file stem; the rename pill adds the extension beside
        // the field. A note with no file yet is named by its first line instead, since
        // that is what its file will be called.
        let note = self.library.active_note();
        let title = note
            .path
            .as_ref()
            .and_then(|path| path.file_stem())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| note.display_title(&self.i18n));
        // With no folder open there is no note to name, and a stray "Untitled"
        // over the gate reads as a bug rather than as a state.
        let unopened = self.persistence.is_none();
        window.set_window_title(if unopened { "Markraft" } else { &title });
        let style = notes_style(self.dark);
        let reduce_motion = cx.reduce_motion();
        let chrome = Self::chrome_spring(self.chrome_visible(), reduce_motion);
        // The window outlines itself while something droppable is over it.
        let accent = style.marker;
        let root = div()
            .key_context("MarkraftApp")
            .track_focus(self.ring.panel())
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
                if this.find_focused(w, cx) || this.focus_step(true, w, cx) {
                    cx.stop_propagation();
                } else {
                    cx.propagate();
                }
            }))
            .capture_action(cx.listener(|this, _: &markraft_gpui::Outdent, w, cx| {
                if this.find_focused(w, cx) || this.focus_step(false, w, cx) {
                    cx.stop_propagation();
                } else {
                    cx.propagate();
                }
            }))
            // The editor's own ⌘&, ⌘*, ⌘( and ⌥⌘C make their block with the schema's
            // markers; the note's take the ones the preferences ask for, as the
            // toolbar's do.
            .capture_action(cx.listener(|this, _: &markraft_gpui::CodeBlock, w, cx| {
                this.run_block_shortcut(doc::Block::Code, w, cx);
            }))
            .capture_action(cx.listener(|this, _: &markraft_gpui::Ordered, w, cx| {
                this.run_block_shortcut(doc::Block::Ordered, w, cx);
            }))
            .capture_action(cx.listener(|this, _: &markraft_gpui::Bullet, w, cx| {
                this.run_block_shortcut(doc::Block::Bullet, w, cx);
            }))
            .capture_action(cx.listener(|this, _: &markraft_gpui::Task, w, cx| {
                this.run_block_shortcut(doc::Block::Task, w, cx);
            }))
            // Every keystroke passes here on its way down, which is where the chrome
            // learns that someone is at the window. Space additionally runs the ringed
            // control: it is bound to nothing, so it only ever reaches here when no text
            // field owns the keyboard.
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, w, cx| {
                this.note_key_press(cx);
                if event.keystroke.key == "space"
                    && !event.keystroke.modifiers.modified()
                    && this.focus_activate(w, cx)
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
            // Dropping belongs to the whole window: the Browse panel and the screens
            // that have no folder yet are where a dropped file or folder is most
            // likely to be aimed.
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                this.drop_paths(paths.0.iter().cloned().collect(), window, cx);
            }))
            // The outline only exists while something is being dragged over, so the
            // note keeps the whole window the rest of the time.
            .drag_over::<ExternalPaths>(move |s, _, _, _| s.border_2().border_color(accent))
            .on_action(cx.listener(|this, _: &Save, w, cx| this.save_now(w, cx)))
            .on_action(cx.listener(|this, _: &CopyMarkdown, _, cx| this.copy_markdown(cx)))
            .on_action(cx.listener(|this, _: &Quit, window, cx| this.quit(window, cx)))
            .on_action(cx.listener(|this, _: &Hide, w, cx| this.dismiss(w, cx)))
            .on_action(cx.listener(|this, _: &HideWindow, w, cx| this.intent(Intent::Hide, w, cx)))
            .on_action(cx.listener(|this, _: &NewNote, w, cx| this.intent(Intent::New, w, cx)))
            .on_action(cx.listener(|this, _: &Browse, w, cx| this.intent(Intent::Browse, w, cx)))
            .on_action(cx.listener(|this, _: &Actions, w, cx| this.intent(Intent::Actions, w, cx)))
            .on_action(cx.listener(|this, _: &ExCommand, w, cx| this.open_ex(w, cx)))
            .on_action(
                cx.listener(|this, _: &Settings, w, cx| this.intent(Intent::Settings, w, cx)),
            )
            .on_action(cx.listener(|this, _: &Link, w, cx| this.intent(Intent::Link, w, cx)))
            .on_action(cx.listener(|this, _: &Export, w, cx| this.intent(Intent::Export, w, cx)))
            .on_action(
                cx.listener(|this, _: &ExportHtml, w, cx| this.intent(Intent::ExportHtml, w, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ExportPdf, w, cx| this.intent(Intent::ExportPdf, w, cx)),
            )
            .on_action(cx.listener(|this, _: &Print, w, cx| this.intent(Intent::Print, w, cx)))
            .on_action(cx.listener(|this, _: &IncreaseTextSize, w, cx| {
                this.set_preference(Pref::TextSize(this.preferences.text_size + 1.), w, cx)
            }))
            .on_action(cx.listener(|this, _: &DecreaseTextSize, w, cx| {
                this.set_preference(Pref::TextSize(this.preferences.text_size - 1.), w, cx)
            }))
            .on_action(cx.listener(|this, _: &ResetTextSize, w, cx| {
                this.set_preference(
                    Pref::TextSize(crate::storage::Preferences::DEFAULT_TEXT_SIZE),
                    w,
                    cx,
                )
            }))
            .on_action(
                cx.listener(|this, _: &OpenMarkdown, w, cx| {
                    this.intent(Intent::OpenMarkdown, w, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &Find, w, cx| this.open_find(w, cx)))
            .on_action(cx.listener(|this, _: &VimFind, w, cx| this.open_vim_find(w, cx)))
            .on_action(cx.listener(|this, _: &VimFindNext, _, cx| this.repeat_vim_find(true, cx)))
            .on_action(
                cx.listener(|this, _: &VimFindPrevious, _, cx| this.repeat_vim_find(false, cx)),
            )
            .on_action(cx.listener(|this, _: &FindNext, w, cx| this.find_next(w, cx)))
            .on_action(cx.listener(|this, _: &FindPrevious, w, cx| this.find_previous(w, cx)));
        let actions = self
            .chrome_capsule()
            .p(px(4.))
            .child(
                self.icon_button(
                    "actions",
                    format!("{} · ⌘K", self.i18n.text("actions.title")),
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
                    format!("{} · ⌘P", self.i18n.text("command.browse-notes")),
                    Icon::Notes,
                    Intent::Browse,
                    cx,
                )
                .size(px(28.))
                .rounded_full()
                .opacity(1.),
            )
            .child(
                self.icon_button(
                    "new-note",
                    format!("{} · ⌘N", self.i18n.text("command.new-note")),
                    Icon::Plus,
                    Intent::New,
                    cx,
                )
                .size(px(28.))
                .rounded_full()
                .opacity(1.),
            );
        // Both bands are chrome, so the pointer is the window's own arrow and
        // changes only over what can be clicked. A click on the title band stays
        // there; a scroll still reaches the note under it. The footer stays
        // clear, so both a click and a scroll reach the note.
        let toolbar = div()
            .id("toolbar")
            .cursor_default()
            .block_mouse_except_scroll()
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
                    .flex()
                    .justify_center()
                    .text_size(px(12.))
                    .text_color(self.muted())
                    // Only the words are the control, as in a macOS title bar: the band
                    // either side of them stays part of the window.
                    .child(
                        div()
                            .id("note-title")
                            .role(Role::Button)
                            .aria_label(self.i18n.text("notes.rename"))
                            .min_w_0()
                            .truncate()
                            .when(!unopened, |s| {
                                s.cursor_pointer()
                                    .hover(|s| s.text_color(self.control_text()))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        cx.stop_propagation();
                                        if this.interaction.rename().is_some() {
                                            this.close_rename(window, cx);
                                        } else {
                                            this.intent(Intent::Rename, window, cx);
                                        }
                                    }))
                            })
                            .child(if unopened { String::new() } else { title }),
                    )
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
                        Self::chrome_spring(self.presence.pointer_inside(), reduce_motion),
                        |s, phase| s.opacity(phase.interpolate_clamped(0., 1.)),
                    ),
            );
        if self.persistence.is_none() {
            // The notes folder could not be opened. First launch normally creates
            // Documents/Markraft, so this screen is recovery — not a cold-start gate.
            let default_folder = crate::storage::default_notes_folder();
            let offer_default = default_folder.as_ref().is_some_and(|default| {
                self.path.as_ref().is_none_or(|current| {
                    !crate::storage::notes_folder_matches(Some(current), default)
                })
            });
            let (title, explanation) = (
                self.i18n.text("chrome.open-failed"),
                self.i18n.text("chrome.open-explanation"),
            );
            // Choose Folder is the filled action; Enter takes it.
            let accent = notes_style(self.dark).marker;
            let primary = self
                .button_with_hover(
                    "choose-folder",
                    self.i18n.text("chrome.choose-folder"),
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
                .child(self.button(
                    "retry-open",
                    self.i18n.text("chrome.retry"),
                    Intent::Retry,
                    cx,
                ))
                .child(primary)
                .when(offer_default, |s| {
                    s.child(self.button(
                        "use-default-folder",
                        self.i18n.text("chrome.default-folder"),
                        Intent::UseDefaultFolder,
                        cx,
                    ))
                })
                .child(self.button(
                    "open-markdown",
                    self.i18n.text("chrome.open-markdown"),
                    Intent::OpenMarkdown,
                    cx,
                ))
                .when(self.path.is_some(), |s| {
                    s.child(self.button(
                        "reveal-library",
                        self.i18n.text("chrome.show-folder"),
                        Intent::Reveal,
                        cx,
                    ))
                });
            let detail = div()
                .mt_3()
                .text_size(px(12.))
                .text_color(self.muted())
                .child(
                    self.feedback
                        .error()
                        .map(|error| error.render(&self.i18n))
                        .unwrap_or_default(),
                );
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
                    .when(self.feedback.error().is_some(), |s| s.child(detail))
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
            .id("document-body")
            .flex_1()
            .min_h_0()
            .relative()
            .child(self.editor())
            // Over the editor, which still owns the keyboard: its own placeholder
            // would otherwise invite writing into a file that takes none.
            .children(self.unreadable_file(cx))
            .child(fade(px(80.), true))
            .child(fade(px(56.), true))
            .child(fade(px(128.), false))
            .child(fade(px(64.), false))
            .child(toolbar)
            .when(
                self.find_open && self.interaction.panel() == Panel::Editor,
                |body| body.child(self.find_bar(cx)),
            )
            .child(
                div()
                    .id("footer-band")
                    .cursor_default()
                    .absolute()
                    .bottom_0()
                    .left_0()
                    .right_0()
                    .child(self.footer(count, window.bounds().size.width, cx)),
            );
        root.child(body)
            // Both banners are live regions, so a failure is spoken rather than only shown.
            .when_some(self.feedback.platform_error().cloned(), |s, error| {
                let message = self.i18n.text_with(
                    "chrome.shortcut-error",
                    &[("error", &error.render(&self.i18n))],
                );
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
            .when_some(self.feedback.notice().cloned(), |s, notice| {
                let announced = match notice.action() {
                    Some(action) => format!(
                        "{} · {}",
                        notice.text.render(&self.i18n),
                        action.label.render(&self.i18n)
                    ),
                    None => notice.text.render(&self.i18n),
                };
                let toast = div()
                    .id("notice")
                    .role(Role::Status)
                    .aria_label(announced)
                    // A folder's notices are sentences rather than acknowledgments,
                    // so the toast wraps instead of running off the window.
                    .max_w(window.bounds().size.width - px(24.))
                    .flex()
                    .items_center()
                    .gap_1()
                    // Wide enough that the text clears the curve of the rounded ends.
                    .px_4()
                    .py_1()
                    .rounded_full()
                    .bg(self.surface_color())
                    .border_1()
                    .border_color(self.border_color())
                    .shadow(popover_shadow())
                    .text_size(px(12.))
                    .text_color(self.control_text())
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(notice.text.render(&self.i18n)),
                    )
                    .when_some(notice.action().cloned(), |s, action| {
                        let path = action.path.clone();
                        s.child(div().text_color(self.muted()).child("·")).child(
                            div()
                                .id("notice-reveal")
                                .role(Role::Button)
                                .aria_label(action.label.render(&self.i18n))
                                .cursor_pointer()
                                .hover(|s| s.opacity(0.7))
                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.stop_propagation();
                                    cx.reveal_path(&path);
                                }))
                                .child(action.label.render(&self.i18n)),
                        )
                    });
                s.child(
                    // Centred above the footer, so it never sits on the count or the
                    // buttons there. The row itself takes no clicks from the note.
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .bottom(super::FOOTER_HEIGHT + px(4.))
                        .flex()
                        .justify_center()
                        // Keyed by what it says, so each notice rises rather than
                        // swapping its words inside one that is already up.
                        .child(
                            toast.with_spring(
                                SharedString::from(format!("notice-{}", notice.text)),
                                SpringAnimation::new(SpringConfig::new(900., 60., 1.))
                                    .to(true)
                                    .from(false)
                                    .playback(playback(reduce_motion)),
                                |s, phase| {
                                    s.opacity(phase.interpolate_clamped(0., 1.))
                                        .mt(phase.interpolate_clamped(px(12.), px(0.)))
                                        .mb(phase.interpolate_clamped(px(-12.), px(0.)))
                                },
                            ),
                        ),
                )
            })
            .when(
                self.interaction.format_menu().is_some()
                    && self.interaction.panel() == Panel::Editor,
                |s| {
                    s.child(popover_enter(
                        "format-enter",
                        self.format_popover(window, cx),
                        true,
                        reduce_motion,
                    ))
                },
            )
            .when_some(self.rename_pill(window, cx), |s, pill| {
                s.child(popover_enter("rename-enter", pill, false, reduce_motion))
            })
            .when_some(self.link_pill(window, cx), |s, pill| {
                s.child(popover_enter("link-enter", pill, true, reduce_motion))
            })
            .when_some(self.file_status_card(window, cx), |s, card| {
                s.child(popover_enter(
                    "file-status-enter",
                    card,
                    true,
                    reduce_motion,
                ))
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
            .when(self.interaction.panel() != Panel::Editor, |s| {
                s.child(popover_enter(
                    "overlay-enter",
                    self.overlay(window, cx),
                    false,
                    reduce_motion,
                ))
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

/// Whether hovering the control should name it. The glyph and its place already
/// do for a pin and a trash can on a note, a checkmark beside a field, the link
/// pill's pencil, clipboard, open arrow and eraser, and the alignment trio; and a
/// format menu names its own entries as soon as it opens, right under it. The
/// accessible name stays either way.
fn shows_tooltip(intent: &Intent) -> bool {
    !matches!(
        intent,
        Intent::PinNote(_)
            | Intent::TrashNote(_)
            | Intent::ApplyLink
            | Intent::ApplyRename
            | Intent::EditLink
            | Intent::CopyLink
            | Intent::OpenLink
            | Intent::Unlink
            | Intent::FormatMenu(FormatMenu::Block | FormatMenu::Inline | FormatMenu::List)
            | Intent::Table(TableEdit::Align(_))
    )
}

struct Hint {
    label: SharedString,
    dark: bool,
}
impl Render for Hint {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let (text, keys) = self
            .label
            .split_once(" · ")
            .unwrap_or((self.label.as_ref(), ""));
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
            .child(text.to_owned())
            .when(!keys.is_empty(), |s| s.child(keycaps(keys, self.dark)))
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod shortcut_tests {
    // Not `use super::*`: that would bring gpui's `test` macro in over the built-in one.
    use super::{Command, Intent, TableEdit, shortcut_label};
    use crate::doc;

    #[test]
    fn locale_date_labels_translate_whole_messages() {
        let now = 1_700_000_000_000;
        let english = crate::locale::I18n::english();
        let translated = crate::locale::I18n::fixture("zh-Hans");
        assert_eq!(super::relative_day(now, now, &english), "Edited today");
        assert_eq!(super::relative_day(now, now, &translated), "测试：今天编辑");
        assert_eq!(
            super::relative_day(now - 3 * 86_400_000, now, &translated),
            "测试：3 天前编辑"
        );
    }

    #[test]
    fn a_command_takes_its_shortcut_from_the_one_table() {
        for intent in [
            Intent::New,
            Intent::Mark(doc::Inline::Bold),
            Intent::Block(doc::Block::Heading(3)),
            Intent::Table(TableEdit::RowAfter),
            Intent::Rename,
        ] {
            let command = Command::new("id", "Label", intent.clone());
            assert_eq!(command.shortcut, shortcut_label(&intent));
        }
    }

    #[test]
    fn the_table_gives_every_surface_the_same_labels() {
        // The bindings the toolbar menus and the `/` menu show.
        assert_eq!(shortcut_label(&Intent::Block(doc::Block::Paragraph)), "⌘0");
        for level in 1..=6 {
            assert_eq!(
                shortcut_label(&Intent::Block(doc::Block::Heading(level))),
                ["⌘1", "⌘2", "⌘3", "⌘4", "⌘5", "⌘6"][usize::from(level) - 1]
            );
        }
        assert_eq!(shortcut_label(&Intent::Mark(doc::Inline::Bold)), "⌘B");
        assert_eq!(shortcut_label(&Intent::Mark(doc::Inline::Italic)), "⌘I");
        assert_eq!(
            shortcut_label(&Intent::Mark(doc::Inline::Strikethrough)),
            "⇧⌘S"
        );
        assert_eq!(shortcut_label(&Intent::Block(doc::Block::Ordered)), "⇧⌘7");
        assert_eq!(shortcut_label(&Intent::Block(doc::Block::Bullet)), "⇧⌘8");
        assert_eq!(shortcut_label(&Intent::Block(doc::Block::Task)), "⇧⌘9");
        assert_eq!(shortcut_label(&Intent::Link), "⌘L");
        assert_eq!(shortcut_label(&Intent::Rename), "");
        assert_eq!(shortcut_label(&Intent::Hide), "⌘W");
        // `:wq` saves and hides, but no single key does both.
        assert_eq!(
            shortcut_label(&Intent::Then(vec![Intent::Save, Intent::Hide])),
            ""
        );
    }
}
