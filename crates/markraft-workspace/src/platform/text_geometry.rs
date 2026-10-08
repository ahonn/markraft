//! Shared selection geometry for native text presentations.

use gpui::{Bounds, Pixels, Point};
use objc2_foundation::{NSPoint, NSRect, NSSize};

/// Convert an exact text baseline without adding rectangle or caret dimensions.
pub(crate) fn view_point(point: Point<Pixels>, native: NSRect, flipped: bool) -> NSPoint {
    let x = f64::from(f32::from(point.x));
    let y = f64::from(f32::from(point.y));
    NSPoint::new(
        native.origin.x + x,
        native.origin.y + if flipped { y } else { native.size.height - y },
    )
}

/// Convert GPUI window-local, top-down points into the native view's bounds.
/// A caret needs a nonempty width to avoid AppKit's whole-view fallback.
pub(crate) fn view_rect(bounds: Bounds<Pixels>, native: NSRect, flipped: bool) -> NSRect {
    let x = f64::from(f32::from(bounds.origin.x));
    let y = f64::from(f32::from(bounds.origin.y));
    let width = f64::from(f32::from(bounds.size.width)).max(1.0);
    let height = f64::from(f32::from(bounds.size.height)).max(1.0);
    NSRect::new(
        NSPoint::new(
            native.origin.x + x,
            native.origin.y
                + if flipped {
                    y
                } else {
                    native.size.height - y - height
                },
        ),
        NSSize::new(width, height),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, px, size};

    #[test]
    fn text_baseline_preserves_the_character_origin() {
        let baseline = point(px(30.), px(54.));
        let native = NSRect::new(NSPoint::new(10., 15.), NSSize::new(500., 300.));
        assert_eq!(view_point(baseline, native, true), NSPoint::new(40., 69.));
        assert_eq!(view_point(baseline, native, false), NSPoint::new(40., 261.));
    }

    #[test]
    fn selection_rect_preserves_dimensions_in_both_coordinate_systems() {
        let selection = Bounds::new(point(px(30.), px(40.)), size(px(80.), px(20.)));
        let native = NSRect::new(NSPoint::new(10., 15.), NSSize::new(500., 300.));
        assert_eq!(
            view_rect(selection, native, true),
            NSRect::new(NSPoint::new(40., 55.), NSSize::new(80., 20.)),
        );
        assert_eq!(
            view_rect(selection, native, false),
            NSRect::new(NSPoint::new(40., 255.), NSSize::new(80., 20.)),
        );
    }

    #[test]
    fn caret_rect_stays_nonempty_at_the_bottom_edge() {
        let caret = Bounds::new(point(px(20.), px(80.)), size(px(0.), px(20.)));
        let native = NSRect::new(NSPoint::new(0., 0.), NSSize::new(200., 100.));
        assert_eq!(
            view_rect(caret, native, false),
            NSRect::new(NSPoint::new(20., 0.), NSSize::new(1., 20.)),
        );
    }
}
