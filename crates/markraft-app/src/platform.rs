//! macOS integration owned by the application, not the reusable editor.
//!
//! Create and use this object on AppKit's main thread. GPUI owns the native
//! window; native pointers below are borrowed only for the duration of a call.

use crate::locale::Message;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState, hotkey::HotKey};
use objc2::{
    AllocAnyThread, MainThreadMarker, class,
    encode::{Encode, Encoding},
    msg_send,
    rc::Retained,
    runtime::{AnyClass, AnyObject, Bool},
};
use objc2_app_kit::{NSBitmapImageRep, NSImage};
use objc2_foundation::{NSData, NSSize};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::{ffi::CStr, path::Path, ptr, str::FromStr};
use tray_icon::{
    Icon, TrayIcon, TrayIconBuilder,
    menu::{Menu, MenuId, MenuItem, PredefinedMenuItem},
};

// Minimal AppKit geometry for struct-returning messages; objc2 checks the encoding.
#[repr(C)]
#[derive(Clone, Copy)]
struct NSPoint {
    x: f64,
    y: f64,
}
unsafe impl Encode for NSPoint {
    const ENCODING: Encoding = Encoding::Struct("CGPoint", &[f64::ENCODING, f64::ENCODING]);
}
#[repr(C)]
#[derive(Clone, Copy)]
struct NSRect {
    origin: NSPoint,
    size: NSPoint,
}
unsafe impl Encode for NSRect {
    const ENCODING: Encoding = Encoding::Struct(
        "CGRect",
        &[
            NSPoint::ENCODING,
            Encoding::Struct("CGSize", &[f64::ENCODING, f64::ENCODING]),
        ],
    );
}

/// The version this copy says it is, and its build when that says something more:
/// from the bundle, or from the crate when running outside one.
pub fn app_version() -> (String, Option<String>) {
    use objc2_foundation::{NSBundle, NSString};
    let bundle = NSBundle::mainBundle();
    let value = |key: &str| {
        bundle
            .objectForInfoDictionaryKey(&NSString::from_str(key))
            .and_then(|value| value.downcast::<NSString>().ok())
            .map(|value| value.to_string())
    };
    let version =
        value("CFBundleShortVersionString").unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_owned());
    let build = value("CFBundleVersion").filter(|build| *build != version);
    (version, build)
}

/// The macOS version, such as `26.0.1`, read from the kernel.
pub fn system_version() -> Option<String> {
    let name = c"kern.osproductversion";
    let mut buffer = [0u8; 64];
    let mut length = buffer.len();
    let status = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            buffer.as_mut_ptr().cast(),
            &mut length,
            ptr::null_mut(),
            0,
        )
    };
    (status == 0).then(|| {
        CStr::from_bytes_until_nul(&buffer[..length.min(buffer.len())])
            .map(|version| version.to_string_lossy().into_owned())
            .unwrap_or_default()
    })
}

/// What a bug report needs to know about this copy and this Mac.
pub fn debug_info() -> String {
    let (version, build) = app_version();
    let build = build.map(|build| format!(" ({build})")).unwrap_or_default();
    let system = system_version().unwrap_or_else(|| "unknown".to_owned());
    format!(
        "Markraft {version}{build}\nmacOS {system}, {}",
        std::env::consts::ARCH
    )
}

pub use markraft_workspace::platform::{PlatformEvent, Shortcut};
pub struct Platform {
    _tray: TrayIcon,
    hotkeys: GlobalHotKeyManager,
    /// The registered chord of each [`Shortcut`], by its index.
    shortcuts: [Option<HotKey>; 3],
    /// Whether the shortcuts have been let go of while the Settings window records a
    /// new one, so the chord being recorded is not taken by a running shortcut.
    suspended: bool,
    menu_actions: Vec<(MenuId, PlatformEvent)>,
    menu_items: Vec<(MenuItem, &'static str)>,
    // Retained NSRunningApplication; None when no previous app is known.
    previous_app: Option<Retained<AnyObject>>,
}

impl Platform {
    pub fn new() -> Result<Self, Message> {
        let hotkeys = GlobalHotKeyManager::new().map_err(menu_bar_failure)?;
        let i18n = crate::locale::I18n::english();
        let menu = Menu::new();
        let toggle = MenuItem::new(i18n.text("menu.show-hide"), true, None);
        let new_note = MenuItem::new(i18n.text("menu.new-note"), true, None);
        let settings = MenuItem::new(i18n.text("menu.settings"), true, None);
        #[cfg(feature = "direct-distribution")]
        let updates = MenuItem::new(i18n.text("menu.updates"), true, None);
        // An accessory app has no menu bar of its own, so a menu bar's Help menu
        // comes down to this. Debug info and logs are in the command palette.
        let report = MenuItem::new(i18n.text("menu.report"), true, None);
        let quit = MenuItem::new(i18n.text("menu.quit"), true, None);
        menu.append_items(&[
            &toggle,
            &new_note,
            &PredefinedMenuItem::separator(),
            &settings,
            #[cfg(feature = "direct-distribution")]
            &updates,
            &report,
            &PredefinedMenuItem::separator(),
            &quit,
        ])
        .map_err(menu_bar_failure)?;
        let menu_actions = vec![
            (toggle.id().clone(), PlatformEvent::Toggle),
            (new_note.id().clone(), PlatformEvent::NewNote),
            (settings.id().clone(), PlatformEvent::Settings),
            #[cfg(feature = "direct-distribution")]
            (updates.id().clone(), PlatformEvent::CheckForUpdates),
            (report.id().clone(), PlatformEvent::ReportIssue),
            (quit.id().clone(), PlatformEvent::Quit),
        ];
        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_icon(blank_menu_bar_icon()?)
            .with_tooltip("Markraft")
            .build()
            .map_err(menu_bar_failure)?;
        show_menu_bar_image(&tray)?;
        let mut platform = Self {
            _tray: tray,
            hotkeys,
            shortcuts: [None; 3],
            suspended: false,
            menu_actions,
            menu_items: vec![
                (toggle, "menu.show-hide"),
                (new_note, "menu.new-note"),
                (settings, "menu.settings"),
                #[cfg(feature = "direct-distribution")]
                (updates, "menu.updates"),
                (report, "menu.report"),
                (quit, "menu.quit"),
            ],
            previous_app: None,
        };
        platform.remember_frontmost_app();
        // Accessory apps remain available through the status item and shortcut,
        // and can activate their panel without occupying the Dock.
        unsafe {
            let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            let _: Bool = msg_send![app, setActivationPolicy: 1_isize];
        }
        Ok(platform)
    }

    /// Refresh labels without replacing menu IDs or their action bindings.
    pub fn set_locale(&mut self, i18n: &crate::locale::I18n) {
        for (item, key) in &self.menu_items {
            item.set_text(i18n.text(key));
        }
    }

    /// Register before replacing, so an unavailable shortcut preserves the old one.
    /// An empty string explicitly disables the global shortcut.
    pub fn set_shortcut(&mut self, which: Shortcut, shortcut: &str) -> Result<(), Message> {
        // The swap below assumes the current shortcuts are registered.
        self.resume_shortcuts()?;
        let next = if shortcut.trim().is_empty() {
            None
        } else {
            Some(HotKey::from_str(shortcut).map_err(|error| {
                log::warn!("{shortcut} is not a hotkey: {error}");
                Message::new("error.shortcut-invalid").arg("shortcut", shortcut.to_owned())
            })?)
        };
        let current = self.shortcuts[which as usize];
        if current == next {
            return Ok(());
        }
        if next.is_some()
            && Shortcut::ALL
                .iter()
                .any(|other| *other != which && self.shortcuts[*other as usize] == next)
        {
            return Err(
                Message::new("error.shortcut-duplicate").arg("shortcut", shortcut.to_owned())
            );
        }
        if let Some(next) = next {
            self.hotkeys.register(next).map_err(|error| {
                log::warn!("{shortcut} could not be registered: {error}");
                Message::new("error.shortcut-unavailable").arg("shortcut", shortcut.to_owned())
            })?;
        }
        if let Some(previous) = current
            && let Err(error) = self.hotkeys.unregister(previous)
        {
            if let Some(next) = next {
                let _ = self.hotkeys.unregister(next);
            }
            log::warn!("a shortcut could not be released: {error}");
            return Err(Message::new("error.shortcut-release"));
        }
        self.shortcuts[which as usize] = next;
        Ok(())
    }

    /// Let go of the shortcuts without forgetting them, so a recorder can hear a chord
    /// any of them holds.
    pub fn suspend_shortcuts(&mut self) {
        if self.suspended {
            return;
        }
        for shortcut in self.shortcuts.into_iter().flatten() {
            if let Err(error) = self.hotkeys.unregister(shortcut) {
                log::warn!("a shortcut could not be suspended: {error}");
            }
        }
        self.suspended = true;
    }

    /// Take the suspended shortcuts back. Another app may have claimed one meanwhile.
    pub fn resume_shortcuts(&mut self) -> Result<(), Message> {
        if !std::mem::take(&mut self.suspended) {
            return Ok(());
        }
        let mut lost = false;
        for slot in &mut self.shortcuts {
            if let Some(shortcut) = *slot
                && let Err(error) = self.hotkeys.register(shortcut)
            {
                log::warn!("a shortcut could not be resumed: {error}");
                *slot = None;
                lost = true;
            }
        }
        if lost {
            return Err(Message::new("error.shortcut-taken"));
        }
        Ok(())
    }

    /// Whether Markraft is the active app, rather than one of its windows being key.
    pub fn app_is_active(&self) -> bool {
        unsafe {
            let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            let active: Bool = msg_send![app, isActive];
            active.as_bool()
        }
    }

    /// Keep the note above other apps' windows, or let it sit among them.
    pub fn set_always_on_top(&self, window: &gpui::Window, on_top: bool) -> Result<(), Message> {
        let native = native_window(window)?;
        unsafe {
            let _: () = msg_send![native, setFloatingPanel: Bool::new(on_top)];
            // NSFloatingWindowLevel, or NSNormalWindowLevel.
            let _: () = msg_send![native, setLevel: if on_top { 3_isize } else { 0_isize }];
        }
        Ok(())
    }

    /// Put the note on every Space, full-screen ones included, or leave it on the one
    /// it was opened on.
    pub fn set_all_spaces(&self, window: &gpui::Window, all: bool) -> Result<(), Message> {
        // NSWindowCollectionBehaviorCanJoinAllSpaces and …FullScreenAuxiliary.
        const ALL_SPACES: usize = 1 << 0 | 1 << 8;
        let native = native_window(window)?;
        unsafe {
            let behavior: usize = msg_send![native, collectionBehavior];
            let behavior = if all {
                behavior | ALL_SPACES
            } else {
                behavior & !ALL_SPACES
            };
            let _: () = msg_send![native, setCollectionBehavior: behavior];
        }
        Ok(())
    }

    /// Read the operating system's state instead of trusting a saved preference.
    pub fn launch_at_login_enabled(&self) -> bool {
        main_app_service().is_ok_and(|service| {
            let status: isize = unsafe { msg_send![&*service, status] };
            status == 1 // SMAppServiceStatusEnabled
        })
    }

    pub fn set_launch_at_login(&mut self, enabled: bool) -> Result<(), Message> {
        let service = main_app_service()?;
        let status: isize = unsafe { msg_send![&*service, status] };
        if (enabled && status == 1) || (!enabled && matches!(status, 0 | 3)) {
            return Ok(());
        }
        if enabled {
            ensure_installed_bundle()?;
            if status == 2 {
                return Err(login_approval_message());
            }
        }
        // NSError is autoreleased by AppKit. Copy its description within this
        // call; neither the error pointer nor its UTF-8 buffer escapes.
        let mut error: *mut AnyObject = ptr::null_mut();
        let success: Bool = unsafe {
            if enabled {
                msg_send![&*service, registerAndReturnError: &mut error as *mut *mut AnyObject]
            } else {
                msg_send![&*service, unregisterAndReturnError: &mut error as *mut *mut AnyObject]
            }
        };
        let resulting_status: isize = unsafe { msg_send![&*service, status] };
        if enabled && resulting_status == 2 {
            return Err(login_approval_message());
        }
        if success == Bool::NO {
            let detail = if error.is_null() {
                Message::new("error.native-no-detail")
            } else {
                let description: *mut AnyObject = unsafe { msg_send![error, localizedDescription] };
                ns_string_text(description)
                    .map(Message::from)
                    .unwrap_or_else(|| Message::new("error.native-unknown"))
            };
            return Err(Message::new(if enabled {
                "error.login-enable"
            } else {
                "error.login-disable"
            })
            .arg("detail", detail));
        }
        if (enabled && resulting_status != 1) || (!enabled && resulting_status == 1) {
            return Err(Message::new("error.login-not-applied"));
        }
        Ok(())
    }

    /// Use with GPUI's `WindowKind::Floating`, which creates an NSPanel.
    pub fn configure_window(&mut self, window: &mut gpui::Window) -> Result<(), Message> {
        let native = native_window(window)?;
        let view = native_view(window)?;
        unsafe {
            // A window that grows to fit its note is resized before GPUI draws at
            // the new size, and a frame drawn for one size can reach the screen
            // in the other's bounds. Pinned under the title bar, such a frame
            // shows its top at its own scale, which is what the note shows there
            // either way, rather than squeezed or stretched to fit.
            let _: () = msg_send![
                view,
                setLayerContentsPlacement: NS_VIEW_LAYER_CONTENTS_PLACEMENT_TOP
            ];
            // NSWindowCollectionBehaviorCanJoinAllSpaces | FullScreenAuxiliary.
            let _: () = msg_send![native, setCollectionBehavior: (1_usize | (1 << 8))];
            let _: () = msg_send![native, setHidesOnDeactivate: Bool::NO];
            let _: () = msg_send![native, setFloatingPanel: Bool::YES];
            let _: () = msg_send![native, setBecomesKeyOnlyIfNeeded: Bool::NO];
            let _: () = msg_send![native, setLevel: 3_isize]; // NSFloatingWindowLevel
            let _: () = msg_send![native, setExcludedFromWindowsMenu: Bool::YES];
            // Keep only close. Window sizing remains available through edge dragging.
            for button_kind in [1_usize, 2_usize] {
                // NSWindowMiniaturizeButton, NSWindowZoomButton.
                let button: *mut AnyObject = msg_send![native, standardWindowButton: button_kind];
                if !button.is_null() {
                    let _: () = msg_send![button, setEnabled: Bool::NO];
                    let _: () = msg_send![button, setHidden: Bool::YES];
                }
            }
        }
        Ok(())
    }

    /// Whether the pointer is over the window, independent of key or active state.
    /// GPUI hover state follows delivered mouse events; the chrome needs the
    /// pointer's actual position even while another application is active.
    pub fn pointer_inside(&self, window: &gpui::Window) -> bool {
        let Ok(native) = native_window(window) else {
            return true;
        };
        unsafe {
            let visible: Bool = msg_send![native, isVisible];
            let view: *mut AnyObject = msg_send![native, contentView];
            if !visible.as_bool() || view.is_null() {
                return false;
            }
            // Both values are in the window's base coordinate system.
            let pointer: NSPoint = msg_send![native, mouseLocationOutsideOfEventStream];
            let frame: NSRect = msg_send![view, frame];
            pointer.x >= frame.origin.x
                && pointer.x < frame.origin.x + frame.size.x
                && pointer.y >= frame.origin.y
                && pointer.y < frame.origin.y + frame.size.y
        }
    }

    /// The system "Reduce motion" accessibility preference.
    pub fn system_reduce_motion() -> bool {
        unsafe {
            let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
            let reduce: Bool = msg_send![workspace, accessibilityDisplayShouldReduceMotion];
            reduce.as_bool()
        }
    }

    pub fn poll_events(&self) -> Vec<PlatformEvent> {
        let mut events = Vec::new();
        while let Ok(event) = GlobalHotKeyEvent::receiver().try_recv() {
            if event.state != HotKeyState::Pressed {
                continue;
            }
            if let Some(which) = Shortcut::ALL.into_iter().find(|which| {
                self.shortcuts[*which as usize].is_some_and(|key| key.id() == event.id)
            }) {
                events.push(match which {
                    Shortcut::Toggle => PlatformEvent::Toggle,
                    Shortcut::NewNote => PlatformEvent::NewNote,
                    Shortcut::DailyNote => PlatformEvent::DailyNote,
                });
            }
        }
        while let Ok(event) = tray_icon::menu::MenuEvent::receiver().try_recv() {
            if let Some((_, action)) = self.menu_actions.iter().find(|(id, _)| *id == event.id) {
                events.push(*action);
            }
        }
        events
    }

    /// Bring the note forward; with `follow_pointer`, onto the display the pointer is
    /// on, so it appears there rather than being seen to jump.
    pub fn show(&mut self, window: &mut gpui::Window, follow_pointer: bool) -> Result<(), Message> {
        self.remember_frontmost_app();
        let native = native_window(window)?;
        if follow_pointer {
            move_to_pointer_screen(native);
        }
        unsafe {
            let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            let _: () = msg_send![app, activateIgnoringOtherApps: Bool::YES];
            let _: () = msg_send![native, makeKeyAndOrderFront: ptr::null_mut::<AnyObject>()];
        }

        window.activate_window();
        diagnostics("window shown");
        Ok(())
    }

    /// Keep the GPUI window and editor entity alive, including selection/history.
    pub fn hide(&mut self, window: &mut gpui::Window) -> Result<(), Message> {
        let native = native_window(window)?;
        let was_key: Bool = unsafe { msg_send![native, isKeyWindow] };
        unsafe {
            let _: () = msg_send![native, orderOut: ptr::null_mut::<AnyObject>()];
        }
        if was_key.as_bool() {
            self.return_to_previous_app();
        }
        diagnostics("window hidden");
        Ok(())
    }

    /// Whether the window is on screen, rather than ordered out by [`Self::hide`].
    pub fn is_visible(&self, window: &gpui::Window) -> bool {
        native_window(window).is_ok_and(|native| {
            let visible: Bool = unsafe { msg_send![native, isVisible] };
            visible.as_bool()
        })
    }

    /// Hand the foreground back to the app that had it before Markraft took it.
    pub fn return_to_previous_app(&self) {
        let Some(previous) = &self.previous_app else {
            return;
        };
        unsafe {
            let terminated: Bool = msg_send![&**previous, isTerminated];
            if terminated == Bool::NO {
                // NSApplicationActivateIgnoringOtherApps. Restoring the app
                // does not reorder its windows or alter their selection.
                let _: Bool = msg_send![&**previous, activateWithOptions: 2_usize];
            }
        }
    }

    /// Note which app is in front, before a Markraft window takes the foreground.
    pub fn remember_frontmost_app(&mut self) {
        unsafe {
            let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
            let frontmost: *mut AnyObject = msg_send![workspace, frontmostApplication];
            if frontmost.is_null() {
                return;
            }
            let process_id: i32 = msg_send![frontmost, processIdentifier];
            if process_id as u32 != std::process::id() {
                self.previous_app = Retained::retain(frontmost);
            }
        }
    }
}

impl Drop for Platform {
    fn drop(&mut self) {
        for shortcut in self.shortcuts.into_iter().flatten() {
            let _ = self.hotkeys.unregister(shortcut);
        }
    }
}

/// Losing the native window reads the same to the user however it happened.
const NO_NATIVE_WINDOW: &str = "error.native-window";

/// The menu bar item and the global shortcut are one thing to the user, and a
/// failure to set them up leaves the notes themselves working.
fn menu_bar_failure(detail: impl std::fmt::Display) -> Message {
    log::warn!("the menu bar item could not be set up: {detail}");
    Message::new("error.menu-bar-unavailable")
}

/// NSViewLayerContentsPlacementTop: a frame of another size than the view is
/// drawn at its own scale against the view's top edge.
const NS_VIEW_LAYER_CONTENTS_PLACEMENT_TOP: isize = 4;

fn move_to_pointer_screen(native: *mut AnyObject) {
    unsafe {
        let pointer: NSPoint = msg_send![class!(NSEvent), mouseLocation];
        let screens: *mut AnyObject = msg_send![class!(NSScreen), screens];
        let count: usize = msg_send![screens, count];
        let contains = |frame: NSRect| {
            pointer.x >= frame.origin.x
                && pointer.x < frame.origin.x + frame.size.x
                && pointer.y >= frame.origin.y
                && pointer.y < frame.origin.y + frame.size.y
        };
        let Some(target) = (0..count)
            .map(|index| -> *mut AnyObject { msg_send![screens, objectAtIndex: index] })
            .find(|screen| contains(msg_send![*screen, frame]))
        else {
            return;
        };
        let current: *mut AnyObject = msg_send![native, screen];
        if current.is_null() || current == target {
            return;
        }
        let from: NSRect = msg_send![current, visibleFrame];
        let to: NSRect = msg_send![target, visibleFrame];
        let frame: NSRect = msg_send![native, frame];
        // AppKit measures from the bottom-left; the offset kept is from the top-left.
        let left = frame.origin.x - from.origin.x;
        let top = (from.origin.y + from.size.y) - (frame.origin.y + frame.size.y);
        let x = (to.origin.x + left)
            .min(to.origin.x + to.size.x - frame.size.x)
            .max(to.origin.x);
        let y = (to.origin.y + to.size.y - top - frame.size.y)
            .max(to.origin.y)
            .min(to.origin.y + to.size.y - frame.size.y);
        let _: () = msg_send![native, setFrameOrigin: NSPoint { x, y }];
    }
}

fn native_window(window: &gpui::Window) -> Result<*mut AnyObject, Message> {
    let view = native_view(window)?;
    let native: *mut AnyObject = unsafe { msg_send![view, window] };
    if native.is_null() {
        Err(Message::new(NO_NATIVE_WINDOW))
    } else {
        Ok(native)
    }
}

/// GPUI's own view, which draws the window: a subview of the content view.
fn native_view(window: &gpui::Window) -> Result<*mut AnyObject, Message> {
    let handle = HasWindowHandle::window_handle(window).map_err(|error| {
        log::warn!("the window handle is unavailable: {error}");
        Message::new(NO_NATIVE_WINDOW)
    })?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return Err(Message::new("error.native-window-control"));
    };
    Ok(handle.ns_view.as_ptr().cast::<AnyObject>())
}

// Dynamic class lookup keeps startup safe on macOS versions before 13, while
// linking the framework ensures that SMAppService is loaded on supported OSes.
#[link(name = "ServiceManagement", kind = "framework")]
unsafe extern "C" {}

fn main_app_service() -> Result<Retained<AnyObject>, Message> {
    let class =
        AnyClass::get(c"SMAppService").ok_or_else(|| Message::new("error.login-version"))?;
    let service: Option<Retained<AnyObject>> = unsafe { msg_send![class, mainAppService] };
    service.ok_or_else(|| Message::new("error.bundle-not-found"))
}

fn ensure_installed_bundle() -> Result<(), Message> {
    let bundle: *mut AnyObject = unsafe { msg_send![class!(NSBundle), mainBundle] };
    let path: *mut AnyObject = unsafe { msg_send![bundle, bundlePath] };
    let path = ns_string_text(path).ok_or_else(|| Message::new("error.bundle-location"))?;
    let path = Path::new(&path);
    let user_applications =
        std::env::var_os("HOME").map(|home| Path::new(&home).join("Applications"));
    let installed = path.starts_with("/Applications")
        || user_applications.is_some_and(|applications| path.starts_with(applications));
    if path.extension().is_none_or(|extension| extension != "app") || !installed {
        return Err(Message::new("error.install-for-login"));
    }
    Ok(())
}

fn login_approval_message() -> Message {
    Message::new("error.login-approval")
}

fn ns_string_text(string: *mut AnyObject) -> Option<String> {
    if string.is_null() {
        return None;
    }
    let text: *const std::ffi::c_char = unsafe { msg_send![string, UTF8String] };
    if text.is_null() {
        None
    } else {
        Some(
            unsafe { CStr::from_ptr(text) }
                .to_string_lossy()
                .into_owned(),
        )
    }
}

/// tray-icon takes one bitmap and lets AppKit resample it, which blurs either a
/// Retina or a non-Retina menu bar. It also sizes its click target from the image
/// it was given and never again, so it gets a blank of the final size and the
/// status item's button gets the real image afterwards.
fn blank_menu_bar_icon() -> Result<Icon, Message> {
    const PIXELS: u32 = MENU_BAR_IMAGE_POINTS as u32;
    Icon::from_rgba(vec![0; (PIXELS * PIXELS * 4) as usize], PIXELS, PIXELS)
        .map_err(menu_bar_failure)
}

fn show_menu_bar_image(tray: &TrayIcon) -> Result<(), Message> {
    let main_thread = MainThreadMarker::new()
        .ok_or_else(|| menu_bar_failure("the status item is set up off the main thread"))?;
    let button = tray
        .ns_status_item()
        .and_then(|item| item.button(main_thread))
        .ok_or_else(|| menu_bar_failure("the status item has no button"))?;
    let image = menu_bar_image()
        .ok_or_else(|| menu_bar_failure("the menu bar image could not be decoded"))?;
    button.setImage(Some(&image));
    Ok(())
}

const MENU_BAR_IMAGE_POINTS: f64 = 18.;

/// The parallel-cut M as a template image, which macOS tints for the menu bar's
/// appearance. Each representation is drawn on its own pixel grid and AppKit picks
/// one per display; each is rendered from the SVG of the same name in `assets/icon/`.
fn menu_bar_image() -> Option<Retained<NSImage>> {
    const REPRESENTATIONS: [&[u8]; 2] = [
        include_bytes!("../../../assets/icon/markraft-menubar.png"),
        include_bytes!("../../../assets/icon/markraft-menubar@2x.png"),
    ];
    let size = NSSize::new(MENU_BAR_IMAGE_POINTS, MENU_BAR_IMAGE_POINTS);
    let image = NSImage::initWithSize(NSImage::alloc(), size);
    for png in REPRESENTATIONS {
        let representation = NSBitmapImageRep::imageRepWithData(&NSData::with_bytes(png))?;
        // The point size is what makes the 36-pixel bitmap the 2x representation.
        representation.setSize(size);
        image.addRepresentation(&representation);
    }
    image.setTemplate(true);
    Some(image)
}

fn diagnostics(event: &str) {
    if std::env::var_os("MARKRAFT_DIAGNOSTICS").is_some() {
        log::warn!("{event}");
    }
}

impl markraft_workspace::platform::PlatformServices for Platform {
    fn set_locale(&mut self, i18n: &crate::locale::I18n) {
        Platform::set_locale(self, i18n)
    }
    fn configure_window(&mut self, window: &mut gpui::Window) -> Result<(), Message> {
        Platform::configure_window(self, window)
    }
    fn set_shortcut(&mut self, which: Shortcut, shortcut: &str) -> Result<(), Message> {
        Platform::set_shortcut(self, which, shortcut)
    }
    fn suspend_shortcuts(&mut self) {
        Platform::suspend_shortcuts(self)
    }
    fn resume_shortcuts(&mut self) -> Result<(), Message> {
        Platform::resume_shortcuts(self)
    }
    fn app_is_active(&self) -> bool {
        Platform::app_is_active(self)
    }
    fn set_always_on_top(&self, window: &gpui::Window, value: bool) -> Result<(), Message> {
        Platform::set_always_on_top(self, window, value)
    }
    fn set_all_spaces(&self, window: &gpui::Window, value: bool) -> Result<(), Message> {
        Platform::set_all_spaces(self, window, value)
    }
    fn launch_at_login_enabled(&self) -> bool {
        Platform::launch_at_login_enabled(self)
    }
    fn set_launch_at_login(&mut self, value: bool) -> Result<(), Message> {
        Platform::set_launch_at_login(self, value)
    }
    fn pointer_inside(&self, window: &gpui::Window) -> bool {
        Platform::pointer_inside(self, window)
    }
    fn poll_events(&self) -> Vec<PlatformEvent> {
        Platform::poll_events(self)
    }
    fn show(&mut self, window: &mut gpui::Window, follow: bool) -> Result<(), Message> {
        Platform::show(self, window, follow)
    }
    fn hide(&mut self, window: &mut gpui::Window) -> Result<(), Message> {
        Platform::hide(self, window)
    }
    fn is_visible(&self, window: &gpui::Window) -> bool {
        Platform::is_visible(self, window)
    }
    fn return_to_previous_app(&self) {
        Platform::return_to_previous_app(self)
    }
    fn remember_frontmost_app(&mut self) {
        Platform::remember_frontmost_app(self)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // The workspace crate builds without this repository's root, so it carries its
    // own copy of each image it draws. A copy must not fall behind the original.
    #[test]
    fn the_workspace_carries_the_same_icons_as_the_application() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for name in ["Markraft.png", "markraft-menubar.png"] {
            let original = std::fs::read(root.join("assets/icon").join(name)).unwrap();
            let copy = root
                .join("crates/markraft-workspace/assets/icon")
                .join(name);
            assert!(std::fs::read(copy).unwrap() == original, "{name}");
        }
    }

    #[test]
    fn debug_info_names_the_version_and_the_system() {
        let system = system_version().expect("the kernel reports a product version");
        assert!(system.starts_with(|c: char| c.is_ascii_digit()), "{system}");
        let info = debug_info();
        assert!(info.starts_with("Markraft "), "{info}");
        assert!(
            info.contains(&format!("macOS {system}, {}", std::env::consts::ARCH)),
            "{info}"
        );
    }

    // A re-rendered asset of the wrong size would otherwise only show up as a soft
    // or missing menu bar item.
    #[test]
    fn menu_bar_image_has_a_representation_for_each_display_scale() {
        let image = menu_bar_image().unwrap();
        assert!(image.isTemplate());
        let pixels: Vec<_> = image
            .representations()
            .iter()
            .map(|representation| (representation.pixelsWide(), representation.pixelsHigh()))
            .collect();
        assert_eq!(pixels, [(18, 18), (36, 36)]);
    }
}
