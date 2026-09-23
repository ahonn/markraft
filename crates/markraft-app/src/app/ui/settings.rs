//! The Settings window: a window of its own beside the floating note, drawn the way a
//! classic macOS settings window is. Pages are chosen from a row of icons under the
//! title; each page is a form of right-aligned labels; the window keeps its width and
//! takes the height of the page on screen.
//!
//! The preferences still belong to [`NotesApp`]: the window holds no copy of them. It
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
mod shortcut;

use super::*;
use crate::platform::Shortcut;
use crate::storage::Preferences;
use controls::{
    ChordFace, Palette, button, checkbox, chord_face, divider, error, help, line, metrics::*, row,
    segmented, stepper, value,
};
use gpui_base::{Tab, Tabs};
use shortcut::Recorded;
use std::{cell::Cell, rc::Rc};

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
const DEFAULT_SHORTCUT: &str = "Alt+N";

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
    shortcuts: [Option<String>; 2],
    login: Option<String>,
    new_notes: Option<String>,
    images: Option<String>,
}

#[derive(Clone)]
enum Change {
    Theme(Option<bool>),
    AutoHeight(bool),
    VimMode(bool),
    RemoteImages(bool),
    LaunchAtLogin(bool),
    /// Empty turns that global shortcut off.
    Shortcut(Shortcut, String),
    /// A recorder opening or closing: the running shortcuts are let go of meanwhile.
    Recording(bool),
    TextSize(f32),
    HideOnDeactivate(bool),
    AlwaysOnTop(bool),
    ChooseFolder,
    NewNoteLocation,
    ResetNewNoteLocation,
    ImageLocation,
    ResetImageLocation,
    RevealFolder,
}

/// What a page draws, read from the app once per frame.
struct Snapshot {
    dark: bool,
    theme: Option<bool>,
    auto_height: bool,
    vim: bool,
    remote_images: bool,
    /// None without the platform layer, which is what answers the question.
    login: Option<bool>,
    shortcuts: [String; 2],
    text_size: f32,
    hide_on_deactivate: bool,
    always_on_top: bool,
    folder: Option<PathBuf>,
    /// Where new notes and images go, and whether that is other than the default.
    new_notes: Option<(String, bool)>,
    images: Option<(String, bool)>,
    errors: SettingsErrors,
}

impl NotesApp {
    /// ⌘, and the Settings commands: open the window, or bring the open one forward.
    pub(in crate::app) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(platform) = &mut self.platform {
            platform.remember_frontmost_app();
        }
        let link = Link {
            app: cx.entity().downgrade(),
            main: window.window_handle(),
        };
        let near = window.bounds();
        let display = window
            .display(cx)
            .map(|display| (display.id(), display.bounds().size));
        cx.defer(move |cx| present(link, near, display, cx));
    }

    /// `login` asks macOS for the login item's state, which only the General page
    /// shows; the window redraws whenever the note does, so it is not asked idly.
    fn settings_snapshot(&self, login: bool) -> Snapshot {
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
            theme: self.library.preferences.dark_mode,
            auto_height: self.library.preferences.auto_height,
            vim: self.library.preferences.vim_mode,
            remote_images: self.library.preferences.remote_images,
            login: self
                .platform
                .as_ref()
                .filter(|_| login)
                .map(|platform| platform.launch_at_login_enabled()),
            shortcuts: [
                self.library.preferences.hotkey.clone(),
                self.library.preferences.new_note_hotkey.clone(),
            ],
            text_size: self.library.preferences.text_size,
            hide_on_deactivate: self.library.preferences.hide_on_deactivate,
            always_on_top: self.library.preferences.always_on_top,
            folder: self.path.clone(),
            new_notes,
            images,
            errors: self.settings_errors.clone(),
        }
    }

    fn apply_setting(&mut self, change: Change, window: &mut Window, cx: &mut Context<Self>) {
        match change {
            Change::Theme(mode) => {
                self.library.preferences.dark_mode = mode;
                self.apply_theme(window, cx);
                self.schedule_save(cx);
            }
            Change::AutoHeight(enabled) => {
                self.library.preferences.auto_height = enabled;
                self.schedule_save(cx);
            }
            Change::VimMode(enabled) => self.set_vim(enabled, cx),
            Change::RemoteImages(enabled) => self.set_remote_images(enabled, cx),
            Change::LaunchAtLogin(enabled) => {
                if let Some(platform) = &mut self.platform {
                    self.settings_errors.login = platform.set_launch_at_login(enabled).err();
                }
            }
            Change::Shortcut(which, shortcut) => {
                if let Some(platform) = &mut self.platform {
                    let index = which as usize;
                    match platform.set_shortcut(which, &shortcut) {
                        Ok(()) => {
                            let preferences = &mut self.library.preferences;
                            match which {
                                Shortcut::Toggle => preferences.hotkey = shortcut,
                                Shortcut::NewNote => preferences.new_note_hotkey = shortcut,
                            }
                            self.settings_errors.shortcuts[index] = None;
                            self.feedback.set_platform_error(None);
                            self.schedule_save(cx);
                        }
                        Err(error) => self.settings_errors.shortcuts[index] = Some(error),
                    }
                }
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
            Change::TextSize(size) => self.set_text_size(size, cx),
            Change::HideOnDeactivate(enabled) => {
                self.library.preferences.hide_on_deactivate = enabled;
                self.schedule_save(cx);
            }
            Change::AlwaysOnTop(enabled) => {
                self.library.preferences.always_on_top = enabled;
                if let Some(platform) = &self.platform
                    && let Err(error) = platform.set_always_on_top(window, enabled)
                {
                    self.feedback.set_platform_error(Some(error));
                }
                self.schedule_save(cx);
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
    app: WeakEntity<NotesApp>,
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
        is_minimizable: true,
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
            eprintln!("Markraft: the Settings window could not be opened: {error}");
            None
        }
    }
}

/// A path as it is read: under the home folder, from `~`.
fn home_relative(path: &std::path::Path) -> String {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .and_then(|home| {
            let rest = path.strip_prefix(home).ok()?;
            Some(PathBuf::from("~").join(rest))
        })
        .unwrap_or_else(|| path.to_owned())
        .display()
        .to_string()
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
    Notes,
}

impl Page {
    const ALL: [Page; 3] = [Page::General, Page::Editor, Page::Notes];

    fn title(self) -> &'static str {
        match self {
            Page::General => "General",
            Page::Editor => "Editor",
            Page::Notes => "Notes",
        }
    }

    fn icon(self) -> Icon {
        match self {
            Page::General => Icon::Settings,
            Page::Editor => Icon::Edit,
            Page::Notes => Icon::Open,
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
    /// Set while AppKit animates the window to a new height. GPUI draws each step of
    /// it, so a page chosen meanwhile is measured mid-animation; its fit waits for the
    /// frame drawn once this one has settled rather than starting a second animation
    /// inside the first.
    fitting: Rc<Cell<bool>>,
    _subscriptions: Vec<Subscription>,
}

impl SettingsView {
    fn new(app: Entity<NotesApp>, link: Link, window: &mut Window, cx: &mut Context<Self>) -> Self {
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
                if !window.is_window_active() {
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
        Self {
            link,
            page: Page::General,
            toolbar: cx.focus_handle().tab_stop(true),
            recorders,
            recording: None,
            refusal: None,
            moving: false,
            scroll: ScrollHandle::new(),
            page_height: Rc::default(),
            fitted: Rc::default(),
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
            self.page = page;
            self.scroll.set_offset(point(px(0.), px(0.)));
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
        self.link.send(Change::Shortcut(which, shortcut), cx);
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
            .child(chord_face(face, p))
            .into_any_element()
    }

    /// The lines under a shortcut field: while it listens, how to answer it; after a
    /// refusal, why.
    fn shortcut_notes(&self, which: Shortcut, s: &Snapshot, p: Palette) -> Vec<AnyElement> {
        let mut notes = Vec::new();
        if self.recording == Some(which) {
            notes.push(help(
                "Press the new shortcut. Esc cancels; ⌫ turns it off.",
                p,
            ));
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
            s.theme,
            p,
            self.sender(Change::Theme),
        );

        let [toggle, new_note] = &s.shortcuts;
        let mut toggle_line = vec![self.shortcut_field(Shortcut::Toggle, toggle, p, cx)];
        if toggle != DEFAULT_SHORTCUT && self.recording != Some(Shortcut::Toggle) {
            toggle_line.push(
                button(
                    "reset-shortcut",
                    "Reset",
                    p,
                    self.on_click(Change::Shortcut(Shortcut::Toggle, DEFAULT_SHORTCUT.into())),
                )
                .into_any_element(),
            );
        }
        let mut toggle_lines = vec![line(toggle_line)];
        toggle_lines.extend(self.shortcut_notes(Shortcut::Toggle, s, p));

        let mut new_note_line = vec![self.shortcut_field(Shortcut::NewNote, new_note, p, cx)];
        if !new_note.is_empty() && self.recording != Some(Shortcut::NewNote) {
            new_note_line.push(
                button(
                    "clear-new-note-shortcut",
                    "Clear",
                    p,
                    self.on_click(Change::Shortcut(Shortcut::NewNote, String::new())),
                )
                .into_any_element(),
            );
        }
        let mut new_note_lines = vec![line(new_note_line)];
        new_note_lines.extend(self.shortcut_notes(Shortcut::NewNote, s, p));
        new_note_lines.push(help("Both work from any app.", p));

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
        startup.push(help(
            "Markraft stays in the menu bar while the note is hidden.",
            p,
        ));

        vec![
            row(
                Some("Appearance"),
                vec![line(vec![theme.into_any_element()])],
                p,
            ),
            divider(p),
            row(Some("Show and hide"), toggle_lines, p),
            row(Some("New note"), new_note_lines, p),
            divider(p),
            row(
                Some("Note window"),
                vec![
                    checkbox(
                        "always-on-top",
                        "Keep above other windows",
                        s.always_on_top,
                        false,
                        p,
                        self.sender(Change::AlwaysOnTop),
                    )
                    .into_any_element(),
                    checkbox(
                        "hide-on-deactivate",
                        "Hide when another app is used",
                        s.hide_on_deactivate,
                        false,
                        p,
                        self.sender(Change::HideOnDeactivate),
                    )
                    .into_any_element(),
                ],
                p,
            ),
            row(Some("Startup"), startup, p),
        ]
    }

    fn editor(&self, s: &Snapshot, p: Palette) -> Vec<Div> {
        let range = Preferences::TEXT_SIZES;
        let size = s.text_size;
        let link = self.link.clone();
        let mut size_line = vec![
            stepper(
                "text-size",
                format!("{size:.0} pt"),
                (size > *range.start(), size < *range.end()),
                p,
                move |delta, _, cx| link.send(Change::TextSize(size + delta as f32), cx),
            )
            .into_any_element(),
        ];
        if size != Preferences::DEFAULT_TEXT_SIZE {
            size_line.push(
                button(
                    "reset-text-size",
                    "Default",
                    p,
                    self.on_click(Change::TextSize(Preferences::DEFAULT_TEXT_SIZE)),
                )
                .into_any_element(),
            );
        }
        vec![
            row(
                Some("Text size"),
                vec![
                    line(size_line),
                    help("⌘+ and ⌘− change it from the note; ⌘0 puts it back.", p),
                ],
                p,
            ),
            divider(p),
            row(
                Some("Editing"),
                vec![
                    checkbox(
                        "vim-mode",
                        "Vim mode",
                        s.vim,
                        false,
                        p,
                        self.sender(Change::VimMode),
                    )
                    .into_any_element(),
                ],
                p,
            ),
            row(
                Some("Window height"),
                vec![
                    checkbox(
                        "auto-height",
                        "Grow with the note",
                        s.auto_height,
                        false,
                        p,
                        self.sender(Change::AutoHeight),
                    )
                    .into_any_element(),
                    help("Resizing the note by hand turns this off.", p),
                ],
                p,
            ),
            divider(p),
            row(
                Some("Web images"),
                vec![
                    checkbox(
                        "remote-images",
                        "Load images linked from the web",
                        s.remote_images,
                        false,
                        p,
                        self.sender(Change::RemoteImages),
                    )
                    .into_any_element(),
                    help("Loading one tells the server it comes from.", p),
                ],
                p,
            ),
        ]
    }

    fn notes(&self, s: &Snapshot, p: Palette) -> Vec<Div> {
        let click = |change: Change| self.on_click(change);
        let folder = match &s.folder {
            Some(path) => vec![
                value(
                    path.file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.display().to_string()),
                    false,
                    p,
                ),
                value(home_relative(path), true, p),
                line(vec![
                    button("change-folder", "Change…", p, click(Change::ChooseFolder))
                        .into_any_element(),
                    button(
                        "reveal-folder",
                        "Show in Finder",
                        p,
                        click(Change::RevealFolder),
                    )
                    .into_any_element(),
                ]),
            ],
            None => vec![
                value("None — editing individual files", false, p),
                line(vec![
                    button(
                        "change-folder",
                        "Open Folder…",
                        p,
                        click(Change::ChooseFolder),
                    )
                    .into_any_element(),
                ]),
            ],
        };
        let mut folder = folder;
        folder.push(help(
            "Documents/Markraft by default. Any Markdown folder works, an Obsidian vault \
             included.",
            p,
        ));
        let mut page = vec![row(Some("Notes folder"), folder, p)];

        let location = |id: &'static str,
                        (place, custom): &(String, bool),
                        change: Change,
                        reset: Change,
                        refused: &Option<String>| {
            let mut controls = vec![
                value(place.clone(), false, p),
                button(id, "Change…", p, click(change)).into_any_element(),
            ];
            if *custom {
                controls.push(
                    button(
                        SharedString::from(format!("reset-{id}")),
                        "Reset",
                        p,
                        click(reset),
                    )
                    .into_any_element(),
                );
            }
            let mut lines = vec![line(controls)];
            if let Some(refused) = refused {
                lines.push(error(refused.clone(), p));
            }
            lines
        };
        if let (Some(new_notes), Some(images)) = (&s.new_notes, &s.images) {
            page.push(divider(p));
            page.push(row(
                Some("New notes"),
                location(
                    "new-note-location",
                    new_notes,
                    Change::NewNoteLocation,
                    Change::ResetNewNoteLocation,
                    &s.errors.new_notes,
                ),
                p,
            ));
            let mut images = location(
                "image-location",
                images,
                Change::ImageLocation,
                Change::ResetImageLocation,
                &s.errors.images,
            );
            images.push(help("Both are folders inside the notes folder.", p));
            page.push(row(Some("Images"), images, p));
        }
        page
    }
}

impl Render for SettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(snapshot) = self
            .link
            .app
            .upgrade()
            .map(|app| app.read(cx).settings_snapshot(self.page == Page::General))
        else {
            return div();
        };
        let p = Palette::new(snapshot.dark);
        let rows = match self.page {
            Page::General => self.general(&snapshot, p, cx),
            Page::Editor => self.editor(&snapshot, p),
            Page::Notes => self.notes(&snapshot, p),
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
        // Room on the right.
        assert_eq!(
            placement(SCREEN, note(100., 200.), EXTENT),
            point(px(596.), px(200.))
        );
        // Only room on the left.
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
        assert_eq!(Page::Notes.step(true), Page::Notes);
        assert_eq!(Page::Notes.step(false), Page::Editor);
    }
}
