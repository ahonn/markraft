//! AppKit text services for the GPUI editor, using the public pasteboard bridge.
//!
//! AppKit retains no ownership of a responder's `nextResponder`. A session owns
//! the inserted responder and restores the original chain before releasing it.
//! The caller must keep the session alive while a service is running, and must
//! validate its editor snapshot before applying the returned replacement.

use super::native_view;
use crate::locale::Message;
use futures_channel::oneshot;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSPasteboard, NSPasteboardType, NSResponder, NSServicesMenuRequestor, NSView,
};
use objc2_foundation::{NSArray, NSString};
use std::cell::RefCell;

mod selection_anchor;
mod writing_tools;

const STRING_TYPE: &str = "public.utf8-plain-text";

/// Native entry points have different formatting contracts. Keep the origin
/// through asynchronous delivery so the editor can apply the matching policy.
#[derive(Debug, PartialEq)]
pub(crate) enum TextReplacement {
    PlainText(String),
    PreserveStyles(String),
}

pub(crate) struct TextServiceSnapshot {
    pub text: String,
    pub editable: bool,
    pub prose: bool,
}

struct Selection {
    text: String,
    active: bool,
    editable: bool,
    writing_tools_requested: bool,
    replacement: Option<oneshot::Sender<TextReplacement>>,
}

impl Selection {
    fn supports_writing_tools(&self, prose: bool) -> bool {
        self.active && self.editable && prose
    }

    fn supports(&self, send_type: Option<&str>, return_type: Option<&str>) -> bool {
        self.active
            && (send_type.is_none() || !self.text.is_empty())
            && supports_types(send_type, return_type)
            && (return_type.is_none() || self.editable)
    }

    fn replace_plain_text(&mut self, text: String) -> bool {
        // AppKit can deliver a Writing Tools result through Services even after
        // requesting coordinator context. Keep that operation's formatting
        // contract regardless of which native callback returns its text.
        let replacement = if self.writing_tools_requested {
            TextReplacement::PreserveStyles(text)
        } else {
            TextReplacement::PlainText(text)
        };
        self.replace(replacement)
    }

    fn replace(&mut self, replacement: TextReplacement) -> bool {
        if !self.active || !self.editable {
            return false;
        }
        self.active = false;
        self.replacement
            .take()
            .is_some_and(|sender| sender.send(replacement).is_ok())
    }
}

fn supports_types(send_type: Option<&str>, return_type: Option<&str>) -> bool {
    (send_type.is_some() || return_type.is_some())
        && send_type.is_none_or(is_string_type)
        && return_type.is_none_or(is_string_type)
}

fn is_string_type(value: &str) -> bool {
    matches!(value, STRING_TYPE | "NSStringPboardType")
}

define_class!(
    #[unsafe(super(NSResponder))]
    #[thread_kind = MainThreadOnly]
    #[ivars = RefCell<Selection>]
    struct TextRequestor;

    unsafe impl NSObjectProtocol for TextRequestor {}

    unsafe impl NSServicesMenuRequestor for TextRequestor {
        #[unsafe(method(writeSelectionToPasteboard:types:))]
        fn write_selection(&self, pasteboard: &NSPasteboard, types: &NSArray<NSPasteboardType>) -> bool {
            self.write_text(pasteboard, types)
        }

        #[unsafe(method(readSelectionFromPasteboard:))]
        fn read_selection(&self, pasteboard: &NSPasteboard) -> bool {
            self.read_text(pasteboard)
        }
    }

    impl TextRequestor {
        #[unsafe(method_id(validRequestorForSendType:returnType:))]
        fn valid_requestor(
            &self,
            send_type: Option<&NSString>,
            return_type: Option<&NSString>,
        ) -> Option<Retained<AnyObject>> {
            let send_name = send_type.map(ToString::to_string);
            let return_name = return_type.map(ToString::to_string);
            if self.ivars().borrow().supports(send_name.as_deref(), return_name.as_deref()) {
                // The returned requestor has NSObject ownership semantics.
                unsafe { Retained::retain((self as *const Self).cast_mut().cast()) }
            } else {
                // Preserve Services support belonging to other responders.
                unsafe { msg_send![super(self), validRequestorForSendType: send_type, returnType: return_type] }
            }
        }
    }
);

impl TextRequestor {
    fn write_text(&self, pasteboard: &NSPasteboard, types: &NSArray<NSPasteboardType>) -> bool {
        let selection = self.ivars().borrow();
        if !selection.active || selection.text.is_empty() {
            return false;
        }
        let supported: Vec<_> = types
            .iter()
            .filter(|kind| is_string_type(&kind.to_string()))
            .collect();
        if supported.is_empty() {
            return false;
        }
        // Services supply a private pasteboard; never alter the user's
        // general clipboard to transfer the selected text.
        let types = NSArray::from_retained_slice(&supported);
        unsafe { pasteboard.declareTypes_owner(&types, None) };
        let text = NSString::from_str(&selection.text);
        supported
            .iter()
            .all(|kind| pasteboard.setString_forType(&text, kind))
    }

    fn read_text(&self, pasteboard: &NSPasteboard) -> bool {
        if !self.ivars().borrow().active || !self.ivars().borrow().editable {
            return false;
        }
        read_replacement(pasteboard)
            .is_some_and(|text| self.ivars().borrow_mut().replace_plain_text(text))
    }
}

fn read_replacement(pasteboard: &NSPasteboard) -> Option<String> {
    pasteboard
        .stringForType(&NSString::from_str(STRING_TYPE))
        .or_else(|| pasteboard.stringForType(&NSString::from_str("NSStringPboardType")))
        .map(|text| text.to_string())
}

/// Own this for the entire asynchronous service interaction, not just menu tracking.
/// Dropping it cancels pending writeback and restores the responder chain.
pub(crate) struct TextServiceSession {
    view: Retained<NSResponder>,
    previous: Option<Retained<NSResponder>>,
    requestor: Retained<TextRequestor>,
    writing_tools: Option<writing_tools::Snapshot>,
    anchor: Option<selection_anchor::SelectionAnchor>,
}

impl TextServiceSession {
    pub(crate) fn attach(
        window: &gpui::Window,
        snapshot: TextServiceSnapshot,
        bounds: gpui::Bounds<gpui::Pixels>,
    ) -> Result<(Self, oneshot::Receiver<TextReplacement>), Message> {
        let mtm =
            MainThreadMarker::new().ok_or_else(|| Message::new("error.native-window-control"))?;
        let native = unsafe { Retained::retain(native_view(window)?.cast::<NSView>()) }
            .ok_or_else(|| Message::new("error.native-window-control"))?;
        let view: Retained<NSResponder> = native.clone().into_super();
        let types = NSArray::from_retained_slice(&[
            NSString::from_str(STRING_TYPE),
            NSString::from_str("NSStringPboardType"),
        ]);
        NSApplication::sharedApplication(mtm)
            .registerServicesMenuSendTypes_returnTypes(&types, &types);
        let previous = unsafe { view.nextResponder() };
        let (sender, receiver) = oneshot::channel();
        let requestor = TextRequestor::alloc(mtm).set_ivars(RefCell::new(Selection {
            text: snapshot.text,
            active: true,
            editable: snapshot.editable,
            writing_tools_requested: false,
            replacement: Some(sender),
        }));
        let requestor: Retained<TextRequestor> = unsafe { msg_send![super(requestor), init] };
        unsafe {
            requestor.setNextResponder(previous.as_deref());
            view.setNextResponder(Some(&requestor));
        }
        // A coordinator represents one replaceable prose snapshot. Other
        // selections retain their existing send-only/read-only Services support.
        let native_writing = requestor
            .ivars()
            .borrow()
            .supports_writing_tools(snapshot.prose);
        let anchor = selection_anchor::SelectionAnchor::new(&native, bounds);
        let writing_tools = native_writing
            .then(|| writing_tools::Snapshot::new(&view, &requestor, bounds))
            .flatten();
        Ok((
            Self {
                view,
                previous,
                requestor,
                writing_tools,
                anchor,
            },
            receiver,
        ))
    }

    pub(crate) fn cancel(&mut self) {
        self.writing_tools.take();
        self.anchor.take();
        {
            let mut selection = self.requestor.ivars().borrow_mut();
            selection.active = false;
            selection.replacement.take();
        }
        // Another owner may have changed the chain while a native service was
        // open. Restore only the link that this session still owns.
        let current = unsafe { self.view.nextResponder() };
        if current
            .as_deref()
            .is_some_and(|current| std::ptr::eq(current, &*self.requestor as &NSResponder))
        {
            unsafe { self.view.setNextResponder(self.previous.as_deref()) };
        }
    }

    /// Selected text receives AppKit's standard group through Services. Empty
    /// carets need explicit items. Both request context from the coordinator,
    /// but AppKit can return the result through either native bridge.
    pub(crate) fn writing_tools_items(&self) -> Option<&[Retained<objc2_app_kit::NSMenuItem>]> {
        self.writing_tools.as_ref().map(|snapshot| {
            let services = self
                .requestor
                .ivars()
                .borrow()
                .supports(Some(STRING_TYPE), None);
            if services { &[][..] } else { snapshot.items() }
        })
    }
}

impl Drop for TextServiceSession {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pasteboard_results_preserve_styles_only_after_writing_tools_requests_context() {
        for writing_tools_requested in [false, true] {
            let (sender, mut receiver) = oneshot::channel();
            let mut selection = Selection {
                text: "This are a smple sentnce.".into(),
                active: true,
                editable: true,
                writing_tools_requested,
                replacement: Some(sender),
            };
            let corrected = "This is a simple sentence.".to_owned();
            assert!(selection.replace_plain_text(corrected.clone()));
            let expected = if writing_tools_requested {
                TextReplacement::PreserveStyles(corrected)
            } else {
                TextReplacement::PlainText(corrected)
            };
            assert_eq!(receiver.try_recv().unwrap(), Some(expected));
            assert!(!selection.replace_plain_text("Late result".into()));
        }
    }

    #[test]
    fn writing_tools_pasteboard_results_respect_retired_and_readonly_snapshots() {
        for (active, editable) in [(false, true), (true, false)] {
            let (sender, mut receiver) = oneshot::channel();
            let mut selection = Selection {
                text: "Original".into(),
                active,
                editable,
                writing_tools_requested: true,
                replacement: Some(sender),
            };
            assert!(!selection.replace_plain_text("Corrected".into()));
            assert_eq!(receiver.try_recv().unwrap(), None);
        }
    }

    #[test]
    fn native_writing_tools_requires_editable_prose_without_changing_services() {
        let (sender, _receiver) = oneshot::channel();
        let mut selection = Selection {
            text: "Selected prose".into(),
            active: true,
            editable: true,
            writing_tools_requested: false,
            replacement: Some(sender),
        };
        assert!(selection.supports_writing_tools(true));
        // Code is still transferable via Services, but has no prose coordinator.
        assert!(!selection.supports_writing_tools(false));
        assert!(selection.supports(Some(STRING_TYPE), None));
        // Read-only, protected and noncontiguous snapshots cannot return text.
        selection.editable = false;
        assert!(!selection.supports_writing_tools(true));
        assert!(selection.supports(Some(STRING_TYPE), None));
        assert!(!selection.supports(Some(STRING_TYPE), Some(STRING_TYPE)));
        selection.editable = true;
        selection.text.clear();
        // Empty carets keep the coordinator but need its explicit menu group;
        // AppKit cannot create a selection-based Services group for them.
        assert!(selection.supports_writing_tools(true));
        assert!(!selection.supports(Some(STRING_TYPE), None));
        selection.active = false;
        assert!(!selection.supports_writing_tools(true));
    }

    #[test]
    fn requestor_recognizes_native_and_legacy_plain_types() {
        assert!(supports_types(Some(STRING_TYPE), None));
        assert!(supports_types(Some(STRING_TYPE), Some(STRING_TYPE)));
        assert!(supports_types(
            Some("NSStringPboardType"),
            Some(STRING_TYPE)
        ));
        assert!(supports_types(None, Some(STRING_TYPE)));
        assert!(!supports_types(None, None));
        assert!(!supports_types(Some("public.rtf"), Some(STRING_TYPE)));
        assert!(!supports_types(Some("public.html"), Some(STRING_TYPE)));
    }

    #[test]
    fn insertion_services_need_an_editable_caret_but_no_selected_text() {
        let (sender, mut receiver) = oneshot::channel();
        let mut selection = Selection {
            text: String::new(),
            active: true,
            editable: true,
            writing_tools_requested: false,
            replacement: Some(sender),
        };
        assert!(selection.supports(None, Some(STRING_TYPE)));
        assert!(!selection.supports(Some(STRING_TYPE), None));
        selection.editable = false;
        assert!(!selection.supports(None, Some(STRING_TYPE)));
        selection.editable = true;
        assert!(selection.replace(TextReplacement::PlainText("Inserted".into())));
        assert_eq!(
            receiver.try_recv().unwrap(),
            Some(TextReplacement::PlainText("Inserted".into()))
        );
    }

    #[test]
    fn replacement_is_delivered_once_and_disables_the_snapshot() {
        let (sender, mut receiver) = oneshot::channel();
        let mut selection = Selection {
            text: "Original".into(),
            active: true,
            editable: true,
            writing_tools_requested: false,
            replacement: Some(sender),
        };
        assert!(selection.supports(Some(STRING_TYPE), Some(STRING_TYPE)));
        assert!(selection.replace(TextReplacement::PlainText("Rewritten".into())));
        assert_eq!(
            receiver.try_recv().unwrap(),
            Some(TextReplacement::PlainText("Rewritten".into()))
        );
        assert!(!selection.supports(Some(STRING_TYPE), None));
        assert!(!selection.replace(TextReplacement::PlainText("Late result".into())));
    }

    #[test]
    fn cancelled_selection_does_not_advertise_services() {
        let (sender, _receiver) = oneshot::channel();
        let mut selection = Selection {
            text: "Original".into(),
            active: true,
            editable: true,
            writing_tools_requested: false,
            replacement: Some(sender),
        };
        selection.active = false;
        selection.replacement.take();
        assert!(!selection.supports(Some(STRING_TYPE), Some(STRING_TYPE)));
        assert!(!selection.replace(TextReplacement::PlainText("Late result".into())));
    }

    #[test]
    fn read_only_selection_supports_send_only_services() {
        let (sender, _receiver) = oneshot::channel();
        let mut selection = Selection {
            text: "Original".into(),
            active: true,
            editable: false,
            writing_tools_requested: false,
            replacement: Some(sender),
        };
        assert!(selection.supports(Some(STRING_TYPE), None));
        assert!(!selection.supports(Some(STRING_TYPE), Some(STRING_TYPE)));
        assert!(!selection.replace(TextReplacement::PlainText("Read-only replacement".into())));
    }
}
