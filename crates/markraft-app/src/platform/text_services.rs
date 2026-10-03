//! Read-only system text integrations. Native presentations run outside GPUI borrows.

use super::{NSPoint, NSRect, native_view, native_window, text_checking::DetectedData};
use crate::locale::Message;
use gpui::{Pixels, Point, Window};
use objc2::{
    MainThreadMarker, class, msg_send,
    rc::{Allocated, Retained},
    runtime::AnyObject,
};
use objc2_foundation::{NSArray, NSAttributedString, NSString, NSURL};
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
    position: Point<Pixels>,
}

impl TextAnchor {
    pub(crate) fn new(window: &Window, position: Point<Pixels>) -> Result<Self, Message> {
        main_thread()?;
        let view = unsafe { Retained::retain(native_view(window)?) }.ok_or_else(failure)?;
        let window = unsafe { Retained::retain(native_window(window)?) }.ok_or_else(failure)?;
        Ok(Self {
            view,
            window,
            position,
        })
    }

    fn point(&self) -> Result<NSPoint, Message> {
        main_thread()?;
        let attached: *mut AnyObject = unsafe { msg_send![&*self.view, window] };
        let visible: bool = unsafe { msg_send![&*self.window, isVisible] };
        if attached != Retained::as_ptr(&self.window).cast_mut() || !visible {
            return Err(failure());
        }
        let bounds: NSRect = unsafe { msg_send![&*self.view, bounds] };
        let flipped: bool = unsafe { msg_send![&*self.view, isFlipped] };
        Ok(view_point(self.position, bounds, flipped))
    }

    pub(crate) fn show_definition(&self, text: &str) -> Result<(), Message> {
        let point = self.point()?;
        if text.trim().is_empty() {
            return Ok(());
        }
        let text = NSAttributedString::from_nsstring(&NSString::from_str(text));
        // NSView explicitly supports this API for custom text views, without NSTextView.
        unsafe {
            let _: () =
                msg_send![&*self.view, showDefinitionForAttributedString: &*text, atPoint: point];
        }
        Ok(())
    }

    /// Let AppKit provide the installed system's date, address, phone, and flight actions.
    pub(crate) fn data_menu(&self, action: &DetectedData) -> Result<Retained<AnyObject>, Message> {
        let point = self.point()?;
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
        let point = self.point()?;
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
        let rect = NSRect {
            origin: point,
            size: NSPoint { x: 1.0, y: 1.0 },
        };
        unsafe {
            let _: () = msg_send![&*picker, showRelativeToRect: rect, ofView: &*self.view, preferredEdge: 1usize];
        }
        Ok(())
    }
}

fn view_point(position: Point<Pixels>, bounds: NSRect, flipped: bool) -> NSPoint {
    let x = f32::from(position.x) as f64;
    let y = f32::from(position.y) as f64;
    NSPoint {
        x: bounds.origin.x + x,
        y: bounds.origin.y + if flipped { y } else { bounds.size.y - y },
    }
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
    use gpui::{point, px};

    #[test]
    fn text_anchor_respects_view_origin_and_orientation() {
        let bounds = NSRect {
            origin: NSPoint { x: 10.0, y: 20.0 },
            size: NSPoint { x: 300.0, y: 200.0 },
        };
        let position = point(px(30.0), px(40.0));
        let flipped = view_point(position, bounds, true);
        assert_eq!((flipped.x, flipped.y), (40.0, 60.0));
        let ordinary = view_point(position, bounds, false);
        assert_eq!((ordinary.x, ordinary.y), (40.0, 180.0));
    }
}
