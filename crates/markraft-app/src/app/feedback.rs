//! What the window has to tell the user, and whose turn it is to say it.
//!
//! Four carriers, each for a different kind of fact, and one rule between them:
//! the same fact is said by one of them only. A state that outlives its mention
//! — a note that could not be saved, a shortcut the system refused — stays put
//! as an error until it is over. Something that happened and is done with — a
//! note restored, a link that led nowhere — is a notice, which is transient and
//! goes on its own.
//!
//! Notices take turns. A sentence the user has to *read* waits for whatever is
//! on screen instead of replacing it, and the same sentence twice in a row is
//! one sentence: both are rules the callers used to keep by hand, and both are
//! easy to lose in a `VecDeque` anyone can push to.

use gpui::SharedString;
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

/// A queued notice is a sentence, not an acknowledgment, so it is given time to read.
const READING_NOTICE: Duration = Duration::from_secs(8);
/// An acknowledgment of what the user just did, which they are already looking at.
const ACKNOWLEDGMENT: Duration = Duration::from_secs(3);
/// How long the file status indicator stays lit after a keystroke the file refused.
/// Long enough to be seen without following the typing that provoked it.
const FILE_STATUS_FLASH: Duration = Duration::from_millis(900);

/// A transient message over the note.
#[derive(Clone)]
pub(super) struct Notice {
    pub(super) text: SharedString,
    until: Instant,
}

#[derive(Default)]
pub(super) struct Feedback {
    error: Option<String>,
    platform_error: Option<String>,
    notice: Option<Notice>,
    queued: VecDeque<String>,
    flash_until: Option<Instant>,
}

impl Feedback {
    /// Why the last thing the user asked for did not happen. It stands until
    /// something replaces it or clears it, because the state it describes does.
    pub(super) fn error(&self) -> Option<&String> {
        self.error.as_ref()
    }

    pub(super) fn set_error(&mut self, error: String) {
        self.error = Some(error);
    }

    pub(super) fn clear_error(&mut self) {
        self.error = None;
    }

    /// What the system refused the app itself — a shortcut another app holds,
    /// an opening-at-login that did not take. Its own carrier because it is
    /// about the app rather than about the note.
    pub(super) fn platform_error(&self) -> Option<&String> {
        self.platform_error.as_ref()
    }

    pub(super) fn set_platform_error(&mut self, error: Option<String>) {
        self.platform_error = error;
    }

    pub(super) fn notice(&self) -> Option<&Notice> {
        self.notice.as_ref()
    }

    /// An acknowledgment of what the user just did, which replaces whatever is
    /// on screen: they are looking at the thing they just did.
    pub(super) fn inform(&mut self, text: impl AsRef<str>) {
        self.notice = Some(Notice {
            text: text.as_ref().to_owned().into(),
            until: Instant::now() + ACKNOWLEDGMENT,
        });
    }

    /// A sentence the user has to read, rather than an acknowledgment of what
    /// they just did. It waits for the notice on screen instead of replacing it,
    /// and the same sentence queued twice is said once.
    pub(super) fn queue(&mut self, text: String) {
        if !self.queued.contains(&text) {
            self.queued.push_back(text);
        }
    }

    /// Notices no longer carry actions; Escape keeps calling this for the cascade.
    pub(super) fn dismiss_action(&mut self) -> bool {
        false
    }

    /// Light the file indicator: a keystroke the file refused.
    pub(super) fn flash_file_status(&mut self) {
        self.flash_until = Some(Instant::now() + FILE_STATUS_FLASH);
    }

    pub(super) fn file_status_flashing(&self) -> bool {
        self.flash_until.is_some()
    }

    /// Expire what has had its turn and give the next queued sentence its own.
    /// Answers whether anything changed, which is what the caller redraws for.
    pub(super) fn tick(&mut self) -> bool {
        let now = Instant::now();
        let mut changed = false;
        if self
            .notice
            .as_ref()
            .is_some_and(|notice| now > notice.until)
        {
            self.notice = None;
            changed = true;
        }
        if self.flash_until.is_some_and(|until| now >= until) {
            self.flash_until = None;
            changed = true;
        }
        // One queued sentence at a time, once whatever was on screen has had its turn.
        if self.notice.is_none()
            && let Some(text) = self.queued.pop_front()
        {
            self.notice = Some(Notice {
                text: text.into(),
                until: now + READING_NOTICE,
            });
            changed = true;
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Queued sentences take turns: one is on screen, the rest wait. Replacing
    /// the one being read would lose it, since nothing brings it back.
    #[test]
    fn queued_sentences_take_turns() {
        let mut feedback = Feedback::default();
        feedback.queue("first".into());
        feedback.queue("second".into());
        assert!(feedback.notice().is_none(), "nothing until the tick");

        assert!(feedback.tick());
        assert_eq!(
            feedback.notice().map(|n| n.text.to_string()).as_deref(),
            Some("first")
        );
        // The second waits for the first to have had its time.
        assert!(!feedback.tick());
        assert_eq!(
            feedback.notice().map(|n| n.text.to_string()).as_deref(),
            Some("first")
        );

        feedback.notice.as_mut().unwrap().until = Instant::now() - Duration::from_secs(1);
        assert!(feedback.tick());
        assert_eq!(
            feedback.notice().map(|n| n.text.to_string()).as_deref(),
            Some("second")
        );
    }

    /// The same sentence queued twice is said once: a folder that reports the
    /// same thing about ten files should not make the user read it ten times.
    #[test]
    fn the_same_sentence_is_queued_once() {
        let mut feedback = Feedback::default();
        feedback.queue("same".into());
        feedback.queue("same".into());
        assert!(feedback.tick());
        feedback.notice.as_mut().unwrap().until = Instant::now() - Duration::from_secs(1);
        assert!(feedback.tick(), "the notice expires");
        assert!(feedback.notice().is_none(), "and nothing follows it");
    }

    /// An acknowledgment is about what the user just did, so it replaces what is
    /// on screen rather than queueing behind it.
    #[test]
    fn an_acknowledgment_replaces_what_is_on_screen() {
        let mut feedback = Feedback::default();
        feedback.inform("done");
        feedback.inform("done again");
        assert_eq!(
            feedback.notice().map(|n| n.text.to_string()).as_deref(),
            Some("done again")
        );
    }

    /// Notices no longer carry actions, so Escape does not dismiss them.
    #[test]
    fn notices_are_left_to_expire() {
        let mut feedback = Feedback::default();
        feedback.inform("just so you know");
        assert!(!feedback.dismiss_action());
        assert!(feedback.notice().is_some(), "left to expire");
    }

    /// The indicator is attention, not a message: it goes out by itself.
    #[test]
    fn the_file_indicator_goes_out_on_its_own() {
        let mut feedback = Feedback::default();
        assert!(!feedback.file_status_flashing());
        feedback.flash_file_status();
        assert!(feedback.file_status_flashing());
        feedback.flash_until = Some(Instant::now() - Duration::from_millis(1));
        assert!(feedback.tick());
        assert!(!feedback.file_status_flashing());
    }
}
