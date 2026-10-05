//! Read-only system text integrations. Native presentations run outside GPUI borrows.

use super::{native_view, native_window, text_checking::DetectedData};
use crate::locale::Message;
use gpui::{Bounds, Pixels, Window};
use markraft_gpui::{ContextFontRun, ContextTextPresentation};
use objc2::{
    MainThreadMarker, class, msg_send,
    rc::{Allocated, Retained},
    runtime::AnyObject,
};
use objc2_app_kit::NSFontAttributeName;
use objc2_foundation::{
    NSArray, NSMutableAttributedString, NSPoint, NSRange, NSRect, NSString, NSURL,
};
use std::{cell::RefCell, ptr};

#[link(name = "AVFoundation", kind = "framework")]
unsafe extern "C" {}

thread_local! {
    static SPEECH: RefCell<Option<Retained<AnyObject>>> = const { RefCell::new(None) };
    static SHARE_PICKER: RefCell<Option<Retained<AnyObject>>> = const { RefCell::new(None) };
}

/// Retain the actual GPUI view while a text service is deferred for presentation.
pub(crate) struct TextAnchor {
    view: Retained<AnyObject>,
    window: Retained<AnyObject>,
    bounds: Bounds<Pixels>,
}

impl TextAnchor {
    pub(crate) fn new(window: &Window, bounds: Bounds<Pixels>) -> Result<Self, Message> {
        main_thread()?;
        let view = unsafe { Retained::retain(native_view(window)?) }.ok_or_else(failure)?;
        let window = unsafe { Retained::retain(native_window(window)?) }.ok_or_else(failure)?;
        Ok(Self {
            view,
            window,
            bounds,
        })
    }

    fn rect(&self) -> Result<NSRect, Message> {
        main_thread()?;
        let attached: *mut AnyObject = unsafe { msg_send![&*self.view, window] };
        let visible: bool = unsafe { msg_send![&*self.window, isVisible] };
        if attached != Retained::as_ptr(&self.window).cast_mut() || !visible {
            return Err(failure());
        }
        let bounds: NSRect = unsafe { msg_send![&*self.view, bounds] };
        let flipped: bool = unsafe { msg_send![&*self.view, isFlipped] };
        Ok(super::text_geometry::view_rect(
            self.bounds,
            bounds,
            flipped,
        ))
    }

    pub(crate) fn show_definition(
        &self,
        presentation: &ContextTextPresentation,
    ) -> Result<(), Message> {
        self.rect()?;
        if presentation.text.trim().is_empty() {
            return Ok(());
        }
        let text =
            NSMutableAttributedString::from_nsstring(&NSString::from_str(&presentation.text));
        unsafe {
            for run in &presentation.runs {
                let font = lookup_font(run);
                text.addAttribute_value_range(
                    NSFontAttributeName,
                    &font,
                    NSRange::new(run.range.start, run.range.len()),
                );
            }
            let bounds: NSRect = msg_send![&*self.view, bounds];
            let flipped: bool = msg_send![&*self.view, isFlipped];
            let point = super::text_geometry::view_point(presentation.baseline, bounds, flipped);
            // AppKit redraws a highlighted copy at the first character's baseline,
            // not the selection center. Matching its font avoids a second-sized word.
            let _: () =
                msg_send![&*self.view, showDefinitionForAttributedString: &*text, atPoint: point];
        }
        Ok(())
    }

    /// Let AppKit provide the installed system's date, address, phone, and flight actions.
    pub(crate) fn data_menu(&self, action: &DetectedData) -> Result<Retained<AnyObject>, Message> {
        let point = anchor_point(self.rect()?);
        // Only use the original detector result and its exact checked string.
        // Synthetic results lack Reveal metadata and can crash inside AppKit.
        // Rechecking would also shift relative dates.
        let menu: Option<Retained<AnyObject>> = unsafe {
            let checker: *mut AnyObject = msg_send![class!(NSSpellChecker), sharedSpellChecker];
            msg_send![checker, menuForResult: &*action.result, string: &*action.text,
                options: ptr::null::<AnyObject>(), atLocation: point, inView: &*self.view]
        };
        menu.ok_or_else(failure)
    }

    pub(crate) fn share(&self, text: &str) -> Result<(), Message> {
        let rect = self.rect()?;
        if text.trim().is_empty() {
            return Ok(());
        }
        let text = NSString::from_str(text);
        let items = NSArray::from_slice(&[&*text]);
        let picker: Retained<AnyObject> = unsafe {
            let allocated: Allocated<AnyObject> = msg_send![class!(NSSharingServicePicker), alloc];
            msg_send![allocated, initWithItems: &*items]
        };
        // A popover outlives the action. Retain its picker until the next invocation,
        // and release RefCell borrows before AppKit can enter its nested event loop.
        let previous = SHARE_PICKER.with(|slot| slot.replace(Some(picker.clone())));
        if let Some(previous) = previous {
            unsafe {
                let _: () = msg_send![&*previous, close];
            }
        }
        unsafe {
            let _: () = msg_send![&*picker, showRelativeToRect: rect, ofView: &*self.view, preferredEdge: 1usize];
        }
        Ok(())
    }
}

fn lookup_font(run: &ContextFontRun) -> Retained<AnyObject> {
    let family = gpui::font_name_with_fallbacks(&run.font.family, ".AppleSystemUIFont");
    let size = f64::from(f32::from(run.font_size));
    unsafe {
        let manager: *mut AnyObject = msg_send![class!(NSFontManager), sharedFontManager];
        let traits = if run.font.style == gpui::FontStyle::Italic {
            1usize
        } else {
            0
        };
        let weight = if run.font.weight >= gpui::FontWeight::BOLD {
            9isize
        } else {
            5
        };
        let font: Option<Retained<AnyObject>> = msg_send![manager,
            fontWithFamily: &*NSString::from_str(family), traits: traits, weight: weight, size: size];
        font.unwrap_or_else(|| msg_send![class!(NSFont), systemFontOfSize: size])
    }
}

// Data detector menus use the center of the selection rectangle.
fn anchor_point(rect: NSRect) -> NSPoint {
    NSPoint::new(
        rect.origin.x + rect.size.width / 2.,
        rect.origin.y + rect.size.height / 2.,
    )
}

pub(crate) fn start_speaking(text: &str) -> Result<(), Message> {
    main_thread()?;
    if text.trim().is_empty() {
        return Ok(());
    }
    let synthesizer = SPEECH.with(|slot| {
        slot.borrow_mut()
            .get_or_insert_with(|| unsafe { msg_send![class!(AVSpeechSynthesizer), new] })
            .clone()
    });
    let text = NSString::from_str(text);
    unsafe {
        // Replace this application's previous utterance instead of queueing selections.
        let _: bool = msg_send![&*synthesizer, stopSpeakingAtBoundary: 0isize];
        let utterance: Retained<AnyObject> =
            msg_send![class!(AVSpeechUtterance), speechUtteranceWithString: &*text];
        let _: () = msg_send![&*synthesizer, speakUtterance: &*utterance];
    }
    Ok(())
}

pub(crate) fn stop_speaking() {
    if MainThreadMarker::new().is_none() {
        return;
    }
    let synthesizer = SPEECH.with(|slot| slot.borrow().clone());
    if let Some(synthesizer) = synthesizer {
        unsafe {
            let _: bool = msg_send![&*synthesizer, stopSpeakingAtBoundary: 0isize];
        }
    }
}

pub(crate) fn is_speaking() -> bool {
    if MainThreadMarker::new().is_none() {
        return false;
    }
    let synthesizer = SPEECH.with(|slot| slot.borrow().clone());
    synthesizer.is_some_and(|synthesizer| unsafe { msg_send![&*synthesizer, isSpeaking] })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OpenWithApplication {
    pub name: String,
    /// An application bundle file URL supplied by Launch Services.
    pub url: String,
    pub is_default: bool,
}

pub(crate) fn applications_for_url(value: &str) -> Vec<OpenWithApplication> {
    if MainThreadMarker::new().is_none() {
        return Vec::new();
    }
    let Some(url) = NSURL::URLWithString(&NSString::from_str(value)) else {
        return Vec::new();
    };
    unsafe {
        let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        let applications: Retained<NSArray<NSURL>> =
            msg_send![workspace, URLsForApplicationsToOpenURL: &*url];
        let default: Option<Retained<NSURL>> =
            msg_send![workspace, URLForApplicationToOpenURL: &*url];
        let manager: *mut AnyObject = msg_send![class!(NSFileManager), defaultManager];
        let mut result = Vec::new();
        for application in applications.iter() {
            let Some(path) = application.path() else {
                continue;
            };
            let name: Retained<NSString> = msg_send![manager, displayNameAtPath: &*path];
            let name = name.to_string();
            let Some(absolute) = application.absoluteString() else {
                continue;
            };
            let is_default = default
                .as_ref()
                .is_some_and(|default| NSURL::eq(&application, default));
            result.push(OpenWithApplication {
                name: name.strip_suffix(".app").unwrap_or(&name).to_owned(),
                url: absolute.to_string(),
                is_default,
            });
        }
        result.sort_by(|left, right| {
            right
                .is_default
                .cmp(&left.is_default)
                .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
        });
        result.dedup_by(|left, right| left.url == right.url);
        result
    }
}

/// Submit the explicit application choice to Launch Services. Success means the
/// launch request was submitted; the destination application's load is asynchronous.
pub(crate) fn open_with(value: &str, application: &OpenWithApplication) -> Result<(), Message> {
    main_thread()?;
    let url = NSURL::URLWithString(&NSString::from_str(value)).ok_or_else(failure)?;
    let app = NSURL::URLWithString(&NSString::from_str(&application.url)).ok_or_else(failure)?;
    if !app.isFileURL() {
        return Err(failure());
    }
    let urls = NSArray::from_slice(&[&*url]);
    unsafe {
        let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        let configuration: Retained<AnyObject> =
            msg_send![class!(NSWorkspaceOpenConfiguration), configuration];
        let _: () = msg_send![workspace, openURLs: &*urls, withApplicationAtURL: &*app, configuration: &*configuration, completionHandler: ptr::null::<AnyObject>()];
    }
    Ok(())
}

fn main_thread() -> Result<MainThreadMarker, Message> {
    MainThreadMarker::new().ok_or_else(failure)
}

fn failure() -> Message {
    Message::new("error.native-window-control")
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_foundation::NSSize;

    #[test]
    fn data_detector_menus_anchor_at_selection_center() {
        let rect = NSRect::new(NSPoint::new(40., 60.), NSSize::new(80., 20.));
        assert_eq!(anchor_point(rect), NSPoint::new(80., 70.));
    }
}
