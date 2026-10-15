//! The bar caret of an idle editor, blinked by Core Animation.
//!
//! Each blink that GPUI paints is a Metal frame, and the GPU driver keeps its
//! working pools (about 110 MB) resident while frames keep coming. An opacity
//! animation on a layer is composited by the window server instead, so once
//! [`BLINK_TIMEOUT`](super::BLINK_TIMEOUT) has passed the editor hands its bar
//! to a layer above GPUI's own and stops painting a caret.

#[cfg(target_os = "macos")]
pub(crate) use mac::NativeCaret;
#[cfg(not(target_os = "macos"))]
pub(crate) use other::NativeCaret;

#[cfg(target_os = "macos")]
mod mac {
    use gpui::{Bounds, Hsla, Pixels, Rgba, Window, point, px};
    use objc2::rc::Retained;
    use objc2_app_kit::NSView;
    use objc2_core_foundation::{CGFloat, CGPoint, CGRect, CGSize};
    use objc2_core_graphics::CGColor;
    use objc2_foundation::{NSArray, NSNumber, ns_string};
    use objc2_quartz_core::{
        CAKeyframeAnimation, CALayer, CAMediaTiming, CATransaction, kCAAnimationDiscrete,
    };
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    use crate::caret::BLINK_INTERVAL;

    /// A layer over the window that blinks a bar while it is shown.
    #[derive(Default)]
    pub(crate) struct NativeCaret {
        layer: Option<Retained<CALayer>>,
    }

    impl NativeCaret {
        /// Show a bar of `color` at `bounds`, in window coordinates. It starts
        /// in the hidden half of a blink, so it never shows beside a bar that
        /// GPUI painted. Returns `false` when the window has no layer to
        /// hold it.
        pub(crate) fn show(
            &mut self,
            window: &Window,
            bounds: Bounds<Pixels>,
            color: Hsla,
        ) -> bool {
            self.hide();
            let Some(metal) = native_view(window).and_then(|view| view.layer()) else {
                return false;
            };
            // The window server does not composite a Metal layer's sublayers,
            // so the bar goes beside GPUI's layer, just above it.
            let Some(host) = metal.superlayer() else {
                return false;
            };
            let frame = host.convertRect_fromLayer(
                layer_rect(&metal, snap(bounds, window.scale_factor())),
                Some(&metal),
            );
            let Rgba { r, g, b, a } = color.to_rgb();
            // GPUI's colors are sRGB, and its Metal layer is matched as sRGB.
            let color = CGColor::new_srgb(r.into(), g.into(), b.into(), a.into());
            let layer = CALayer::new();
            without_actions(|| {
                layer.setFrame(frame);
                layer.setBackgroundColor(Some(&color));
                layer.addAnimation_forKey(&blink(), Some(ns_string!("blink")));
                host.insertSublayer_above(&layer, Some(&metal));
            });
            self.layer = Some(layer);
            true
        }

        pub(crate) fn hide(&mut self) {
            if let Some(layer) = self.layer.take() {
                without_actions(|| layer.removeFromSuperlayer());
            }
        }
    }

    impl Drop for NativeCaret {
        fn drop(&mut self) {
            self.hide();
        }
    }

    /// Hidden for one interval, then shown for one, the same cadence that GPUI
    /// blinks with.
    fn blink() -> Retained<CAKeyframeAnimation> {
        let animation = CAKeyframeAnimation::animationWithKeyPath(Some(ns_string!("opacity")));
        let values = NSArray::from_retained_slice(&[NSNumber::new_f32(0.), NSNumber::new_f32(1.)]);
        // SAFETY: `opacity` animates numbers, and an array of numbers is an
        // array of objects.
        unsafe { animation.setValues(Some(values.cast_unchecked())) };
        // A discrete animation holds each value from its key time to the next,
        // so it takes one key time more than values, ending at 1.
        animation.setKeyTimes(Some(&NSArray::from_retained_slice(&[
            NSNumber::new_f32(0.),
            NSNumber::new_f32(0.5),
            NSNumber::new_f32(1.),
        ])));
        // SAFETY: a constant of Core Animation, valid for the life of the process.
        animation.setCalculationMode(unsafe { kCAAnimationDiscrete });
        animation.setDuration(2. * BLINK_INTERVAL.as_secs_f64());
        animation.setRepeatCount(f32::INFINITY);
        animation.setRemovedOnCompletion(false);
        animation
    }

    /// The edges GPUI paints a quad at: each on the nearest device pixel,
    /// half a pixel rounding toward zero. Without this the layer would blend
    /// a bar that falls between pixels into a wider, paler one.
    fn snap(bounds: Bounds<Pixels>, scale: f32) -> Bounds<Pixels> {
        let snap = |value: Pixels| {
            let device = f32::from(value) * scale;
            px(device.signum() * (device.abs() - 0.5).ceil() / scale)
        };
        Bounds::from_corners(
            point(snap(bounds.left()), snap(bounds.top())),
            point(snap(bounds.right()), snap(bounds.bottom())),
        )
    }

    /// `bounds`, counted from the window's top-left, in `layer`'s own space.
    fn layer_rect(layer: &CALayer, bounds: Bounds<Pixels>) -> CGRect {
        let [x, top, width, height] = [
            bounds.left(),
            bounds.top(),
            bounds.size.width,
            bounds.size.height,
        ]
        .map(|value| CGFloat::from(f32::from(value)));
        // A layer that is not flipped counts y up from its bottom edge.
        let y = if layer.isGeometryFlipped() {
            top
        } else {
            layer.bounds().size.height - top - height
        };
        CGRect::new(CGPoint::new(x, y), CGSize::new(width, height))
    }

    /// Change layers at once: a layer's frame and color otherwise animate.
    fn without_actions(change: impl FnOnce()) {
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        change();
        CATransaction::commit();
    }

    fn native_view(window: &Window) -> Option<Retained<NSView>> {
        let handle = HasWindowHandle::window_handle(window).ok()?;
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
            return None;
        };
        // SAFETY: GPUI's view is an NSView, used on the main thread, for as
        // long as its window lives.
        unsafe { Retained::retain(handle.ns_view.as_ptr().cast::<NSView>()) }
    }
}

#[cfg(not(target_os = "macos"))]
mod other {
    use gpui::{Bounds, Hsla, Pixels, Window};

    /// No platform layer: GPUI keeps the caret, and an idle one stays visible.
    #[derive(Default)]
    pub(crate) struct NativeCaret;

    impl NativeCaret {
        pub(crate) fn show(&mut self, _: &Window, _: Bounds<Pixels>, _: Hsla) -> bool {
            false
        }

        pub(crate) fn hide(&mut self) {}
    }
}
