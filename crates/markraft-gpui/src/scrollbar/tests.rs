use super::{Geometry, Scrollbar};
use gpui::{
    Axis, Bounds, Context, IntoElement, MouseButton, Pixels, Point, Render, ScrollHandle,
    TestAppContext, VisualTestContext, Window, black, div, point, prelude::*, px, size,
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

/// A scroller 200 wide and 100 tall at the window's origin, over content 400 tall.
struct Harness {
    handle: ScrollHandle,
    /// Whether the thumb shows only around a scroll.
    auto_hide: bool,
    content: Pixels,
    /// Presses that reached the scroller under the thumb.
    presses: Rc<Cell<usize>>,
    holds: Rc<RefCell<Vec<bool>>>,
}

impl Harness {
    fn new(auto_hide: bool) -> Self {
        Self {
            handle: ScrollHandle::new(),
            auto_hide,
            content: px(400.),
            presses: Rc::default(),
            holds: Rc::default(),
        }
    }
}

impl Render for Harness {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let presses = self.presses.clone();
        let holds = self.holds.clone();
        div()
            .child(
                div()
                    .id("scroller")
                    .w(px(200.))
                    .h(px(100.))
                    .track_scroll(&self.handle)
                    .overflow_y_scroll()
                    .on_mouse_down(MouseButton::Left, move |_, _, _| {
                        presses.set(presses.get() + 1)
                    })
                    .child(div().h(self.content)),
            )
            .child(
                Scrollbar::new("scrollbar", &self.handle, black())
                    .auto_hide(self.auto_hide)
                    .on_hold(move |held, _, _| holds.borrow_mut().push(*held)),
            )
    }
}

fn draw(cx: &mut VisualTestContext) {
    cx.update(|window, cx| window.draw(cx).clear(cx));
}

fn press(cx: &mut VisualTestContext, at: Point<Pixels>) {
    cx.simulate_mouse_down(at, MouseButton::Left, Default::default());
    draw(cx);
}

fn drag(cx: &mut VisualTestContext, to: Point<Pixels>) {
    cx.simulate_mouse_move(to, MouseButton::Left, Default::default());
    draw(cx);
}

fn hover(cx: &mut VisualTestContext, at: Point<Pixels>) {
    cx.simulate_mouse_move(at, None, Default::default());
    draw(cx);
}

fn release(cx: &mut VisualTestContext, at: Point<Pixels>) {
    cx.simulate_mouse_up(at, MouseButton::Left, Default::default());
    draw(cx);
}

/// The thumb is 24 tall and travels 68, from 4 below the scroller's top edge.
const ON_THUMB: Point<Pixels> = point(px(194.), px(10.));

#[gpui::test]
fn dragging_the_thumb_scrolls_the_content(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_, _| Harness::new(false));
    draw(cx);
    let (handle, presses) =
        view.read_with(cx, |view, _| (view.handle.clone(), view.presses.clone()));
    assert_eq!(handle.max_offset().y, px(300.));

    press(cx, ON_THUMB);
    assert_eq!(presses.get(), 0, "the press belongs to the thumb");
    assert_eq!(handle.offset().y, px(0.), "a press alone scrolls nothing");

    // Half the thumb's travel is half the content.
    drag(cx, point(px(194.), px(44.)));
    assert_eq!(handle.offset().y, px(-150.));

    // The drag goes on with the pointer off the thumb, and stops at the ends.
    drag(cx, point(px(40.), px(500.)));
    assert_eq!(handle.offset().y, px(-300.));
    drag(cx, point(px(40.), px(-500.)));
    assert_eq!(handle.offset().y, px(0.));

    drag(cx, point(px(40.), px(44.)));
    release(cx, point(px(40.), px(44.)));
    hover(cx, point(px(40.), px(80.)));
    assert_eq!(handle.offset().y, px(-150.), "a released thumb stays put");
}

#[gpui::test]
fn the_thumb_follows_the_grab_point(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_, _| Harness::new(false));
    draw(cx);
    let handle = view.read_with(cx, |view, _| view.handle.clone());

    // Scrolled halfway, the thumb sits 34 lower. Taking hold of it there and letting go
    // leaves the content where it was.
    handle.set_offset(point(px(0.), px(-150.)));
    cx.update(|window, _| window.refresh());
    draw(cx);
    press(cx, point(px(194.), px(58.)));
    drag(cx, point(px(194.), px(58.)));
    assert_eq!(handle.offset().y, px(-150.));
    drag(cx, point(px(194.), px(41.)));
    assert_eq!(handle.offset().y, px(-75.));
}

/// Scroll the content as a wheel would, and draw the frame that follows.
fn scroll(cx: &mut VisualTestContext, handle: &ScrollHandle, to: Pixels) {
    handle.set_offset(point(px(0.), -to));
    cx.update(|window, _| window.refresh());
    draw(cx);
}

/// Let the time a resting thumb lingers go by.
fn rest(cx: &mut VisualTestContext) {
    cx.executor().advance_clock(Duration::from_millis(1300));
    draw(cx);
}

#[gpui::test]
fn a_thumb_that_hides_shows_only_around_a_scroll(cx: &mut TestAppContext) {
    // No fade, so each frame is one of the two end states.
    cx.update(|cx| cx.set_reduce_motion(true));
    let (view, cx) = cx.add_window_view(|_, _| Harness::new(true));
    draw(cx);
    let (handle, presses) =
        view.read_with(cx, |view, _| (view.handle.clone(), view.presses.clone()));

    // At rest there is no thumb: the press goes to the content and drags nothing.
    press(cx, ON_THUMB);
    drag(cx, point(px(194.), px(44.)));
    release(cx, point(px(194.), px(44.)));
    assert_eq!(presses.get(), 1);
    assert_eq!(handle.offset().y, px(0.));

    // A scroll brings the thumb, 34 lower for half the content, and the thumb takes
    // the press.
    scroll(cx, &handle, px(150.));
    press(cx, point(px(194.), px(58.)));
    release(cx, point(px(194.), px(58.)));
    assert_eq!(presses.get(), 1);

    // The pointer left and the content rested: the thumb is gone again.
    hover(cx, point(px(40.), px(58.)));
    rest(cx);
    press(cx, point(px(194.), px(58.)));
    release(cx, point(px(194.), px(58.)));
    assert_eq!(presses.get(), 2);
}

#[gpui::test]
fn a_held_thumb_stays_until_the_pointer_lets_go(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_reduce_motion(true));
    let (view, cx) = cx.add_window_view(|_, _| Harness::new(true));
    draw(cx);
    let (handle, presses) =
        view.read_with(cx, |view, _| (view.handle.clone(), view.presses.clone()));

    scroll(cx, &handle, px(150.));
    hover(cx, point(px(194.), px(58.)));
    rest(cx);
    press(cx, point(px(194.), px(58.)));
    assert_eq!(
        presses.get(),
        0,
        "the thumb is still there to take the press"
    );

    // It lingers after the drag as it does after a scroll, then goes.
    release(cx, point(px(40.), px(58.)));
    hover(cx, point(px(40.), px(58.)));
    rest(cx);
    press(cx, point(px(194.), px(58.)));
    assert_eq!(presses.get(), 1);
}

#[gpui::test]
fn a_hold_lasts_from_the_pointer_arriving_to_the_drag_ending(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_, _| Harness::new(false));
    draw(cx);
    let holds = view.read_with(cx, |view, _| view.holds.clone());

    hover(cx, point(px(40.), px(10.)));
    assert!(holds.borrow().is_empty());
    hover(cx, ON_THUMB);
    assert_eq!(*holds.borrow(), [true]);

    // A drag keeps hold of the thumb wherever the pointer goes.
    press(cx, ON_THUMB);
    drag(cx, point(px(40.), px(44.)));
    assert_eq!(*holds.borrow(), [true]);
    release(cx, point(px(40.), px(44.)));
    assert_eq!(*holds.borrow(), [true, false]);

    // The drag left the thumb 34 lower.
    hover(cx, point(px(194.), px(50.)));
    hover(cx, point(px(40.), px(50.)));
    assert_eq!(*holds.borrow(), [true, false, true, false]);
}

#[gpui::test]
fn a_thumb_that_goes_away_ends_its_hold(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_, _| Harness::new(false));
    draw(cx);
    let holds = view.read_with(cx, |view, _| view.holds.clone());

    hover(cx, ON_THUMB);
    // The content shrinks to fit, which leaves nothing to scroll.
    view.update(cx, |view, cx| {
        view.content = px(80.);
        cx.notify();
    });
    draw(cx);
    cx.run_until_parked();
    assert_eq!(*holds.borrow(), [true, false]);
}

#[gpui::test]
fn a_scroller_keeps_a_lane_for_the_thumb_only_while_it_overflows(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_, _| Harness::new(false));
    draw(cx);
    let handle = view.read_with(cx, |view, _| view.handle.clone());
    assert_eq!(Scrollbar::lane(&handle), Scrollbar::LANE);

    view.update(cx, |view, cx| {
        view.content = px(80.);
        cx.notify();
    });
    draw(cx);
    assert_eq!(Scrollbar::lane(&handle), px(0.));
}

#[test]
fn content_that_fits_and_a_short_scroller_have_no_thumb() {
    let scroller = Bounds::new(point(px(0.), px(0.)), size(px(200.), px(100.)));
    let geometry = |max, before, after| Geometry::new(Axis::Vertical, scroller, max, before, after);
    assert_eq!(geometry(px(0.), px(0.), px(0.)), None);
    assert_eq!(geometry(px(300.), px(40.), px(30.)), None);
    assert!(geometry(px(300.), px(0.), px(0.)).is_some());
}

#[test]
fn the_thumb_stays_clear_of_the_insets() {
    let scroller = Bounds::new(point(px(10.), px(20.)), size(px(200.), px(300.)));
    let geometry = Geometry::new(Axis::Vertical, scroller, px(900.), px(40.), px(30.)).unwrap();
    let first = geometry.thumb(px(0.));
    let last = geometry.thumb(px(900.));
    assert_eq!(first.top(), px(64.));
    assert_eq!(last.bottom(), px(286.));
    assert_eq!(first.right(), px(207.));
    // Dragging the thumb to where a place in the content drew it gives that place back.
    assert_eq!(geometry.scrolled_at(first.top()), px(0.));
    assert_eq!(geometry.scrolled_at(last.top()), px(900.));
}

#[test]
fn a_sideways_thumb_runs_along_the_bottom_edge() {
    let strip = Bounds::new(point(px(10.), px(20.)), size(px(300.), px(80.)));
    let geometry = Geometry::new(Axis::Horizontal, strip, px(300.), px(0.), px(0.)).unwrap();
    let first = geometry.thumb(px(0.));
    let last = geometry.thumb(px(300.));
    assert_eq!(first.left(), px(14.));
    assert_eq!(last.right(), px(306.));
    assert_eq!(first.bottom(), px(97.));
    assert_eq!(first.size.height, px(6.));
    // Half of the strip's width is on screen, so the thumb is half its track.
    assert_eq!(first.size.width, px(146.));
    assert_eq!(geometry.grip(px(0.)).top(), px(86.));
    assert_eq!(geometry.scrolled_at(last.left()), px(300.));
}
