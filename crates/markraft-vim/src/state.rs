//! The mode and the operator-pending state machine. Nothing here touches a document or
//! a window, so the whole of "what does this key mean right now" is testable on its own.

use crate::{edit::Register, motion::MAX_COUNT};

/// The editing mode, reported to the host through `EditorEvent::Extension` whenever it
/// changes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Normal,
    Insert,
    /// Charwise visual selection.
    Visual,
    /// Whole-block visual selection.
    VisualLine,
}

impl Mode {
    /// The value the `vim_mode` key carries in the editor's key context.
    pub(crate) fn context(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Insert => "insert",
            Self::Visual => "visual",
            Self::VisualLine => "visual_line",
        }
    }

    /// The label a host shows for the mode.
    pub fn label(self) -> &'static str {
        match self {
            Self::Normal => "NORMAL",
            Self::Insert => "INSERT",
            Self::Visual => "VISUAL",
            Self::VisualLine => "V-LINE",
        }
    }

    pub(crate) fn is_visual(self) -> bool {
        matches!(self, Self::Visual | Self::VisualLine)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Operator {
    Delete,
    Change,
    Yank,
}

impl Operator {
    /// The value the `vim_operator` key carries while this operator waits for a motion.
    pub(crate) fn context(self) -> &'static str {
        match self {
            Self::Delete => "d",
            Self::Change => "c",
            Self::Yank => "y",
        }
    }
}

/// The half-typed command: a count, then optionally an operator and a second count.
/// `2d3w` means "delete three words, twice".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Pending {
    count: Option<usize>,
    operator: Option<Operator>,
    operator_count: Option<usize>,
}

impl Pending {
    pub(crate) fn is_empty(self) -> bool {
        self == Self::default()
    }

    pub(crate) fn operator(self) -> Option<Operator> {
        self.operator
    }

    /// The count typed since the operator, without consuming anything. `gg` and `G` read
    /// it as the block to go to, which is not a repetition.
    pub(crate) fn count(self) -> Option<usize> {
        self.count
    }

    /// Append a decimal digit. A leading `0` is not a count but the line-start motion,
    /// which the caller performs instead; every later `0` is a digit.
    pub(crate) fn digit(&mut self, digit: usize) -> bool {
        let Some(count) = self.count else {
            if digit == 0 {
                return false;
            }
            self.count = Some(digit);
            return true;
        };
        self.count = Some(
            count
                .saturating_mul(10)
                .saturating_add(digit)
                .min(MAX_COUNT),
        );
        true
    }

    /// Arm `operator`, taking any count typed before it. Arming the one already waiting
    /// reports `true`: that is how `dd`, `cc` and `yy` are spelled.
    pub(crate) fn arm(&mut self, operator: Operator) -> bool {
        if self.operator == Some(operator) {
            return true;
        }
        self.operator = Some(operator);
        self.operator_count = self.count.take();
        false
    }

    /// The count the command runs with, and the end of the command: the operator and
    /// both counts are consumed whether or not the caller uses them.
    pub(crate) fn take(&mut self) -> usize {
        let count = self
            .count
            .take()
            .unwrap_or(1)
            .saturating_mul(self.operator_count.take().unwrap_or(1))
            .clamp(1, MAX_COUNT);
        self.operator = None;
        count
    }

    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }
}

/// Everything the extension remembers between keystrokes. Shared behind a `RefCell`
/// between `update`, which observes the editor, and the action handlers, which do not
/// get `&mut self`.
#[derive(Default)]
pub(crate) struct State {
    pub mode: Mode,
    pub pending: Pending,
    /// The grapheme `v` or `V` started on; the other end of a visual selection.
    pub visual_anchor: usize,
    /// The unnamed register. The clipboard carries the same content; the register is
    /// what remembers that it was linewise.
    pub register: Option<Register>,
    /// Whether an input method is composing, mirrored from the editor so that the key
    /// context can keep Escape away from a live composition.
    pub composing: bool,
    /// The spelling of a style a change emptied, which Escape takes away when
    /// nothing was typed into it.
    pub emptied: Option<markraft_core::kind::conceal::EmptiedPair>,
    /// The mode the host has been told about.
    reported: Option<Mode>,
}

impl State {
    /// The mode to announce, once, when it is not the one the host already knows.
    pub(crate) fn report(&mut self) -> Option<Mode> {
        (self.reported != Some(self.mode)).then(|| {
            self.reported = Some(self.mode);
            self.mode
        })
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::{Operator, Pending};

    #[test]
    fn a_leading_zero_is_a_motion_and_a_later_one_is_a_digit() {
        let mut pending = Pending::default();
        assert!(!pending.digit(0));
        assert!(pending.is_empty());
        assert!(pending.digit(1));
        assert!(pending.digit(0));
        assert_eq!(pending.take(), 10);
        assert!(pending.is_empty());
    }

    #[test]
    fn an_operator_takes_the_count_before_it_and_multiplies_the_one_after() {
        let mut pending = Pending::default();
        assert!(pending.digit(2));
        assert!(!pending.arm(Operator::Delete));
        assert_eq!(pending.operator(), Some(Operator::Delete));
        assert!(pending.digit(3));
        assert_eq!(pending.take(), 6);
        assert!(pending.is_empty());
        assert_eq!(pending.operator(), None);
    }

    #[test]
    fn arming_the_waiting_operator_again_is_the_doubled_form() {
        let mut pending = Pending::default();
        assert!(!pending.arm(Operator::Yank));
        assert!(pending.arm(Operator::Yank));
        // A different operator replaces it rather than doubling.
        assert!(!pending.arm(Operator::Change));
        assert_eq!(pending.operator(), Some(Operator::Change));
        assert_eq!(pending.take(), 1);
    }

    #[test]
    fn a_bare_command_runs_once_and_an_absurd_count_is_bounded() {
        let mut pending = Pending::default();
        assert_eq!(pending.take(), 1);
        for _ in 0..30 {
            pending.digit(9);
        }
        assert_eq!(pending.take(), super::MAX_COUNT);
    }

    #[test]
    fn clearing_forgets_a_half_typed_command() {
        let mut pending = Pending::default();
        pending.digit(4);
        pending.arm(Operator::Delete);
        pending.digit(2);
        assert!(!pending.is_empty());
        pending.clear();
        assert!(pending.is_empty());
        assert_eq!(pending.take(), 1);
    }
}
