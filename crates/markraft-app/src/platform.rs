//! macOS integration owned by the application, not the reusable editor.
//!
//! Create and use this object on AppKit's main thread. GPUI owns the native
//! window; native pointers below are borrowed only for the duration of a call.
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState, hotkey::HotKey};
use objc2::{
    class,
    encode::{Encode, Encoding},
    msg_send,
    rc::Retained,
    runtime::{AnyClass, AnyObject, Bool},
};
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlatformEvent {
    Toggle,
    NewNote,
    Settings,
    Quit,
}

pub struct Platform {
    _tray: TrayIcon,
    hotkeys: GlobalHotKeyManager,
    shortcut: Option<HotKey>,
    menu_actions: Vec<(MenuId, PlatformEvent)>,
    // Retained NSRunningApplication; None when no previous app is known.
    previous_app: Option<Retained<AnyObject>>,
}

impl Platform {
    pub fn new() -> Result<Self, String> {
        let hotkeys = GlobalHotKeyManager::new().map_err(|error| error.to_string())?;
        let menu = Menu::new();
        let toggle = MenuItem::new("Show / Hide Notes", true, None);
        let new_note = MenuItem::new("New Note", true, None);
        let settings = MenuItem::new("Settings…", true, None);
        let quit = MenuItem::new("Quit Markraft Notes", true, None);
        menu.append_items(&[
            &toggle,
            &new_note,
            &PredefinedMenuItem::separator(),
            &settings,
            &PredefinedMenuItem::separator(),
            &quit,
        ])
        .map_err(|error| error.to_string())?;
        let menu_actions = vec![
            (toggle.id().clone(), PlatformEvent::Toggle),
            (new_note.id().clone(), PlatformEvent::NewNote),
            (settings.id().clone(), PlatformEvent::Settings),
            (quit.id().clone(), PlatformEvent::Quit),
        ];
        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_icon(note_icon()?)
            .with_icon_as_template(true)
            .with_tooltip("Markraft Notes")
            .build()
            .map_err(|error| error.to_string())?;
        let mut platform = Self {
            _tray: tray,
            hotkeys,
            shortcut: None,
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
    pub fn set_shortcut(&mut self, shortcut: &str) -> Result<(), String> {
        let next = if shortcut.trim().is_empty() {
            None
        } else {
            Some(HotKey::from_str(shortcut).map_err(|error| error.to_string())?)
        };
        if self.shortcut == next {
            return Ok(());
        }
        if let Some(next) = next {
            self.hotkeys
                .register(next)
                .map_err(|error| format!("Could not register {shortcut}: {error}"))?;
        }
        if let Some(previous) = self.shortcut
            && let Err(error) = self.hotkeys.unregister(previous)
        {
            if let Some(next) = next {
                let _ = self.hotkeys.unregister(next);
            }
            return Err(error.to_string());
        }
        self.shortcut = next;
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
                "Could not {} launch at login: {detail}. Use a signed Markraft Notes app \
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
            // Keep native traffic lights visible while reserving window sizing
            // for edge dragging. FullScreenNone cannot be combined with the
            // FullScreenAuxiliary behavior required above.
            for button_kind in [1_usize, 2_usize] {
                // NSWindowMiniaturizeButton, NSWindowZoomButton.
                let button: *mut AnyObject = msg_send![native, standardWindowButton: button_kind];
                if !button.is_null() {
                    let _: () = msg_send![button, setEnabled: Bool::NO];
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

    /// Fade the native close, minimize and zoom buttons together with the GPUI chrome.
    /// Transparent buttons stay in place; they are only reachable while the pointer is
    /// inside the window, which is exactly when they are visible.
    pub fn set_traffic_lights_visible(&self, window: &gpui::Window, visible: bool, animated: bool) {
        let Ok(native) = native_window(window) else {
            return;
        };
        let alpha = if visible { 1.0_f64 } else { 0.0_f64 };
        unsafe {
            let context_class = class!(NSAnimationContext);
            if animated {
                let _: () = msg_send![context_class, beginGrouping];
                let context: *mut AnyObject = msg_send![context_class, currentContext];
                let _: () = msg_send![context, setDuration: 0.2_f64];
            }
            for button_kind in [0_usize, 1_usize, 2_usize] {
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
            if event.state == HotKeyState::Pressed
                && self.shortcut.is_some_and(|key| key.id() == event.id)
            {
                events.push(PlatformEvent::Toggle);
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
        unsafe {
            let was_key: Bool = msg_send![native, isKeyWindow];
            let _: () = msg_send![native, orderOut: ptr::null_mut::<AnyObject>()];
            if was_key != Bool::NO
                && let Some(previous) = &self.previous_app
            {
                let terminated: Bool = msg_send![&**previous, isTerminated];
                if terminated == Bool::NO {
                    // NSApplicationActivateIgnoringOtherApps. Restoring the app
                    // does not reorder its windows or alter their selection.
                    let _: Bool = msg_send![&**previous, activateWithOptions: 2_usize];
                }
            }
        }
        diagnostics("window hidden");
        Ok(())
    }

    fn remember_frontmost_app(&mut self) {
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
        if let Some(shortcut) = self.shortcut {
            let _ = self.hotkeys.unregister(shortcut);
        }
    }
}

fn native_window(window: &gpui::Window) -> Result<*mut AnyObject, String> {
    let handle = HasWindowHandle::window_handle(window).map_err(|error| error.to_string())?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return Err("Floating Notes requires a macOS AppKit window".into());
    };
    let native: *mut AnyObject =
        unsafe { msg_send![handle.ns_view.as_ptr().cast::<AnyObject>(), window] };
    if native.is_null() {
        Err("The editor view has no native window".into())
    } else {
        Ok(native)
    }
}

// Dynamic class lookup keeps startup safe on macOS versions before 13, while
// linking the framework ensures that SMAppService is loaded on supported OSes.
#[link(name = "ServiceManagement", kind = "framework")]
unsafe extern "C" {}

fn main_app_service() -> Result<Retained<AnyObject>, String> {
    let class = AnyClass::get(c"SMAppService")
        .ok_or_else(|| "Launch at login requires macOS 13 or later.".to_owned())?;
    let service: Option<Retained<AnyObject>> = unsafe { msg_send![class, mainAppService] };
    service.ok_or_else(|| "macOS could not find the Markraft Notes app bundle.".to_owned())
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
        return Err(
            "Move Markraft Notes.app to Applications and launch that copy \
                    before enabling launch at login."
                .into(),
        );
    }
    Ok(())
}

fn login_approval_message() -> String {
    format!("Allow Markraft Notes in {LOGIN_ITEMS} to enable launch at login.")
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

fn note_icon() -> Result<Icon, String> {
    const SIZE: usize = 18;
    let mut pixels = vec![0_u8; SIZE * SIZE * 4];
    for y in 2..16 {
        for x in 3..15 {
            let border = x == 3 || x == 14 || y == 2 || y == 15;
            let line = (6..=11).contains(&x) && (y == 6 || y == 9 || y == 12);
            if border || line {
                pixels[(y * SIZE + x) * 4 + 3] = 255;
            }
        }
    }
    Icon::from_rgba(pixels, SIZE as u32, SIZE as u32).map_err(|error| error.to_string())
}

fn diagnostics(event: &str) {
    if std::env::var_os("MARKRAFT_DIAGNOSTICS").is_some() {
        eprintln!("Markraft: {event}");
    }
}
