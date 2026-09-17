use std::time::Duration;

pub(crate) const BLINK_INTERVAL: Duration = Duration::from_millis(500);

/// A composition or selected range pauses blinking. Restarting an input session
/// always exposes the caret before waiting for the first timer tick.
#[derive(Default)]
pub(crate) struct CaretBlink {
    pub(crate) visible: bool,
    enabled: bool,
}

impl CaretBlink {
    pub(crate) fn reset(&mut self, focused: bool, composing: bool, selection_empty: bool) -> bool {
        self.visible = true;
        self.enabled = focused && !composing && selection_empty;
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
        assert!(caret.reset(true, false, true));
        assert!(caret.visible);
        assert!(caret.tick());
        assert!(!caret.visible);
        assert!(caret.reset(true, false, true));
        assert!(caret.visible);
        assert!(caret.tick());
        assert!(!caret.visible);
        assert!(caret.tick());
        assert!(caret.visible);
    }

    #[test]
    fn composition_selection_and_inactive_sessions_pause_the_timer() {
        let mut caret = CaretBlink::default();
        for (focused, composing, empty) in [
            (true, true, true),
            (true, false, false),
            (false, false, true),
        ] {
            assert!(!caret.reset(focused, composing, empty));
            for _ in 0..3 {
                assert!(!caret.tick());
                assert!(caret.visible);
            }
        }
        assert!(caret.reset(true, false, true));
        assert!(caret.tick());
        assert!(!caret.visible);
    }
}
