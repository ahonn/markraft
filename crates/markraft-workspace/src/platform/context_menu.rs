//! Native menu tracking without retaining a GPUI entity or application borrow.
//!
//! Each popup owns its action target. Workspace menus neither consume Muda
//! events nor replace the host's process-wide menu event handler.

mod caption;
pub(crate) use caption::selection_label;

use super::{NSPoint, NSRect, native_view, native_window};
use crate::locale::Message;
use gpui::{Bounds, Pixels, Point, Window};
use muda::{CheckMenuItem, ContextMenu, IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use objc2::{
    DefinedClass, MainThreadMarker, MainThreadOnly, class, define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, NSObjectProtocol},
    sel,
};
use objc2_app_kit::NSImage;
use objc2_foundation::{NSArray, NSObject, NSString};
use std::{cell::Cell, ptr};

#[derive(Clone, Copy, Debug)]
pub(crate) enum TitleStyle {
    Bold,
    Italic,
    Underline,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ItemAppearance {
    title_style: Option<TitleStyle>,
}

/// Semantic menu data. Item IDs belong to the caller and are local to this menu.
#[derive(Clone, Debug)]
pub(crate) enum Row {
    Item {
        id: usize,
        label: String,
        enabled: bool,
        checked: Option<bool>,
        symbol: Option<&'static str>,
        appearance: ItemAppearance,
    },
    Separator,
    WritingTools,
    Submenu {
        label: String,
        enabled: bool,
        children: Vec<Row>,
        symbol: Option<&'static str>,
    },
    DetectedData {
        result: super::text_checking::DetectedData,
    },
}

impl Row {
    pub(crate) fn with_title_style(mut self, style: TitleStyle) -> Self {
        if let Self::Item { appearance, .. } = &mut self {
            appearance.title_style = Some(style);
        }
        self
    }

    pub(crate) fn with_symbol(mut self, name: &'static str) -> Self {
        match &mut self {
            Self::Item { symbol, .. } | Self::Submenu { symbol, .. } => *symbol = Some(name),
            _ => {}
        }
        self
    }
}

/// Owns the native objects while a deferred presentation is pending or tracking.
/// This must be created, shown, and dropped on the main thread.
pub(crate) struct PreparedMenu {
    menu: Menu,
    view: Retained<AnyObject>,
    window: Retained<AnyObject>,
    target: Retained<WorkspaceMenuTarget>,
    _system_menus: Vec<Retained<AnyObject>>,
}

impl PreparedMenu {
    pub(crate) fn new(
        rows: &[Row],
        window: &Window,
        anchor: Bounds<Pixels>,
        writing_tools: Option<&[Retained<objc2_app_kit::NSMenuItem>]>,
    ) -> Result<Self, Message> {
        MainThreadMarker::new().ok_or_else(|| failure("menu prepared off the main thread"))?;
        // GPUI owns both objects. Keep them alive while presentation is deferred,
        // then check that the view is still attached to this visible window.
        let view = unsafe { Retained::retain(native_view(window)?) }
            .ok_or_else(|| failure("the editor view is unavailable"))?;
        let native_window = unsafe { Retained::retain(native_window(window)?) }
            .ok_or_else(|| failure("the editor window is unavailable"))?;
        let menu = Menu::new();
        let target = WorkspaceMenuTarget::new();
        for row in rows
            .iter()
            .filter(|row| !matches!(row, Row::DetectedData { .. } | Row::WritingTools))
        {
            menu.append(&*build_row(row)?).map_err(failure)?;
        }
        let mut system_menus = Vec::new();
        decorate(
            menu.ns_menu().cast(),
            rows,
            window,
            anchor,
            &mut system_menus,
            writing_tools.unwrap_or_default(),
            &target,
        )?;
        if writing_tools.is_some() {
            // A coordinated snapshot gets exactly one group: AppKit's Services
            // group for selected text, or our explicit group for an empty caret.
            // The Services insertion is independent of this native menu option.
            unsafe {
                let native = menu.ns_menu().cast::<AnyObject>();
                let _: () = msg_send![native, setAutomaticallyInsertsWritingToolsItems: false];
            }
        }
        Ok(Self {
            menu,
            view,
            window: native_window,
            target,
            _system_menus: system_menus,
        })
    }

    /// Track synchronously, returning an item ID or `None` when dismissed.
    ///
    /// Call from the foreground executor **outside** a GPUI entity/window update:
    /// AppKit runs a nested event loop here and may dispatch GPUI callbacks.
    pub(crate) fn show(self, position: Point<Pixels>) -> Option<usize> {
        MainThreadMarker::new()?;
        let attached: *mut AnyObject = unsafe { msg_send![&*self.view, window] };
        let visible: bool = unsafe { msg_send![&*self.window, isVisible] };
        if attached != Retained::as_ptr(&self.window).cast_mut() || !visible {
            return None;
        }
        let bounds: NSRect = unsafe { msg_send![&*self.view, bounds] };
        let flipped: bool = unsafe { msg_send![&*self.view, isFlipped] };
        let point = view_point(position, bounds, flipped);
        let native = self.menu.ns_menu().cast::<AnyObject>();
        // Access the NSMenu without keeping a Muda RefCell borrow through the
        // nested loop. GPUI positions are logical points, like AppKit's bounds.
        unsafe {
            let location: NSPoint =
                msg_send![&*self.view, convertPoint: point, toView: ptr::null_mut::<AnyObject>()];
            let number: isize = msg_send![&*self.window, windowNumber];
            let event: Option<Retained<AnyObject>> = msg_send![class!(NSEvent),
                mouseEventWithType: 3usize, location: location, modifierFlags: 0usize,
                timestamp: 0.0f64, windowNumber: number, context: ptr::null_mut::<AnyObject>(),
                eventNumber: 0isize, clickCount: 1isize, pressure: 1.0f32
            ];
            // AppKit inserts Services using the snapshot in the responder chain.
            // Writing Tools requests coordinator context, but AppKit may return
            // the result through the same pasteboard bridge as ordinary Services.
            if let Some(event) = event {
                let _: () = msg_send![class!(NSMenu), popUpContextMenu: native,
                    withEvent: &*event, forView: &*self.view];
            }
        }
        self.target.ivars().get()
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

fn build_row(row: &Row) -> Result<Box<dyn IsMenuItem>, Message> {
    match row {
        Row::Item {
            label,
            enabled,
            checked,
            ..
        } => {
            let item: Box<dyn IsMenuItem> = match checked {
                Some(checked) => Box::new(CheckMenuItem::new(label, *enabled, *checked, None)),
                None => Box::new(MenuItem::new(label, *enabled, None)),
            };
            Ok(item)
        }
        Row::Separator => Ok(Box::new(PredefinedMenuItem::separator())),
        Row::Submenu {
            label,
            enabled,
            children,
            ..
        } => {
            let submenu = Submenu::new(label, *enabled);
            for child in children {
                submenu.append(&*build_row(child)?).map_err(failure)?;
            }
            Ok(Box::new(submenu))
        }
        Row::DetectedData { .. } | Row::WritingTools => {
            unreachable!("system items are inserted after native construction")
        }
    }
}

/// Keep symbols as native template images so AppKit supplies selection, disabled,
/// dark-mode and display-scale rendering, just as it does for its own menu items.
fn decorate(
    menu: *mut AnyObject,
    rows: &[Row],
    window: &Window,
    anchor: Bounds<Pixels>,
    system_menus: &mut Vec<Retained<AnyObject>>,
    writing_tools: &[Retained<objc2_app_kit::NSMenuItem>],
    target: &WorkspaceMenuTarget,
) -> Result<(), Message> {
    let mut index = 0isize;
    for row in rows {
        if matches!(row, Row::WritingTools) {
            if !writing_tools.is_empty() {
                unsafe {
                    let separator: Retained<AnyObject> =
                        msg_send![class!(NSMenuItem), separatorItem];
                    let _: () = msg_send![menu, insertItem: &*separator, atIndex: index];
                    index += 1;
                    for item in writing_tools {
                        let copied: Retained<AnyObject> = msg_send![&**item, copy];
                        let _: () = msg_send![menu, insertItem: &*copied, atIndex: index];
                        index += 1;
                    }
                }
            }
            continue;
        }
        if let Row::DetectedData { result } = row {
            let source =
                super::text_services::TextAnchor::new(window, anchor)?.data_menu(result)?;
            unsafe {
                let items: Retained<NSArray<AnyObject>> = msg_send![&*source, itemArray];
                for item in items.iter() {
                    let copied: Retained<AnyObject> = msg_send![&*item, copy];
                    let _: () = msg_send![menu, insertItem: &*copied, atIndex: index];
                    index += 1;
                }
            }
            system_menus.push(source);
            continue;
        }
        let item: *mut AnyObject = unsafe { msg_send![menu, itemAtIndex: index] };
        let symbol = match row {
            Row::Item { symbol, .. } | Row::Submenu { symbol, .. } => *symbol,
            _ => None,
        };
        if let Some(symbol) = symbol
            && let Some(image) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
                &NSString::from_str(symbol),
                None,
            )
        {
            image.setTemplate(true);
            unsafe {
                let _: () = msg_send![item, setImage: &*image];
            }
            prefer_visible_image(item);
        }
        if let Row::Submenu { children, .. } = row {
            let submenu: *mut AnyObject = unsafe { msg_send![item, submenu] };
            decorate(
                submenu,
                children,
                window,
                anchor,
                system_menus,
                writing_tools,
                target,
            )?;
        }
        if let Row::Item {
            id,
            label,
            appearance,
            ..
        } = row
        {
            // Muda builds and retains the menu hierarchy. Route only our items
            // directly, leaving Services and host menu targets untouched.
            unsafe {
                let _: () = msg_send![item, setTag: *id as isize];
                let _: () = msg_send![item, setTarget: target];
                let _: () = msg_send![item, setAction: sel!(markraftWorkspaceSelect:)];
            }
            decorate_item(item, label, appearance);
        }
        index += 1;
    }
    Ok(())
}

fn decorate_item(item: *mut AnyObject, label: &str, appearance: &ItemAppearance) {
    use objc2_app_kit::{NSFontAttributeName, NSUnderlineStyleAttributeName};
    unsafe {
        if let Some(style) = appearance.title_style {
            let attributes: Retained<AnyObject> = msg_send![class!(NSMutableDictionary), new];
            match style {
                TitleStyle::Bold | TitleStyle::Italic => {
                    let font: Retained<AnyObject> =
                        msg_send![class!(NSFont), menuFontOfSize: 0.0f64];
                    let manager: *mut AnyObject =
                        msg_send![class!(NSFontManager), sharedFontManager];
                    let mask = if matches!(style, TitleStyle::Bold) {
                        2usize
                    } else {
                        1usize
                    };
                    let font: Retained<AnyObject> =
                        msg_send![manager, convertFont: &*font, toHaveTrait: mask];
                    let _: () =
                        msg_send![&*attributes, setObject: &*font, forKey: NSFontAttributeName];
                }
                TitleStyle::Underline => {
                    let value: Retained<AnyObject> =
                        msg_send![class!(NSNumber), numberWithInteger: 1isize];
                    let _: () = msg_send![&*attributes, setObject: &*value, forKey: NSUnderlineStyleAttributeName];
                }
            }
            let title: objc2::rc::Allocated<AnyObject> =
                msg_send![class!(NSAttributedString), alloc];
            let title: Retained<AnyObject> = msg_send![title, initWithString: &*NSString::from_str(label), attributes: &*attributes];
            let _: () = msg_send![item, setAttributedTitle: &*title];
        }
    }
}

fn prefer_visible_image(item: *mut AnyObject) {
    // macOS 27 hides images by default; NSTextView explicitly keeps these
    // symbols visible. Earlier releases already display assigned images.
    unsafe {
        let available: bool =
            msg_send![item, respondsToSelector: sel!(setPreferredImageVisibility:)];
        if available {
            let _: () = msg_send![item, setPreferredImageVisibility: 1isize];
        }
    }
}

fn failure(detail: impl std::fmt::Display) -> Message {
    log::warn!("the context menu could not be displayed: {detail}");
    Message::new("error.native-window-control")
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Cell<Option<usize>>]
    struct WorkspaceMenuTarget;

    unsafe impl NSObjectProtocol for WorkspaceMenuTarget {}

    impl WorkspaceMenuTarget {
        #[unsafe(method(markraftWorkspaceSelect:))]
        fn select(&self, sender: &AnyObject) {
            let tag: isize = unsafe { msg_send![sender, tag] };
            self.ivars().set(usize::try_from(tag).ok());
        }
    }
);

impl WorkspaceMenuTarget {
    fn new() -> Retained<Self> {
        let allocated =
            Self::alloc(MainThreadMarker::new().expect("native menus use the main thread"))
                .set_ivars(Cell::new(None));
        unsafe { msg_send![super(allocated), init] }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_coordinates_respect_native_view_orientation() {
        let bounds = NSRect {
            origin: NSPoint { x: 2., y: 3. },
            size: NSPoint { x: 600., y: 400. },
        };
        let position = gpui::point(gpui::px(20.), gpui::px(30.));
        let flipped = view_point(position, bounds, true);
        assert_eq!((flipped.x, flipped.y), (22., 33.));
        let unflipped = view_point(position, bounds, false);
        assert_eq!((unflipped.x, unflipped.y), (22., 373.));
    }
}
