use std::time::Duration;

pub(crate) const BLINK_INTERVAL: Duration = Duration::from_millis(500);

/// A composition or selected range pauses blinking, and a caret that covers the
/// grapheme it rests on never blinks at all: a block or underline caret is a
/// modal editor's cursor, which is steady, and blinking one hides the character
/// under it. Restarting an input session always exposes the caret before waiting
/// for the first timer tick.
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
