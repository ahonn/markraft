//! The Settings window: a window of its own beside the floating note, drawn the way a
//! classic macOS settings window is. Pages are chosen from a row of icons under the
//! title; each page is a form of right-aligned labels; the window keeps its width and
//! takes the height of the page on screen.
//!
//! The preferences still belong to [`MarkraftApp`]: the window holds no copy of them. It
//! reads what the app holds each frame and hands every change back as a [`Change`],
//! applied in the note window's context because the theme, the editors and the folder
//! prompts all belong there. The app is observed, so a change made anywhere redraws it.
//!
//! Two properties are load-bearing and invisible in the layout. The window is raised
//! and closed from a later turn of the run loop: every route in arrives inside the
//! dispatch of a keystroke or a menu event, and taking key status or removing a window
//! from in there re-enters AppKit or GPUI. And a close, however it happens, gives the
//! global shortcut back and returns the foreground if the note is not on screen, since
//! Markraft has no Dock icon to come back through.

mod controls;
mod placement;
mod select;
mod shortcut;

use super::*;
use crate::locale::{I18n, LanguagePreference, available_languages};
use crate::platform::Shortcut;
use crate::storage::{
    BulletMarker, CodeFence, EditorFont, EmphasisMarker, HardBreakStyle, ImageNaming, LineHeight,
    LineWidth, NoteNaming, OrderedDelimiter, Pref, Preferences, SettingsWindowPlacement, Summon,
    TabKey,
};
use controls::{
    ChordFace, Palette, button, checkbox, chord_face, error, group_gap, line, metrics::*, row,
    segmented, stepper,
};
use gpui_base::{Tab, Tabs};
use placement::{SettingsDisplay, initial_bounds, select_display};
use select::{MenuItem, MenuRow};
use shortcut::Recorded;
use std::{cell::Cell, rc::Rc, sync::Arc};

actions!(
    markraft_settings,
    [
        CloseSettings,
        NextControl,
        PreviousControl,
        PreviousPage,
        NextPage
    ]
);

const WIDTH: f32 = 520.;
/// Where the window starts before the first page has been measured.
const HEIGHT: f32 = 420.;
/// Past this the page scrolls rather than the window growing.
const MAX_HEIGHT: f32 = 720.;
const KEY_CONTEXT: &str = "MarkraftSettings";
const TOOLBAR_CONTEXT: &str = "MarkraftSettingsToolbar";
const REPOSITORY: &str = "https://github.com/ahonn/markraft";
const RELEASES: &str = "https://github.com/ahonn/markraft/releases";
const NEW_ISSUE: &str = "https://github.com/ahonn/markraft/issues/new";
const ABOUT_ICON: f32 = 64.;
const ABOUT_NAME_SIZE: f32 = 15.;
const ABOUT_TOP: f32 = 22.;

/// The app's icon, decoded once: the About page is the one place it is drawn.
fn app_icon() -> Arc<Image> {
    static ICON: std::sync::LazyLock<Arc<Image>> = std::sync::LazyLock::new(|| {
        Arc::new(Image::from_bytes(
            ImageFormat::Png,
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/icon/Markraft.png"
            ))
            .to_vec(),
        ))
    });
    ICON.clone()
}

/// Scoped to the window, so the note's own bindings for the same keys stay its own.
pub(in crate::app) fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("escape", CloseSettings, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-w", CloseSettings, Some(KEY_CONTEXT)),
        KeyBinding::new("tab", NextControl, Some(KEY_CONTEXT)),
        KeyBinding::new("shift-tab", PreviousControl, Some(KEY_CONTEXT)),
        KeyBinding::new("left", PreviousPage, Some(TOOLBAR_CONTEXT)),
        KeyBinding::new("right", NextPage, Some(TOOLBAR_CONTEXT)),
    ]);
}

/// Why a change made in the Settings window was refused, kept by the app so the row
/// that asked can say so. Cleared as each one is asked again, and when the window goes.
#[derive(Clone, Default)]
pub(in crate::app) struct SettingsErrors {
    /// By [`Shortcut`]: the shortcut that shows the note, the one for a new note, then
    /// the one for today's daily note.
    pub(in crate::app) shortcuts: [Option<Message>; 3],
    login: Option<Message>,
    new_notes: Option<Message>,
    daily: Option<Message>,
    images: Option<Message>,
    updates: Option<Message>,
}

#[derive(Clone)]
enum Change {
    /// A preference; see [`Pref`].
    Pref(Pref),
    /// The window came forward: re-read what macOS keeps outside the app.
    Refresh,
    WindowPlacement(SettingsWindowPlacement),
    LaunchAtLogin(bool),
    /// A recorder opening or closing: the running shortcuts are let go of meanwhile.
    Recording(bool),
    ChooseFolder,
    NewNoteLocation,
    ResetNewNoteLocation,
    ImageLocation,
    ResetImageLocation,
    RevealFolder,
    NewNoteName(NoteNaming),
    AutomaticUpdates(bool),
    CheckForUpdates,
    ImageName(ImageNaming),
    DailyFolder,
    ResetDailyFolder,
    /// A date format the window has already checked.
    DailyFormat(String),
    DailyTemplate(Option<PathBuf>),
    ChooseDailyTemplate,
    SyncDailyFromObsidian,
}

/// The preference `which` global shortcut is.
fn shortcut_pref(which: Shortcut, shortcut: String) -> Pref {
    match which {
        Shortcut::Toggle => Pref::Hotkey(shortcut),
        Shortcut::NewNote => Pref::NewNoteHotkey(shortcut),
        Shortcut::DailyNote => Pref::DailyNoteHotkey(shortcut),
    }
}

/// Options preserve requests outside the registry, including regional aliases.
fn language_options(
    i18n: &I18n,
    preference: &LanguagePreference,
) -> Vec<(String, LanguagePreference)> {
    let mut languages = vec![(
        i18n.text("settings.follow-system"),
        LanguagePreference::System,
    )];
    languages.extend(available_languages().iter().map(|language| {
        (
            language.name.clone(),
            LanguagePreference::Locale(language.id.clone()),
        )
    }));
    // A custom tag may resolve to a registered language or English fallback.
    if let LanguagePreference::Locale(locale) = preference
        && !languages
            .iter()
            .any(|(_, available)| available == preference)
    {
        languages.push((
            i18n.text_with("language.custom", &[("language", locale)]),
            preference.clone(),
        ));
    }
    languages
}

/// What a page draws, read from the app once per frame.
struct Snapshot {
    i18n: I18n,
    dark: bool,
    preferences: Preferences,
    /// None without the platform layer, which is what answers the question.
    login: Option<bool>,
    folder: Option<PathBuf>,
    /// Where new notes and images go, and whether that is other than the default.
    new_notes: Option<(String, bool)>,
    images: Option<(String, bool)>,
    new_note_name: NoteNaming,
    /// Whether this copy updates itself. One that a store updates has no update
    /// controls to show.
    self_updating: bool,
    /// None where this copy has no updater to ask: unbundled, or not configured.
    automatic_updates: Option<bool>,
    image_name: ImageNaming,
    daily: crate::daily::DailySettings,
    /// Where daily notes go, and whether that is other than the notes folder itself.
    daily_folder: Option<(String, bool)>,
    /// The Obsidian vault this folder is keeps its daily notes otherwise.
    obsidian_differs: bool,
    errors: SettingsErrors,
}

impl MarkraftApp {
    /// ⌘, and the Settings commands: open the window, or bring the open one forward.
    pub(crate) fn open_bundled_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(platform) = &mut self.platform {
            platform.remember_frontmost_app();
        }
        let link = Link {
            app: cx.entity().downgrade(),
            main: window.window_handle(),
        };
        if self.launch_at_login.is_none() {
            self.refresh_launch_at_login();
        }
        let near = window.bounds();
        let display = window.display(cx).map(|display| display.id());
        cx.defer(move |cx| present(link, near, display, cx));
    }

    pub(in crate::app) fn open_settings(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        cx.emit(crate::host::WorkspaceEvent::OpenSettings);
    }

    /// Read every frame of the Settings window, including each step of its resize, so
    /// it asks nothing of the system: what macOS keeps is cached by [`Change::Refresh`].
    fn settings_snapshot(&self) -> Snapshot {
        let workspace = &self.notes.library.workspace;
        let placed = |root: &PathBuf| {
            let new_notes = (
                folder_label(root, &workspace.new_note_directory),
                !workspace.new_note_directory.as_os_str().is_empty(),
            );
            let images = match &workspace.attachments {
                crate::storage::AttachmentPolicy::Default => {
                    (self.i18n.text("settings.assets-beside-note"), false)
                }
                crate::storage::AttachmentPolicy::WorkspaceFolder(path) => {
                    (folder_label(root, path), true)
                }
            };
            (new_notes, images)
        };
        let (new_notes, images) = self.path.as_ref().map(placed).unzip();
        let daily_folder = self.path.as_ref().map(|root| {
            (
                folder_label(root, &workspace.daily.folder),
                !workspace.daily.folder.as_os_str().is_empty(),
            )
        });
        Snapshot {
            i18n: self.i18n.clone(),
            dark: self.dark,
            preferences: self.preferences.clone(),
            login: self.launch_at_login,
            folder: self.path.clone(),
            new_notes,
            images,
            new_note_name: workspace.new_note_name,
            self_updating: self.updater.updates_itself(),
            automatic_updates: self.updater.automatically_checks(),
            image_name: workspace.image_name,
            daily: workspace.daily.clone(),
            daily_folder,
            obsidian_differs: self
                .obsidian_daily
                .as_ref()
                .is_some_and(|obsidian| *obsidian != workspace.daily),
            errors: self.settings_errors.clone(),
        }
    }

    fn refresh_launch_at_login(&mut self) {
        self.launch_at_login = self
            .platform
            .as_ref()
            .map(|platform| platform.launch_at_login_enabled());
    }

    fn apply_setting(&mut self, change: Change, window: &mut Window, cx: &mut Context<Self>) {
        match change {
            // Only remembered for the next opening: nothing drawn reads it, so it
            // redraws neither window while the Settings window is resizing.
            Change::Pref(Pref::SettingsPage(page)) => {
                self.preferences.settings_page = page;
                self.save.schedule(Instant::now());
                return;
            }
            Change::WindowPlacement(placement) => {
                if self.preferences.settings_window.as_ref() != Some(&placement) {
                    self.preferences.settings_window = Some(placement);
                    self.save.schedule(Instant::now());
                }
                return;
            }
            Change::Pref(pref) => self.set_preference(pref, window, cx),
            Change::Refresh => {
                self.refresh_launch_at_login();
                self.obsidian_daily = self.read_obsidian_daily();
            }
            Change::LaunchAtLogin(enabled) => {
                if let Some(platform) = &mut self.platform {
                    self.settings_errors.login = platform.set_launch_at_login(enabled).err();
                }
                self.refresh_launch_at_login();
            }
            Change::Recording(true) => {
                self.settings_errors.shortcuts = Default::default();
                if let Some(platform) = &mut self.platform {
                    platform.suspend_shortcuts();
                }
            }
            Change::Recording(false) => {
                if let Some(platform) = &mut self.platform
                    && let Err(error) = platform.resume_shortcuts()
                {
                    self.settings_errors.shortcuts[0] = Some(error);
                }
            }
            Change::ChooseFolder => self.choose_folder(window, cx),
            Change::NewNoteLocation => {
                self.settings_errors.new_notes = None;
                self.configure_new_notes(window, cx);
            }
            Change::ResetNewNoteLocation => {
                self.settings_errors.new_notes = None;
                self.notes.library.workspace.new_note_directory = PathBuf::new();
                self.schedule_save(cx);
            }
            Change::ImageLocation => {
                self.settings_errors.images = None;
                self.configure_images(window, cx);
            }
            Change::ResetImageLocation => {
                self.settings_errors.images = None;
                self.notes.library.workspace.attachments =
                    crate::storage::AttachmentPolicy::Default;
                self.schedule_save(cx);
            }
            Change::RevealFolder => {
                if let Some(path) = &self.path {
                    cx.reveal_path(path);
                }
            }
            Change::NewNoteName(naming) => {
                self.notes.library.workspace.new_note_name = naming;
                self.schedule_save(cx);
            }
            Change::AutomaticUpdates(enabled) => {
                if let Err(error) = self.updater.set_automatically_checks(enabled) {
                    self.settings_errors.updates = Some(error);
                }
            }
            Change::ImageName(naming) => {
                self.notes.library.workspace.image_name = naming;
                self.schedule_save(cx);
            }
            Change::CheckForUpdates => {
                self.settings_errors.updates = self.updater.check().err();
            }
            Change::DailyFolder => {
                self.settings_errors.daily = None;
                self.configure_daily_folder(window, cx);
            }
            Change::ResetDailyFolder => {
                self.settings_errors.daily = None;
                self.notes.library.workspace.daily.folder = PathBuf::new();
                self.schedule_save(cx);
            }
            Change::DailyFormat(format) => {
                self.notes.library.workspace.daily.format = format;
                self.schedule_save(cx);
            }
            Change::DailyTemplate(template) => {
                self.settings_errors.daily = None;
                self.notes.library.workspace.daily.template = template;
                self.schedule_save(cx);
            }
            Change::ChooseDailyTemplate => {
                self.settings_errors.daily = None;
                self.choose_daily_template(window, cx);
            }
            Change::SyncDailyFromObsidian => {
                if let Some(obsidian) = self.obsidian_daily.clone() {
                    self.settings_errors.daily = None;
                    self.notes.library.workspace.daily = obsidian;
                    self.schedule_save(cx);
                }
            }
        }
        cx.notify();
    }

    /// The window is gone: the shortcut it may have let go of comes back, and the
    /// foreground goes back to the app that had it if the note is not on screen.
    fn settings_closed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings_errors = SettingsErrors::default();
        if let Some(platform) = &mut self.platform {
            if let Err(error) = platform.resume_shortcuts() {
                self.feedback.set_platform_error(Some(error));
            }
            if !platform.is_visible(window) {
                platform.return_to_previous_app();
            }
        }
        cx.notify();
    }

    pub(in crate::app) fn set_settings_error_new_notes(&mut self, error: Option<Message>) {
        self.settings_errors.new_notes = error;
    }

    pub(in crate::app) fn set_settings_error_images(&mut self, error: Option<Message>) {
        self.settings_errors.images = error;
    }

    pub(in crate::app) fn set_settings_error_daily(&mut self, error: Option<Message>) {
        self.settings_errors.daily = error;
    }
}

/// The way back from the window to the app it configures.
#[derive(Clone)]
struct Link {
    app: WeakEntity<MarkraftApp>,
    main: AnyWindowHandle,
}

impl Link {
    /// Applied from a later turn, in the note window's context: the change may redraw
    /// the note, open a folder prompt or re-register the shortcut, none of which
    /// belongs inside this window's dispatch.
    fn send(&self, change: Change, cx: &mut App) {
        let link = self.clone();
        cx.defer(move |cx| {
            let _ = link.main.update(cx, |_, window, cx| {
                let _ = link
                    .app
                    .update(cx, |app, cx| app.apply_setting(change, window, cx));
            });
        });
    }

    /// Called while the window is still on screen: macOS lets only the active app hand
    /// the foreground on, and once its last window is gone Markraft is no longer that.
    fn closed(&self, cx: &mut App) {
        if cx.has_global::<OpenSettings>() {
            cx.remove_global::<OpenSettings>();
        }
        let _ = self.main.update(cx, |_, window, cx| {
            let _ = self
                .app
                .update(cx, |app, cx| app.settings_closed(window, cx));
        });
    }
}

/// Room between the date format field's outline and its text.
const DAILY_FORMAT_PAD_X: f32 = 8.;

/// The date format field: the note window's query field, on the Settings window's
/// ground.
fn daily_format_style(dark: bool) -> EditorStyle {
    let mut style = crate::app::query_style(dark);
    style.background = Palette::new(dark).field;
    style
}

/// The one Settings window there may be. A second request brings this one forward.
struct OpenSettings(WindowHandle<SettingsView>);

impl Global for OpenSettings {}

fn present(link: Link, near: Bounds<Pixels>, display: Option<DisplayId>, cx: &mut App) {
    let window = match cx.try_global::<OpenSettings>() {
        Some(open) => open.0,
        None => {
            let Some(window) = create(link, near, display, cx) else {
                return;
            };
            cx.set_global(OpenSettings(window));
            window
        }
    };
    // Key status is taken from a later turn: the window was opened unfocused because
    // `makeKeyAndOrderFront:` inside the dispatch that asked for it hangs.
    cx.spawn(async move |cx| {
        cx.update(|cx| {
            cx.activate(true);
            let _ = window.update(cx, |view, window, cx| {
                window.activate_window();
                window.focus(&view.toolbar, cx);
            });
        });
    })
    .detach();
}

fn create(
    link: Link,
    near: Bounds<Pixels>,
    display: Option<DisplayId>,
    cx: &mut App,
) -> Option<WindowHandle<SettingsView>> {
    let app = link.app.upgrade()?;
    let saved = app
        .read(cx)
        .preferences
        .settings_window
        .as_ref()
        .filter(|saved| saved.is_valid());
    let displays: Vec<_> = cx
        .displays()
        .iter()
        .map(SettingsDisplay::from_display)
        .collect();
    let fallback = display.or_else(|| cx.primary_display().map(|display| display.id()));
    let display = select_display(&displays, saved, fallback);
    let bounds = initial_bounds(display, saved, near);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        display_id: display.map(|display| display.id),
        titlebar: Some(TitlebarOptions {
            title: Some(app.read(cx).i18n.text("settings.title").into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(12.), px(9.))),
        }),
        focus: false,
        show: true,
        kind: WindowKind::Normal,
        is_movable: true,
        // A settings window keeps its width; its height follows the page.
        is_resizable: false,
        // ⌘, brings it back at once, so macOS settings windows are not minimized.
        is_minimizable: false,
        window_background: WindowBackgroundAppearance::Opaque,
        ..Default::default()
    };
    let opened = cx.open_window(options, |window, cx| {
        cx.new(|cx| SettingsView::new(app, link.clone(), window, cx))
    });
    match opened {
        Ok(handle) => {
            let _ = handle.update(cx, |_, window, cx| {
                // The traffic light: a close this code never sees otherwise.
                window.on_window_should_close(cx, move |_, cx| {
                    link.closed(cx);
                    true
                });
            });
            Some(handle)
        }
        Err(error) => {
            log::warn!("the Settings window could not be opened: {error}");
            None
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Page {
    General,
    Editor,
    Markdown,
    Files,
    About,
}

impl Page {
    const ALL: [Page; 5] = [
        Page::General,
        Page::Editor,
        Page::Markdown,
        Page::Files,
        Page::About,
    ];

    fn title(self, i18n: &I18n) -> String {
        i18n.text(match self {
            Page::General => "settings.page-general",
            Page::Editor => "settings.page-editor",
            Page::Files => "settings.page-files",
            Page::Markdown => "settings.page-markdown",
            Page::About => "settings.page-about",
        })
    }

    /// What the preferences remember the page by.
    fn key(self) -> &'static str {
        match self {
            Page::General => "general",
            Page::Editor => "editor",
            Page::Files => "files",
            Page::Markdown => "markdown",
            Page::About => "about",
        }
    }

    /// The page remembered as `key`, or the first one for a key no page has.
    fn remembered(key: &str) -> Page {
        Page::ALL
            .into_iter()
            .find(|page| page.key() == key)
            .unwrap_or(Page::General)
    }

    fn icon(self) -> Icon {
        match self {
            Page::General => Icon::Settings,
            Page::Editor => Icon::Typing,
            Page::Files => Icon::Open,
            Page::Markdown => Icon::Markdown,
            Page::About => Icon::About,
        }
    }

    fn step(self, forward: bool) -> Page {
        let index = Page::ALL.iter().position(|page| *page == self).unwrap_or(0);
        let next = if forward {
            (index + 1).min(Page::ALL.len() - 1)
        } else {
            index.saturating_sub(1)
        };
        Page::ALL[next]
    }
}

pub(in crate::app) struct SettingsView {
    link: Link,
    title: String,
    page: Page,
    /// The toolbar, which the window opens on, so ← and → move between pages.
    toolbar: FocusHandle,
    /// One field for each global shortcut, by [`Shortcut`].
    recorders: [FocusHandle; 3],
    /// The daily note date format as typed, which reaches the folder's settings only
    /// once it names every day on its own; until then it says why beneath it.
    daily_format: Entity<EditorView>,
    daily_format_problem: Option<crate::daily::FormatProblem>,
    /// The format the folder's settings held when the field last followed them, so a
    /// change made elsewhere — a sync from Obsidian — reaches the field, and one typed
    /// here does not come back to it.
    daily_format_seen: String,
    /// Whether the field was last styled dark.
    daily_format_dark: bool,
    /// The shortcut whose field is listening.
    recording: Option<Shortcut>,
    /// Why the chord just pressed cannot be a global shortcut. Said under the field,
    /// which stays open for another try.
    refusal: Option<&'static str>,
    /// Set between a press on the title band or toolbar and the first drag; the move
    /// blocks until the drag ends, so it must not start on the press itself.
    moving: bool,
    scroll: ScrollHandle,
    /// The page's own height as last laid out, and the window height last asked for,
    /// so the window follows the page without asking twice for the same height.
    page_height: Rc<Cell<f32>>,
    fitted: Rc<Cell<f32>>,
    /// Which pop-up button's menu is open, and the row lit in it.
    selects: select::Selects,
    /// Set while AppKit animates the window to a new height. GPUI draws each step of
    /// it, so a page chosen meanwhile is measured mid-animation; its fit waits for the
    /// frame drawn once this one has settled rather than starting a second animation
    /// inside the first.
    fitting: Rc<Cell<bool>>,
    /// A display change must refit even if the selected page did not change.
    display: Option<SettingsDisplay>,
    _subscriptions: Vec<Subscription>,
}

impl SettingsView {
    fn new(
        app: Entity<MarkraftApp>,
        link: Link,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let recorders = [
            cx.focus_handle().tab_stop(true),
            cx.focus_handle().tab_stop(true),
            cx.focus_handle().tab_stop(true),
        ];
        let (format, dark, i18n) = {
            let app = app.read(cx);
            (
                app.notes.library.workspace.daily.format.clone(),
                app.dark,
                app.i18n.clone(),
            )
        };
        let daily_format = cx.new(|cx| {
            let mut editor = EditorView::single_line(cx)
                .with_style(daily_format_style(dark))
                .with_messages(i18n.editor_messages());
            editor.set_value(&format, cx);
            editor.set_aria_label(i18n.text("settings.daily-format"), cx);
            editor
        });
        let own = window.window_handle();
        let this = cx.entity().downgrade();
        let mut subscriptions = vec![
            cx.observe(&app, |view, app, cx| {
                view.follow_daily_format(&app, cx);
                cx.notify();
            }),
            cx.subscribe(&daily_format, |view, editor, event: &EditorEvent, cx| {
                if matches!(event, EditorEvent::Changed { .. }) {
                    let text = editor.read(cx).text().trim().to_owned();
                    view.daily_format_changed(text, cx);
                }
            }),
            // Ahead of every binding, so a listening recorder hears ⌘W or Tab as part
            // of a chord rather than as the window's own command.
            cx.intercept_keystrokes(move |event, window, cx| {
                if window.window_handle() != own {
                    return;
                }
                let taken = this
                    .update(cx, |view, cx| view.intercept(&event.keystroke, cx))
                    .unwrap_or(false);
                if taken {
                    cx.stop_propagation();
                }
            }),
            cx.observe_window_bounds(window, |view, window, cx| {
                let display = window
                    .display(cx)
                    .as_ref()
                    .map(SettingsDisplay::from_display);
                if view.display != display {
                    view.display = display.clone();
                    view.fitted.set(0.);
                    cx.notify();
                }
                let bounds = window.bounds();
                view.link.send(
                    Change::WindowPlacement(SettingsWindowPlacement {
                        origin: [bounds.origin.x.into(), bounds.origin.y.into()],
                        height: bounds.size.height.into(),
                        display_uuid: display.and_then(|display| display.uuid),
                    }),
                    cx,
                );
            }),
            cx.observe_window_activation(window, |view, window, cx| {
                if window.is_window_active() {
                    view.link.send(Change::Refresh, cx);
                } else {
                    view.stop_recording(cx);
                }
            }),
            // However the view goes, a shortcut it let go of comes back.
            cx.on_release(|view, cx| {
                if view.recording.is_some() {
                    view.link.send(Change::Recording(false), cx);
                }
            }),
        ];
        for recorder in &recorders {
            subscriptions
                .push(cx.on_focus_out(recorder, window, |view, _, _, cx| view.stop_recording(cx)));
        }
        // The window opens on the page it was last closed on, as macOS settings do.
        let page = Page::remembered(&app.read(cx).preferences.settings_page);
        Self {
            title: app.read(cx).i18n.text("settings.title"),
            link,
            page,
            toolbar: cx.focus_handle().tab_stop(true),
            recorders,
            daily_format,
            daily_format_problem: None,
            daily_format_seen: format,
            daily_format_dark: dark,
            recording: None,
            refusal: None,
            moving: false,
            scroll: ScrollHandle::new(),
            page_height: Rc::default(),
            fitted: Rc::default(),
            selects: select::Selects::new(cx),
            fitting: Rc::default(),
            display: window
                .display(cx)
                .as_ref()
                .map(SettingsDisplay::from_display),
            _subscriptions: subscriptions,
        }
    }

    /// Take a daily note format the folder's settings changed to elsewhere, and the
    /// theme, into the field.
    fn follow_daily_format(&mut self, app: &Entity<MarkraftApp>, cx: &mut Context<Self>) {
        let (format, dark) = {
            let app = app.read(cx);
            (app.notes.library.workspace.daily.format.clone(), app.dark)
        };
        if dark != self.daily_format_dark {
            self.daily_format_dark = dark;
            self.daily_format.update(cx, |editor, cx| {
                editor.set_style(daily_format_style(dark), cx)
            });
        }
        if format == self.daily_format_seen {
            return;
        }
        self.daily_format_seen = format.clone();
        if self.daily_format.read(cx).text().trim() != format {
            self.daily_format_problem = None;
            self.daily_format
                .update(cx, |editor, cx| editor.set_value(&format, cx));
        }
    }

    /// A format typed into the field goes to the folder's settings once it gives every
    /// day a name of its own; until then the field says why not.
    fn daily_format_changed(&mut self, text: String, cx: &mut Context<Self>) {
        let problem = crate::daily::validate_format(&text, super::daily_notes::date_locale()).err();
        if problem != self.daily_format_problem {
            self.daily_format_problem = problem;
            cx.notify();
        }
        if problem.is_none() && text != self.daily_format_seen {
            self.daily_format_seen = text.clone();
            self.link.send(Change::DailyFormat(text), cx);
        }
    }

    /// A listener for a control: the change it makes, from the value it reports.
    fn sender<T: 'static>(
        &self,
        change: impl Fn(T) -> Change + 'static,
    ) -> impl Fn(T, &mut Window, &mut App) + 'static {
        let link = self.link.clone();
        move |value, _, cx| link.send(change(value), cx)
    }

    /// A button's listener: the one change it makes.
    fn on_click(&self, change: Change) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
        let link = self.link.clone();
        move |_, _, cx| link.send(change.clone(), cx)
    }

    fn set_page(&mut self, page: Page, cx: &mut Context<Self>) {
        if self.page != page {
            self.stop_recording(cx);
            self.selects.close();
            self.page = page;
            self.scroll.set_offset(point(px(0.), px(0.)));
            self.link
                .send(Change::Pref(Pref::SettingsPage(page.key().to_owned())), cx);
            cx.notify();
        }
    }

    /// One keystroke while a recorder listens; false leaves it to the window.
    fn intercept(&mut self, keystroke: &Keystroke, cx: &mut Context<Self>) -> bool {
        let Some(which) = self.recording else {
            return false;
        };
        match shortcut::record(keystroke) {
            Recorded::Waiting => {}
            Recorded::Cancelled => self.stop_recording(cx),
            Recorded::Cleared => self.commit(which, String::new(), cx),
            Recorded::Bound(chord) => self.commit(which, chord, cx),
            Recorded::Refused(reason) => self.refusal = Some(reason),
        }
        cx.notify();
        true
    }

    fn start_recording(&mut self, which: Shortcut, cx: &mut Context<Self>) {
        if self.recording == Some(which) {
            return;
        }
        let already = self.recording.replace(which).is_some();
        self.refusal = None;
        if !already {
            self.link.send(Change::Recording(true), cx);
        }
        cx.notify();
    }

    fn stop_recording(&mut self, cx: &mut Context<Self>) {
        if self.recording.take().is_some() {
            self.refusal = None;
            self.link.send(Change::Recording(false), cx);
            cx.notify();
        }
    }

    /// Registering the chord is also what takes the suspended shortcuts back.
    fn commit(&mut self, which: Shortcut, shortcut: String, cx: &mut Context<Self>) {
        self.recording = None;
        self.refusal = None;
        self.link
            .send(Change::Pref(shortcut_pref(which, shortcut)), cx);
    }

    /// Escape and ⌘W. Removed from a later turn: this runs inside the dispatch of the
    /// keystroke, into the window being removed.
    fn close(&mut self, _: &CloseSettings, window: &mut Window, cx: &mut Context<Self>) {
        self.stop_recording(cx);
        let handle = window.window_handle();
        let link = self.link.clone();
        cx.spawn(async move |_, cx| {
            cx.update(|cx| {
                link.closed(cx);
                let _ = handle.update(cx, |_, window, _| window.remove_window());
            });
        })
        .detach();
    }

    /// The title band and the row of pages under it, drawn as one surface that moves
    /// the window when dragged, the way an AppKit toolbar does.
    fn header(&self, i18n: &I18n, p: Palette, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("settings-header")
            .flex()
            .flex_col()
            .items_center()
            .flex_shrink_0()
            .pb(px(TOOLBAR_PAD_BOTTOM))
            .bg(p.toolbar)
            .border_b_1()
            .border_color(p.border)
            .on_click(|event, window, _| {
                if event.click_count() == 2 {
                    window.titlebar_double_click();
                }
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|view, _, _, _| view.moving = true),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|view, _, _, _| view.moving = false),
            )
            .on_mouse_move(cx.listener(|view, _, window, _| {
                if std::mem::take(&mut view.moving) {
                    window.start_window_move();
                }
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .w_full()
                    .h(px(TITLE_HEIGHT))
                    .text_size(px(TITLE_SIZE))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(self.page.title(i18n)),
            )
            .child(self.toolbar(i18n, p, cx))
    }

    fn toolbar(&self, i18n: &I18n, p: Palette, cx: &mut Context<Self>) -> impl IntoElement {
        Tabs::new("settings-pages")
            .key_context(TOOLBAR_CONTEXT)
            .track_focus(&self.toolbar)
            .aria_label(i18n.text("settings.pages"))
            .on_action(
                cx.listener(|view, _: &PreviousPage, _, cx| {
                    view.set_page(view.page.step(false), cx)
                }),
            )
            .on_action(
                cx.listener(|view, _: &NextPage, _, cx| view.set_page(view.page.step(true), cx)),
            )
            .flex()
            .gap(px(TOOLBAR_GAP))
            .children(Page::ALL.into_iter().enumerate().map(|(index, page)| {
                let selected = page == self.page;
                Tab::new(SharedString::from(format!("settings-page-{index}")))
                    .selected(selected)
                    .set_position(index + 1, Page::ALL.len())
                    .accessibility_label(page.title(i18n))
                    .flex_col()
                    .gap(px(TOOLBAR_LABEL_GAP))
                    .min_w(px(TOOLBAR_ITEM_MIN_WIDTH))
                    .px(px(TOOLBAR_ITEM_PAD_X))
                    .py(px(TOOLBAR_ITEM_PAD_Y))
                    .rounded(px(TOOLBAR_ITEM_RADIUS))
                    .text_size(px(TOOLBAR_LABEL_SIZE))
                    // The chosen page's tab is marked by its ground alone: the system raises
                    // it on glass, which a flat window has nothing to draw with.
                    .text_color(if selected { p.text } else { p.subtitle })
                    .when(selected, |item| item.bg(p.selected))
                    .when(!selected, |item| {
                        item.hover(move |style| style.bg(p.hover).text_color(p.text))
                    })
                    .on_click(cx.listener(move |view, _, window, cx| {
                        window.focus(&view.toolbar, cx);
                        view.set_page(page, cx);
                    }))
                    .child(sized_icon(
                        page.icon(),
                        if selected { p.accent } else { p.subtitle },
                        TOOLBAR_ICON,
                    ))
                    .child(page.title(i18n))
            }))
    }

    /// The field for one global shortcut: its chord, or the recorder listening for one.
    /// A click or Space starts it; the next chord pressed replaces the shortcut.
    fn shortcut_field(
        &self,
        which: Shortcut,
        chord: &str,
        i18n: &I18n,
        p: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let bound = !chord.trim().is_empty();
        let face = if self.recording == Some(which) {
            ChordFace::Recording(i18n.text("settings.press-shortcut"))
        } else if bound {
            ChordFace::Bound(shortcut::glyphs(chord))
        } else {
            ChordFace::Unbound(i18n.text("settings.record-shortcut"))
        };
        let name = match which {
            Shortcut::Toggle => i18n.text("settings.toggle-shortcut"),
            Shortcut::NewNote => i18n.text("settings.new-note-shortcut"),
            Shortcut::DailyNote => i18n.text("settings.daily-note-shortcut"),
        };
        let clear = (bound && self.recording != Some(which)).then(|| {
            div()
                .id(SharedString::from(format!(
                    "clear-shortcut-{}",
                    which as usize
                )))
                .role(Role::Button)
                .aria_label(i18n.text(match which {
                    Shortcut::Toggle => "settings.clear-toggle-shortcut",
                    Shortcut::NewNote => "settings.clear-new-note-shortcut",
                    Shortcut::DailyNote => "settings.clear-daily-note-shortcut",
                }))
                .flex_shrink_0()
                .cursor_pointer()
                // Its own click, not the field's: clearing is not recording.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(self.on_click(Change::Pref(shortcut_pref(which, String::new()))))
                .child(sized_icon(Icon::ClearField, p.subtitle, RECORDER_CLEAR))
                .into_any_element()
        });
        div()
            .id(SharedString::from(format!("shortcut-{}", which as usize)))
            .track_focus(&self.recorders[which as usize])
            .role(Role::Button)
            .aria_label(if bound {
                i18n.text_with(
                    "settings.bound-shortcut",
                    &[("name", &name), ("chord", chord)],
                )
            } else {
                i18n.text_with("settings.unbound-shortcut", &[("name", &name)])
            })
            .cursor_pointer()
            .on_click(cx.listener(move |view, _, window, cx| {
                window.focus(&view.recorders[which as usize], cx);
                view.start_recording(which, cx);
            }))
            .child(chord_face(face, clear, p))
            .into_any_element()
    }

    /// The lines under a shortcut field: while it listens, how to answer it; after a
    /// refusal, why.
    fn shortcut_notes(&self, which: Shortcut, s: &Snapshot, p: Palette) -> Vec<AnyElement> {
        let mut notes = Vec::new();
        if self.recording == Some(which) {
            if let Some(refusal) = self.refusal {
                notes.push(error(s.i18n.text(refusal), p));
            }
        } else if let Some(refused) = &s.errors.shortcuts[which as usize] {
            notes.push(error(refused.render(&s.i18n), p));
        }
        notes
    }

    fn general(&self, s: &Snapshot, p: Palette, cx: &mut Context<Self>) -> Vec<Div> {
        let languages = language_options(&s.i18n, &s.preferences.language);
        let language = self.select(
            "language",
            s.i18n.text("settings.language"),
            &languages,
            s.preferences.language.clone(),
            |value| Change::Pref(Pref::Language(value)),
            p,
            cx,
        );
        let theme = segmented(
            "appearance",
            s.i18n.text("settings.appearance"),
            &[
                (s.i18n.text("settings.auto"), None),
                (s.i18n.text("settings.light"), Some(false)),
                (s.i18n.text("settings.dark"), Some(true)),
            ],
            s.preferences.dark_mode,
            p,
            self.sender(|value| Change::Pref(Pref::Theme(value))),
        );

        let (toggle, new_note) = (&s.preferences.hotkey, &s.preferences.new_note_hotkey);
        let summon = self.select(
            "summon",
            s.i18n.text("settings.show-on-open"),
            &[
                (s.i18n.text("settings.last-note"), Summon::LastNote),
                (s.i18n.text("settings.new-note-option"), Summon::NewNote),
                (s.i18n.text("settings.daily-note-option"), Summon::DailyNote),
            ],
            s.preferences.summon,
            |value| Change::Pref(Pref::Summon(value)),
            p,
            cx,
        );
        let mut toggle_lines = vec![line(vec![self.shortcut_field(
            Shortcut::Toggle,
            toggle,
            &s.i18n,
            p,
            cx,
        )])];
        toggle_lines.extend(self.shortcut_notes(Shortcut::Toggle, s, p));
        let mut new_note_lines = vec![line(vec![self.shortcut_field(
            Shortcut::NewNote,
            new_note,
            &s.i18n,
            p,
            cx,
        )])];
        new_note_lines.extend(self.shortcut_notes(Shortcut::NewNote, s, p));
        let mut daily_note_lines = vec![line(vec![self.shortcut_field(
            Shortcut::DailyNote,
            &s.preferences.daily_note_hotkey,
            &s.i18n,
            p,
            cx,
        )])];
        daily_note_lines.extend(self.shortcut_notes(Shortcut::DailyNote, s, p));

        let mut startup = vec![
            checkbox(
                "launch-at-login",
                s.i18n.text("settings.launch-at-login"),
                s.login.unwrap_or(false),
                s.login.is_none(),
                p,
                self.sender(Change::LaunchAtLogin),
            )
            .into_any_element(),
        ];
        if let Some(refused) = &s.errors.login {
            startup.push(error(refused.render(&s.i18n), p));
        }

        // Launching at login comes first: for an app that lives in the menu bar it is
        // what decides whether it is there at all.
        vec![
            row(Some(s.i18n.text("settings.startup")), startup, p),
            row(
                Some(s.i18n.text("settings.language")),
                vec![line(vec![language])],
                p,
            ),
            group_gap(),
            row(Some(s.i18n.text("settings.show-and-hide")), toggle_lines, p),
            row(Some(s.i18n.text("settings.new-note")), new_note_lines, p),
            row(
                Some(s.i18n.text("settings.daily-note")),
                daily_note_lines,
                p,
            ),
            group_gap(),
            row(
                Some(s.i18n.text("settings.note-window")),
                vec![
                    checkbox(
                        "always-on-top",
                        s.i18n.text("settings.always-on-top"),
                        s.preferences.always_on_top,
                        false,
                        p,
                        self.sender(|value| Change::Pref(Pref::AlwaysOnTop(value))),
                    )
                    .into_any_element(),
                    checkbox(
                        "hide-on-deactivate",
                        s.i18n.text("settings.hide-on-deactivate"),
                        s.preferences.hide_on_deactivate,
                        false,
                        p,
                        self.sender(|value| Change::Pref(Pref::HideOnDeactivate(value))),
                    )
                    .into_any_element(),
                    checkbox(
                        "auto-height",
                        s.i18n.text("settings.auto-height"),
                        s.preferences.auto_height,
                        false,
                        p,
                        self.sender(|value| Change::Pref(Pref::AutoHeight(value))),
                    )
                    .into_any_element(),
                    checkbox(
                        "show-word-count",
                        s.i18n.text("settings.show-word-count"),
                        s.preferences.show_word_count,
                        false,
                        p,
                        self.sender(|value| Change::Pref(Pref::ShowWordCount(value))),
                    )
                    .into_any_element(),
                    checkbox(
                        "all-spaces",
                        s.i18n.text("settings.all-spaces"),
                        s.preferences.all_spaces,
                        false,
                        p,
                        self.sender(|value| Change::Pref(Pref::AllSpaces(value))),
                    )
                    .into_any_element(),
                    checkbox(
                        "follow-pointer",
                        s.i18n.text("settings.follow-pointer"),
                        s.preferences.follow_pointer,
                        false,
                        p,
                        self.sender(|value| Change::Pref(Pref::FollowPointer(value))),
                    )
                    .into_any_element(),
                ],
                p,
            ),
            row(
                Some(s.i18n.text("settings.show-on-open")),
                vec![line(vec![summon])],
                p,
            ),
            group_gap(),
            row(
                Some(s.i18n.text("settings.appearance")),
                vec![line(vec![theme.into_any_element()])],
                p,
            ),
        ]
    }

    fn editor(&self, s: &Snapshot, p: Palette, cx: &mut Context<Self>) -> Vec<Div> {
        let range = Preferences::TEXT_SIZES;
        let size = s.preferences.text_size;
        let link = self.link.clone();
        let mut size_line = vec![
            stepper(
                "text-size",
                s.i18n.text_with(
                    "settings.text-size-value",
                    &[("size", &format!("{size:.0}"))],
                ),
                (size > *range.start(), size < *range.end()),
                (
                    s.i18n.text("settings.smaller"),
                    s.i18n.text("settings.larger"),
                ),
                p,
                move |delta, _, cx| {
                    link.send(Change::Pref(Pref::TextSize(size + delta as f32)), cx)
                },
            )
            .into_any_element(),
        ];
        if size != Preferences::DEFAULT_TEXT_SIZE {
            size_line.push(
                button(
                    "reset-text-size",
                    s.i18n.text("settings.default"),
                    p,
                    self.on_click(Change::Pref(Pref::TextSize(Preferences::DEFAULT_TEXT_SIZE))),
                )
                .into_any_element(),
            );
        }
        let font = self.select(
            "font",
            s.i18n.text("settings.font"),
            &[
                (s.i18n.text("settings.font-system"), EditorFont::System),
                (s.i18n.text("settings.font-serif"), EditorFont::Serif),
                (s.i18n.text("settings.font-rounded"), EditorFont::Rounded),
                (s.i18n.text("settings.font-mono"), EditorFont::Mono),
            ],
            s.preferences.font,
            |value| Change::Pref(Pref::Font(value)),
            p,
            cx,
        );
        let line_height = self.select(
            "line-height",
            s.i18n.text("settings.line-height"),
            &[
                (s.i18n.text("settings.line-tight"), LineHeight::Tight),
                (s.i18n.text("settings.line-normal"), LineHeight::Normal),
                (s.i18n.text("settings.line-relaxed"), LineHeight::Relaxed),
            ],
            s.preferences.line_height,
            |value| Change::Pref(Pref::LineHeight(value)),
            p,
            cx,
        );
        let line_width = self.select(
            "line-width",
            s.i18n.text("settings.line-width"),
            &[
                (s.i18n.text("settings.line-narrow"), LineWidth::Narrow),
                (s.i18n.text("settings.line-normal"), LineWidth::Normal),
                (s.i18n.text("settings.line-full"), LineWidth::Full),
            ],
            s.preferences.line_width,
            |value| Change::Pref(Pref::LineWidth(value)),
            p,
            cx,
        );
        let tab_key = self.select(
            "tab-key",
            s.i18n.text("settings.tab-key"),
            &[
                (s.i18n.text("settings.tab"), TabKey::Tab),
                (s.i18n.text("settings.two-spaces"), TabKey::TwoSpaces),
                (s.i18n.text("settings.four-spaces"), TabKey::FourSpaces),
            ],
            s.preferences.tab_key,
            |value| Change::Pref(Pref::TabKey(value)),
            p,
            cx,
        );
        vec![
            row(
                Some(s.i18n.text("settings.text-size")),
                vec![line(size_line)],
                p,
            ),
            row(
                Some(s.i18n.text("settings.font")),
                vec![line(vec![font])],
                p,
            ),
            row(
                Some(s.i18n.text("settings.line-height")),
                vec![line(vec![line_height])],
                p,
            ),
            row(
                Some(s.i18n.text("settings.line-width")),
                vec![line(vec![line_width])],
                p,
            ),
            group_gap(),
            row(
                Some(s.i18n.text("settings.editing")),
                vec![
                    checkbox(
                        "markdown-shortcuts",
                        s.i18n.text("settings.markdown-shortcuts"),
                        s.preferences.markdown_shortcuts,
                        false,
                        p,
                        self.sender(|value| Change::Pref(Pref::MarkdownShortcuts(value))),
                    )
                    .into_any_element(),
                    checkbox(
                        "auto-pair",
                        s.i18n.text("settings.auto-pair"),
                        s.preferences.auto_pair,
                        false,
                        p,
                        self.sender(|value| Change::Pref(Pref::AutoPair(value))),
                    )
                    .into_any_element(),
                    checkbox(
                        "vim-mode",
                        s.i18n.text("settings.vim-mode"),
                        s.preferences.vim_mode,
                        false,
                        p,
                        self.sender(|value| Change::Pref(Pref::VimMode(value))),
                    )
                    .into_any_element(),
                ],
                p,
            ),
            row(
                Some(s.i18n.text("settings.tab-in-code")),
                vec![line(vec![tab_key])],
                p,
            ),
            group_gap(),
            row(
                Some(s.i18n.text("settings.images")),
                vec![
                    checkbox(
                        "remote-images",
                        s.i18n.text("settings.remote-images"),
                        s.preferences.remote_images,
                        false,
                        p,
                        self.sender(|value| Change::Pref(Pref::RemoteImages(value))),
                    )
                    .into_any_element(),
                    checkbox(
                        "animate-images",
                        s.i18n.text("settings.animate-images"),
                        s.preferences.animate_images,
                        false,
                        p,
                        self.sender(|value| Change::Pref(Pref::AnimateImages(value))),
                    )
                    .into_any_element(),
                ],
                p,
            ),
        ]
    }

    /// Where the notes are and where new files go, each a folder pop-up: the folder on
    /// the button, and what can be done with it — show it, choose another, go back to
    /// the default — in its menu.
    fn files(&self, s: &Snapshot, p: Palette, cx: &mut Context<Self>) -> Vec<Div> {
        let Some(root) = &s.folder else {
            let choose = self.pop_up(
                "notes-folder",
                s.i18n.text("settings.notes-folder"),
                s.i18n.text("settings.none").into(),
                vec![MenuItem::action(
                    s.i18n.text("settings.choose-folder"),
                    Change::ChooseFolder,
                )],
                p,
                cx,
            );
            return vec![row(
                Some(s.i18n.text("settings.notes-folder")),
                vec![line(vec![choose])],
                p,
            )];
        };
        let root_name = root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| root.display().to_string());
        let notes_folder = self.pop_up(
            "notes-folder",
            s.i18n.text("settings.notes-folder"),
            root_name.clone().into(),
            vec![
                MenuItem::choice(root_name.clone(), None, true),
                MenuRow::Separator,
                MenuItem::action(s.i18n.text("settings.show-in-finder"), Change::RevealFolder),
                MenuItem::action(s.i18n.text("settings.choose-folder"), Change::ChooseFolder),
            ],
            p,
            cx,
        );
        let mut page = vec![row(
            Some(s.i18n.text("settings.notes-folder")),
            vec![line(vec![notes_folder])],
            p,
        )];

        // A location inside the notes folder: the default, or the folder chosen instead,
        // which the button names by its last component.
        let location = |id: &'static str,
                        label: String,
                        default: String,
                        (place, custom): &(String, bool),
                        reset: Change,
                        choose: Change,
                        cx: &mut Context<Self>| {
            let face = if *custom {
                place.rsplit('/').next().unwrap_or(place).to_owned()
            } else {
                default.clone()
            };
            let mut rows = vec![MenuItem::choice(default, custom.then_some(reset), !custom)];
            if *custom {
                rows.push(MenuItem::choice(place.clone(), None, true));
            }
            rows.push(MenuRow::Separator);
            rows.push(MenuItem::action(
                s.i18n.text("settings.choose-folder"),
                choose,
            ));
            self.pop_up(id, label, face.into(), rows, p, cx)
        };

        if let (Some(new_notes), Some(images)) = (&s.new_notes, &s.images) {
            page.push(group_gap());
            let mut new_note_lines = vec![line(vec![location(
                "new-note-folder",
                s.i18n.text("settings.save-new-notes"),
                root_name.clone(),
                new_notes,
                Change::ResetNewNoteLocation,
                Change::NewNoteLocation,
                cx,
            )])];
            if let Some(refused) = &s.errors.new_notes {
                new_note_lines.push(error(refused.render(&s.i18n), p));
            }
            page.push(row(
                Some(s.i18n.text("settings.save-new-notes")),
                new_note_lines,
                p,
            ));
            let naming = self.select(
                "new-note-name",
                s.i18n.text("settings.name-new-notes"),
                &[
                    (s.i18n.text("settings.first-line"), NoteNaming::FirstLine),
                    (s.i18n.text("settings.date-time"), NoteNaming::DateTime),
                ],
                s.new_note_name,
                Change::NewNoteName,
                p,
                cx,
            );
            page.push(row(
                Some(s.i18n.text("settings.name-new-notes")),
                vec![line(vec![naming])],
                p,
            ));

            page.push(group_gap());
            let mut image_lines = vec![line(vec![location(
                "image-folder",
                s.i18n.text("settings.save-images"),
                s.i18n.text("settings.beside-each-note"),
                images,
                Change::ResetImageLocation,
                Change::ImageLocation,
                cx,
            )])];
            if let Some(refused) = &s.errors.images {
                image_lines.push(error(refused.render(&s.i18n), p));
            }
            page.push(row(
                Some(s.i18n.text("settings.save-images")),
                image_lines,
                p,
            ));
            let image_name = self.select(
                "image-name",
                s.i18n.text("settings.name-images"),
                &[
                    (s.i18n.text("settings.random-id"), ImageNaming::RandomId),
                    (
                        s.i18n.text("settings.note-name-date"),
                        ImageNaming::NoteAndDate,
                    ),
                ],
                s.image_name,
                Change::ImageName,
                p,
                cx,
            );
            page.push(row(
                Some(s.i18n.text("settings.name-images")),
                vec![line(vec![image_name])],
                p,
            ));

            if let Some(daily_folder) = &s.daily_folder {
                page.push(group_gap());
                page.extend(self.daily_rows(
                    location(
                        "daily-folder",
                        s.i18n.text("settings.save-daily-notes"),
                        root_name.clone(),
                        daily_folder,
                        Change::ResetDailyFolder,
                        Change::DailyFolder,
                        cx,
                    ),
                    s,
                    p,
                    cx,
                ));
            }

            page.push(group_gap());
            page.push(row(
                Some(s.i18n.text("settings.deleting")),
                vec![
                    checkbox(
                        "confirm-delete",
                        s.i18n.text("settings.confirm-delete"),
                        s.preferences.confirm_delete,
                        false,
                        p,
                        self.sender(|value| Change::Pref(Pref::ConfirmDelete(value))),
                    )
                    .into_any_element(),
                ],
                p,
            ));
        }
        page
    }

    /// Where daily notes go, what they are called and what they start as, with the
    /// way to take Obsidian's when this folder is also a vault that keeps them
    /// otherwise.
    fn daily_rows(
        &self,
        folder: AnyElement,
        s: &Snapshot,
        p: Palette,
        cx: &mut Context<Self>,
    ) -> Vec<Div> {
        let mut folder_lines = vec![line(vec![folder])];
        if let Some(refused) = &s.errors.daily {
            folder_lines.push(error(refused.render(&s.i18n), p));
        }

        let field = div()
            .id("daily-format")
            .flex()
            .items_center()
            .flex_shrink_0()
            .w(px(SELECT_WIDTH))
            .h(px(SELECT_HEIGHT))
            .px(px(DAILY_FORMAT_PAD_X))
            .rounded(px(SELECT_RADIUS))
            .bg(p.field)
            .border_1()
            .border_color(p.field_border)
            .overflow_hidden()
            .child(div().flex_1().min_w_0().child(self.daily_format.clone()))
            .into_any_element();
        let mut format_lines = vec![line(vec![field])];
        let typed = self.daily_format.read(cx).text().trim().to_owned();
        format_lines.push(match self.daily_format_problem {
            Some(problem) => error(s.i18n.text(problem.message_key()), p),
            None => {
                let preview = crate::daily::DailySettings {
                    format: typed,
                    ..s.daily.clone()
                }
                .path_for(
                    super::daily_notes::today(),
                    super::daily_notes::date_locale(),
                );
                div()
                    .text_size(px(HELP_SIZE))
                    .text_color(p.subtitle)
                    .child(s.i18n.text_with(
                        "settings.daily-format-preview",
                        &[("path", &preview.display().to_string())],
                    ))
                    .into_any_element()
            }
        });

        let none = s.i18n.text("settings.none");
        let mut template_rows = vec![MenuItem::choice(
            none.clone(),
            s.daily
                .template
                .is_some()
                .then_some(Change::DailyTemplate(None)),
            s.daily.template.is_none(),
        )];
        let face = match &s.daily.template {
            Some(template) => {
                let name = template
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
                    .unwrap_or_else(|| template.display().to_string());
                template_rows.push(MenuItem::choice(name.clone(), None, true));
                name
            }
            None => none,
        };
        template_rows.push(MenuRow::Separator);
        template_rows.push(MenuItem::action(
            s.i18n.text("settings.choose-file"),
            Change::ChooseDailyTemplate,
        ));
        let template = self.pop_up(
            "daily-template",
            s.i18n.text("settings.daily-template"),
            face.into(),
            template_rows,
            p,
            cx,
        );

        let mut rows = vec![
            row(
                Some(s.i18n.text("settings.save-daily-notes")),
                folder_lines,
                p,
            ),
            row(Some(s.i18n.text("settings.daily-format")), format_lines, p),
            row(
                Some(s.i18n.text("settings.daily-template")),
                vec![line(vec![template])],
                p,
            ),
        ];
        if s.obsidian_differs {
            rows.push(row(
                None,
                vec![line(vec![
                    button(
                        "sync-obsidian",
                        s.i18n.text("settings.sync-obsidian"),
                        p,
                        // The button goes once the settings match, and the keyboard
                        // would go with it; it returns to the pages, where the window
                        // opened, so Escape and ⌘W still close it.
                        cx.listener(|view, _: &ClickEvent, window, cx| {
                            window.focus(&view.toolbar, cx);
                            view.link.send(Change::SyncDailyFromObsidian, cx);
                        }),
                    )
                    .into_any_element(),
                ])],
                p,
            ));
        }
        rows
    }

    fn markdown(&self, s: &Snapshot, p: Palette, cx: &mut Context<Self>) -> Vec<Div> {
        let bullet = self.select(
            "bullet-marker",
            s.i18n.text("settings.bullet-list"),
            &[
                (s.i18n.text("settings.bullet-dash"), BulletMarker::Dash),
                (s.i18n.text("settings.bullet-star"), BulletMarker::Star),
                (s.i18n.text("settings.bullet-plus"), BulletMarker::Plus),
            ],
            s.preferences.bullet_marker,
            |value| Change::Pref(Pref::Bullet(value)),
            p,
            cx,
        );
        let fence = self.select(
            "code-fence",
            s.i18n.text("settings.code-block"),
            &[
                ("```".to_owned(), CodeFence::Backticks),
                ("~~~".to_owned(), CodeFence::Tildes),
            ],
            s.preferences.code_fence,
            |value| Change::Pref(Pref::Fence(value)),
            p,
            cx,
        );
        let ordered = self.select(
            "ordered-delimiter",
            s.i18n.text("settings.numbered-list"),
            &[
                (
                    s.i18n.text("settings.ordered-period"),
                    OrderedDelimiter::Period,
                ),
                (
                    s.i18n.text("settings.ordered-parenthesis"),
                    OrderedDelimiter::Parenthesis,
                ),
            ],
            s.preferences.ordered_delimiter,
            |value| Change::Pref(Pref::OrderedDelimiter(value)),
            p,
            cx,
        );
        let hard_break = self.select(
            "hard-break",
            s.i18n.text("settings.line-break"),
            &[
                (s.i18n.text("settings.backslash"), HardBreakStyle::Backslash),
                (s.i18n.text("settings.spaces"), HardBreakStyle::Spaces),
            ],
            s.preferences.hard_break,
            |value| Change::Pref(Pref::HardBreak(value)),
            p,
            cx,
        );
        let emphasis = self.select(
            "emphasis-marker",
            s.i18n.text("settings.emphasis"),
            &[
                (s.i18n.text("settings.emphasis-star"), EmphasisMarker::Star),
                (
                    s.i18n.text("settings.emphasis-underscore"),
                    EmphasisMarker::Underscore,
                ),
            ],
            s.preferences.emphasis_marker,
            |value| Change::Pref(Pref::Emphasis(value)),
            p,
            cx,
        );
        vec![
            row(
                Some(s.i18n.text("settings.bullet-list")),
                vec![line(vec![bullet])],
                p,
            ),
            row(
                Some(s.i18n.text("settings.numbered-list")),
                vec![line(vec![ordered])],
                p,
            ),
            row(
                Some(s.i18n.text("settings.code-block")),
                vec![line(vec![fence])],
                p,
            ),
            row(
                Some(s.i18n.text("settings.emphasis")),
                vec![line(vec![emphasis])],
                p,
            ),
            row(
                Some(s.i18n.text("settings.line-break")),
                vec![line(vec![hard_break])],
                p,
            ),
            group_gap(),
            row(
                Some(s.i18n.text("settings.math")),
                vec![
                    checkbox(
                        "auto-number-equations",
                        s.i18n.text("settings.auto-number-equations"),
                        s.preferences.auto_number_equations,
                        false,
                        p,
                        self.sender(|value| Change::Pref(Pref::AutoNumberEquations(value))),
                    )
                    .into_any_element(),
                    div()
                        .text_size(px(HELP_SIZE))
                        .text_color(p.subtitle)
                        .child(s.i18n.text("settings.math-help"))
                        .into_any_element(),
                ],
                p,
            ),
            group_gap(),
            row(
                Some(s.i18n.text("settings.emoji")),
                vec![
                    checkbox(
                        "emoji-characters",
                        s.i18n.text("settings.emoji-characters"),
                        s.preferences.emoji_characters,
                        false,
                        p,
                        self.sender(|value| Change::Pref(Pref::EmojiCharacters(value))),
                    )
                    .into_any_element(),
                ],
                p,
            ),
        ]
    }

    fn about(&self, s: &Snapshot, p: Palette) -> Vec<Div> {
        let (version, build) = crate::platform::app_version();
        let version = match build {
            Some(build) => s.i18n.text_with(
                "settings.version-build",
                &[("version", &version), ("build", &build)],
            ),
            None => s
                .i18n
                .text_with("settings.version", &[("version", &version)]),
        };
        let identity = div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(4.))
            // As much room above the icon as there is under the version, so the
            // identity sits centred between the toolbar and the rows.
            .pt(px(ABOUT_TOP))
            .pb(px(6.))
            .child(img(app_icon()).size(px(ABOUT_ICON)))
            .child(
                div()
                    .text_size(px(ABOUT_NAME_SIZE))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Markraft"),
            )
            .child(
                div()
                    .text_size(px(HELP_SIZE))
                    .text_color(p.subtitle)
                    .child(version),
            );

        let updates = {
            let mut updates = vec![
                checkbox(
                    "automatic-updates",
                    s.i18n.text("settings.automatic-updates"),
                    s.automatic_updates.unwrap_or(false),
                    s.automatic_updates.is_none(),
                    p,
                    self.sender(Change::AutomaticUpdates),
                )
                .into_any_element(),
            ];
            // A copy that cannot update itself shows both controls dimmed, without saying
            // why: only the builds from GitHub carry an update feed.
            let available = s.automatic_updates.is_some();
            updates.push(line(vec![
                button(
                    "check-for-updates",
                    s.i18n.text("settings.check-updates"),
                    p,
                    self.on_click(Change::CheckForUpdates),
                )
                .disabled(!available)
                .when(!available, |button| button.opacity(0.45).cursor_default())
                .into_any_element(),
            ]));
            if let Some(refused) = &s.errors.updates {
                updates.push(error(refused.render(&s.i18n), p));
            }

            updates
        };

        let open = |url: &'static str| {
            move |_: &ClickEvent, _: &mut Window, cx: &mut App| cx.open_url(url)
        };
        let links = vec![
            button("github", "GitHub", p, open(REPOSITORY)).into_any_element(),
            button(
                "release-notes",
                s.i18n.text("settings.release-notes"),
                p,
                open(RELEASES),
            )
            .into_any_element(),
            button(
                "report-issue",
                s.i18n.text("settings.report-issue"),
                p,
                open(NEW_ISSUE),
            )
            .into_any_element(),
        ];
        let mut page = vec![identity, group_gap()];
        if s.self_updating {
            page.push(row(Some(s.i18n.text("settings.updates")), updates, p));
        }
        page.push(row(
            Some(s.i18n.text("settings.links")),
            vec![line(links)],
            p,
        ));
        page.push(
            div()
                .pt(px(10.))
                .flex()
                .justify_center()
                .text_size(px(HELP_SIZE))
                .text_color(p.subtitle)
                .child(s.i18n.text("settings.copyright")),
        );
        page
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(snapshot) = self
            .link
            .app
            .upgrade()
            .map(|app| app.read(cx).settings_snapshot())
        else {
            return div();
        };
        let title = snapshot.i18n.text("settings.title");
        if self.title != title {
            window.set_window_title(&title);
            self.title = title;
        }
        let p = Palette::new(snapshot.dark);
        let rows = match self.page {
            Page::General => self.general(&snapshot, p, cx),
            Page::Editor => self.editor(&snapshot, p, cx),
            Page::Files => self.files(&snapshot, p, cx),
            Page::Markdown => self.markdown(&snapshot, p, cx),
            Page::About => self.about(&snapshot, p),
        };
        let page_height = self.page_height.clone();
        let measured = page_height.clone();
        let fitted = self.fitted.clone();
        let fitting = self.fitting.clone();
        div()
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(Self::close))
            .on_action(cx.listener(|_, _: &NextControl, window, cx| window.focus_next(cx)))
            .on_action(cx.listener(|_, _: &PreviousControl, window, cx| window.focus_prev(cx)))
            .size_full()
            .flex()
            .flex_col()
            .bg(p.surface)
            .text_color(p.text)
            .font_family(".SystemUIFont")
            // The header and the page have both been laid out by now: the window takes
            // their height, from a later turn because AppKit resizes it.
            .on_children_prepainted(move |bounds, window, cx| {
                let Some(header) = bounds.first() else {
                    return;
                };
                let wanted = (f32::from(header.size.height) + measured.get())
                    .round()
                    .min(MAX_HEIGHT);
                if measured.get() <= 0. || (wanted - fitted.get()).abs() < 1. || fitting.get() {
                    return;
                }
                let animate = fitted.get() > 0. && !cx.reduce_motion();
                fitted.set(wanted);
                fitting.set(true);
                let fitting = fitting.clone();
                let handle = window.window_handle();
                // From a task rather than inside an update: AppKit resizes the window
                // on the spot, and GPUI only hears of each size while the app is free.
                cx.spawn(async move |cx| {
                    let native = handle
                        .update(cx, |_, window, _| crate::platform::NativeWindow::of(window))
                        .ok()
                        .flatten();
                    if let Some(native) = native {
                        native.fit_height(wanted, animate);
                    }
                    fitting.set(false);
                    // A page chosen while the window moved is fitted from this frame.
                    let _ = handle.update(cx, |_, window, _| window.refresh());
                })
                .detach();
            })
            .child(self.header(&snapshot.i18n, p, cx))
            .child(
                div()
                    .id("settings-page")
                    .track_scroll(&self.scroll)
                    .overflow_y_scroll()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .on_children_prepainted(move |bounds, _, _| {
                                if let Some(page) = bounds.first() {
                                    page_height.set(f32::from(page.size.height));
                                }
                            })
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(ROW_GAP))
                                    .px(px(PAGE_PAD_X))
                                    .pt(px(PAGE_TOP))
                                    .pb(px(PAGE_BOTTOM))
                                    .children(rows),
                            ),
                    ),
            )
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::{OpenSettings, Page, language_options};
    use crate::locale::{I18n, LanguagePreference, available_languages};

    #[::core::prelude::v1::test]
    fn language_choices_keep_native_names_and_do_not_duplicate_a_supported_selection() {
        let selected = LanguagePreference::Locale("en".into());
        let choices = language_options(&I18n::english(), &selected);
        assert_eq!(choices[0].1, LanguagePreference::System);
        assert_eq!(choices.len(), available_languages().len() + 1);
        for language in available_languages() {
            assert!(choices.contains(&(
                language.name.clone(),
                LanguagePreference::Locale(language.id.clone()),
            )));
        }
        assert_eq!(
            choices
                .iter()
                .filter(|(_, value)| value == &selected)
                .count(),
            1
        );
    }

    #[::core::prelude::v1::test]
    fn custom_language_requests_remain_visible_without_claiming_they_are_unsupported() {
        for locale in ["en-AU", "future-Language"] {
            let selected = LanguagePreference::Locale(locale.into());
            let choices = language_options(&I18n::english(), &selected);
            assert_eq!(
                choices.iter().find(|(_, value)| value == &selected),
                Some(&(format!("{locale} (custom)"), selected)),
            );
            assert_eq!(choices.len(), available_languages().len() + 2);
        }
    }

    #[::core::prelude::v1::test]
    fn arrow_keys_walk_the_pages_without_wrapping() {
        assert_eq!(Page::General.step(false), Page::General);
        assert_eq!(Page::General.step(true), Page::Editor);
        assert_eq!(Page::Markdown.step(true), Page::Files);
        assert_eq!(Page::About.step(true), Page::About);
        assert_eq!(Page::About.step(false), Page::Files);
    }

    #[::core::prelude::v1::test]
    fn the_window_opens_on_the_page_it_was_left_on() {
        for page in Page::ALL {
            assert_eq!(Page::remembered(page.key()), page);
        }
        assert_eq!(Page::remembered(""), Page::General);
        assert_eq!(Page::remembered("gone"), Page::General);
    }

    #[gpui::test]
    fn window_bounds_are_saved_and_restored_after_close(cx: &mut gpui::TestAppContext) {
        use crate::storage::SettingsWindowPlacement;
        use gpui::{point, px, size};

        let mut h = crate::e2e::harness::open(cx, |preferences| {
            preferences.settings_window = Some(SettingsWindowPlacement {
                origin: [100., 120.],
                height: 300.,
                display_uuid: None,
            });
        });
        let app = h.app.clone();
        let open = |cx: &mut gpui::VisualTestContext| {
            cx.update(|window, cx| app.update(cx, |app, cx| app.open_bundled_settings(window, cx)));
            cx.run_until_parked();
            cx.update(|_, cx| cx.global::<OpenSettings>().0)
        };
        let settings = open(h.cx);
        settings
            .update(&mut *h.cx, |_, window, _| {
                assert_eq!(window.bounds().origin, point(px(100.), px(120.)));
                assert_eq!(window.bounds().size.height, px(300.));
            })
            .unwrap();

        // The bounds observer must replace stale preferences with the live window.
        h.app
            .update(h.cx, |app, _| app.preferences.settings_window = None);
        h.cx.simulate_window_resize(settings.into(), size(px(super::WIDTH), px(360.)));
        h.cx.run_until_parked();
        let saved = h
            .saved_preferences(|p| p.settings_window.as_ref().is_some_and(|s| s.height == 360.))
            .unwrap()
            .settings_window
            .unwrap();
        assert_eq!(saved.origin, [100., 120.]);
        assert!(saved.display_uuid.is_some());

        settings
            .update(&mut *h.cx, |view, window, cx| {
                view.close(&super::CloseSettings, window, cx)
            })
            .unwrap();
        h.cx.run_until_parked();
        let reopened = open(h.cx);
        reopened
            .update(&mut *h.cx, |_, window, _| {
                assert_eq!(window.bounds().origin, point(px(100.), px(120.)));
                assert_eq!(window.bounds().size.height, px(360.));
            })
            .unwrap();
        assert_eq!(
            open(h.cx),
            reopened,
            "opening Settings again reuses its window"
        );
    }

    /// Every page draws, with each control it can hold: a pop-up button the page
    /// adds must also have a place in the focus order.
    #[gpui::test]
    fn every_page_draws(cx: &mut gpui::TestAppContext) {
        let h = crate::e2e::harness::open_with(
            cx,
            &[(".obsidian/daily-notes.json", r#"{"folder": "Journal"}"#)],
            |_| {},
        );
        let app = h.app.clone();
        h.cx.update(|window, cx| {
            app.update(cx, |app, cx| {
                // The vault keeps its daily notes elsewhere, so the sync button draws.
                app.obsidian_daily = app.read_obsidian_daily();
                assert!(app.obsidian_daily.is_some());
                app.open_bundled_settings(window, cx);
            })
        });
        h.cx.run_until_parked();
        let settings =
            h.cx.update(|_, cx| cx.try_global::<OpenSettings>().map(|open| open.0))
                .expect("the Settings window");
        for page in Page::ALL {
            settings
                .update(&mut *h.cx, |view, window, cx| {
                    view.set_page(page, cx);
                    window.refresh();
                })
                .expect("the Settings window");
            h.cx.run_until_parked();
            settings
                .update(&mut *h.cx, |view, _, _| assert_eq!(view.page, page))
                .expect("the Settings window");
        }
    }
}
