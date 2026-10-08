use super::*;
use crate::{
    BackendError, BackendMutation, BackendNote, BackendSnapshot, ChangeNotifier, NewRecord,
    NotesBackend, StorageRevision,
};
use futures::executor::block_on;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct State {
    records: HashMap<String, BackendNote>,
    sequence: u64,
    fail: Option<String>,
    /// Commit the write, then report a failure, as a lost reply does.
    lose_reply: bool,
    /// Full loads served, to tell them apart from reads by ID.
    loads: u64,
    /// Reads by ID that still answer from before the latest write.
    stale_reads: u32,
    notifier: Option<ChangeNotifier>,
    assets: HashMap<String, crate::Asset>,
}
#[derive(Clone, Default)]
struct Memory(Arc<Mutex<State>>);
impl NotesBackend for Memory {
    fn set_change_notifier(&mut self, notify: ChangeNotifier) {
        self.0.lock().unwrap().notifier = Some(notify);
    }
    fn capabilities(&self) -> crate::BackendCapabilities {
        crate::BackendCapabilities {
            file_operations: false,
            assets: true,
        }
    }
    fn read_asset(&mut self, id: &crate::AssetId) -> Result<crate::Asset, BackendError> {
        let state = self.0.lock().unwrap();
        state
            .assets
            .get(&id.0)
            .cloned()
            .ok_or(BackendError::Invalid("No such asset".into()))
    }
    fn write_asset(&mut self, asset: crate::Asset) -> Result<(), BackendError> {
        let mut state = self.0.lock().unwrap();
        if state
            .assets
            .get(&asset.id.0)
            .is_some_and(|stored| stored.bytes != asset.bytes)
        {
            return Err(BackendError::Invalid("The ID names other content".into()));
        }
        state.assets.insert(asset.id.0.clone(), asset);
        Ok(())
    }
    fn load(&mut self) -> Result<BackendSnapshot, BackendError> {
        let mut state = self.0.lock().unwrap();
        state.loads += 1;
        Ok(BackendSnapshot {
            notes: state.records.values().cloned().collect(),
            ..Default::default()
        })
    }
    fn read(&mut self, ids: &[NoteId]) -> Result<Vec<BackendNote>, BackendError> {
        let mut state = self.0.lock().unwrap();
        if state.stale_reads > 0 {
            state.stale_reads -= 1;
            return Ok(Vec::new());
        }
        Ok(ids
            .iter()
            .filter_map(|id| state.records.get(id.as_str()).cloned())
            .collect())
    }
    fn commit(&mut self, mutation: BackendMutation) -> Result<StorageRevision, BackendError> {
        let mut state = self.0.lock().unwrap();
        let (id, expected) = match &mutation {
            BackendMutation::Put { id, expected, .. } => (id, expected.clone()),
            BackendMutation::Delete { id, expected, .. } => (id, Some(expected.clone())),
        };
        if state.fail.as_deref() == Some(id.as_str()) {
            return Err(BackendError::Unavailable("Injected write failure".into()));
        }
        let actual = state.records.get(id.as_str()).map(|r| r.revision.clone());
        if actual != expected {
            return Err(BackendError::Conflict {
                id: id.clone(),
                actual,
            });
        }
        state.sequence += 1;
        let revision = StorageRevision(state.sequence.to_string());
        match mutation {
            BackendMutation::Put {
                id,
                markdown,
                title,
                logical_key,
                created_at,
                updated_at,
                pinned,
                ..
            } => {
                state.records.insert(
                    id.to_string(),
                    BackendNote {
                        id,
                        markdown,
                        title,
                        logical_key,
                        revision: revision.clone(),
                        created_at,
                        updated_at,
                        pinned,
                    },
                );
            }
            BackendMutation::Delete { id, .. } => {
                state.records.remove(id.as_str());
            }
        }
        if state.lose_reply {
            return Err(BackendError::Unavailable("Injected lost reply".into()));
        }
        Ok(revision)
    }
}

#[test]
fn database_notes_keep_exact_source_and_durable_revisions_without_paths() {
    block_on(async {
        let backend = Memory::default();
        let mut notes = NotesLibrary::from_backend(Box::new(backend.clone())).unwrap();
        let raw = "---\ntitle: untouched\n---\n\nHeading\n=======\n\n  Text with spaces.\n";
        let id = notes.create(raw).unwrap();
        assert_eq!(notes.note(&id).unwrap().markdown, raw);
        let receipt = notes.flush().await.unwrap();
        assert!(receipt.saved.contains(&id));
        assert!(!receipt.drafts.contains(&id));
        let before = notes.note(&id).unwrap();
        assert!(before.path.is_none());
        let revision = before.storage_revision.unwrap();
        notes.close().await.unwrap();
        let mut reopened = NotesLibrary::from_backend(Box::new(backend)).unwrap();
        assert_eq!(reopened.note(&id).unwrap().markdown, raw);
        assert_eq!(reopened.note(&id).unwrap().storage_revision, Some(revision));
        let current = reopened.note(&id).unwrap();
        reopened
            .edit(
                &id,
                current.revision,
                &raw.replace("Text with spaces.", "Changed."),
            )
            .unwrap();
        reopened.flush().await.unwrap();
        assert!(
            reopened
                .note(&id)
                .unwrap()
                .markdown
                .starts_with("---\ntitle: untouched\n---\n\nHeading\n=======")
        );
        reopened.close().await.unwrap();
    });
}

#[test]
fn independent_sessions_report_persisted_conflict_and_keep_local_source() {
    block_on(async {
        let backend = Memory::default();
        let mut first = NotesLibrary::from_backend(Box::new(backend.clone())).unwrap();
        let id = first.create("# Shared\n\nBase\n").unwrap();
        first.flush().await.unwrap();
        let mut second = NotesLibrary::from_backend(Box::new(backend.clone())).unwrap();
        first
            .edit(
                &id,
                first.note(&id).unwrap().revision,
                "# Shared\n\nFirst writer\n",
            )
            .unwrap();
        second
            .edit(
                &id,
                second.note(&id).unwrap().revision,
                "# Shared\n\nLocal pending\n",
            )
            .unwrap();
        first.flush().await.unwrap();
        let Err(NotesError::SaveFailed { receipt, error }) = second.flush().await else {
            panic!("expected durable CAS conflict")
        };
        // A conflict keeps the local edits, so it is not reported as a store failure.
        assert!(matches!(error, crate::fs::StoreError::Conflict(_)));
        assert!(receipt.conflicts.contains(&id));
        assert!(receipt.saved.is_empty());
        assert!(second.note(&id).unwrap().markdown.contains("Local pending"));
        assert!(
            backend.0.lock().unwrap().records[id.as_str()]
                .markdown
                .contains("First writer")
        );
        assert!(matches!(
            second.refresh().await,
            Err(NotesError::UnsavedChanges)
        ));
        second.discard_local_changes_and_reload().await.unwrap();
        second.close().await.unwrap();
        first.close().await.unwrap();
    });
}

#[test]
fn partial_failure_acknowledges_only_committed_notes_and_retry_remains_open() {
    block_on(async {
        let backend = Memory::default();
        let mut notes = NotesLibrary::from_backend(Box::new(backend.clone())).unwrap();
        let left = notes.create("Left\n").unwrap();
        let right = notes.create("Right\n").unwrap();
        backend.0.lock().unwrap().fail = Some(right.to_string());
        let Err(NotesError::SaveFailed { receipt, .. }) = notes.close().await else {
            panic!("expected injected failure")
        };
        assert!(receipt.saved.contains(&left));
        assert!(!receipt.saved.contains(&right));
        assert!(!notes.session.library.changes.contains_key(left.as_str()));
        assert!(notes.session.library.changes.contains_key(right.as_str()));
        let durable = backend.0.lock().unwrap().records[left.as_str()]
            .revision
            .clone();
        backend.0.lock().unwrap().fail = None;
        notes.close().await.unwrap();
        let state = backend.0.lock().unwrap();
        assert_eq!(state.records[left.as_str()].revision, durable);
        assert_eq!(state.records[right.as_str()].markdown, "Right\n");
    });
}

#[test]
fn logical_creation_is_idempotent_and_rename_does_not_rewrite_heading() {
    block_on(async {
        let backend = Memory::default();
        let mut notes = NotesLibrary::from_backend(Box::new(backend.clone())).unwrap();
        let record = NewRecord {
            markdown: "Original heading\n================\n".into(),
            title: Some("Daily".into()),
            logical_key: Some("daily:2026-10-08".into()),
        };
        let id = notes.create_record(record.clone()).await.unwrap();
        assert_eq!(notes.create_record(record).await.unwrap(), id);
        assert_eq!(notes.search("Daily").unwrap().len(), 1);
        notes.rename_note(&id, "Renamed metadata").await.unwrap();
        assert_eq!(notes.search("Renamed metadata").unwrap().len(), 1);
        assert!(notes.search("Daily").unwrap().is_empty());
        let note = notes.note(&id).unwrap();
        assert_eq!(note.title, "Renamed metadata");
        assert_eq!(note.markdown, "Original heading\n================\n");
        notes.delete(&id, note.revision).unwrap();
        notes.flush().await.unwrap();
        assert!(!backend.0.lock().unwrap().records.contains_key(id.as_str()));
        notes.close().await.unwrap();
    });
}

#[test]
fn external_refresh_preserves_a_dirty_baseline_until_conflict_is_resolved() {
    block_on(async {
        let backend = Memory::default();
        let mut notes = NotesLibrary::from_backend(Box::new(backend.clone())).unwrap();
        let id = notes.create("Heading\n=======\n\nBase\n").unwrap();
        notes.flush().await.unwrap();
        let snapshot = notes.note(&id).unwrap();
        notes
            .edit(&id, snapshot.revision, "Heading\n=======\n\nLocal\n")
            .unwrap();
        let mut remote = backend.clone();
        let old = remote.load().unwrap().notes.remove(0);
        remote
            .commit(BackendMutation::Put {
                id: id.clone(),
                expected: Some(old.revision),
                markdown: "# Remote\n".into(),
                title: None,
                logical_key: None,
                created_at: old.created_at,
                updated_at: old.updated_at + 1,
                pinned: false,
            })
            .unwrap();
        notes.worker().unwrap().refresh();
        // The snapshot request is an ordered, non-mutating queue barrier.
        notes
            .worker()
            .unwrap()
            .snapshot_async(
                notes.session.library.note(id.as_str()).unwrap().clone(),
                0,
                false,
            )
            .await
            .unwrap();
        notes.poll_changes().unwrap();
        assert!(notes.note(&id).unwrap().markdown.contains("Local"));
        assert!(notes.flush().await.is_err());
        assert!(notes.note(&id).unwrap().markdown.contains("Local"));
        notes.discard_local_changes_and_reload().await.unwrap();
        assert_eq!(notes.note(&id).unwrap().markdown, "# Remote\n");
        notes.close().await.unwrap();
    });
}

#[test]
fn file_backend_create_preserves_exact_markdown_too() {
    block_on(async {
        let root = tempfile::tempdir().unwrap();
        let mut notes = NotesLibrary::open(NotesConfig::new(
            root.path().join("notes"),
            root.path().join("state"),
        ))
        .unwrap();
        let raw = "---\ntitle: untouched\n---\n\nHeading\n=======\n\nText  \n";
        let id = notes.create(raw).unwrap();
        notes.flush().await.unwrap();
        let path = notes.note(&id).unwrap().path.unwrap();
        assert_eq!(std::fs::read_to_string(path).unwrap(), raw);
        notes.close().await.unwrap();
    });
}

#[test]
fn dirty_external_recovery_creates_a_durable_copy_before_adopting_remote() {
    block_on(async {
        let backend = Memory::default();
        let mut notes = NotesLibrary::from_backend(Box::new(backend.clone())).unwrap();
        let id = notes
            .create("---\ncustom: retained\n---\n\nHeading\n=======\n\nBase\n")
            .unwrap();
        notes.flush().await.unwrap();
        let baseline = notes.note(&id).unwrap();
        notes
            .edit(
                &id,
                baseline.revision,
                &baseline.markdown.replace("Base", "Unsaved local"),
            )
            .unwrap();
        let mut remote = backend.clone();
        let old = remote.load().unwrap().notes.remove(0);
        remote
            .commit(BackendMutation::Put {
                id: id.clone(),
                expected: Some(old.revision),
                markdown: "Remote replacement\n".into(),
                title: None,
                logical_key: None,
                created_at: old.created_at,
                updated_at: old.updated_at + 1,
                pinned: false,
            })
            .unwrap();
        notes.worker().unwrap().refresh();
        let local = notes.session.library.note(id.as_str()).unwrap().clone();
        notes
            .worker()
            .unwrap()
            .snapshot_async(local.clone(), 0, false)
            .await
            .unwrap();
        notes.worker().unwrap().recover_async(local).await.unwrap();
        {
            let state = backend.0.lock().unwrap();
            let copy = state.records.values().find(|note| note.id != id).unwrap();
            assert_eq!(
                copy.markdown,
                baseline.markdown.replace("Base", "Unsaved local")
            );
            assert!(
                copy.title
                    .as_deref()
                    .unwrap()
                    .contains(" (conflicted copy ")
            );
            assert!(copy.logical_key.is_none());
        }
        notes.discard_local_changes_and_reload().await.unwrap();
        notes.close().await.unwrap();
    });
}

#[test]
fn accepted_external_source_becomes_the_baseline_for_the_next_edit() {
    block_on(async {
        let backend = Memory::default();
        let mut notes = NotesLibrary::from_backend(Box::new(backend.clone())).unwrap();
        let id = notes.create("Old\n===\n").unwrap();
        notes.flush().await.unwrap();
        let mut remote = backend.clone();
        let old = remote.load().unwrap().notes.remove(0);
        let raw = "---\nversion: remote\n---\n\nNew\n===\n\nBody\n";
        remote
            .commit(BackendMutation::Put {
                id: id.clone(),
                expected: Some(old.revision),
                markdown: raw.into(),
                title: None,
                logical_key: None,
                created_at: old.created_at,
                updated_at: old.updated_at + 1,
                pinned: false,
            })
            .unwrap();
        notes.worker().unwrap().refresh();
        notes
            .worker()
            .unwrap()
            .snapshot_async(
                notes.session.library.note(id.as_str()).unwrap().clone(),
                0,
                false,
            )
            .await
            .unwrap();
        assert!(notes.poll_changes().unwrap());
        let current = notes.note(&id).unwrap();
        notes
            .edit(&id, current.revision, &raw.replace("Body", "Edited"))
            .unwrap();
        notes.flush().await.unwrap();
        assert_eq!(
            backend.0.lock().unwrap().records[id.as_str()].markdown,
            raw.replace("Body", "Edited")
        );
        notes.close().await.unwrap();
    });
}

#[test]
fn markdown_import_keeps_source_but_refuses_unresolved_relative_resources() {
    block_on(async {
        let root = tempfile::tempdir().unwrap();
        let backend = Memory::default();
        let mut notes = NotesLibrary::from_backend(Box::new(backend)).unwrap();
        let source = "---\ncustom: yes\n---\n\nImported\n========\n";
        let path = root.path().join("import.md");
        std::fs::write(&path, source).unwrap();
        let id = notes.open_file(path).await.unwrap();
        assert!(notes.note(&id).unwrap().path.is_none());
        assert_eq!(notes.note(&id).unwrap().markdown, source);
        let path = root.path().join("with-image.md");
        std::fs::write(&path, "![image](relative.png)\n").unwrap();
        assert!(matches!(
            notes.open_file(path).await,
            Err(NotesError::Storage(StoreError::Backend(
                BackendError::Unsupported(_)
            )))
        ));
        let path = root.path().join("with-stable-asset.md");
        let source = "![image](markraft-asset:stable-asset-id)\n";
        std::fs::write(&path, source).unwrap();
        let id = notes.open_file(path).await.unwrap();
        assert_eq!(notes.note(&id).unwrap().markdown, source);
        let path = root.path().join("with-unsupported-asset.md");
        std::fs::write(&path, "![image](asset://unsupported)\n").unwrap();
        assert!(matches!(
            notes.open_file(path).await,
            Err(NotesError::Storage(StoreError::Backend(
                BackendError::Unsupported(_)
            )))
        ));
        notes.close().await.unwrap();
    });
}

#[test]
fn invalid_reload_leaves_the_previous_exact_source_and_revision_intact() {
    block_on(async {
        let backend = Memory::default();
        let mut notes = NotesLibrary::from_backend(Box::new(backend.clone())).unwrap();
        let raw = "---\noriginal: true\n---\n\nHeading\n=======\n";
        let id = notes.create(raw).unwrap();
        notes.flush().await.unwrap();
        let before = notes.note(&id).unwrap();
        {
            let mut state = backend.0.lock().unwrap();
            let mut malformed = state.records[id.as_str()].clone();
            malformed.id = NoteId::new("");
            state.records.insert("".into(), malformed);
        }
        assert!(notes.refresh().await.is_err());
        let after = notes.note(&id).unwrap();
        assert_eq!(after.markdown, raw);
        assert_eq!(after.storage_revision, before.storage_revision);
        backend.0.lock().unwrap().records.remove("");
        notes.close().await.unwrap();
    });
}

#[test]
fn a_stale_local_deletion_never_removes_an_unseen_remote_revision() {
    block_on(async {
        let backend = Memory::default();
        let mut notes = NotesLibrary::from_backend(Box::new(backend.clone())).unwrap();
        let id = notes.create("Delete candidate\n").unwrap();
        notes.flush().await.unwrap();
        notes
            .delete(&id, notes.note(&id).unwrap().revision)
            .unwrap();
        let mut remote = backend.clone();
        let old = remote.load().unwrap().notes.remove(0);
        remote
            .commit(BackendMutation::Put {
                id: id.clone(),
                expected: Some(old.revision),
                markdown: "Remote work must survive\n".into(),
                title: None,
                logical_key: None,
                created_at: old.created_at,
                updated_at: old.updated_at + 1,
                pinned: false,
            })
            .unwrap();
        notes.worker().unwrap().refresh();
        let Err(NotesError::SaveFailed { receipt, .. }) = notes.flush().await else {
            panic!("stale delete must conflict")
        };
        assert!(receipt.conflicts.contains(&id));
        assert!(receipt.removed.is_empty());
        assert_eq!(
            backend.0.lock().unwrap().records[id.as_str()].markdown,
            "Remote work must survive\n"
        );
        notes.discard_local_changes_and_reload().await.unwrap();
        notes.close().await.unwrap();
    });
}

#[test]
fn reopening_a_logical_note_cannot_promote_a_dirty_edit_to_a_new_remote_revision() {
    block_on(async {
        let backend = Memory::default();
        let mut notes = NotesLibrary::from_backend(Box::new(backend.clone())).unwrap();
        let request = NewRecord {
            markdown: "Base\n".into(),
            title: Some("Daily".into()),
            logical_key: Some("daily:2026-10-08".into()),
        };
        let id = notes.create_record(request.clone()).await.unwrap();
        notes
            .edit(&id, notes.note(&id).unwrap().revision, "Local pending\n")
            .unwrap();
        let mut remote = backend.clone();
        let old = remote.load().unwrap().notes.remove(0);
        remote
            .commit(BackendMutation::Put {
                id: id.clone(),
                expected: Some(old.revision),
                markdown: "Remote newer\n".into(),
                title: old.title,
                logical_key: old.logical_key,
                created_at: old.created_at,
                updated_at: old.updated_at + 1,
                pinned: false,
            })
            .unwrap();
        assert_eq!(notes.create_record(request).await.unwrap(), id);
        assert!(notes.flush().await.is_err());
        assert_eq!(
            backend.0.lock().unwrap().records[id.as_str()].markdown,
            "Remote newer\n"
        );
        assert_eq!(notes.note(&id).unwrap().markdown, "Local pending\n");
        notes.discard_local_changes_and_reload().await.unwrap();
        notes.close().await.unwrap();
    });
}

/// Wait until the worker has handled every request sent before this call.
async fn settle(notes: &NotesLibrary) {
    let any = notes.session.library.notes[0].clone();
    notes
        .worker()
        .unwrap()
        .snapshot_async(any, 0, false)
        .await
        .unwrap();
}

#[test]
fn a_logical_key_names_one_note_for_every_session() {
    block_on(async {
        let backend = Memory::default();
        let key = "daily:2026-10-08";
        let record = |markdown: &str| NewRecord {
            markdown: markdown.into(),
            title: None,
            logical_key: Some(key.into()),
        };
        let mut first = NotesLibrary::from_backend(Box::new(backend.clone())).unwrap();
        let mut second = NotesLibrary::from_backend(Box::new(backend.clone())).unwrap();
        let id = first.create_record(record("First\n")).await.unwrap();
        assert_eq!(id, NoteId::for_logical_key(key));
        // The second session loaded before the note existed, and its first read
        // still misses it, so its create reaches storage and loses there.
        backend.0.lock().unwrap().stale_reads = 1;
        assert_eq!(second.create_record(record("Second\n")).await.unwrap(), id);
        assert_eq!(second.note(&id).unwrap().markdown, "First\n");
        {
            let state = backend.0.lock().unwrap();
            assert_eq!(state.records.len(), 1);
            assert_eq!(state.records[id.as_str()].markdown, "First\n");
        }
        first.close().await.unwrap();
        second.close().await.unwrap();
    });
}

#[test]
fn a_retry_after_a_lost_reply_is_not_a_conflict() {
    block_on(async {
        let backend = Memory::default();
        let mut notes = NotesLibrary::from_backend(Box::new(backend.clone())).unwrap();
        let id = notes.create("Kept\n").unwrap();
        backend.0.lock().unwrap().lose_reply = true;
        assert!(notes.flush().await.is_err());
        assert!(notes.session.library.changes.contains_key(id.as_str()));
        backend.0.lock().unwrap().lose_reply = false;
        let receipt = notes.flush().await.unwrap();
        assert!(receipt.saved.contains(&id));
        let stored = backend.0.lock().unwrap().records[id.as_str()].clone();
        assert_eq!(stored.markdown, "Kept\n");
        assert_eq!(
            notes.note(&id).unwrap().storage_revision,
            Some(stored.revision.clone())
        );
        // The same applies to an update whose reply was lost.
        let current = notes.note(&id).unwrap();
        notes.edit(&id, current.revision, "Kept twice\n").unwrap();
        backend.0.lock().unwrap().lose_reply = true;
        assert!(notes.flush().await.is_err());
        backend.0.lock().unwrap().lose_reply = false;
        notes.flush().await.unwrap();
        assert_eq!(notes.note(&id).unwrap().markdown, "Kept twice\n");
        assert!(!notes.session.library.note(id.as_str()).unwrap().conflicted);
        notes.close().await.unwrap();
    });
}

#[test]
fn a_named_change_reads_only_the_named_notes() {
    block_on(async {
        let backend = Memory::default();
        let mut notes = NotesLibrary::from_backend(Box::new(backend.clone())).unwrap();
        let changed = notes.create("Changed\n").unwrap();
        let removed = notes.create("Removed\n").unwrap();
        let untouched = notes.create("Untouched\n").unwrap();
        notes.flush().await.unwrap();
        let mut remote = backend.clone();
        let revision = |id: &NoteId| backend.0.lock().unwrap().records[id.as_str()].clone();
        let old = revision(&changed);
        remote
            .commit(BackendMutation::Put {
                id: changed.clone(),
                expected: Some(old.revision),
                markdown: "Remote\n".into(),
                title: None,
                logical_key: None,
                created_at: old.created_at,
                updated_at: old.updated_at + 1,
                pinned: false,
            })
            .unwrap();
        remote
            .commit(BackendMutation::Delete {
                id: removed.clone(),
                expected: revision(&removed).revision,
                deleted_at: 1,
            })
            .unwrap();
        let (notifier, loads) = {
            let state = backend.0.lock().unwrap();
            (state.notifier.clone().unwrap(), state.loads)
        };
        notifier.changed(vec![changed.clone(), removed.clone()]);
        settle(&notes).await;
        assert!(notes.poll_changes().unwrap());
        assert_eq!(notes.note(&changed).unwrap().markdown, "Remote\n");
        assert!(notes.note(&removed).is_err());
        assert_eq!(notes.note(&untouched).unwrap().markdown, "Untouched\n");
        assert_eq!(backend.0.lock().unwrap().loads, loads);
        notes.close().await.unwrap();
    });
}

#[test]
fn an_asset_is_named_by_its_content() {
    block_on(async {
        let backend = Memory::default();
        let mut notes = NotesLibrary::from_backend(Box::new(backend.clone())).unwrap();
        let asset = crate::Asset::new("image/png", vec![1, 2, 3]);
        let id = asset.id.clone();
        assert_eq!(id, crate::Asset::new("image/gif", vec![1, 2, 3]).id);
        notes.write_asset(asset.clone()).await.unwrap();
        notes.write_asset(asset).await.unwrap();
        assert_eq!(backend.0.lock().unwrap().assets.len(), 1);
        assert_eq!(notes.read_asset(id.clone()).await.unwrap().bytes, [1, 2, 3]);
        // An ID that does not match the content is refused before it reaches storage.
        let forged = crate::Asset {
            id: crate::AssetId("chosen-by-the-caller".into()),
            media_type: "image/png".into(),
            bytes: vec![4],
        };
        assert!(notes.write_asset(forged).await.is_err());
        assert_eq!(backend.0.lock().unwrap().assets.len(), 1);
        // Storage that returns other bytes for an ID is not believed.
        backend
            .0
            .lock()
            .unwrap()
            .assets
            .get_mut(&id.0)
            .unwrap()
            .bytes = vec![9];
        assert!(notes.read_asset(id.clone()).await.is_err());
        let markdown = format!(
            "![one]({})\n\n![again]({})\n\n![remote](https://example.com/a.png)\n",
            id.source(),
            id.source()
        );
        assert_eq!(crate::asset_references(&markdown).unwrap(), vec![id]);
        notes.close().await.unwrap();
    });
}

#[test]
fn the_test_backend_keeps_the_backend_contract() {
    crate::conformance::check(|| {
        let store = Memory::default();
        move || store.clone()
    });
}
