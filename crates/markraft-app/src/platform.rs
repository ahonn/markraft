//! macOS integration owned by the application, not the reusable editor.
//!
//! Create and use this object on AppKit's main thread. GPUI owns the native
//! window; native pointers below are borrowed only for the duration of a call.
pub(crate) mod symbols;

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
    menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem},
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

const LOGIN_ITEMS: &str = "System Settings → General → Login Items & Extensions";

/// Seconds this Mac's clock stands ahead of UTC, including whatever daylight saving is
/// in force. Timestamps are stored in UTC; a date shown to the user has to be the one
/// on their calendar, so it is read through this.
pub fn local_utc_offset() -> i64 {
    unsafe {
        let zone: *mut AnyObject = msg_send![class!(NSTimeZone), localTimeZone];
        if zone.is_null() {
            return 0;
        }
        let seconds: isize = msg_send![zone, secondsFromGMT];
        seconds as i64
    }
}

/// The two global shortcuts: one shows and hides the note, the other opens a new one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Shortcut {
    Toggle,
    NewNote,
}

impl Shortcut {
    const ALL: [Shortcut; 2] = [Shortcut::Toggle, Shortcut::NewNote];

    fn index(self) -> usize {
        self as usize
    }

    fn event(self) -> PlatformEvent {
        match self {
            Shortcut::Toggle => PlatformEvent::Toggle,
            Shortcut::NewNote => PlatformEvent::NewNote,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlatformEvent {
    Toggle,
    NewNote,
    Settings,
    CheckForUpdates,
    Quit,
}

pub struct Platform {
    _tray: TrayIcon,
    hotkeys: GlobalHotKeyManager,
    /// The registered chord of each [`Shortcut`], by its index.
    shortcuts: [Option<HotKey>; 2],
    /// Whether the shortcuts have been let go of while the Settings window records a
    /// new one, so the chord being recorded is not taken by a running shortcut.
    suspended: bool,
    menu_actions: Vec<(MenuId, PlatformEvent)>,
    // Retained NSRunningApplication; None when no previous app is known.
    previous_app: Option<Retained<AnyObject>>,
}

impl Platform {
    pub fn new() -> Result<Self, String> {
        let hotkeys = GlobalHotKeyManager::new().map_err(menu_bar_failure)?;
        let menu = Menu::new();
        let toggle = MenuItem::new("Show / Hide Notes", true, None);
        let new_note = MenuItem::new("New Note", true, None);
        let settings = MenuItem::new("Settings…", true, None);
        let updates = MenuItem::new("Check for Updates…", true, None);
        let quit = MenuItem::new("Quit Markraft", true, None);
        menu.append_items(&[
            &toggle,
            &new_note,
            &PredefinedMenuItem::separator(),
            &settings,
            &updates,
            &PredefinedMenuItem::separator(),
            &quit,
        ])
        .map_err(menu_bar_failure)?;
        let menu_actions = vec![
            (toggle.id().clone(), PlatformEvent::Toggle),
            (new_note.id().clone(), PlatformEvent::NewNote),
            (settings.id().clone(), PlatformEvent::Settings),
            (updates.id().clone(), PlatformEvent::CheckForUpdates),
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
            shortcuts: [None; 2],
            suspended: false,
            menu_actions,
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

    /// Register before replacing, so an unavailable shortcut preserves the old one.
    /// An empty string explicitly disables the global shortcut.
    pub fn set_shortcut(&mut self, which: Shortcut, shortcut: &str) -> Result<(), String> {
        // The swap below assumes the current shortcuts are registered.
        self.resume_shortcuts()?;
        let next = if shortcut.trim().is_empty() {
            None
        } else {
            Some(HotKey::from_str(shortcut).map_err(|error| {
                eprintln!("Markraft: {shortcut} is not a hotkey: {error}");
                format!(
                    "“{shortcut}” is not a shortcut Markraft understands. \
                     Try one like Alt+N or Ctrl+Shift+Space."
                )
            })?)
        };
        let current = self.shortcuts[which.index()];
        if current == next {
            return Ok(());
        }
        if next.is_some()
            && Shortcut::ALL
                .iter()
                .any(|other| *other != which && self.shortcuts[other.index()] == next)
        {
            return Err(format!(
                "“{shortcut}” is already Markraft's other shortcut. Choose a different one."
            ));
        }
        if let Some(next) = next {
            self.hotkeys.register(next).map_err(|error| {
                eprintln!("Markraft: {shortcut} could not be registered: {error}");
                format!(
                    "“{shortcut}” is not available — another app is probably using it. \
                     Choose a different shortcut."
                )
            })?;
        }
        if let Some(previous) = current
            && let Err(error) = self.hotkeys.unregister(previous)
        {
            if let Some(next) = next {
                let _ = self.hotkeys.unregister(next);
            }
            eprintln!("Markraft: a shortcut could not be released: {error}");
            return Err("Markraft could not release the shortcut it was using. \
                        Quit and reopen Markraft, then set it again."
                .into());
        }
        self.shortcuts[which.index()] = next;
        Ok(())
    }

    /// Let go of the shortcuts without forgetting them, so a recorder can hear a chord
    /// either of them holds.
    pub fn suspend_shortcuts(&mut self) {
        if self.suspended {
            return;
        }
        for shortcut in self.shortcuts.into_iter().flatten() {
            if let Err(error) = self.hotkeys.unregister(shortcut) {
                eprintln!("Markraft: a shortcut could not be suspended: {error}");
            }
        }
        self.suspended = true;
    }

    /// Take the suspended shortcuts back. Another app may have claimed one meanwhile.
    pub fn resume_shortcuts(&mut self) -> Result<(), String> {
        if !std::mem::take(&mut self.suspended) {
            return Ok(());
        }
        let mut lost = false;
        for slot in &mut self.shortcuts {
            if let Some(shortcut) = *slot
                && let Err(error) = self.hotkeys.register(shortcut)
            {
                eprintln!("Markraft: a shortcut could not be resumed: {error}");
                *slot = None;
                lost = true;
            }
        }
        if lost {
            return Err("A shortcut was taken by another app while it was being \
                        changed. Choose a different shortcut."
                .into());
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
    pub fn set_always_on_top(&self, window: &gpui::Window, on_top: bool) -> Result<(), String> {
        let native = native_window(window)?;
        unsafe {
            let _: () = msg_send![native, setFloatingPanel: Bool::new(on_top)];
            // NSFloatingWindowLevel, or NSNormalWindowLevel.
            let _: () = msg_send![native, setLevel: if on_top { 3_isize } else { 0_isize }];
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

    pub fn set_launch_at_login(&mut self, enabled: bool) -> Result<(), String> {
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
                "macOS did not provide an error description".to_owned()
            } else {
                let description: *mut AnyObject = unsafe { msg_send![error, localizedDescription] };
                ns_string_text(description).unwrap_or_else(|| "Unknown macOS error".to_owned())
            };
            return Err(format!(
                "Could not {} launch at login: {detail}. Use a signed Markraft app \
                 installed in Applications, and check {LOGIN_ITEMS}.",
                if enabled { "enable" } else { "disable" },
            ));
        }
        if (enabled && resulting_status != 1) || (!enabled && resulting_status == 1) {
            return Err(format!(
                "macOS has not applied the change. Check {LOGIN_ITEMS}."
            ));
        }
        Ok(())
    }

    /// Use with GPUI's `WindowKind::Floating`, which creates an NSPanel.
    pub fn configure_window(&mut self, window: &mut gpui::Window) -> Result<(), String> {
        let native = native_window(window)?;
        unsafe {
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

    /// Fade the native close button with window hover, independently of activation.
    pub fn set_traffic_lights_alpha(&self, window: &gpui::Window, alpha: f32, animated: bool) {
        let Ok(native) = native_window(window) else {
            return;
        };
        let alpha = f64::from(alpha.clamp(0., 1.));
        unsafe {
            let context_class = class!(NSAnimationContext);
            if animated {
                let _: () = msg_send![context_class, beginGrouping];
                let context: *mut AnyObject = msg_send![context_class, currentContext];
                let _: () = msg_send![context, setDuration: 0.2_f64];
            }
            // Minimize and zoom stay hidden, regardless of window hover.
            for button_kind in [0_usize] {
                let button: *mut AnyObject = msg_send![native, standardWindowButton: button_kind];
                if button.is_null() {
                    continue;
                }
                let target: *mut AnyObject = if animated {
                    msg_send![button, animator]
                } else {
                    button
                };
                let _: () = msg_send![target, setAlphaValue: alpha];
            }
            if animated {
                let _: () = msg_send![context_class, endGrouping];
            }
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
            if let Some(which) = Shortcut::ALL
                .into_iter()
                .find(|which| self.shortcuts[which.index()].is_some_and(|key| key.id() == event.id))
            {
                events.push(which.event());
            }
        }
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if let Some((_, action)) = self.menu_actions.iter().find(|(id, _)| *id == event.id) {
                events.push(*action);
            }
        }
        events
    }

    pub fn show(&mut self, window: &mut gpui::Window) -> Result<(), String> {
        self.remember_frontmost_app();
        let native = native_window(window)?;
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
    pub fn hide(&mut self, window: &mut gpui::Window) -> Result<(), String> {
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
const NO_NATIVE_WINDOW: &str = "Markraft could not find its own window. Quit and reopen Markraft.";

/// The menu bar item and the global shortcut are one thing to the user, and a
/// failure to set them up leaves the notes themselves working.
fn menu_bar_failure(detail: impl std::fmt::Display) -> String {
    eprintln!("Markraft: the menu bar item could not be set up: {detail}");
    "Markraft could not put its icon in the menu bar. Notes still work, but the \
     menu bar item and the shortcut that opens them are unavailable."
        .to_owned()
}

/// A window's AppKit side, taken out of GPUI so it can be changed outside GPUI's own
/// update. AppKit tells GPUI of every size a resize passes through, and GPUI can only
/// hear it while nothing else is updating the app: a resize made inside an update
/// leaves GPUI drawing at the size the window had before.
pub struct NativeWindow {
    window: *mut AnyObject,
    view: *mut AnyObject,
}

impl NativeWindow {
    /// Only for use within the same turn of the run loop, while the window is open.
    pub fn of(window: &gpui::Window) -> Option<Self> {
        Some(Self {
            window: native_window(window).ok()?,
            view: native_view(window).ok()?,
        })
    }

    /// Give the window a content height of `height`, keeping its top edge where it
    /// is, the way a settings window grows and shrinks from its title bar as its
    /// pages change. An animation runs to its end inside this call.
    pub fn fit_height(&self, height: f32, animate: bool) {
        unsafe {
            // Should a step of the animation come before GPUI has drawn at its size,
            // the last frame is pinned under the title bar rather than stretched.
            const NS_VIEW_LAYER_CONTENTS_PLACEMENT_TOP: isize = 4;
            let _: () = msg_send![
                self.view,
                setLayerContentsPlacement: NS_VIEW_LAYER_CONTENTS_PLACEMENT_TOP
            ];
            let frame: NSRect = msg_send![self.window, frame];
            let content: NSRect = msg_send![self.window, contentRectForFrameRect: frame];
            let chrome = frame.size.y - content.size.y;
            let next_height = f64::from(height) + chrome;
            let next = NSRect {
                origin: NSPoint {
                    x: frame.origin.x,
                    // AppKit's origin is the bottom-left corner: moving it keeps the top.
                    y: frame.origin.y + frame.size.y - next_height,
                },
                size: NSPoint {
                    x: frame.size.x,
                    y: next_height,
                },
            };
            let _: () = msg_send![
                self.window,
                setFrame: next,
                display: Bool::YES,
                animate: Bool::new(animate)
            ];
        }
    }
}

fn native_window(window: &gpui::Window) -> Result<*mut AnyObject, String> {
    let view = native_view(window)?;
    let native: *mut AnyObject = unsafe { msg_send![view, window] };
    if native.is_null() {
        Err(NO_NATIVE_WINDOW.into())
    } else {
        Ok(native)
    }
}

/// GPUI's own view, which draws the window: a subview of the content view.
fn native_view(window: &gpui::Window) -> Result<*mut AnyObject, String> {
    let handle = HasWindowHandle::window_handle(window).map_err(|error| {
        eprintln!("Markraft: the window handle is unavailable: {error}");
        NO_NATIVE_WINDOW.to_owned()
    })?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return Err(
            "Markraft could not take charge of its window, so it cannot float above \
             other apps. Quit and reopen Markraft."
                .into(),
        );
    };
    Ok(handle.ns_view.as_ptr().cast::<AnyObject>())
}

// Dynamic class lookup keeps startup safe on macOS versions before 13, while
// linking the framework ensures that SMAppService is loaded on supported OSes.
#[link(name = "ServiceManagement", kind = "framework")]
unsafe extern "C" {}

fn main_app_service() -> Result<Retained<AnyObject>, String> {
    let class = AnyClass::get(c"SMAppService")
        .ok_or_else(|| "Launch at login requires macOS 13 or later.".to_owned())?;
    let service: Option<Retained<AnyObject>> = unsafe { msg_send![class, mainAppService] };
    service.ok_or_else(|| "macOS could not find the Markraft app bundle.".to_owned())
}

fn ensure_installed_bundle() -> Result<(), String> {
    let bundle: *mut AnyObject = unsafe { msg_send![class!(NSBundle), mainBundle] };
    let path: *mut AnyObject = unsafe { msg_send![bundle, bundlePath] };
    let path = ns_string_text(path).ok_or_else(|| "Could not locate the app bundle.".to_owned())?;
    let path = Path::new(&path);
    let user_applications =
        std::env::var_os("HOME").map(|home| Path::new(&home).join("Applications"));
    let installed = path.starts_with("/Applications")
        || user_applications.is_some_and(|applications| path.starts_with(applications));
    if path.extension().is_none_or(|extension| extension != "app") || !installed {
        return Err("Move Markraft.app to Applications and launch that copy \
                    before enabling launch at login."
            .into());
    }
    Ok(())
}

fn login_approval_message() -> String {
    format!("Allow Markraft in {LOGIN_ITEMS} to enable launch at login.")
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
fn blank_menu_bar_icon() -> Result<Icon, String> {
    const PIXELS: u32 = MENU_BAR_IMAGE_POINTS as u32;
    Icon::from_rgba(vec![0; (PIXELS * PIXELS * 4) as usize], PIXELS, PIXELS)
        .map_err(menu_bar_failure)
}

fn show_menu_bar_image(tray: &TrayIcon) -> Result<(), String> {
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
/// one per display; `assets/icon/README.md` has the render commands.
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
        eprintln!("Markraft: {event}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
