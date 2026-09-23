//! Whether someone is at the window, which is what the chrome follows.
//!
//! The window is a note first: the title recedes and the corner buttons fade
//! while nothing is happening, and come back the moment someone is there. What
//! counts as being there is not one thing — the pointer inside the window, the
//! window active, a panel open, a keystroke a moment ago — and the parts that
//! answer it are read from several places a frame. They live here so that the
//! answer is given once, in one place, rather than assembled again at each
//! caller from fields anyone can set.
//!
//! The corner buttons are the exception, and deliberately so: they follow the
//! pointer alone ([`Presence::pointer_inside`]). Typing, focus and open panels
//! must not keep them on screen over a note someone is writing.

use std::time::{Duration, Instant};

/// How long a keystroke counts as someone being at the window.
const KEY_PRESENCE: Duration = Duration::from_millis(2500);

pub(super) struct Presence {
    pointer_inside: bool,
    window_active: bool,
    last_key_at: Option<Instant>,
    /// What the last frame drew, so that a change can be told from a redraw.
    chrome_shown: bool,
}

impl Presence {
    pub(super) fn new(pointer_inside: bool, window_active: bool) -> Presence {
        Presence {
            pointer_inside,
            window_active,
            last_key_at: None,
            chrome_shown: true,
        }
    }

    /// Whether the pointer is over the window. The corner buttons follow this
    /// alone.
    pub(super) fn pointer_inside(&self) -> bool {
        self.pointer_inside
    }

    pub(super) fn window_active(&self) -> bool {
        self.window_active
    }

    /// Whether anything says someone is at the window. `busy` is what the rest
    /// of the app knows and this does not: a panel or a menu is open.
    pub(super) fn at_window(&self, busy: bool) -> bool {
        self.pointer_inside
            || self.window_active
            || busy
            || self
                .last_key_at
                .is_some_and(|at| at.elapsed() < KEY_PRESENCE)
    }

    /// A keystroke counts as presence for a moment, so the chrome does not
    /// vanish from under someone who is writing with the pointer parked outside
    /// the window.
    pub(super) fn note_key_press(&mut self) {
        self.last_key_at = Some(Instant::now());
    }

    /// Answers whether this is news, which is what the caller redraws for.
    pub(super) fn set_pointer_inside(&mut self, inside: bool) -> bool {
        let changed = self.pointer_inside != inside;
        self.pointer_inside = inside;
        changed
    }

    pub(super) fn set_window_active(&mut self, active: bool) -> bool {
        let changed = self.window_active != active;
        self.window_active = active;
        changed
    }

    /// Record what the chrome is doing now, and say whether it changed. The
    /// keystroke timer expires on its own, so this is asked once a tick rather
    /// than only where its inputs change.
    pub(super) fn chrome_changed(&mut self, visible: bool) -> bool {
        let changed = self.chrome_shown != visible;
        self.chrome_shown = visible;
        changed
    }
}

/// The window's own size, and the one change to it the app asked for.
///
/// Following its content means resizing the window, and the resize comes back
/// as a change the user could just as well have made by dragging the edge —
/// which turns following off. The size the app asked for is remembered so that
/// exactly that one change is not read as the user's.
#[derive(Default)]
pub(super) struct WindowSize {
    last: gpui::Size<gpui::Pixels>,
    expected: Option<gpui::Size<gpui::Pixels>>,
}

impl WindowSize {
    pub(super) fn new(last: gpui::Size<gpui::Pixels>) -> WindowSize {
        WindowSize {
            last,
            expected: None,
        }
    }

    /// Whether a resize the app asked for has yet to come back.
    pub(super) fn waiting(&self) -> bool {
        self.expected.is_some()
    }

    /// The app is about to resize the window to `size`.
    pub(super) fn expect(&mut self, size: gpui::Size<gpui::Pixels>) {
        self.expected = Some(size);
    }

    /// The window is now `size`. `None` when nothing moved; `true` when the
    /// change is the one the app asked for, and `false` when it is the user's,
    /// which is what stops the window from following its content.
    pub(super) fn settled(&mut self, size: gpui::Size<gpui::Pixels>) -> Option<bool> {
        if size == self.last {
            return None;
        }
        let ours = self.expected.is_some_and(|expected| {
            // A window manager may land a pixel or two off what was asked for.
            (expected.width - size.width).abs() <= gpui::px(2.)
                && (expected.height - size.height).abs() <= gpui::px(2.)
        });
        self.expected = None;
        self.last = size;
        Some(ours)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{px, size};

    /// Someone writing with the pointer parked outside the window is still
    /// there, for a moment after each keystroke.
    #[test]
    fn a_keystroke_counts_as_presence_for_a_moment() {
        let mut presence = Presence::new(false, false);
        assert!(!presence.at_window(false));
        presence.note_key_press();
        assert!(presence.at_window(false));
        presence.last_key_at = Some(Instant::now() - KEY_PRESENCE);
        assert!(!presence.at_window(false), "the moment has passed");
    }

    /// Each of the other three says someone is there on its own.
    #[test]
    fn the_pointer_the_window_and_an_open_panel_each_say_someone_is_there() {
        assert!(Presence::new(true, false).at_window(false), "pointer");
        assert!(Presence::new(false, true).at_window(false), "active");
        assert!(
            Presence::new(false, false).at_window(true),
            "a panel is open"
        );
    }

    /// Only a change is news; a redraw that finds the same answer is not.
    #[test]
    fn only_a_change_is_worth_a_redraw() {
        let mut presence = Presence::new(false, false);
        assert!(presence.set_pointer_inside(true));
        assert!(!presence.set_pointer_inside(true));
        // The chrome starts out drawn, so turning it off is the first change.
        assert!(!presence.chrome_changed(true));
        assert!(presence.chrome_changed(false));
    }

    /// Following the content resizes the window, and that resize must not be
    /// read as the user dragging the edge — which is what turns following off.
    #[test]
    fn the_resize_the_app_asked_for_is_not_the_users() {
        let mut window = WindowSize::new(size(px(600.), px(400.)));
        assert_eq!(
            window.settled(size(px(600.), px(400.))),
            None,
            "nothing moved"
        );

        window.expect(size(px(600.), px(500.)));
        assert!(window.waiting());
        // A window manager may land a pixel or two off what was asked for.
        assert_eq!(window.settled(size(px(600.), px(501.))), Some(true));
        assert!(!window.waiting());

        // Anything else is the user's.
        assert_eq!(window.settled(size(px(600.), px(700.))), Some(false));

        // Including a resize that lands far from the one asked for.
        window.expect(size(px(600.), px(500.)));
        assert_eq!(window.settled(size(px(600.), px(640.))), Some(false));
        assert!(!window.waiting(), "the request is answered either way");
    }
}
