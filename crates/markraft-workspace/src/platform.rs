//! Window-scoped macOS editing services and explicit host process capabilities.
//!
//! Create and use this object on AppKit's main thread. GPUI owns the native
//! window; native pointers below are borrowed only for the duration of a call.
pub(crate) mod checking_panel;
pub(crate) mod context_menu;
pub(crate) mod print;
pub(crate) mod symbols;
pub(crate) mod text_checking;
pub(crate) mod text_geometry;
pub(crate) mod text_requestor;
pub(crate) mod text_services;
#[cfg(feature = "native-translation")]
pub(crate) mod translation;
#[cfg(not(feature = "native-translation"))]
#[path = "platform/translation_disabled.rs"]
pub(crate) mod translation;

use crate::locale::Message;
use objc2::{
    class,
    encode::{Encode, Encoding},
    msg_send,
    runtime::{AnyObject, Bool},
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::{ffi::CStr, ptr};

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

/// The global shortcuts: one shows and hides the note, one opens a new note, and one
/// opens today's daily note.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Shortcut {
    Toggle,
    NewNote,
    DailyNote,
}

impl Shortcut {
    pub const ALL: [Shortcut; 3] = [Shortcut::Toggle, Shortcut::NewNote, Shortcut::DailyNote];
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlatformEvent {
    Toggle,
    NewNote,
    DailyNote,
    Settings,
    CheckForUpdates,
    ReportIssue,
    Quit,
}

/// Host-owned application services. The embedded workspace never creates these.
pub trait PlatformServices {
    fn set_locale(&mut self, _: &crate::locale::I18n) {}
    fn configure_window(&mut self, _: &mut gpui::Window) -> Result<(), Message> {
        Ok(())
    }
    fn set_shortcut(&mut self, _: Shortcut, _: &str) -> Result<(), Message> {
        Ok(())
    }
    fn suspend_shortcuts(&mut self) {}
    fn resume_shortcuts(&mut self) -> Result<(), Message> {
        Ok(())
    }
    fn app_is_active(&self) -> bool {
        true
    }
    fn set_always_on_top(&self, _: &gpui::Window, _: bool) -> Result<(), Message> {
        Ok(())
    }
    fn set_all_spaces(&self, _: &gpui::Window, _: bool) -> Result<(), Message> {
        Ok(())
    }
    fn launch_at_login_enabled(&self) -> bool {
        false
    }
    fn set_launch_at_login(&mut self, _: bool) -> Result<(), Message> {
        Ok(())
    }
    fn pointer_inside(&self, _: &gpui::Window) -> bool {
        true
    }
    fn poll_events(&self) -> Vec<PlatformEvent> {
        Vec::new()
    }
    fn show(&mut self, window: &mut gpui::Window, _: bool) -> Result<(), Message> {
        window.activate_window();
        Ok(())
    }
    fn hide(&mut self, _: &mut gpui::Window) -> Result<(), Message> {
        Ok(())
    }
    fn is_visible(&self, _: &gpui::Window) -> bool {
        true
    }
    fn return_to_previous_app(&self) {}
    fn remember_frontmost_app(&mut self) {}
}
pub type Platform = Box<dyn PlatformServices>;

/// Losing the native window reads the same to the user however it happened.
const NO_NATIVE_WINDOW: &str = "error.native-window";

/// NSViewLayerContentsPlacementTop: a frame of another size than the view is
/// drawn at its own scale against the view's top edge.
const NS_VIEW_LAYER_CONTENTS_PLACEMENT_TOP: isize = 4;

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

    /// Give the window a content height of `height`, keeping its top edge where
    /// space allows and keeping it inside the screen's visible area. An animation
    /// runs to its end inside this call.
    pub fn fit_height(&self, height: f32, animate: bool) {
        unsafe {
            // Should a step of the animation come before GPUI has drawn at its size,
            // the last frame is pinned under the title bar rather than stretched.
            let _: () = msg_send![
                self.view,
                setLayerContentsPlacement: NS_VIEW_LAYER_CONTENTS_PLACEMENT_TOP
            ];
            let frame: NSRect = msg_send![self.window, frame];
            let content: NSRect = msg_send![self.window, contentRectForFrameRect: frame];
            let chrome = frame.size.y - content.size.y;
            let screen: *mut AnyObject = msg_send![self.window, screen];
            let visible = (!screen.is_null()).then(|| msg_send![screen, visibleFrame]);
            let next = fitted_window_frame(frame, f64::from(height) + chrome, visible);
            let _: () = msg_send![
                self.window,
                setFrame: next,
                display: Bool::YES,
                animate: Bool::new(animate)
            ];
        }
    }
}

/// AppKit frames share global coordinates with a bottom-left origin. Keep the
/// window's width; a side Dock can require a horizontal move but not a resize.
fn fitted_window_frame(frame: NSRect, height: f64, visible: Option<NSRect>) -> NSRect {
    let mut next = NSRect {
        origin: NSPoint {
            x: frame.origin.x,
            y: frame.origin.y + frame.size.y - height,
        },
        size: NSPoint {
            x: frame.size.x,
            y: height,
        },
    };
    if let Some(visible) = visible {
        next.size.y = height.min(visible.size.y);
        next.origin.x = frame
            .origin
            .x
            .min(visible.origin.x + visible.size.x - next.size.x)
            .max(visible.origin.x);
        next.origin.y = (frame.origin.y + frame.size.y - next.size.y)
            .min(visible.origin.y + visible.size.y - next.size.y)
            .max(visible.origin.y);
    }
    next
}

/// Move `native` to the display under the pointer, where it sits as far from that
/// display's top-left as it did from its own, pulled back inside if it would overhang.
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

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, width: f64, height: f64) -> NSRect {
        NSRect {
            origin: NSPoint { x, y },
            size: NSPoint {
                x: width,
                y: height,
            },
        }
    }

    fn frame_values(frame: NSRect) -> [f64; 4] {
        [frame.origin.x, frame.origin.y, frame.size.x, frame.size.y]
    }

    #[test]
    fn fit_height_keeps_the_top_edge_when_the_page_fits() {
        let visible = rect(0., 40., 1440., 836.);
        let frame = rect(120., 400., 520., 420.);
        for height in [300., 720.] {
            let next = fitted_window_frame(frame, height, Some(visible));
            assert_eq!(frame_values(next), [120., 820. - height, 520., height]);
        }
    }

    #[test]
    fn fit_height_moves_a_growing_window_above_the_bottom_dock() {
        let next = fitted_window_frame(
            rect(120., 60., 520., 420.),
            720.,
            Some(rect(0., 40., 1440., 836.)),
        );
        assert_eq!(frame_values(next), [120., 40., 520., 720.]);
    }

    #[test]
    fn fit_height_limits_tall_pages_to_the_visible_screen() {
        let next = fitted_window_frame(
            rect(-1300., 1200., 520., 420.),
            720.,
            Some(rect(-1440., 1000., 1440., 600.)),
        );
        assert_eq!(frame_values(next), [-1300., 1000., 520., 600.]);
    }

    #[test]
    fn fit_height_avoids_side_docks_without_changing_the_width() {
        for (visible, x, expected_x) in [
            (rect(80., 0., 1360., 876.), 20., 80.),
            (rect(0., 0., 1360., 876.), 900., 840.),
            // A narrower display still keeps the title bar's left edge reachable.
            (rect(80., 0., 400., 876.), 900., 80.),
        ] {
            let next = fitted_window_frame(rect(x, 400., 520., 420.), 600., Some(visible));
            assert_eq!(frame_values(next), [expected_x, 220., 520., 600.]);
        }
    }

    #[test]
    fn fit_height_keeps_the_top_edge_when_the_screen_is_unavailable() {
        let next = fitted_window_frame(rect(120., 60., 520., 420.), 720., None);
        assert_eq!(frame_values(next), [120., -240., 520., 720.]);
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
}
