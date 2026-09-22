use std::{
    collections::HashSet,
    time::{Duration, Instant},
};

const SAVE_DELAY: Duration = Duration::from_millis(350);
pub(super) const NAME_SETTLES: Duration = Duration::from_millis(2000);

/// Tracks whether the current workspace revision has reached durable storage.
/// Durability includes recovery storage; it does not imply that every note has a
/// Markdown file or that its external-file conflict has been resolved.
#[derive(Default)]
pub(super) struct SaveState {
    revision: u64,
    dirty: bool,
    deadline: Option<Instant>,
    last_receipt: Option<u64>,
    receipt_floor: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum SaveCompletion {
    Ignored,
    MetadataOnly,
    Current,
}

impl SaveState {
    #[cfg(test)]
    pub(super) fn revision(&self) -> u64 {
        self.revision
    }

    pub(super) fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub(super) fn schedule(&mut self, now: Instant) {
        self.revision += 1;
        self.dirty = true;
        self.deadline = Some(now + SAVE_DELAY);
    }

    /// Explicit saves and reloads supersede acknowledgments for earlier snapshots.
    pub(super) fn barrier(&mut self) -> u64 {
        self.revision += 1;
        self.deadline = None;
        self.revision
    }

    pub(super) fn take_due(&mut self, now: Instant) -> Option<u64> {
        if self.deadline.is_some_and(|deadline| now >= deadline) {
            self.deadline = None;
            Some(self.revision)
        } else {
            None
        }
    }

    /// A stale snapshot can still report newly assigned paths, but must never
    /// overwrite metadata from a newer receipt or clear newer unsaved edits.
    pub(super) fn apply_completion(&mut self, revision: u64, success: bool) -> SaveCompletion {
        if revision < self.receipt_floor
            || revision > self.revision
            || self.last_receipt.is_some_and(|last| revision <= last)
        {
            return SaveCompletion::Ignored;
        }
        self.last_receipt = Some(revision);
        if revision < self.revision {
            return SaveCompletion::MetadataOnly;
        }
        self.dirty = !success;
        SaveCompletion::Current
    }

    pub(super) fn reset(&mut self) {
        self.receipt_floor = self.barrier();
        self.dirty = false;
        self.last_receipt = None;
    }
}

/// Which notes have already been asked about their conflict, and whether the
/// question is on screen right now.
///
/// Two rules the callers used to keep by hand. A note is asked once: answering
/// "Keep Mine" leaves the conflict standing, and asking again on every save
/// would make the note unusable. And only one question at a time — the dialog is
/// modal, so a second one would stack behind the first and be answered blind.
/// Asking again on purpose (⌘S, the footer indicator, the command) is
/// [`Conflicts::ask_again`], which is the one way to forget an answer.
#[derive(Default)]
pub(super) struct Conflicts {
    asked: HashSet<String>,
    on_screen: bool,
}

impl Conflicts {
    /// Whether a question for `id` would go up, asking nothing and changing
    /// nothing. The poll runs twenty times a second and the note it asks about
    /// is usually one that has been answered, so the caller checks this before
    /// it builds the strings [`Conflicts::ask`] would need.
    pub(super) fn would_ask(&self, id: &str) -> bool {
        !self.on_screen && !self.asked.contains(id)
    }

    /// Whether to put the question up for `id`, which from here on counts as
    /// asked. `false` when it has been asked already or another question is up.
    pub(super) fn ask(&mut self, id: &str) -> bool {
        if self.on_screen || self.asked.contains(id) {
            return false;
        }
        self.asked.insert(id.to_owned());
        self.on_screen = true;
        true
    }

    /// The question has been answered and the screen is free.
    pub(super) fn answered(&mut self) {
        self.on_screen = false;
    }

    /// Forget that `id` was asked, so the next [`Conflicts::ask`] puts the
    /// question up again.
    pub(super) fn ask_again(&mut self, id: &str) {
        self.asked.remove(id);
    }

    /// Forget every answer: the notes these were about are gone.
    pub(super) fn reset(&mut self) {
        self.asked.clear();
    }
}

struct HeldDraft {
    id: String,
    deadline: Instant,
}

/// Holds a draft in recovery while its first filename is still being composed.
/// Once released, an unfiled draft is not held again until the workspace resets;
/// emptying a held draft is the exception, since there is no settled name yet.
#[derive(Default)]
pub(super) struct DraftNaming {
    held: Option<HeldDraft>,
    released: HashSet<String>,
}

impl DraftNaming {
    pub(super) fn held_id(&self) -> Option<&str> {
        self.held.as_ref().map(|held| held.id.as_str())
    }

    /// Observe selection or composition changes. A released hold needs another
    /// save even when the transition itself did not edit the document.
    pub(super) fn observe(
        &mut self,
        active_id: &str,
        eligible: bool,
        composing: bool,
        now: Instant,
        nonempty: impl Fn(&str) -> bool,
    ) -> bool {
        let naming = eligible && !self.released.contains(active_id);
        let released = if self
            .held
            .as_ref()
            .is_some_and(|held| held.id != active_id || !naming)
        {
            self.release(nonempty)
        } else {
            false
        };
        if naming && self.held.is_none() {
            self.held = Some(HeldDraft {
                id: active_id.to_owned(),
                deadline: now + NAME_SETTLES,
            });
        }
        // Candidate text is not a committed document edit, but its title must not
        // settle while an input method still owns the composition.
        if composing && let Some(held) = &mut self.held {
            held.deadline = now + NAME_SETTLES;
        }
        released
    }

    pub(super) fn committed_edit(&mut self, id: &str, now: Instant) {
        if let Some(held) = &mut self.held
            && held.id == id
        {
            held.deadline = now + NAME_SETTLES;
        }
    }

    pub(super) fn due(&self, now: Instant) -> bool {
        self.held.as_ref().is_some_and(|held| now >= held.deadline)
    }

    /// Used for settling, switching sessions, losing focus, and explicit saves.
    pub(super) fn release(&mut self, nonempty: impl Fn(&str) -> bool) -> bool {
        let Some(held) = self.held.take() else {
            return false;
        };
        if nonempty(&held.id) {
            self.released.insert(held.id);
        }
        true
    }

    /// Forget identities once they are filed or deleted. Removing an ineligible
    /// hold requires no additional save because there is no filename left to settle.
    pub(super) fn retain(&mut self, keep: impl Fn(&str) -> bool) {
        self.released.retain(|id| keep(id));
        if self.held.as_ref().is_some_and(|held| !keep(&held.id)) {
            self.held = None;
        }
    }

    pub(super) fn reset(&mut self) {
        self.held = None;
        self.released.clear();
    }
}

#[derive(Default)]
pub(super) enum QuitState {
    #[default]
    Idle,
    Prompting,
}

impl QuitState {
    /// Only the request that opens the prompt may continue the quit operation.
    pub(super) fn begin(&mut self) -> bool {
        if matches!(self, Self::Prompting) {
            return false;
        }
        *self = Self::Prompting;
        true
    }

    pub(super) fn cancel(&mut self) {
        *self = Self::Idle;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A note is asked about its conflict once. "Keep Mine" leaves the conflict
    /// standing, and a question that came back on every save would make the note
    /// unusable; only asking on purpose forgets the answer.
    #[test]
    fn a_conflict_is_asked_about_once_until_someone_asks_again() {
        let mut conflicts = Conflicts::default();
        assert!(conflicts.ask("a"));
        conflicts.answered();
        assert!(!conflicts.ask("a"), "answered once already");
        conflicts.ask_again("a");
        assert!(conflicts.ask("a"));
        conflicts.answered();
        // Another note is another question.
        assert!(conflicts.ask("b"));
    }

    /// The dialog is modal, so a second question would stack behind the first
    /// and be answered blind.
    #[test]
    fn only_one_conflict_question_is_on_screen_at_a_time() {
        let mut conflicts = Conflicts::default();
        assert!(conflicts.ask("a"));
        assert!(!conflicts.ask("b"), "one is already up");
        conflicts.answered();
        assert!(conflicts.ask("b"));
    }

    /// A new workspace is new notes: the answers were about the old ones.
    #[test]
    fn reopening_the_folder_forgets_every_answer() {
        let mut conflicts = Conflicts::default();
        assert!(conflicts.ask("a"));
        conflicts.answered();
        conflicts.reset();
        assert!(conflicts.ask("a"));
    }

    #[test]
    fn edits_debounce_autosave_without_clearing_dirty_on_dispatch() {
        let now = Instant::now();
        let mut save = SaveState::default();
        save.schedule(now);
        save.schedule(now + Duration::from_millis(300));
        assert_eq!(save.take_due(now + SAVE_DELAY), None);
        assert_eq!(save.take_due(now + Duration::from_millis(650)), Some(2));
        assert!(save.is_dirty());
        assert_eq!(save.take_due(now + Duration::from_secs(1)), None);
    }

    #[test]
    fn stale_save_results_cannot_clear_or_reintroduce_dirty_state() {
        let now = Instant::now();
        let mut save = SaveState::default();
        save.schedule(now);
        let previous = save.revision();
        save.schedule(now);
        assert_eq!(
            save.apply_completion(previous, true),
            SaveCompletion::MetadataOnly
        );
        assert!(save.is_dirty());
        assert_eq!(
            save.apply_completion(save.revision(), true),
            SaveCompletion::Current
        );
        assert_eq!(
            save.apply_completion(previous, false),
            SaveCompletion::Ignored
        );
        assert!(!save.is_dirty());
    }

    #[test]
    fn explicit_save_supersedes_pending_autosave_and_keeps_failure_dirty() {
        let now = Instant::now();
        let mut save = SaveState::default();
        save.schedule(now);
        let previous = save.revision();
        let revision = save.barrier();
        assert_eq!(save.take_due(now + SAVE_DELAY), None);
        assert_eq!(
            save.apply_completion(previous, true),
            SaveCompletion::MetadataOnly
        );
        assert_eq!(
            save.apply_completion(revision, false),
            SaveCompletion::Current
        );
        assert!(save.is_dirty());
        let retry = save.barrier();
        assert_eq!(save.apply_completion(retry, true), SaveCompletion::Current);
        assert!(!save.is_dirty());
    }

    #[test]
    fn reloaded_workspace_ignores_prior_save_failures() {
        let now = Instant::now();
        let mut save = SaveState::default();
        save.schedule(now);
        let previous = save.revision();
        save.reset();
        assert_eq!(
            save.apply_completion(previous, false),
            SaveCompletion::Ignored
        );
        assert!(!save.is_dirty());
        assert_eq!(save.take_due(now + SAVE_DELAY), None);
    }

    #[test]
    fn queued_autosave_cannot_regress_metadata_after_a_synchronous_flush() {
        let now = Instant::now();
        let mut save = SaveState::default();
        save.schedule(now);
        let queued = save.take_due(now + SAVE_DELAY).unwrap();
        let flushed = save.barrier();
        assert_eq!(
            save.apply_completion(flushed, true),
            SaveCompletion::Current
        );
        assert_eq!(save.apply_completion(queued, true), SaveCompletion::Ignored);
        assert!(!save.is_dirty());
    }

    #[test]
    fn accepted_stale_metadata_advances_monotonically_while_edits_are_unsaved() {
        let now = Instant::now();
        let mut save = SaveState::default();
        save.schedule(now);
        let oldest = save.revision();
        save.schedule(now);
        let newer = save.revision();
        save.schedule(now);
        assert_eq!(
            save.apply_completion(newer, true),
            SaveCompletion::MetadataOnly
        );
        assert_eq!(save.apply_completion(oldest, true), SaveCompletion::Ignored);
        assert_eq!(save.apply_completion(newer, true), SaveCompletion::Ignored);
        assert!(save.is_dirty());
    }

    #[test]
    fn reset_rejects_previous_workspace_receipts_even_after_new_edits() {
        let now = Instant::now();
        let mut save = SaveState::default();
        save.schedule(now);
        let previous = save.revision();
        save.reset();
        save.schedule(now);
        assert_eq!(
            save.apply_completion(previous, true),
            SaveCompletion::Ignored
        );
        assert!(save.is_dirty());
        assert_eq!(
            save.apply_completion(save.revision(), true),
            SaveCompletion::Current
        );
    }

    #[test]
    fn only_edits_to_held_note_extend_its_naming_deadline() {
        let now = Instant::now();
        let mut naming = DraftNaming::default();
        naming.observe("draft", true, false, now, |_| true);
        naming.committed_edit("other", now + NAME_SETTLES);
        assert!(naming.due(now + NAME_SETTLES));
        naming.committed_edit("draft", now + Duration::from_secs(1));
        assert!(!naming.due(now + NAME_SETTLES));
        assert!(naming.due(now + Duration::from_secs(3)));
    }

    #[test]
    fn metadata_saves_do_not_delay_a_settled_title() {
        let now = Instant::now();
        let mut naming = DraftNaming::default();
        let mut save = SaveState::default();
        naming.observe("draft", true, false, now, |_| true);
        save.schedule(now + Duration::from_secs(1));
        assert!(naming.due(now + NAME_SETTLES));
    }

    #[test]
    fn composition_extends_naming_without_a_committed_edit() {
        let now = Instant::now();
        let mut naming = DraftNaming::default();
        naming.observe("draft", true, false, now, |_| true);
        naming.observe("draft", true, true, now + NAME_SETTLES, |_| true);
        assert!(!naming.due(now + NAME_SETTLES));
        assert!(naming.due(now + NAME_SETTLES * 2));
    }

    #[test]
    fn selection_observation_does_not_restart_the_settling_clock() {
        let now = Instant::now();
        let mut naming = DraftNaming::default();
        naming.observe("draft", true, false, now, |_| true);
        assert!(!naming.observe("draft", true, false, now + NAME_SETTLES, |_| true));
        assert!(naming.due(now + NAME_SETTLES));
    }

    #[test]
    fn switching_notes_releases_previous_name_and_holds_the_new_note() {
        let now = Instant::now();
        let mut naming = DraftNaming::default();
        naming.observe("first", true, false, now, |_| true);
        assert!(naming.observe("second", true, false, now, |_| true));
        assert_eq!(naming.held_id(), Some("second"));
        assert!(naming.observe("first", true, false, now, |_| true));
        assert_eq!(naming.held_id(), None);
    }

    #[test]
    fn emptying_a_held_draft_allows_its_next_title_to_settle_again() {
        let now = Instant::now();
        let mut naming = DraftNaming::default();
        naming.observe("draft", true, false, now, |_| true);
        assert!(naming.observe("draft", false, false, now, |_| false));
        naming.observe("draft", true, false, now, |_| true);
        assert_eq!(naming.held_id(), Some("draft"));
    }

    #[test]
    fn blur_or_explicit_save_releases_once_and_never_reholds_a_nonempty_title() {
        let now = Instant::now();
        let mut naming = DraftNaming::default();
        naming.observe("draft", true, false, now, |_| true);
        assert!(naming.release(|_| true));
        assert!(!naming.release(|_| true));
        naming.observe("draft", true, false, now, |_| true);
        assert_eq!(naming.held_id(), None);
        assert!(!naming.due(now + NAME_SETTLES));
    }

    #[test]
    fn resetting_workspace_discards_held_and_released_identities() {
        let now = Instant::now();
        let mut naming = DraftNaming::default();
        naming.observe("released", true, false, now, |_| true);
        naming.release(|_| true);
        naming.observe("held", true, false, now, |_| true);
        naming.reset();
        assert_eq!(naming.held_id(), None);
        naming.observe("released", true, false, now, |_| true);
        assert_eq!(naming.held_id(), Some("released"));
    }

    #[test]
    fn pruning_removes_filed_and_deleted_identities() {
        let now = Instant::now();
        let mut naming = DraftNaming::default();
        naming.observe("filed", true, false, now, |_| true);
        naming.release(|_| true);
        naming.observe("draft", true, false, now, |_| true);
        naming.release(|_| true);
        naming.observe("deleted", true, false, now, |_| true);
        naming.retain(|id| id == "draft");
        assert_eq!(naming.held_id(), None);
        assert_eq!(naming.released, HashSet::from(["draft".to_owned()]));
    }

    #[test]
    fn duplicate_quit_requests_share_one_prompt_until_it_finishes() {
        let mut quit = QuitState::default();
        assert!(quit.begin());
        assert!(!quit.begin());
        quit.cancel();
        assert!(quit.begin());
    }
}
