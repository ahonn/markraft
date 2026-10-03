//! Limited Writing Tools attached to the real editor view, with a guarded snapshot.
use super::*;
use block2::DynBlock;
use objc2::{AllocAnyThread, runtime::ProtocolObject, sel};
use objc2_app_kit::{
    NSBezierPath, NSMenuItem, NSTextPreview, NSView, NSWritingToolsBehavior,
    NSWritingToolsCoordinator, NSWritingToolsCoordinatorAnimationParameters,
    NSWritingToolsCoordinatorContext, NSWritingToolsCoordinatorContextScope,
    NSWritingToolsCoordinatorDelegate, NSWritingToolsCoordinatorState,
    NSWritingToolsCoordinatorTextAnimation, NSWritingToolsCoordinatorTextReplacementReason,
    NSWritingToolsResultOptions,
};
use objc2_foundation::{NSAttributedString, NSPoint, NSRange, NSRect, NSSize, NSValue};
use std::{cell::Cell, ptr::NonNull};

/// Changes stay local until the limited Writing Tools operation finishes. A
/// native operation may deliver several replacements with successive offsets.
struct Buffer {
    original: String,
    text: String,
}

impl Buffer {
    fn new(text: String) -> Self {
        Self {
            original: text.clone(),
            text,
        }
    }

    fn replace(&mut self, range: NSRange, replacement: &str) -> bool {
        let Some(end) = range.location.checked_add(range.length) else {
            return false;
        };
        let byte_offset = |offset| {
            let mut utf16 = 0;
            for (byte, ch) in self.text.char_indices() {
                if utf16 == offset {
                    return Some(byte);
                }
                utf16 += ch.len_utf16();
            }
            (utf16 == offset).then_some(self.text.len())
        };
        let (Some(start), Some(end)) = (byte_offset(range.location), byte_offset(end)) else {
            return false;
        };
        self.text.replace_range(start..end, replacement);
        true
    }

    fn finish(&self, active: bool, selection: &mut Selection) -> bool {
        active
            && self.text != self.original
            && selection.replace(TextReplacement::PreserveStyles(self.text.clone()))
    }
}

struct State {
    view: Retained<NSView>,
    requestor: Retained<TextRequestor>,
    context: RefCell<Option<Retained<NSWritingToolsCoordinatorContext>>>,
    buffer: RefCell<Buffer>,
    caret: NSRect,
    active: Cell<bool>,
}

impl State {
    fn active(&self) -> bool {
        self.active.get() && self.requestor.ivars().borrow().active
    }

    fn owns_context(&self, context: &NSWritingToolsCoordinatorContext) -> bool {
        self.active()
            && self
                .context
                .borrow()
                .as_ref()
                .is_some_and(|current| current.identifier() == context.identifier())
    }

    fn paths(&self, context: &NSWritingToolsCoordinatorContext) -> Retained<NSArray<NSBezierPath>> {
        if self.owns_context(context) {
            NSArray::from_retained_slice(&[NSBezierPath::bezierPathWithRect(self.caret)])
        } else {
            NSArray::new()
        }
    }
}

define_class!(
    #[unsafe(super(objc2_foundation::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = State]
    struct Delegate;
    unsafe impl NSObjectProtocol for Delegate {}
    unsafe impl NSWritingToolsCoordinatorDelegate for Delegate {
        #[unsafe(method(writingToolsCoordinator:requestsContextsForScope:completion:))]
        fn contexts(
            &self,
            _: &NSWritingToolsCoordinator,
            _: NSWritingToolsCoordinatorContextScope,
            completion: &DynBlock<dyn Fn(NonNull<NSArray<NSWritingToolsCoordinatorContext>>)>,
        ) {
            let state = self.ivars();
            let contexts = if state.active() {
                let text = {
                    let mut selection = state.requestor.ivars().borrow_mut();
                    selection.writing_tools_requested = true;
                    selection.text.clone()
                };
                *state.buffer.borrow_mut() = Buffer::new(text.clone());
                let attributed = NSAttributedString::initWithString(
                    NSAttributedString::alloc(),
                    &NSString::from_str(&text),
                );
                let context = NSWritingToolsCoordinatorContext::initWithAttributedString_range(
                    NSWritingToolsCoordinatorContext::alloc(),
                    &attributed,
                    NSRange::new(0, text.encode_utf16().count()),
                );
                *state.context.borrow_mut() = Some(context.clone());
                NSArray::from_retained_slice(&[context])
            } else {
                NSArray::new()
            };
            completion.call((NonNull::from(&*contexts),));
        }
        #[unsafe(method(writingToolsCoordinator:replaceRange:inContext:proposedText:reason:animationParameters:completion:))]
        fn replace(
            &self,
            _: &NSWritingToolsCoordinator,
            range: NSRange,
            context: &NSWritingToolsCoordinatorContext,
            text: &NSAttributedString,
            reason: NSWritingToolsCoordinatorTextReplacementReason,
            _: Option<&NSWritingToolsCoordinatorAnimationParameters>,
            completion: &DynBlock<dyn Fn(*mut NSAttributedString)>,
        ) {
            let state = self.ivars();
            let accepted = state.owns_context(context)
                && state.requestor.ivars().borrow().editable
                && reason == NSWritingToolsCoordinatorTextReplacementReason::Noninteractive
                && state
                    .buffer
                    .borrow_mut()
                    .replace(range, &text.string().to_string());
            completion.call((if accepted {
                text as *const NSAttributedString as *mut NSAttributedString
            } else {
                std::ptr::null_mut()
            },));
        }
        #[unsafe(method(writingToolsCoordinator:selectRanges:inContext:completion:))]
        fn select(
            &self,
            _: &NSWritingToolsCoordinator,
            _: &NSArray<NSValue>,
            _: &NSWritingToolsCoordinatorContext,
            completion: &DynBlock<dyn Fn()>,
        ) {
            // Limited mode keeps its temporary selection inside the system UI.
            // The editor's original caret remains the write-back target.
            completion.call(());
        }
        #[unsafe(method(writingToolsCoordinator:requestsBoundingBezierPathsForRange:inContext:completion:))]
        fn bounds(
            &self,
            _: &NSWritingToolsCoordinator,
            _: NSRange,
            context: &NSWritingToolsCoordinatorContext,
            completion: &DynBlock<dyn Fn(NonNull<NSArray<NSBezierPath>>)>,
        ) {
            completion.call((NonNull::from(&*self.ivars().paths(context)),));
        }
        #[unsafe(method(writingToolsCoordinator:requestsUnderlinePathsForRange:inContext:completion:))]
        fn underlines(
            &self,
            _: &NSWritingToolsCoordinator,
            _: NSRange,
            _: &NSWritingToolsCoordinatorContext,
            completion: &DynBlock<dyn Fn(NonNull<NSArray<NSBezierPath>>)>,
        ) {
            completion.call((NonNull::from(&*NSArray::new()),));
        }
        #[unsafe(method(writingToolsCoordinator:prepareForTextAnimation:forRange:inContext:completion:))]
        fn prepare(
            &self,
            _: &NSWritingToolsCoordinator,
            _: NSWritingToolsCoordinatorTextAnimation,
            _: NSRange,
            _: &NSWritingToolsCoordinatorContext,
            completion: &DynBlock<dyn Fn()>,
        ) {
            completion.call(());
        }
        #[unsafe(method(writingToolsCoordinator:requestsPreviewForTextAnimation:ofRange:inContext:completion:))]
        fn preview(
            &self,
            _: &NSWritingToolsCoordinator,
            _: NSWritingToolsCoordinatorTextAnimation,
            _: NSRange,
            _: &NSWritingToolsCoordinatorContext,
            completion: &DynBlock<dyn Fn(*mut NSArray<NSTextPreview>)>,
        ) {
            completion.call((std::ptr::null_mut(),));
        }
        #[unsafe(method(writingToolsCoordinator:requestsPreviewForRect:inContext:completion:))]
        fn preview_rect(
            &self,
            _: &NSWritingToolsCoordinator,
            _: NSRect,
            _: &NSWritingToolsCoordinatorContext,
            completion: &DynBlock<dyn Fn(*mut NSTextPreview)>,
        ) {
            completion.call((std::ptr::null_mut(),));
        }
        #[unsafe(method(writingToolsCoordinator:finishTextAnimation:forRange:inContext:completion:))]
        fn finish_animation(
            &self,
            _: &NSWritingToolsCoordinator,
            _: NSWritingToolsCoordinatorTextAnimation,
            _: NSRange,
            _: &NSWritingToolsCoordinatorContext,
            completion: &DynBlock<dyn Fn()>,
        ) {
            completion.call(());
        }
        #[unsafe(method(writingToolsCoordinator:willChangeToState:completion:))]
        fn state_changed(
            &self,
            _: &NSWritingToolsCoordinator,
            state: NSWritingToolsCoordinatorState,
            completion: &DynBlock<dyn Fn()>,
        ) {
            completion.call(());
            if state == NSWritingToolsCoordinatorState::Inactive {
                let state = self.ivars();
                state.buffer.borrow().finish(
                    state.active.get(),
                    &mut state.requestor.ivars().borrow_mut(),
                );
            }
        }
    }
);

pub(super) struct Snapshot {
    delegate: Retained<Delegate>,
    coordinator: Retained<NSWritingToolsCoordinator>,
    previous: Option<Retained<NSWritingToolsCoordinator>>,
    items: Vec<Retained<NSMenuItem>>,
}

impl Snapshot {
    pub(super) fn new(
        view: &NSResponder,
        requestor: &TextRequestor,
        position: gpui::Point<gpui::Pixels>,
        line_height: gpui::Pixels,
    ) -> Option<Self> {
        let mtm = MainThreadMarker::new()?;
        objc2::runtime::AnyClass::get(c"NSWritingToolsCoordinator")?;
        if !NSWritingToolsCoordinator::isWritingToolsAvailable(mtm) {
            return None;
        }
        let view =
            unsafe { Retained::retain((view as *const NSResponder).cast_mut().cast::<NSView>())? };
        let bounds = view.bounds();
        let x = bounds.origin.x + f64::from(f32::from(position.x));
        let offset = f64::from(f32::from(position.y));
        let y = bounds.origin.y
            + if view.isFlipped() {
                offset
            } else {
                bounds.size.height - offset
            };
        let delegate = Delegate::alloc(mtm).set_ivars(State {
            view: view.clone(),
            requestor: unsafe { Retained::retain((requestor as *const TextRequestor).cast_mut())? },
            context: RefCell::new(None),
            buffer: RefCell::new(Buffer::new(requestor.ivars().borrow().text.clone())),
            caret: NSRect::new(
                NSPoint::new(x, y),
                NSSize::new(1.0, f64::from(f32::from(line_height)).max(1.0)),
            ),
            active: Cell::new(true),
        });
        let delegate: Retained<Delegate> = unsafe { msg_send![super(delegate), init] };
        let coordinator = NSWritingToolsCoordinator::initWithDelegate(
            NSWritingToolsCoordinator::alloc(mtm),
            Some(ProtocolObject::from_ref(&*delegate)),
        );
        coordinator.setPreferredBehavior(NSWritingToolsBehavior::Limited);
        coordinator.setPreferredResultOptions(NSWritingToolsResultOptions::PlainText);
        let previous = view.writingToolsCoordinator();
        view.setWritingToolsCoordinator(Some(&coordinator));
        let mut snapshot = Self {
            delegate,
            coordinator,
            previous,
            items: vec![],
        };
        // The public factory groups the standard entry points before additional
        // transformation actions. Preserve their system metadata and visuals.
        let native_items = NSMenuItem::writingToolsItems(mtm);
        for item in native_items.iter() {
            let group_identifier: Option<Retained<NSString>> =
                unsafe { msg_send![&*item, identifier] };
            let children = item.submenu().map_or_else(
                || NSArray::from_retained_slice(std::slice::from_ref(&item)),
                |menu| menu.itemArray(),
            );
            for child in children.iter() {
                if child.action() == Some(sel!(showWritingTools:)) && snapshot.items.len() < 3 {
                    let copied: Retained<NSMenuItem> = unsafe { msg_send![&*child, copy] };
                    // NSTextView uses the factory group identity on each flat
                    // entry. Preserve it when removing the submenu container,
                    // so AppKit can recognize the already-present native group.
                    if let Some(identifier) = group_identifier.as_deref() {
                        unsafe {
                            let _: () = msg_send![&*copied, setIdentifier: identifier];
                        }
                    }
                    // Keep the factory's nil target. NSApplication handles this
                    // responder-chain action for a custom NSView coordinator;
                    // NSView itself does not implement showWritingTools:.
                    snapshot.items.push(copied);
                }
            }
        }
        (!snapshot.items.is_empty()).then_some(snapshot)
    }

    pub(super) fn items(&self) -> &[Retained<NSMenuItem>] {
        &self.items
    }
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        let state = self.delegate.ivars();
        state.active.set(false);
        self.coordinator.stopWritingTools();
        if state
            .view
            .writingToolsCoordinator()
            .as_ref()
            .is_some_and(|current| std::ptr::eq(&**current, &*self.coordinator))
        {
            state
                .view
                .setWritingToolsCoordinator(self.previous.as_deref());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2::ClassType;

    #[test]
    fn public_coordinator_callbacks_are_registered() {
        for selector in [
            sel!(writingToolsCoordinator:requestsContextsForScope:completion:),
            sel!(writingToolsCoordinator:replaceRange:inContext:proposedText:reason:animationParameters:completion:),
            sel!(writingToolsCoordinator:willChangeToState:completion:),
        ] {
            assert!(Delegate::class().instance_method(selector).is_some());
        }
    }

    #[test]
    fn system_application_handles_the_standard_writing_tools_action() {
        assert!(
            NSApplication::class()
                .instance_method(sel!(showWritingTools:))
                .is_some()
        );
        // Our coordinator delegate supplies content, never an action trampoline
        // that incorrectly sends an NSTextView-only implementation to NSView.
        assert!(
            Delegate::class()
                .instance_method(sel!(showWritingTools:))
                .is_none()
        );
    }

    #[test]
    fn accepted_utf16_edits_are_collected_and_delivered_once() {
        let mut buffer = Buffer::new(String::new());
        assert!(buffer.replace(NSRange::new(0, 0), "Hello 😀"));
        assert!(!buffer.replace(NSRange::new(7, 1), "invalid surrogate"));
        assert!(buffer.replace(NSRange::new(6, 2), "world"));
        let (sender, mut receiver) = oneshot::channel();
        let mut selection = Selection {
            text: String::new(),
            active: true,
            editable: true,
            writing_tools_requested: false,
            replacement: Some(sender),
        };
        assert_eq!(receiver.try_recv().unwrap(), None);
        assert!(buffer.finish(true, &mut selection));
        assert_eq!(
            receiver.try_recv().unwrap(),
            Some(TextReplacement::PreserveStyles("Hello world".into()))
        );
        assert!(!buffer.finish(true, &mut selection));
    }

    #[test]
    fn unchanged_cancel_retired_and_readonly_snapshots_do_not_write_back() {
        let mut buffer = Buffer::new(String::new());
        let (sender, mut receiver) = oneshot::channel();
        let mut selection = Selection {
            text: String::new(),
            active: true,
            editable: true,
            writing_tools_requested: false,
            replacement: Some(sender),
        };
        assert!(!buffer.finish(true, &mut selection));
        assert!(buffer.replace(NSRange::new(0, 0), "Composed"));
        assert!(!buffer.finish(false, &mut selection));
        selection.editable = false;
        assert!(!buffer.finish(true, &mut selection));
        selection.editable = true;
        selection.active = false;
        assert!(!buffer.finish(true, &mut selection));
        assert_eq!(receiver.try_recv().unwrap(), None);
    }
}
