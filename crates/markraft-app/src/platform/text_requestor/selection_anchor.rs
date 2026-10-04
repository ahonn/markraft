//! Snapshot geometry for the public Writing Tools pasteboard presentation.
//!
//! AppKit asks the contextual menu's NSView for `selectionAnchorRect`, not its
//! Services requestor. Read only native snapshot data in that callback: calling
//! GPUI's text-input client here could reenter an outstanding editor update.

use crate::platform::text_geometry::view_rect;
use gpui::{Bounds, Pixels};
use objc2::{
    ClassType, MainThreadMarker, Message,
    encode::Encode,
    ffi,
    rc::Retained,
    runtime::{AnyClass, AnyObject, AnyProtocol, Imp, ProtocolBuilder, Sel},
    sel,
};
use objc2_app_kit::NSView;
use objc2_foundation::{NSRect, NSValue};
use std::{ffi::CString, ptr};

// The address is a process-lifetime association key, unique to this module.
static ANCHOR_KEY: u8 = 0;

/// Keep the selection anchor installed until its asynchronous service retires.
/// The NSView is main-thread-only, so this guard cannot cross threads.
/// The app retires the previous service before attaching another to this view.
pub(super) struct SelectionAnchor {
    view: Retained<NSView>,
    value: Retained<NSValue>,
    previous: Option<Retained<AnyObject>>,
}

impl SelectionAnchor {
    pub(super) fn new(view: &NSView, bounds: Bounds<Pixels>) -> Option<Self> {
        MainThreadMarker::new()?;
        if !install_selector(view.class()) {
            return None;
        }
        let rect = view_rect(bounds, view.bounds(), view.isFlipped());
        let value = NSValue::new(rect);
        // The association owns its value. Retain the predecessor before replacing
        // it so restoration does not rely on an autorelease.
        let previous = unsafe { Retained::retain(associated(view).cast_mut()) };
        unsafe { set_associated(view, Retained::as_ptr(&value).cast_mut().cast()) };
        Some(Self {
            view: view.retain(),
            value,
            previous,
        })
    }
}

impl Drop for SelectionAnchor {
    fn drop(&mut self) {
        // Do not overwrite a newer owner's geometry.
        if ptr::eq(associated(&self.view), Retained::as_ptr(&self.value).cast()) {
            let previous = self
                .previous
                .as_ref()
                .map_or(ptr::null_mut(), |value| Retained::as_ptr(value).cast_mut());
            unsafe { set_associated(&self.view, previous) };
        }
    }
}

fn associated(view: &NSView) -> *const AnyObject {
    // AppKit and this module access the association only on the main thread.
    unsafe {
        ffi::objc_getAssociatedObject(
            (view as *const NSView).cast(),
            (&raw const ANCHOR_KEY).cast(),
        )
    }
}

unsafe fn set_associated(view: &NSView, value: *mut AnyObject) {
    unsafe {
        ffi::objc_setAssociatedObject(
            (view as *const NSView).cast_mut().cast(),
            (&raw const ANCHOR_KEY).cast(),
            value,
            ffi::OBJC_ASSOCIATION_RETAIN_NONATOMIC,
        )
    };
}

fn install_selector(class: &AnyClass) -> bool {
    // Extend only the concrete editor view class, never all AppKit views.
    if ptr::eq(class, NSView::class()) {
        return false;
    }
    let selector = sel!(selectionAnchorRect);
    type AnchorMethod = unsafe extern "C-unwind" fn(&NSView, Sel) -> NSRect;
    // IMP erases the signature. The callback and encoding below both implement
    // the public NSRect-returning, zero-argument instance method, including the
    // target architecture's struct-return ABI.
    let implementation: Imp =
        unsafe { std::mem::transmute::<AnchorMethod, Imp>(selection_anchor_rect as AnchorMethod) };
    if let Some(method) = class.instance_method(selector) {
        // GPUI or another integration may already provide the public selector.
        // Respect that implementation; never replace or swizzle it.
        return ptr::fn_addr_eq(method.implementation(), implementation);
    }
    // Unlike an Objective-C @protocol reference, Rust bindings do not emit
    // protocol metadata. AppKit may not register this optional protocol itself.
    let protocol = AnyProtocol::get(c"NSViewContentSelectionInfo").or_else(|| {
        let mut builder = ProtocolBuilder::new(c"NSViewContentSelectionInfo")?;
        builder.add_protocol(AnyProtocol::get(c"NSObject")?);
        builder.add_method_description::<(), NSRect>(selector, false);
        Some(builder.register())
    });
    let Some(protocol) = protocol else {
        return false;
    };
    let encoding = CString::new(format!("{}@:", NSRect::ENCODING))
        .expect("Objective-C type encodings contain no NUL");
    let class_ptr = (class as *const AnyClass).cast_mut();
    // class_addMethod copies the encoding and refuses to replace an existing
    // method. Only this public selector is added; the view's class is unchanged.
    if !unsafe { ffi::class_addMethod(class_ptr, selector, implementation, encoding.as_ptr()) }
        .as_bool()
    {
        return false;
    }
    if !class.conforms_to(protocol) {
        unsafe { ffi::class_addProtocol(class_ptr, protocol) };
    }
    true
}

unsafe extern "C-unwind" fn selection_anchor_rect(view: &NSView, _: Sel) -> NSRect {
    let value = associated(view);
    if value.is_null() {
        // Preserve AppKit's ordinary whole-view anchor outside a live session.
        return view.bounds();
    }
    // This private key stores only an NSValue containing an NSRect. Its owning
    // association remains alive throughout this synchronous main-thread call.
    unsafe { (&*value.cast::<NSValue>()).get::<NSRect>() }
}
