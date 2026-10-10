//! The overlay scrollbar every scrolling surface shares.
//!
//! [`Scrollbar`] follows a scroller that tracks a [`ScrollHandle`]: it draws the thumb
//! over the scroller's right edge and lets the pointer drag it. Add it after the
//! scroller, in the same parent. It then reads the geometry the scroller published this
//! frame, paints over the content, and does not scroll away with it.
//!
//! A grid wider than the note scrolls sideways by the editor's own means, with no handle.
//! It gets the same thumb, laid along its bottom edge.
//!
//! The system says when a scrollbar shows: always, or only around a scroll. In the
//! second case the thumb appears when its content scrolls, stays while the pointer holds
//! it, and fades once both have rested.

use gpui::*;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::{Duration, Instant},
};

const THUMB_WIDTH: Pixels = px(6.);
/// The gap between the thumb and the edge of the scroller it runs along.
const EDGE_GAP: Pixels = px(3.);
/// The gap between the thumb's travel and the scroller's edges at its two ends.
const END_GAP: Pixels = px(4.);
/// How much of the scroller's edge takes the pointer beside the thumb, which is too
/// thin to aim at.
const GRIP_WIDTH: Pixels = px(14.);
const MIN_THUMB: Pixels = px(24.);
/// A track shorter than this shows no thumb: it would have nowhere to travel.
const MIN_TRACK: Pixels = px(32.);
/// How much more opaque the thumb is while the pointer holds it.
const HELD_EMPHASIS: f32 = 1.5;
/// How long the thumb stays once its content has rested and the pointer has let go.
const LINGER: Duration = Duration::from_millis(1200);
/// How long the thumb takes to fade away. It appears at once.
const FADE: Duration = Duration::from_millis(250);

type HoldListener = Rc<dyn Fn(&bool, &mut Window, &mut App)>;
type ScrollTo = Rc<dyn Fn(Pixels, &mut Window, &mut App)>;

/// What a thumb scrolls. How far it is scrolled is a distance from the start of the
/// content, whichever way the source itself counts.
#[derive(Clone)]
enum Scrolled {
    /// The vertical scroll of the scroller that tracks this handle.
    Handle(ScrollHandle),
    /// The sideways scroll of a strip the caller moves itself: where the strip is this
    /// frame, how far it is scrolled, and how far it may be.
    Strip {
        strip: Bounds<Pixels>,
        scrolled: Pixels,
        overflow: Pixels,
        scroll_to: ScrollTo,
    },
}

impl Scrolled {
    fn axis(&self) -> Axis {
        match self {
            Self::Handle(_) => Axis::Vertical,
            Self::Strip { .. } => Axis::Horizontal,
        }
    }

    fn viewport(&self) -> Bounds<Pixels> {
        match self {
            Self::Handle(handle) => handle.bounds(),
            Self::Strip { strip, .. } => *strip,
        }
    }

    /// How far the content can scroll.
    fn max(&self) -> Pixels {
        match self {
            Self::Handle(handle) => handle.max_offset().y,
            Self::Strip { overflow, .. } => *overflow,
        }
    }

    fn scrolled(&self) -> Pixels {
        match self {
            Self::Handle(handle) => -handle.offset().y,
            Self::Strip { scrolled, .. } => *scrolled,
        }
    }

    fn scroll_to(&self, scrolled: Pixels, window: &mut Window, cx: &mut App) {
        match self {
            Self::Handle(handle) => {
                let mut offset = handle.offset();
                offset.y = -scrolled;
                handle.set_offset(offset);
            }
            Self::Strip { scroll_to, .. } => scroll_to(scrolled, window, cx),
        }
    }
}

/// A draggable thumb for the vertical scroll of one [`ScrollHandle`]. It shows when the
/// system's scroll bar setting says it does.
pub struct Scrollbar {
    id: ElementId,
    scrolled: Scrolled,
    color: Hsla,
    top: Pixels,
    bottom: Pixels,
    /// Stands in for the system's setting.
    auto_hide: Option<bool>,
    on_hold: Option<HoldListener>,
}

impl Scrollbar {
    /// The room a scroller keeps at its right edge for the thumb to stand clear of its
    /// content.
    pub const LANE: Pixels = px(12.);

    /// [`Self::LANE`] while the content of the scroller that tracks `handle` overflows,
    /// and nothing while it fits. A scroller whose rows reach its right edge pads that
    /// edge by this much, so the thumb covers no row.
    pub fn lane(handle: &ScrollHandle) -> Pixels {
        // What the last frame measured. A scrollbar asks for another frame when its
        // scroller starts or stops overflowing, so a stale answer lasts one frame.
        if handle.max_offset().y > px(1.) {
            Self::LANE
        } else {
            px(0.)
        }
    }

    pub fn new(id: impl Into<ElementId>, handle: &ScrollHandle, color: Hsla) -> Self {
        Self::of(id.into(), Scrolled::Handle(handle.clone()), color)
    }

    /// A thumb along the bottom edge of `strip`, which shows a part of something
    /// `overflow` wider than itself, scrolled `scrolled` from its start. Dragging the
    /// thumb asks `scroll_to` for another place.
    pub(crate) fn sideways(
        id: impl Into<ElementId>,
        strip: Bounds<Pixels>,
        scrolled: Pixels,
        overflow: Pixels,
        color: Hsla,
        scroll_to: impl Fn(Pixels, &mut Window, &mut App) + 'static,
    ) -> Self {
        let scrolled = Scrolled::Strip {
            strip,
            scrolled,
            overflow,
            scroll_to: Rc::new(scroll_to),
        };
        Self::of(id.into(), scrolled, color)
    }

    fn of(id: ElementId, scrolled: Scrolled, color: Hsla) -> Self {
        Self {
            id,
            scrolled,
            color,
            top: px(0.),
            bottom: px(0.),
            auto_hide: None,
            on_hold: None,
        }
    }

    /// Keep the thumb clear of what covers the scroller's top and bottom edges.
    pub fn inset(mut self, top: Pixels, bottom: Pixels) -> Self {
        self.top = top;
        self.bottom = bottom;
        self
    }

    /// Show the thumb only around a scroll, or always, whatever the system says.
    #[cfg(test)]
    pub(crate) fn auto_hide(mut self, auto_hide: bool) -> Self {
        self.auto_hide = Some(auto_hide);
        self
    }

    /// Hear when the pointer takes hold of the thumb, by resting on it or dragging it,
    /// and when it lets go.
    pub fn on_hold(mut self, listener: impl Fn(&bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_hold = Some(Rc::new(listener));
        self
    }
}

/// Where the thumb travels for one scroller, in window coordinates. Lengths and places
/// are along the scrollbar's axis unless they say otherwise.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Geometry {
    axis: Axis,
    /// The thumb's leading edge with nothing scrolled.
    start: Pixels,
    /// How far the thumb's leading edge moves from there to the end of the content.
    travel: Pixels,
    thumb: Pixels,
    /// The scroller's edge the thumb runs along: the right one, or the bottom one.
    edge: Pixels,
    /// How far the content can scroll.
    max: Pixels,
}

impl Geometry {
    /// None while the content fits or the scroller is too short for a thumb. `before`
    /// and `after` are what covers the two ends of the scroller.
    fn new(
        axis: Axis,
        scroller: Bounds<Pixels>,
        max: Pixels,
        before: Pixels,
        after: Pixels,
    ) -> Option<Self> {
        let viewport = scroller.size.along(axis);
        let track = viewport - before - after - END_GAP * 2.;
        if max <= px(1.) || track <= MIN_TRACK {
            return None;
        }
        let thumb = (track * (viewport / (viewport + max))).max(MIN_THUMB);
        Some(Self {
            axis,
            start: scroller.origin.along(axis) + before + END_GAP,
            travel: track - thumb,
            thumb,
            edge: scroller.bottom_right().along(axis.invert()),
            max,
        })
    }

    /// The thumb's leading edge with the content scrolled `scrolled` from its start.
    fn lead(&self, scrolled: Pixels) -> Pixels {
        self.start + self.travel * (scrolled / self.max).clamp(0., 1.)
    }

    /// A box `thickness` deep against the scroller's edge, less `gap`, over the thumb's
    /// own stretch of the track.
    fn along_edge(&self, scrolled: Pixels, thickness: Pixels, gap: Pixels) -> Bounds<Pixels> {
        let (lead, across) = (self.lead(scrolled), self.edge - gap - thickness);
        match self.axis {
            Axis::Vertical => Bounds::new(point(across, lead), size(thickness, self.thumb)),
            Axis::Horizontal => Bounds::new(point(lead, across), size(self.thumb, thickness)),
        }
    }

    fn thumb(&self, scrolled: Pixels) -> Bounds<Pixels> {
        self.along_edge(scrolled, THUMB_WIDTH, EDGE_GAP)
    }

    /// The part of the scroller that takes the pointer for the thumb.
    fn grip(&self, scrolled: Pixels) -> Bounds<Pixels> {
        self.along_edge(scrolled, GRIP_WIDTH, px(0.))
    }

    /// How far the content is scrolled with the thumb's leading edge at `lead`.
    fn scrolled_at(&self, lead: Pixels) -> Pixels {
        self.max * ((lead - self.start) / self.travel).clamp(0., 1.)
    }
}

/// What the pointer is doing with the thumb. It outlives the frame.
#[derive(Clone, Copy, Default)]
struct Pointer {
    /// How far past the thumb's leading edge a drag took hold of it.
    grab: Option<Pixels>,
    hovered: bool,
}

impl Pointer {
    fn holds(&self) -> bool {
        self.grab.is_some() || self.hovered
    }
}

/// What a scrollbar keeps from one frame to the next.
#[derive(Default)]
struct State {
    pointer: Cell<Pointer>,
    /// How far the content was scrolled when the last frame drew it.
    scrolled: Cell<Option<Pixels>>,
    /// Whether the last frame had a thumb to draw.
    overflowing: Cell<bool>,
    /// The content scrolled a moment ago, or the pointer has just let go of the thumb.
    lingering: Cell<bool>,
    /// The timer that ends the lingering. Replacing it cancels the one before.
    rest: RefCell<Option<Task<()>>>,
    /// Whether the last frame showed the thumb, and since when it has been fading.
    shown: Cell<bool>,
    fading: Cell<Option<Instant>>,
}

impl State {
    /// The state of the scrollbar with this id, made on its first frame.
    fn of(id: Option<&GlobalElementId>, window: &mut Window) -> Rc<Self> {
        window.with_element_state(
            id.expect("a scrollbar has an id"),
            |state: Option<Rc<Self>>, _| {
                let state = state.unwrap_or_default();
                (state.clone(), state)
            },
        )
    }

    /// Keep the thumb for a while from now.
    fn linger(self: &Rc<Self>, view: EntityId, cx: &App) {
        self.lingering.set(true);
        let state = Rc::downgrade(self);
        *self.rest.borrow_mut() = Some(cx.spawn(async move |cx| {
            cx.background_executor().timer(LINGER).await;
            if let Some(state) = state.upgrade() {
                state.lingering.set(false);
                cx.update(|cx| cx.notify(view));
            }
        }));
    }

    /// How opaque the thumb is this frame. Reduced motion keeps the two end states and
    /// drops the fade between them.
    fn opacity(&self, shown: bool, window: &Window, cx: &App) -> f32 {
        if shown {
            self.shown.set(true);
            self.fading.set(None);
            return 1.;
        }
        if self.shown.replace(false) && !cx.reduce_motion() {
            self.fading.set(Some(Instant::now()));
        }
        let Some(since) = self.fading.get() else {
            return 0.;
        };
        let left = 1. - since.elapsed().as_secs_f32() / FADE.as_secs_f32();
        if left <= 0. {
            self.fading.set(None);
            return 0.;
        }
        window.request_animation_frame();
        left
    }
}

/// The one place the pointer's state changes, so every change repaints the thumb and
/// reports a hold that began or ended.
#[derive(Clone)]
struct Grip {
    state: Rc<State>,
    on_hold: Option<HoldListener>,
    view: EntityId,
}

impl Grip {
    fn set(&self, next: Pointer, window: &mut Window, cx: &mut App) {
        let before = self.state.pointer.replace(next);
        if before.holds() != next.holds() {
            if !next.holds() {
                self.state.linger(self.view, cx);
            }
            if let Some(on_hold) = &self.on_hold {
                on_hold(&next.holds(), window, cx);
            }
        }
        cx.notify(self.view);
    }
}

/// A thumb on screen this frame: where it travels, what takes the pointer for it, and
/// how opaque it is.
pub struct Shown {
    geometry: Geometry,
    hitbox: Hitbox,
    opacity: f32,
}

impl IntoElement for Scrollbar {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for Scrollbar {
    type RequestLayoutState = ();
    type PrepaintState = Option<Shown>;
    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }
    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        // Out of the flow and without a size: the thumb is placed from the scroller's
        // bounds, not from this element's.
        let style = Style {
            position: Position::Absolute,
            ..Style::default()
        };
        (window.request_layout(style, [], cx), ())
    }
    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Shown> {
        let state = State::of(id, window);
        let scrolled = &self.scrolled;
        let geometry = Geometry::new(
            scrolled.axis(),
            scrolled.viewport(),
            scrolled.max(),
            self.top,
            self.bottom,
        );
        // The scroller was laid out before it knew whether it would overflow. Another
        // frame lets it make room for the thumb, or take that room back.
        if state.overflowing.replace(geometry.is_some()) != geometry.is_some() {
            window.request_animation_frame();
        }
        let now = scrolled.scrolled();
        if state
            .scrolled
            .replace(Some(now))
            .is_some_and(|before| before != now)
        {
            state.linger(window.current_view(), cx);
        }
        let geometry = geometry?;
        let auto_hide = self
            .auto_hide
            .unwrap_or_else(|| cx.should_auto_hide_scrollbars());
        let shown = !auto_hide || state.lingering.get() || state.pointer.get().holds();
        let opacity = state.opacity(shown, window, cx);
        // A thumb that is not there takes no pointer input.
        if opacity <= 0. {
            return None;
        }
        // The content behind keeps the wheel, so the note still scrolls with the pointer
        // on its thumb.
        let hitbox =
            window.insert_hitbox(geometry.grip(now), HitboxBehavior::BlockMouseExceptScroll);
        Some(Shown {
            geometry,
            hitbox,
            opacity,
        })
    }
    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        shown: &mut Option<Shown>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let state = State::of(id, window);
        let Some(Shown {
            geometry,
            hitbox,
            opacity,
        }) = shown.take()
        else {
            // The thumb went away under the pointer, and no event will report the
            // hold ending.
            if state.pointer.take().holds()
                && let Some(on_hold) = self.on_hold.clone()
            {
                window.defer(cx, move |window, cx| on_hold(&false, window, cx));
            }
            return;
        };
        let now = state.pointer.get();
        if now.grab.is_some() {
            // A hitbox is a new one every frame, so the capture is renewed with it.
            window.capture_pointer(hitbox.id);
        }
        let emphasis = if now.holds() { HELD_EMPHASIS } else { 1. };
        let color = Hsla {
            a: (self.color.a * opacity * emphasis).min(1.),
            ..self.color
        };
        window.paint_quad(
            fill(geometry.thumb(self.scrolled.scrolled()), color).corner_radii(THUMB_WIDTH / 2.),
        );
        window.set_cursor_style(CursorStyle::Arrow, &hitbox);

        let grip = Grip {
            state,
            on_hold: self.on_hold.clone(),
            view: window.current_view(),
        };
        window.on_mouse_event({
            let (grip, hitbox, scrolled) = (grip.clone(), hitbox.clone(), self.scrolled.clone());
            move |event: &MouseDownEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble
                    || event.button != MouseButton::Left
                    || !hitbox.is_hovered(window)
                {
                    return;
                }
                let lead = geometry.lead(scrolled.scrolled());
                grip.set(
                    Pointer {
                        grab: Some(event.position.along(geometry.axis) - lead),
                        hovered: true,
                    },
                    window,
                    cx,
                );
                window.capture_pointer(hitbox.id);
                // The press belongs to the thumb: what lies under it neither takes the
                // focus nor moves a caret.
                cx.stop_propagation();
            }
        });
        window.on_mouse_event({
            let (grip, hitbox, scrolled) = (grip.clone(), hitbox.clone(), self.scrolled.clone());
            move |event: &MouseMoveEvent, phase, window, cx| {
                let now = grip.state.pointer.get();
                match (now.grab, phase) {
                    (Some(grab), DispatchPhase::Capture) => {
                        if event.dragging() {
                            let lead = event.position.along(geometry.axis) - grab;
                            scrolled.scroll_to(geometry.scrolled_at(lead), window, cx);
                            cx.notify(grip.view);
                        } else {
                            // The button came up where no event reported it.
                            window.release_pointer();
                            let hovered = hitbox.is_hovered(window);
                            grip.set(
                                Pointer {
                                    grab: None,
                                    hovered,
                                },
                                window,
                                cx,
                            );
                        }
                        cx.stop_propagation();
                    }
                    (None, DispatchPhase::Bubble) => {
                        let hovered = hitbox.is_hovered(window);
                        if hovered != now.hovered {
                            grip.set(
                                Pointer {
                                    grab: None,
                                    hovered,
                                },
                                window,
                                cx,
                            );
                        }
                    }
                    _ => {}
                }
            }
        });
        window.on_mouse_event({
            let (grip, hitbox) = (grip.clone(), hitbox.clone());
            move |event: &MouseUpEvent, phase, window, cx| {
                if phase != DispatchPhase::Capture
                    || event.button != MouseButton::Left
                    || grip.state.pointer.get().grab.is_none()
                {
                    return;
                }
                // Released first, or the captured thumb would count as hovered wherever
                // the pointer is.
                window.release_pointer();
                let hovered = hitbox.is_hovered(window);
                grip.set(
                    Pointer {
                        grab: None,
                        hovered,
                    },
                    window,
                    cx,
                );
                cx.stop_propagation();
            }
        });
        // The pointer can leave the window without a last move to say it left the thumb.
        window.on_mouse_event(move |_: &MouseExitEvent, phase, window, cx| {
            let now = grip.state.pointer.get();
            if phase == DispatchPhase::Bubble && now.hovered && now.grab.is_none() {
                grip.set(Pointer::default(), window, cx);
            }
        });
    }
}

#[cfg(test)]
mod tests;
