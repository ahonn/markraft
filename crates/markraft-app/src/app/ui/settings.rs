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
mod select;
mod shortcut;

use super::*;
use crate::platform::Shortcut;
use crate::storage::{
    BulletMarker, CodeFence, EditorFont, EmphasisMarker, HardBreakStyle, ImageNaming, LineHeight,
    LineWidth, NoteNaming, OrderedDelimiter, Pref, Preferences, Summon, TabKey,
};
use controls::{
    ChordFace, Palette, button, checkbox, chord_face, error, group_gap, line, metrics::*, row,
    segmented, stepper,
};
use gpui_base::{Tab, Tabs};
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
            include_bytes!("../../../../../assets/icon/Markraft.png").to_vec(),
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
    /// By [`Shortcut`]: the shortcut that shows the note, then the one for a new note.
    pub(in crate::app) shortcuts: [Option<String>; 2],
    login: Option<String>,
    new_notes: Option<String>,
    images: Option<String>,
    updates: Option<String>,
}

#[derive(Clone)]
enum Change {
    /// A preference; see [`Pref`].
    Pref(Pref),
    /// The window came forward: re-read what macOS keeps outside the app.
    Refresh,
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
}

/// The preference `which` global shortcut is.
fn shortcut_pref(which: Shortcut, shortcut: String) -> Pref {
    match which {
        Shortcut::Toggle => Pref::Hotkey(shortcut),
        Shortcut::NewNote => Pref::NewNoteHotkey(shortcut),
    }
}

/// What a page draws, read from the app once per frame.
struct Snapshot {
    dark: bool,
    preferences: Preferences,
    /// None without the platform layer, which is what answers the question.
    login: Option<bool>,
    folder: Option<PathBuf>,
    /// Where new notes and images go, and whether that is other than the default.
    new_notes: Option<(String, bool)>,
    images: Option<(String, bool)>,
    new_note_name: NoteNaming,
    /// None where this copy has no updater to ask: unbundled, or not configured.
    automatic_updates: Option<bool>,
    image_name: ImageNaming,
    errors: SettingsErrors,
}

impl MarkraftApp {
    /// ⌘, and the Settings commands: open the window, or bring the open one forward.
    pub(in crate::app) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        let display = window
            .display(cx)
            .map(|display| (display.id(), display.bounds().size));
        cx.defer(move |cx| present(link, near, display, cx));
    }

    /// Read every frame of the Settings window, including each step of its resize, so
    /// it asks nothing of the system: what macOS keeps is cached by [`Change::Refresh`].
    fn settings_snapshot(&self) -> Snapshot {
        let workspace = &self.library.workspace;
        let placed = |root: &PathBuf| {
            let new_notes = (
                folder_label(root, &workspace.new_note_directory),
                !workspace.new_note_directory.as_os_str().is_empty(),
            );
            let images = match &workspace.attachments {
                crate::storage::AttachmentPolicy::Default => {
                    ("Assets folder beside each note".to_owned(), false)
                }
                crate::storage::AttachmentPolicy::WorkspaceFolder(path) => {
                    (folder_label(root, path), true)
                }
            };
            (new_notes, images)
        };
        let (new_notes, images) = self.path.as_ref().map(placed).unzip();
        Snapshot {
            dark: self.dark,
            preferences: self.preferences.clone(),
            login: self.launch_at_login,
            folder: self.path.clone(),
            new_notes,
            images,
            new_note_name: workspace.new_note_name,
            automatic_updates: self.updater.automatically_checks(),
            image_name: workspace.image_name,
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
            Change::Pref(pref) => self.set_preference(pref, window, cx),
            Change::Refresh => self.refresh_launch_at_login(),
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
                self.library.workspace.new_note_directory = PathBuf::new();
                self.schedule_save(cx);
            }
            Change::ImageLocation => {
                self.settings_errors.images = None;
                self.configure_images(window, cx);
            }
            Change::ResetImageLocation => {
                self.settings_errors.images = None;
                self.library.workspace.attachments = crate::storage::AttachmentPolicy::Default;
                self.schedule_save(cx);
            }
            Change::RevealFolder => {
                if let Some(path) = &self.path {
                    cx.reveal_path(path);
                }
            }
            Change::NewNoteName(naming) => {
                self.library.workspace.new_note_name = naming;
                self.schedule_save(cx);
            }
            Change::AutomaticUpdates(enabled) => {
                if let Err(error) = self.updater.set_automatically_checks(enabled) {
                    self.settings_errors.updates = Some(error);
                }
            }
            Change::ImageName(naming) => {
                self.library.workspace.image_name = naming;
                self.schedule_save(cx);
            }
            Change::CheckForUpdates => {
                self.settings_errors.updates = self.updater.check().err();
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

    pub(in crate::app) fn set_settings_error_new_notes(&mut self, error: Option<String>) {
        self.settings_errors.new_notes = error;
    }

    pub(in crate::app) fn set_settings_error_images(&mut self, error: Option<String>) {
        self.settings_errors.images = error;
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

/// The one Settings window there may be. A second request brings this one forward.
struct OpenSettings(WindowHandle<SettingsView>);

impl Global for OpenSettings {}

fn present(
    link: Link,
    near: Bounds<Pixels>,
    display: Option<(DisplayId, Size<Pixels>)>,
    cx: &mut App,
) {
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
    display: Option<(DisplayId, Size<Pixels>)>,
    cx: &mut App,
) -> Option<WindowHandle<SettingsView>> {
    let app = link.app.upgrade()?;
    let extent = size(px(WIDTH), px(HEIGHT));
    let origin = display
        .map(|(_, screen)| placement(screen, near, extent))
        .unwrap_or_default();
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(origin, extent))),
        display_id: display.map(|(id, _)| id),
        titlebar: Some(TitlebarOptions {
            title: Some("Settings".into()),
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

/// Where the window opens on the note's display: beside the note, so the note — which
/// floats above ordinary windows — does not cover it; centred when neither side has
/// room. Coordinates are the display's own, as GPUI takes a new window's bounds.
fn placement(screen: Size<Pixels>, near: Bounds<Pixels>, extent: Size<Pixels>) -> Point<Pixels> {
    let margin = px(16.);
    // Clear of the menu bar.
    let top = px(40.);
    let fits = |x: Pixels| x >= margin && x + extent.width <= screen.width - margin;
    let y = near
        .top()
        .min(screen.height - extent.height - margin)
        .max(top);
    for x in [near.right() + margin, near.left() - margin - extent.width] {
        if fits(x) {
            return point(x, y);
        }
    }
    point(
        ((screen.width - extent.width) / 2.).max(px(0.)),
        ((screen.height - extent.height) / 2.).max(top),
    )
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

    fn title(self) -> &'static str {
        match self {
            Page::General => "General",
            Page::Editor => "Editor",
            Page::Files => "Files",
            Page::Markdown => "Markdown",
            Page::About => "About",
        }
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
    page: Page,
    /// The toolbar, which the window opens on, so ← and → move between pages.
    toolbar: FocusHandle,
    /// One field for each global shortcut, by [`Shortcut`].
    recorders: [FocusHandle; 2],
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
        ];
        let own = window.window_handle();
        let this = cx.entity().downgrade();
        let mut subscriptions = vec![
            cx.observe(&app, |_, _, cx| cx.notify()),
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
            link,
            page,
            toolbar: cx.focus_handle().tab_stop(true),
            recorders,
            recording: None,
            refusal: None,
            moving: false,
            scroll: ScrollHandle::new(),
            page_height: Rc::default(),
            fitted: Rc::default(),
            selects: select::Selects::new(cx),
            fitting: Rc::default(),
            _subscriptions: subscriptions,
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
    fn header(&self, p: Palette, cx: &mut Context<Self>) -> impl IntoElement {
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
                    .child(self.page.title()),
            )
            .child(self.toolbar(p, cx))
    }

    fn toolbar(&self, p: Palette, cx: &mut Context<Self>) -> impl IntoElement {
        Tabs::new("settings-pages")
            .key_context(TOOLBAR_CONTEXT)
            .track_focus(&self.toolbar)
            .aria_label("Settings pages")
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
                    .accessibility_label(page.title())
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
                    .child(page.title())
            }))
    }

    /// The field for one global shortcut: its chord, or the recorder listening for one.
    /// A click or Space starts it; the next chord pressed replaces the shortcut.
    fn shortcut_field(
        &self,
        which: Shortcut,
        chord: &str,
        p: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let bound = !chord.trim().is_empty();
        let face = if self.recording == Some(which) {
            ChordFace::Recording
        } else if bound {
            ChordFace::Bound(shortcut::glyphs(chord))
        } else {
            ChordFace::Unbound
        };
        let name = match which {
            Shortcut::Toggle => "Show and hide shortcut",
            Shortcut::NewNote => "New note shortcut",
        };
        let clear = (bound && self.recording != Some(which)).then(|| {
            div()
                .id(SharedString::from(format!(
                    "clear-shortcut-{}",
                    which as usize
                )))
                .role(Role::Button)
                .aria_label(format!("Clear {}", name.to_lowercase()))
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
                format!("{name}, {chord}")
            } else {
                format!("{name}, none")
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
                notes.push(error(refusal, p));
            }
        } else if let Some(refused) = &s.errors.shortcuts[which as usize] {
            notes.push(error(refused.clone(), p));
        }
        notes
    }

    fn general(&self, s: &Snapshot, p: Palette, cx: &mut Context<Self>) -> Vec<Div> {
        let theme = segmented(
            "appearance",
            "Appearance",
            &[("Auto", None), ("Light", Some(false)), ("Dark", Some(true))],
            s.preferences.dark_mode,
            p,
            self.sender(|value| Change::Pref(Pref::Theme(value))),
        );

        let (toggle, new_note) = (&s.preferences.hotkey, &s.preferences.new_note_hotkey);
        let summon = self.select(
            "summon",
            "Show on open",
            &[
                ("Last Note", Summon::LastNote),
                ("New Note", Summon::NewNote),
            ],
            s.preferences.summon,
            |value| Change::Pref(Pref::Summon(value)),
            p,
            cx,
        );
        let mut toggle_lines = vec![line(vec![self.shortcut_field(
            Shortcut::Toggle,
            toggle,
            p,
            cx,
        )])];
        toggle_lines.extend(self.shortcut_notes(Shortcut::Toggle, s, p));
        let mut new_note_lines = vec![line(vec![self.shortcut_field(
            Shortcut::NewNote,
            new_note,
            p,
            cx,
        )])];
        new_note_lines.extend(self.shortcut_notes(Shortcut::NewNote, s, p));

        let mut startup = vec![
            checkbox(
                "launch-at-login",
                "Launch at login",
                s.login.unwrap_or(false),
                s.login.is_none(),
                p,
                self.sender(Change::LaunchAtLogin),
            )
            .into_any_element(),
        ];
        if let Some(refused) = &s.errors.login {
            startup.push(error(refused.clone(), p));
        }

        // Launching at login comes first: for an app that lives in the menu bar it is
        // what decides whether it is there at all.
        vec![
            row(Some("Startup"), startup, p),
            group_gap(),
            row(Some("Show and hide"), toggle_lines, p),
            row(Some("New note"), new_note_lines, p),
            group_gap(),
            row(
                Some("Note window"),
                vec![
                    checkbox(
                        "always-on-top",
                        "Keep above other windows",
                        s.preferences.always_on_top,
                        false,
                        p,
                        self.sender(|value| Change::Pref(Pref::AlwaysOnTop(value))),
                    )
                    .into_any_element(),
                    checkbox(
                        "hide-on-deactivate",
                        "Hide when another app is used",
                        s.preferences.hide_on_deactivate,
                        false,
                        p,
                        self.sender(|value| Change::Pref(Pref::HideOnDeactivate(value))),
                    )
                    .into_any_element(),
                    checkbox(
                        "auto-height",
                        "Grow with the note",
                        s.preferences.auto_height,
                        false,
                        p,
                        self.sender(|value| Change::Pref(Pref::AutoHeight(value))),
                    )
                    .into_any_element(),
                    checkbox(
                        "all-spaces",
                        "Show on all desktops",
                        s.preferences.all_spaces,
                        false,
                        p,
                        self.sender(|value| Change::Pref(Pref::AllSpaces(value))),
                    )
                    .into_any_element(),
                    checkbox(
                        "follow-pointer",
                        "Open on the display with the pointer",
                        s.preferences.follow_pointer,
                        false,
                        p,
                        self.sender(|value| Change::Pref(Pref::FollowPointer(value))),
                    )
                    .into_any_element(),
                ],
                p,
            ),
            row(Some("Show on open"), vec![line(vec![summon])], p),
            group_gap(),
            row(
                Some("Appearance"),
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
                format!("{size:.0} pt"),
                (size > *range.start(), size < *range.end()),
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
                    "Default",
                    p,
                    self.on_click(Change::Pref(Pref::TextSize(Preferences::DEFAULT_TEXT_SIZE))),
                )
                .into_any_element(),
            );
        }
        let font = self.select(
            "font",
            "Font",
            &[
                ("System", EditorFont::System),
                ("Serif", EditorFont::Serif),
                ("Rounded", EditorFont::Rounded),
                ("Mono", EditorFont::Mono),
            ],
            s.preferences.font,
            |value| Change::Pref(Pref::Font(value)),
            p,
            cx,
        );
        let line_height = self.select(
            "line-height",
            "Line height",
            &[
                ("Tight", LineHeight::Tight),
                ("Normal", LineHeight::Normal),
                ("Relaxed", LineHeight::Relaxed),
            ],
            s.preferences.line_height,
            |value| Change::Pref(Pref::LineHeight(value)),
            p,
            cx,
        );
        let line_width = self.select(
            "line-width",
            "Line width",
            &[
                ("Narrow", LineWidth::Narrow),
                ("Normal", LineWidth::Normal),
                ("Full", LineWidth::Full),
            ],
            s.preferences.line_width,
            |value| Change::Pref(Pref::LineWidth(value)),
            p,
            cx,
        );
        let tab_key = self.select(
            "tab-key",
            "Tab key",
            &[
                ("Tab", TabKey::Tab),
                ("2 Spaces", TabKey::TwoSpaces),
                ("4 Spaces", TabKey::FourSpaces),
            ],
            s.preferences.tab_key,
            |value| Change::Pref(Pref::TabKey(value)),
            p,
            cx,
        );
        vec![
            row(Some("Text size"), vec![line(size_line)], p),
            row(Some("Font"), vec![line(vec![font])], p),
            row(Some("Line height"), vec![line(vec![line_height])], p),
            row(Some("Line width"), vec![line(vec![line_width])], p),
            group_gap(),
            row(
                Some("Editing"),
                vec![
                    checkbox(
                        "markdown-shortcuts",
                        "Format Markdown as you type",
                        s.preferences.markdown_shortcuts,
                        false,
                        p,
                        self.sender(|value| Change::Pref(Pref::MarkdownShortcuts(value))),
                    )
                    .into_any_element(),
                    checkbox(
                        "auto-pair",
                        "Pair brackets and quotes",
                        s.preferences.auto_pair,
                        false,
                        p,
                        self.sender(|value| Change::Pref(Pref::AutoPair(value))),
                    )
                    .into_any_element(),
                    checkbox(
                        "vim-mode",
                        "Vim mode",
                        s.preferences.vim_mode,
                        false,
                        p,
                        self.sender(|value| Change::Pref(Pref::VimMode(value))),
                    )
                    .into_any_element(),
                ],
                p,
            ),
            row(Some("Tab in code"), vec![line(vec![tab_key])], p),
            group_gap(),
            row(
                Some("Images"),
                vec![
                    checkbox(
                        "remote-images",
                        "Load images linked from the web",
                        s.preferences.remote_images,
                        false,
                        p,
                        self.sender(|value| Change::Pref(Pref::RemoteImages(value))),
                    )
                    .into_any_element(),
                    checkbox(
                        "animate-images",
                        "Play animated images under the pointer",
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

    /// Where the notes are and where new files go, each a folder pop-up the way Safari
    /// picks its download folder: the folder on the button, and what can be done
    /// with it — show it, choose another, go back to the default — in its menu.
    fn files(&self, s: &Snapshot, p: Palette, cx: &mut Context<Self>) -> Vec<Div> {
        let Some(root) = &s.folder else {
            let choose = self.pop_up(
                "notes-folder",
                "Notes folder",
                "None".into(),
                vec![MenuItem::action("Choose Folder…", Change::ChooseFolder)],
                p,
                cx,
            );
            return vec![row(Some("Notes folder"), vec![line(vec![choose])], p)];
        };
        let root_name = root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| root.display().to_string());
        let notes_folder = self.pop_up(
            "notes-folder",
            "Notes folder",
            root_name.clone().into(),
            vec![
                MenuItem::choice(root_name.clone(), None, true),
                MenuRow::Separator,
                MenuItem::action("Show in Finder", Change::RevealFolder),
                MenuItem::action("Choose Folder…", Change::ChooseFolder),
            ],
            p,
            cx,
        );
        let mut page = vec![row(Some("Notes folder"), vec![line(vec![notes_folder])], p)];

        // A location inside the notes folder: the default, or the folder chosen instead,
        // which the button names by its last component.
        let location = |id: &'static str,
                        label: &'static str,
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
            rows.push(MenuItem::action("Choose Folder…", choose));
            self.pop_up(id, label, face.into(), rows, p, cx)
        };

        if let (Some(new_notes), Some(images)) = (&s.new_notes, &s.images) {
            page.push(group_gap());
            let mut new_note_lines = vec![line(vec![location(
                "new-note-folder",
                "Save new notes in",
                root_name.clone(),
                new_notes,
                Change::ResetNewNoteLocation,
                Change::NewNoteLocation,
                cx,
            )])];
            if let Some(refused) = &s.errors.new_notes {
                new_note_lines.push(error(refused.clone(), p));
            }
            page.push(row(Some("Save new notes in"), new_note_lines, p));
            let naming = self.select(
                "new-note-name",
                "Name new notes",
                &[
                    ("First Line", NoteNaming::FirstLine),
                    ("Date and Time", NoteNaming::DateTime),
                ],
                s.new_note_name,
                Change::NewNoteName,
                p,
                cx,
            );
            page.push(row(Some("Name new notes"), vec![line(vec![naming])], p));

            page.push(group_gap());
            let mut image_lines = vec![line(vec![location(
                "image-folder",
                "Save images in",
                "Beside Each Note".to_owned(),
                images,
                Change::ResetImageLocation,
                Change::ImageLocation,
                cx,
            )])];
            if let Some(refused) = &s.errors.images {
                image_lines.push(error(refused.clone(), p));
            }
            page.push(row(Some("Save images in"), image_lines, p));
            let image_name = self.select(
                "image-name",
                "Name images",
                &[
                    ("Random ID", ImageNaming::RandomId),
                    ("Note Name and Date", ImageNaming::NoteAndDate),
                ],
                s.image_name,
                Change::ImageName,
                p,
                cx,
            );
            page.push(row(Some("Name images"), vec![line(vec![image_name])], p));

            page.push(group_gap());
            page.push(row(
                Some("Deleting"),
                vec![
                    checkbox(
                        "confirm-delete",
                        "Ask before moving a note to the Trash",
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

    fn markdown(&self, s: &Snapshot, p: Palette, cx: &mut Context<Self>) -> Vec<Div> {
        let bullet = self.select(
            "bullet-marker",
            "Bullet list",
            &[
                ("-  Item", BulletMarker::Dash),
                ("*  Item", BulletMarker::Star),
                ("+  Item", BulletMarker::Plus),
            ],
            s.preferences.bullet_marker,
            |value| Change::Pref(Pref::Bullet(value)),
            p,
            cx,
        );
        let fence = self.select(
            "code-fence",
            "Code block",
            &[("```", CodeFence::Backticks), ("~~~", CodeFence::Tildes)],
            s.preferences.code_fence,
            |value| Change::Pref(Pref::Fence(value)),
            p,
            cx,
        );
        let ordered = self.select(
            "ordered-delimiter",
            "Numbered list",
            &[
                ("1.  Item", OrderedDelimiter::Period),
                ("1)  Item", OrderedDelimiter::Parenthesis),
            ],
            s.preferences.ordered_delimiter,
            |value| Change::Pref(Pref::OrderedDelimiter(value)),
            p,
            cx,
        );
        let hard_break = self.select(
            "hard-break",
            "Line break",
            &[
                ("Backslash", HardBreakStyle::Backslash),
                ("Two Spaces", HardBreakStyle::Spaces),
            ],
            s.preferences.hard_break,
            |value| Change::Pref(Pref::HardBreak(value)),
            p,
            cx,
        );
        let emphasis = self.select(
            "emphasis-marker",
            "Emphasis",
            &[
                ("*Italic*  **Bold**", EmphasisMarker::Star),
                ("_Italic_  __Bold__", EmphasisMarker::Underscore),
            ],
            s.preferences.emphasis_marker,
            |value| Change::Pref(Pref::Emphasis(value)),
            p,
            cx,
        );
        vec![
            row(Some("Bullet list"), vec![line(vec![bullet])], p),
            row(Some("Numbered list"), vec![line(vec![ordered])], p),
            row(Some("Code block"), vec![line(vec![fence])], p),
            row(Some("Emphasis"), vec![line(vec![emphasis])], p),
            row(Some("Line break"), vec![line(vec![hard_break])], p),
            group_gap(),
            row(
                Some("Emoji"),
                vec![
                    checkbox(
                        "emoji-characters",
                        "Insert emoji as characters",
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
            Some(build) => format!("Version {version} ({build})"),
            None => format!("Version {version}"),
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

        let mut updates = vec![
            checkbox(
                "automatic-updates",
                "Check for updates automatically",
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
                "Check for Updates…",
                p,
                self.on_click(Change::CheckForUpdates),
            )
            .disabled(!available)
            .when(!available, |button| button.opacity(0.45).cursor_default())
            .into_any_element(),
        ]));
        if let Some(refused) = &s.errors.updates {
            updates.push(error(refused.clone(), p));
        }

        let open = |url: &'static str| {
            move |_: &ClickEvent, _: &mut Window, cx: &mut App| cx.open_url(url)
        };
        let links = vec![
            button("github", "GitHub", p, open(REPOSITORY)).into_any_element(),
            button("release-notes", "Release Notes", p, open(RELEASES)).into_any_element(),
            button("report-issue", "Report an Issue", p, open(NEW_ISSUE)).into_any_element(),
        ];
        let mut page = vec![
            identity,
            group_gap(),
            row(Some("Updates"), updates, p),
            row(Some("Links"), vec![line(links)], p),
        ];
        page.push(
            div()
                .pt(px(10.))
                .flex()
                .justify_center()
                .text_size(px(HELP_SIZE))
                .text_color(p.subtitle)
                .child("© 2026 Yuexun Jiang. Released under the MIT License."),
        );
        page
    }
}

impl Render for SettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(snapshot) = self
            .link
            .app
            .upgrade()
            .map(|app| app.read(cx).settings_snapshot())
        else {
            return div();
        };
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
            .child(self.header(p, cx))
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
    use super::{Page, placement};
    use gpui::{Bounds, point, px, size};

    const SCREEN: gpui::Size<gpui::Pixels> = size(px(1440.), px(900.));
    const EXTENT: gpui::Size<gpui::Pixels> = size(px(640.), px(460.));

    fn note(x: f32, y: f32) -> Bounds<gpui::Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(480.), px(320.)))
    }

    #[::core::prelude::v1::test]
    fn the_window_opens_beside_the_note_rather_than_under_it() {
        assert_eq!(
            placement(SCREEN, note(100., 200.), EXTENT),
            point(px(596.), px(200.))
        );
        assert_eq!(
            placement(SCREEN, note(900., 200.), EXTENT),
            point(px(244.), px(200.))
        );
    }

    #[::core::prelude::v1::test]
    fn without_room_on_either_side_it_is_centred() {
        let wide = Bounds::new(point(px(300.), px(200.)), size(px(900.), px(320.)));
        assert_eq!(placement(SCREEN, wide, EXTENT), point(px(400.), px(220.)));
    }

    #[::core::prelude::v1::test]
    fn it_stays_below_the_menu_bar_and_above_the_bottom_edge() {
        assert_eq!(placement(SCREEN, note(100., 0.), EXTENT).y, px(40.));
        assert_eq!(placement(SCREEN, note(100., 800.), EXTENT).y, px(424.));
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
}
