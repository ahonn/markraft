//! The Settings window: an ordinary, resizable window of its own beside the floating
//! note. Pages are chosen from a row of icons under the title, the way macOS settings
//! windows have it; each page is cards of rows, laid out like cmdspace's.
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
use controls::{
    ChordFace, Palette, Row, button, chord_face, metrics::*, section, segmented, switch,
};
use gpui_base::{Tab, Tabs};
use shortcut::Recorded;

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

const WIDTH: f32 = 560.;
const HEIGHT: f32 = 480.;
const MIN_WIDTH: f32 = 480.;
const MIN_HEIGHT: f32 = 360.;
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
    shortcut: Option<String>,
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
    /// Empty turns the global shortcut off.
    Shortcut(String),
    /// The recorder opening or closing: the running shortcut is let go of meanwhile.
    Recording(bool),
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
    shortcut: String,
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
            shortcut: self.library.preferences.hotkey.clone(),
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
            Change::Shortcut(shortcut) => {
                if let Some(platform) = &mut self.platform {
                    match platform.set_shortcut(&shortcut) {
                        Ok(()) => {
                            self.library.preferences.hotkey = shortcut;
                            self.settings_errors.shortcut = None;
                            self.feedback.set_platform_error(None);
                            self.schedule_save(cx);
                        }
                        Err(error) => self.settings_errors.shortcut = Some(error),
                    }
                }
            }
            Change::Recording(true) => {
                self.settings_errors.shortcut = None;
                if let Some(platform) = &mut self.platform {
                    platform.suspend_shortcut();
                }
            }
            Change::Recording(false) => {
                if let Some(platform) = &mut self.platform
                    && let Err(error) = platform.resume_shortcut()
                {
                    self.settings_errors.shortcut = Some(error);
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
        }
        cx.notify();
    }

    /// The window is gone: the shortcut it may have let go of comes back, and the
    /// foreground goes back to the app that had it if the note is not on screen.
    fn settings_closed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings_errors = SettingsErrors::default();
        if let Some(platform) = &mut self.platform {
            if let Err(error) = platform.resume_shortcut() {
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
        is_resizable: true,
        is_minimizable: true,
        window_min_size: Some(size(px(MIN_WIDTH), px(MIN_HEIGHT))),
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
    recorder: FocusHandle,
    recording: bool,
    /// Why the chord just pressed cannot be a global shortcut. Said under the field,
    /// which stays open for another try.
    refusal: Option<&'static str>,
    /// Set between a press on the title band or toolbar and the first drag; the move blocks
    /// until the drag ends, so it must not start on the press itself.
    moving: bool,
    scroll: ScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl SettingsView {
    fn new(app: Entity<NotesApp>, link: Link, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let recorder = cx.focus_handle().tab_stop(true);
        let own = window.window_handle();
        let this = cx.entity().downgrade();
        let subscriptions = vec![
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
            cx.on_focus_out(&recorder, window, |view, _, _, cx| view.stop_recording(cx)),
            cx.observe_window_activation(window, |view, window, cx| {
                if !window.is_window_active() {
                    view.stop_recording(cx);
                }
            }),
            // However the view goes, a shortcut it let go of comes back.
            cx.on_release(|view, cx| {
                if view.recording {
                    view.link.send(Change::Recording(false), cx);
                }
            }),
        ];
        Self {
            link,
            page: Page::General,
            toolbar: cx.focus_handle().tab_stop(true),
            recorder,
            recording: false,
            refusal: None,
            moving: false,
            scroll: ScrollHandle::new(),
            _subscriptions: subscriptions,
        }
    }

    fn send(&self, change: Change, cx: &mut App) {
        self.link.send(change, cx);
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

    /// One keystroke while the recorder listens; false leaves it to the window.
    fn intercept(&mut self, keystroke: &Keystroke, cx: &mut Context<Self>) -> bool {
        if !self.recording {
            return false;
        }
        match shortcut::record(keystroke) {
            Recorded::Waiting => {}
            Recorded::Cancelled => self.stop_recording(cx),
            Recorded::Cleared => self.commit(String::new(), cx),
            Recorded::Bound(chord) => self.commit(chord, cx),
            Recorded::Refused(reason) => self.refusal = Some(reason),
        }
        cx.notify();
        true
    }

    fn start_recording(&mut self, cx: &mut Context<Self>) {
        if !self.recording {
            self.recording = true;
            self.refusal = None;
            self.send(Change::Recording(true), cx);
            cx.notify();
        }
    }

    fn stop_recording(&mut self, cx: &mut Context<Self>) {
        if self.recording {
            self.recording = false;
            self.refusal = None;
            self.send(Change::Recording(false), cx);
            cx.notify();
        }
    }

    /// Registering the chord is also what takes the suspended shortcut back.
    fn commit(&mut self, shortcut: String, cx: &mut Context<Self>) {
        self.recording = false;
        self.refusal = None;
        self.send(Change::Shortcut(shortcut), cx);
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

    fn general(&self, s: &Snapshot, p: Palette, cx: &mut Context<Self>) -> Vec<Div> {
        let appearance = Row::new("Theme")
            .description("Follow the system, or keep one look.")
            .trailing(segmented(
                "appearance",
                "Appearance",
                &[("Auto", None), ("Light", Some(false)), ("Dark", Some(true))],
                s.theme,
                p,
                self.sender(Change::Theme),
            ))
            .render(p);

        let bound = !s.shortcut.trim().is_empty();
        let face = if self.recording {
            ChordFace::Recording
        } else if bound {
            ChordFace::Bound(shortcut::glyphs(&s.shortcut))
        } else {
            ChordFace::Unbound
        };
        let field = div()
            .id("shortcut-field")
            .track_focus(&self.recorder)
            .role(Role::Button)
            .aria_label(if bound {
                format!("Global shortcut, {}", s.shortcut)
            } else {
                "Global shortcut, none".to_owned()
            })
            .cursor_pointer()
            .on_click(cx.listener(|view, _, window, cx| {
                window.focus(&view.recorder, cx);
                view.start_recording(cx);
            }))
            .child(chord_face(face, p));
        let hint = if self.recording {
            Some("Press the new shortcut. Esc cancels; ⌫ turns it off.")
        } else {
            None
        };
        let error = self
            .refusal
            .map(ToOwned::to_owned)
            .or_else(|| s.errors.shortcut.clone());
        let mut shortcut = Row::new("Show and hide Markraft")
            .description("Works from any app.")
            .hint(hint)
            .error(error);
        if s.shortcut != DEFAULT_SHORTCUT && !self.recording {
            shortcut = shortcut.trailing(button(
                "reset-shortcut",
                "Reset",
                p,
                self.on_click(Change::Shortcut(DEFAULT_SHORTCUT.into())),
            ));
        }
        let shortcut = shortcut.trailing(field).render(p);

        let login = Row::new("Launch at login")
            .description("Keep Markraft in the menu bar after you log in.")
            .error(s.errors.login.clone())
            .disabled(s.login.is_none())
            .trailing(switch(
                "launch-at-login",
                "Launch at login",
                s.login.unwrap_or(false),
                p,
                cx,
                self.sender(Change::LaunchAtLogin),
            ))
            .render(p);

        vec![
            section("Appearance", vec![appearance], None, p),
            section("Global shortcut", vec![shortcut], None, p),
            section(
                "Startup",
                vec![login],
                Some(
                    "Settings are kept on this Mac. Closing the note keeps Markraft running."
                        .into(),
                ),
                p,
            ),
        ]
    }

    fn editor(&self, s: &Snapshot, p: Palette, cx: &mut Context<Self>) -> Vec<Div> {
        let vim = Row::new("Vim mode")
            .description("Modal editing in every note.")
            .trailing(switch(
                "vim-mode",
                "Vim mode",
                s.vim,
                p,
                cx,
                self.sender(Change::VimMode),
            ))
            .render(p);
        let grow = Row::new("Grow with content")
            .description("The note's window grows with what you write.")
            .trailing(switch(
                "auto-height",
                "Grow with content",
                s.auto_height,
                p,
                cx,
                self.sender(Change::AutoHeight),
            ))
            .render(p);
        let remote = Row::new("Load remote images")
            .description(
                "Show pictures a note links from the web. Opening the note tells their server.",
            )
            .trailing(switch(
                "remote-images",
                "Load remote images",
                s.remote_images,
                p,
                cx,
                self.sender(Change::RemoteImages),
            ))
            .render(p);
        vec![
            section(
                "Editing",
                vec![vim, grow],
                Some("Resizing the note by hand turns growing off.".into()),
                p,
            ),
            section("Images", vec![remote], None, p),
        ]
    }

    fn notes(&self, s: &Snapshot, p: Palette) -> Vec<Div> {
        let click = |change: Change| self.on_click(change);
        let folder = match &s.folder {
            Some(path) => Row::new(
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string()),
            )
            .description(home_relative(path))
            .single_line()
            .trailing(button(
                "reveal-folder",
                "Show in Finder",
                p,
                click(Change::RevealFolder),
            ))
            .trailing(button(
                "change-folder",
                "Change…",
                p,
                click(Change::ChooseFolder),
            )),
            None => Row::new("No folder")
                .description("Editing individual files.")
                .trailing(button(
                    "change-folder",
                    "Open Folder…",
                    p,
                    click(Change::ChooseFolder),
                )),
        }
        .render(p);
        let mut sections = vec![section(
            "Notes folder",
            vec![folder],
            Some("Documents/Markraft by default. Any Markdown folder works, an Obsidian vault included.".into()),
            p,
        )];

        let location = |label: &'static str,
                        id: &'static str,
                        (place, custom): &(String, bool),
                        change: Change,
                        reset: Change,
                        error: &Option<String>| {
            let mut row = Row::new(label)
                .description(place.clone())
                .single_line()
                .error(error.clone());
            if *custom {
                row = row.trailing(button(
                    SharedString::from(format!("reset-{id}")),
                    "Reset",
                    p,
                    click(reset),
                ));
            }
            row.trailing(button(id, "Change…", p, click(change)))
                .render(p)
        };
        if let (Some(new_notes), Some(images)) = (&s.new_notes, &s.images) {
            sections.push(section(
                "Locations",
                vec![
                    location(
                        "New notes",
                        "new-note-location",
                        new_notes,
                        Change::NewNoteLocation,
                        Change::ResetNewNoteLocation,
                        &s.errors.new_notes,
                    ),
                    location(
                        "Images",
                        "image-location",
                        images,
                        Change::ImageLocation,
                        Change::ResetImageLocation,
                        &s.errors.images,
                    ),
                ],
                Some("Both are folders inside the notes folder.".into()),
                p,
            ));
        }
        sections
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
        let sections = match self.page {
            Page::General => self.general(&snapshot, p, cx),
            Page::Editor => self.editor(&snapshot, p, cx),
            Page::Notes => self.notes(&snapshot, p),
        };
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
                            .flex()
                            .flex_col()
                            .w_full()
                            .max_w(px(PAGE_MAX_WIDTH + PAGE_PADDING * 2.))
                            .mx_auto()
                            .px(px(PAGE_PADDING))
                            .pt(px(PAGE_TOP))
                            .pb(px(PAGE_BOTTOM))
                            .children(sections),
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
