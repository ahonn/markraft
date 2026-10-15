use std::time::Duration;

use gpui::Pixels;

mod native;

pub(crate) use native::NativeCaret;

pub(crate) const BLINK_INTERVAL: Duration = Duration::from_millis(500);
/// How long GPUI blinks a caret without input before it hands the caret to a
/// [`NativeCaret`]. Every blink GPUI paints is a frame, and the GPU driver keeps
/// ~110 MB of working pools resident until it has been idle for about ten
/// seconds.
pub(crate) const BLINK_TIMEOUT: Duration = Duration::from_secs(10);
const BLINK_TICKS: u32 = (BLINK_TIMEOUT.as_millis() / BLINK_INTERVAL.as_millis()) as u32;
const _: () = assert!(
    BLINK_TICKS.is_multiple_of(2),
    "the hand-off must fall on a tick that hides the caret"
);

/// The caret as the view shows it, beyond where the selection puts it: which
/// side of a wrapped row's break it stands on, the column a run of vertical
/// moves keeps, and whether the next paint has to bring it into view.
///
/// Every change goes through a move named for what happened, which is what
/// keeps the one rule the three values obey: an edit or a pointer move forgets
/// the preferred column and only a vertical move sets it — after the edit
/// funnel has run, since the funnel forgets it — and every selection change
/// asks for a reveal that stands until prepaint takes it.
#[derive(Default)]
pub(crate) struct CaretView {
    upstream: bool,
    preferred_x: Option<Pixels>,
    reveal: bool,
}

impl CaretView {
    pub(crate) fn upstream(&self) -> bool {
        self.upstream
    }

    pub(crate) fn preferred_x(&self) -> Option<Pixels> {
        self.preferred_x
    }

    pub(crate) fn reveal_pending(&self) -> bool {
        self.reveal
    }

    /// Take the pending reveal, if any: prepaint asks once per frame.
    pub(crate) fn take_reveal(&mut self) -> bool {
        std::mem::take(&mut self.reveal)
    }

    /// Something other than a move wants the caret shown — a style change.
    pub(crate) fn ask_reveal(&mut self) {
        self.reveal = true;
    }

    /// The document or the selection changed through the editor's own funnel:
    /// the caret is drawn downstream, the column is forgotten and the caret is
    /// brought into view.
    pub(crate) fn moved_by_edit(&mut self) {
        self.upstream = false;
        self.preferred_x = None;
        self.reveal = true;
    }

    /// An extension dispatched an edit: as [`Self::moved_by_edit`], except that
    /// the column stays, so a modal editor's own vertical moves keep it across
    /// the edits they dispatch between rows.
    pub(crate) fn edited_by_extension(&mut self) {
        self.upstream = false;
        self.reveal = true;
    }

    /// An extension set the selection: the caret lands on the side of the break
    /// the move chose, the column is forgotten and the caret is brought into view.
    pub(crate) fn moved_by_command(&mut self, upstream: bool) {
        self.upstream = upstream;
        self.preferred_x = None;
        self.reveal = true;
    }

    /// A move landed on `upstream`'s side of a wrapped row's break.
    pub(crate) fn landed(&mut self, upstream: bool) {
        self.upstream = upstream;
    }

    /// A vertical move reached its row: the column it left from is kept for
    /// the next one.
    pub(crate) fn moved_vertically(&mut self, x: Pixels) {
        self.preferred_x = Some(x);
    }

    /// Put back the column a move that was only correcting the caret within its
    /// row would otherwise have forgotten.
    pub(crate) fn keep_column(&mut self, column: Option<Pixels>) {
        self.preferred_x = column;
    }

    /// A horizontal move, a pointer press or an edit: the column no longer means
    /// anything.
    pub(crate) fn forget_column(&mut self) {
        self.preferred_x = None;
    }
}

/// A composition or selected range pauses blinking, and a caret that covers the
/// grapheme it rests on never blinks at all: a block or underline caret is a
/// modal editor's cursor, which is steady, and blinking one hides the character
/// under it. `steady` also carries the reduced-motion setting, which holds every
/// caret shape still. Restarting an input session always exposes the caret before
/// waiting for the first timer tick, and restarts the [`BLINK_TIMEOUT`] after
/// which GPUI hands an idle caret to a [`NativeCaret`].
#[derive(Default)]
pub(crate) struct CaretBlink {
    pub(crate) visible: bool,
    enabled: bool,
    ticks_left: u32,
    handoff: Handoff,
}

/// Where an idle caret is in its hand-off to a [`NativeCaret`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Handoff {
    /// GPUI paints the caret.
    #[default]
    None,
    /// The last tick hid the caret. The next paint hands it over where it
    /// stands, or keeps it steady when no native layer can take it.
    Pending,
    /// A native layer blinks the caret. Any later paint takes it back: the
    /// frame may have moved the text or drawn over it.
    Shown,
}

/// What the blink timer does after a tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tick {
    /// The caret toggled: repaint and wait for the next tick.
    Blink,
    /// The caret hid for the hand-off: repaint once and stop.
    HandOff,
    /// Blinking is paused: stop.
    Stop,
}

impl CaretBlink {
    pub(crate) fn reset(
        &mut self,
        focused: bool,
        composing: bool,
        selection_empty: bool,
        steady: bool,
    ) -> bool {
        self.visible = true;
        self.enabled = focused && !composing && selection_empty && !steady;
        self.ticks_left = BLINK_TICKS;
        self.handoff = Handoff::None;
        self.enabled
    }

    /// Toggle the caret. Once [`BLINK_TIMEOUT`] has passed, hide it one last
    /// time for the hand-off instead.
    pub(crate) fn tick(&mut self) -> Tick {
        if !self.enabled {
            return Tick::Stop;
        }
        self.visible = !self.visible;
        if self.ticks_left == 0 {
            self.enabled = false;
            self.handoff = Handoff::Pending;
            return Tick::HandOff;
        }
        self.ticks_left -= 1;
        Tick::Blink
    }

    pub(crate) fn handoff(&self) -> Handoff {
        self.handoff
    }

    /// A native layer took the caret.
    pub(crate) fn handed_over(&mut self) {
        debug_assert_eq!(self.handoff, Handoff::Pending);
        self.handoff = Handoff::Shown;
    }

    /// No native layer took the caret: GPUI shows it, steady, until the next reset.
    pub(crate) fn settle(&mut self) {
        self.visible = true;
        self.handoff = Handoff::None;
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::{BLINK_INTERVAL, BLINK_TIMEOUT, CaretBlink, Handoff, Tick};

    #[test]
    fn input_reveals_a_hidden_caret_before_blinking_resumes() {
        let mut caret = CaretBlink::default();
        assert!(caret.reset(true, false, true, false));
        assert!(caret.visible);
        assert_eq!(caret.tick(), Tick::Blink);
        assert!(!caret.visible);
        assert!(caret.reset(true, false, true, false));
        assert!(caret.visible);
        assert_eq!(caret.tick(), Tick::Blink);
        assert!(!caret.visible);
        assert_eq!(caret.tick(), Tick::Blink);
        assert!(caret.visible);
    }

    #[test]
    fn composition_selection_steady_shapes_and_inactive_sessions_pause_the_timer() {
        let mut caret = CaretBlink::default();
        for (focused, composing, empty, steady) in [
            (true, true, true, false),
            (true, false, false, false),
            (false, false, true, false),
            // A block or underline caret is steady, so it never blinks away
            // from the grapheme it covers.
            (true, false, true, true),
        ] {
            assert!(!caret.reset(focused, composing, empty, steady));
            for _ in 0..3 {
                assert_eq!(caret.tick(), Tick::Stop);
                assert!(caret.visible);
                assert_eq!(caret.handoff(), Handoff::None);
            }
        }
        assert!(caret.reset(true, false, true, false));
        assert_eq!(caret.tick(), Tick::Blink);
        assert!(!caret.visible);
    }

    /// Blink until the timeout, and return how many blinks there were.
    fn blink_to_the_handoff(caret: &mut CaretBlink) -> u128 {
        let mut blinks = 0;
        loop {
            match caret.tick() {
                Tick::Blink => blinks += 1,
                Tick::HandOff => return blinks,
                Tick::Stop => panic!("blinking stopped before the hand-off"),
            }
            assert!(blinks <= 100, "the hand-off never came");
        }
    }

    #[test]
    fn an_idle_caret_is_handed_off_while_it_is_hidden() {
        let mut caret = CaretBlink::default();
        assert!(caret.reset(true, false, true, false));
        assert_eq!(
            blink_to_the_handoff(&mut caret),
            BLINK_TIMEOUT.as_millis() / BLINK_INTERVAL.as_millis()
        );
        assert!(!caret.visible);
        assert_eq!(caret.handoff(), Handoff::Pending);
        caret.handed_over();
        assert_eq!(caret.handoff(), Handoff::Shown);
        assert_eq!(caret.tick(), Tick::Stop);
        assert!(!caret.visible);

        assert!(caret.reset(true, false, true, false));
        assert!(caret.visible);
        assert_eq!(caret.handoff(), Handoff::None);
        assert_eq!(caret.tick(), Tick::Blink);
    }

    #[test]
    fn a_caret_no_layer_takes_settles_visible_until_the_next_reset() {
        let mut caret = CaretBlink::default();
        assert!(caret.reset(true, false, true, false));
        blink_to_the_handoff(&mut caret);
        caret.settle();
        assert!(caret.visible);
        assert_eq!(caret.handoff(), Handoff::None);
        assert_eq!(caret.tick(), Tick::Stop);
        assert!(caret.visible);

        assert!(caret.reset(true, false, true, false));
        assert_eq!(caret.tick(), Tick::Blink);
        assert!(!caret.visible);
    }
}
