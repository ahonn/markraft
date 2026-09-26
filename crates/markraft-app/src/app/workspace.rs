use std::time::{Duration, Instant};

/// Work belongs to one workspace generation. Replacing a folder invalidates
/// every outstanding completion without requiring cancellation of disk writes.
#[derive(Default)]
pub(super) struct Operations {
    pub epoch: u64,
    pub pending: usize,
    pub flushing: bool,
    pub opening: u64,
    pub external: std::collections::HashMap<String, u64>,
    pub retry: Vec<crate::vault::External>,
}

impl Operations {
    pub fn reset(&mut self) {
        *self = Self {
            epoch: self.epoch + 1,
            ..Self::default()
        };
    }

    pub fn external_revision(&mut self, id: &str) -> u64 {
        let revision = self.external.entry(id.to_owned()).or_default();
        *revision += 1;
        *revision
    }
}

/// Decide whether adopting disk state requires a durable copy of local edits.
/// Permission-only changes keep the local document and need no recovery.
pub(super) fn needs_recovery(
    local: Option<&crate::storage::Note>,
    change: &crate::vault::External,
) -> bool {
    use crate::vault::External;
    let Some(local) = local else { return false };
    match change {
        External::Updated { previous, note } => {
            local.document != note.document
                && previous.as_ref().is_none_or(|previous| {
                    previous.document != note.document && previous.document != local.document
                })
        }
        External::Removed(note) => local.document != note.document,
    }
}

const SAVE_DELAY: Duration = Duration::from_millis(350);

/// Tracks whether the current workspace revision has reached durable storage.
/// Durability includes recovery storage; it does not imply that every note has a
/// Markdown file.
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
    #[cfg_attr(coverage_nightly, coverage(off))]
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
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn reconciliation_preserves_local_edits_until_recovery_succeeds() {
        use crate::{doc, storage::Library, vault::External};
        let mut library = Library::default();
        let id = library.active_id.clone();
        library.set_document(&id, doc::from_markdown("original"));
        let before = library.active_note().clone();
        let mut disk = before.clone();
        disk.document = doc::from_markdown("external");
        let update = External::Updated {
            previous: Some(before.clone()),
            note: disk,
        };
        assert!(!needs_recovery(Some(&before), &update));
        library.set_document(&id, doc::from_markdown("local"));
        assert!(needs_recovery(Some(library.active_note()), &update));
        assert!(needs_recovery(
            Some(library.active_note()),
            &External::Removed(before.clone())
        ));

        let mut permissions = before.clone();
        permissions.read_only = Some("Read-only".into());
        assert!(!needs_recovery(
            Some(library.active_note()),
            &External::Updated {
                previous: Some(before),
                note: permissions,
            }
        ));
        assert!(!needs_recovery(None, &update));
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
    fn duplicate_quit_requests_share_one_prompt_until_it_finishes() {
        let mut quit = QuitState::default();
        assert!(quit.begin());
        assert!(!quit.begin());
        quit.cancel();
        assert!(quit.begin());
    }
}
