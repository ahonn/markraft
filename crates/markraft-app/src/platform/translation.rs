//! Public SwiftUI translation popover, anchored in the native editor view.

use super::{NSRect, native_view};
use crate::locale::Message;
use futures_channel::oneshot;
use gpui::{Pixels, Point, Window};
use objc2::{MainThreadMarker, msg_send};
use std::cell::RefCell;
use std::ffi::{CStr, CString, c_char, c_void};
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::rc::Rc;

unsafe extern "C" {
    fn markraft_translation_available() -> bool;
    fn markraft_translation_begin(
        view: *mut c_void,
        x: f64,
        y: f64,
        text: *const c_char,
        editable: bool,
        context: *mut c_void,
        callback: extern "C" fn(*mut c_void, *const c_char),
    ) -> *mut c_void;
    fn markraft_translation_cancel(session: *mut c_void);
}

type Completion = RefCell<Option<oneshot::Sender<Option<String>>>>;

/// Cancellation dismisses the popover and resolves the receiver with `None`.
/// Keep this alive until the receiver resolves or the editor context expires.
pub(crate) struct Translation {
    native: NonNull<c_void>,
    // Swift holds a borrowed pointer to this stable allocation until cancel.
    _completion: Box<Completion>,
    _main_thread: PhantomData<Rc<()>>,
}

pub(crate) fn available() -> bool {
    unsafe { markraft_translation_available() }
}

impl Translation {
    pub(crate) fn show(
        window: &Window,
        position: Point<Pixels>,
        text: &str,
        editable: bool,
    ) -> Result<(Self, oneshot::Receiver<Option<String>>), Message> {
        MainThreadMarker::new().ok_or_else(|| Message::new("error.native-window-control"))?;
        if !available() {
            return Err(Message::new("error.native-window-control"));
        }
        let view = native_view(window)?;
        let bounds: NSRect = unsafe { msg_send![view, bounds] };
        let flipped: bool = unsafe { msg_send![view, isFlipped] };
        let x = bounds.origin.x + f64::from(f32::from(position.x));
        let offset_y = f64::from(f32::from(position.y));
        let y = bounds.origin.y
            + if flipped {
                offset_y
            } else {
                bounds.size.y - offset_y
            };
        let text = CString::new(text).map_err(|_| Message::new("error.native-window-control"))?;
        let (sender, receiver) = oneshot::channel();
        let mut completion = Box::new(RefCell::new(Some(sender)));
        let native = unsafe {
            markraft_translation_begin(
                view.cast(),
                x,
                y,
                text.as_ptr(),
                editable,
                (&mut *completion as *mut Completion).cast(),
                completed,
            )
        };
        let native =
            NonNull::new(native).ok_or_else(|| Message::new("error.native-window-control"))?;
        Ok((
            Self {
                native,
                _completion: completion,
                _main_thread: PhantomData,
            },
            receiver,
        ))
    }
}

extern "C" fn completed(context: *mut c_void, text: *const c_char) {
    // Swift invokes this on the main thread at most once. The owning Translation
    // keeps the allocation valid until native cancellation has returned.
    let completion = unsafe { &*context.cast::<Completion>() };
    let replacement = if text.is_null() {
        None
    } else {
        Some(
            unsafe { CStr::from_ptr(text) }
                .to_string_lossy()
                .into_owned(),
        )
    };
    if let Some(sender) = completion.borrow_mut().take() {
        let _ = sender.send(replacement);
    }
}

impl Drop for Translation {
    fn drop(&mut self) {
        unsafe { markraft_translation_cancel(self.native.as_ptr()) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_callback_copies_utf8_text_and_completes_once() {
        let (sender, mut receiver) = oneshot::channel();
        let mut completion = Box::new(RefCell::new(Some(sender)));
        let context = (&mut *completion as *mut Completion).cast();
        let translated = CString::new("翻译完成 — café").unwrap();
        completed(context, translated.as_ptr());
        drop(translated);
        completed(context, std::ptr::null());
        assert_eq!(
            receiver.try_recv().unwrap(),
            Some(Some("翻译完成 — café".into()))
        );
    }

    #[test]
    fn native_dismissal_resolves_without_replacement() {
        let (sender, mut receiver) = oneshot::channel();
        let mut completion = Box::new(RefCell::new(Some(sender)));
        completed(
            (&mut *completion as *mut Completion).cast(),
            std::ptr::null(),
        );
        assert_eq!(receiver.try_recv().unwrap(), Some(None));
    }
}
