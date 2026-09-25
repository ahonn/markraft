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
//! one sentence. Both rules live here rather than with the callers, since both
//! are easy to lose in a `VecDeque` anyone can push to.
//!
//! Everything said here is also logged, once per change, so a screenshot of
//! the window can be matched to the log a report carries.

use gpui::SharedString;
use std::{
    collections::VecDeque,
    path::PathBuf,
    time::{Duration, Instant},
};

/// A queued notice is a sentence, not an acknowledgment, so it is given time to read.
const READING_NOTICE: Duration = Duration::from_secs(8);
/// An acknowledgment of what the user just did, which they are already looking at.
const ACKNOWLEDGMENT: Duration = Duration::from_secs(3);
/// A notice with a button, which takes a moment to reach.
const WITH_ACTION: Duration = Duration::from_secs(8);
/// How long the file status indicator stays lit after a keystroke the file refused.
/// Long enough to be seen without following the typing that provoked it.
const FILE_STATUS_FLASH: Duration = Duration::from_millis(900);

/// A clickable follow-up on a notice — reveal a path in Finder.
#[derive(Clone)]
pub(super) struct NoticeAction {
    pub(super) label: SharedString,
    pub(super) path: PathBuf,
}

/// A transient message over the note. One that carries an action shows it as a
/// button and stays longer, because reaching that button takes a moment.
#[derive(Clone)]
pub(super) struct Notice {
    pub(super) text: SharedString,
    until: Instant,
    pub(super) action: Option<NoticeAction>,
    #[cfg(test)]
    from_queue: bool,
}

impl Notice {
    pub(super) fn action(&self) -> Option<&NoticeAction> {
        self.action.as_ref()
    }
}

#[derive(Default)]
pub(super) struct Feedback {
    error: Option<String>,
    platform_error: Option<String>,
    notice: Option<Notice>,
    /// Sentences waiting their turn, each with the path its button reveals.
    queued: VecDeque<(String, Option<PathBuf>)>,
    flash_until: Option<Instant>,
}

impl Feedback {
    /// Why the last thing the user asked for did not happen. It stands until
    /// something replaces it or clears it, because the state it describes does.
    pub(super) fn error(&self) -> Option<&String> {
        self.error.as_ref()
    }

    /// Show `error` in the banner. Worded here, once, whatever reported it.
    pub(super) fn set_error(&mut self, error: impl ToString) {
        let error = error.to_string();
        if self.error.as_ref() != Some(&error) {
            log::error!("shown: {error}");
        }
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
        if let Some(error) = error
            .as_ref()
            .filter(|&error| self.platform_error.as_ref() != Some(error))
        {
            log::warn!("shown: {error}");
        }
        self.platform_error = error;
    }

    pub(super) fn notice(&self) -> Option<&Notice> {
        self.notice.as_ref()
    }

    /// An acknowledgment of what the user just did, which replaces whatever is
    /// on screen: they are looking at the thing they just did.
    pub(super) fn inform(&mut self, text: impl AsRef<str>) {
        log::debug!("shown: {}", text.as_ref());
        self.notice = Some(Notice {
            text: text.as_ref().to_owned().into(),
            until: Instant::now() + ACKNOWLEDGMENT,
            action: None,
            #[cfg(test)]
            from_queue: false,
        });
    }

    /// An acknowledgment with a path the user can reveal — stays long enough to click.
    pub(super) fn inform_with_reveal(&mut self, text: impl AsRef<str>, path: PathBuf) {
        log::info!("shown: {} ({})", text.as_ref(), path.display());
        self.notice = Some(Notice {
            text: text.as_ref().to_owned().into(),
            until: Instant::now() + WITH_ACTION,
            action: Some(NoticeAction {
                label: "Show in Finder".into(),
                path,
            }),
            #[cfg(test)]
            from_queue: false,
        });
    }

    /// A sentence the user has to read, rather than an acknowledgment of what
    /// they just did. It waits for the notice on screen instead of replacing it,
    /// and the same sentence queued twice is said once.
    pub(super) fn queue(&mut self, text: String) {
        self.enqueue(text, None);
    }

    /// [`Self::queue`], with a button that reveals `path` in Finder: what a
    /// sentence would otherwise have to spell out as a path.
    pub(super) fn queue_with_reveal(&mut self, text: String, path: PathBuf) {
        self.enqueue(text, Some(path));
    }

    /// [`Self::queue`], with a button when there is a `path` to reveal.
    pub(super) fn enqueue(&mut self, text: String, path: Option<PathBuf>) {
        if !self.queued.iter().any(|(queued, _)| *queued == text) {
            match &path {
                Some(path) => log::info!("shown: {text} ({})", path.display()),
                None => log::info!("shown: {text}"),
            }
            self.queued.push_back((text, path));
        }
    }

    /// Queued sentences still pending or already being displayed. Immediate
    /// acknowledgments are excluded, so polling cannot change what tests observe.
    #[cfg(test)]
    pub(super) fn queued(&self) -> impl Iterator<Item = &str> {
        self.notice
            .iter()
            .filter(|notice| notice.from_queue)
            .map(|notice| notice.text.as_ref())
            .chain(self.queued.iter().map(|(text, _)| text.as_str()))
    }

    /// Dismiss a notice that carries an action, and say whether there was one.
    /// A notice with nothing to press is left alone: it goes by itself.
    pub(super) fn dismiss_action(&mut self) -> bool {
        if self.notice.as_ref().is_some_and(|n| n.action.is_some()) {
            self.notice = None;
            return true;
        }
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
            && let Some((text, path)) = self.queued.pop_front()
        {
            self.notice = Some(Notice {
                text: text.into(),
                until: now
                    + if path.is_some() {
                        WITH_ACTION
                    } else {
                        READING_NOTICE
                    },
                action: path.map(|path| NoticeAction {
                    label: "Show in Finder".into(),
                    path,
                }),
                #[cfg(test)]
                from_queue: true,
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
    fn a_queued_sentence_can_carry_a_button() {
        let mut feedback = Feedback::default();
        feedback.queue_with_reveal("saved aside".into(), PathBuf::from("/kept.md"));
        assert!(feedback.tick());
        let notice = feedback.notice().expect("the notice");
        assert_eq!(notice.text.to_string(), "saved aside");
        assert_eq!(
            notice.action().map(|action| action.path.clone()),
            Some(PathBuf::from("/kept.md"))
        );
    }

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

    /// Escape dismisses a notice that offers a button, because that one waits
    /// for an answer. One that offers nothing is left to go on its own.
    #[test]
    fn only_a_notice_with_a_button_is_dismissed() {
        let mut feedback = Feedback::default();
        feedback.inform("just so you know");
        assert!(!feedback.dismiss_action());
        assert!(feedback.notice().is_some(), "left to expire");

        feedback.inform_with_reveal("Moved to Trash", PathBuf::from("/notes"));
        assert!(feedback.dismiss_action());
        assert!(feedback.notice().is_none());
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
