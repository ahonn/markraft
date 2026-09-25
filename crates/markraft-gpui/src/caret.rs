use std::time::Duration;

use gpui::Pixels;

pub(crate) const BLINK_INTERVAL: Duration = Duration::from_millis(500);

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
/// waiting for the first timer tick.
#[derive(Default)]
pub(crate) struct CaretBlink {
    pub(crate) visible: bool,
    enabled: bool,
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
        self.enabled
    }

    pub(crate) fn tick(&mut self) -> bool {
        if self.enabled {
            self.visible = !self.visible;
        }
        self.enabled
    }
}

#[cfg(test)]
mod tests {
    use super::CaretBlink;

    #[test]
    fn input_reveals_a_hidden_caret_before_blinking_resumes() {
        let mut caret = CaretBlink::default();
        assert!(caret.reset(true, false, true, false));
        assert!(caret.visible);
        assert!(caret.tick());
        assert!(!caret.visible);
        assert!(caret.reset(true, false, true, false));
        assert!(caret.visible);
        assert!(caret.tick());
        assert!(!caret.visible);
        assert!(caret.tick());
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
                assert!(!caret.tick());
                assert!(caret.visible);
            }
        }
        assert!(caret.reset(true, false, true, false));
        assert!(caret.tick());
        assert!(!caret.visible);
    }
}
