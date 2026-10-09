//! A notes backend that keeps everything in memory, for tests of the store and of
//! a host. It can be told to fail, so that a test can see what a failure leaves.
use crate::{
    BackendError, BackendMutation, BackendNote, BackendSnapshot, ChangeNotifier, NoteId,
    NotesBackend, StorageRevision,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

#[derive(Default)]
pub struct MemoryState {
    pub records: HashMap<String, BackendNote>,
    pub sequence: u64,
    pub fail: Option<String>,
    /// Commit the write, then report a failure, as a lost reply does.
    pub lose_reply: bool,
    /// Full loads served, to tell them apart from reads by ID.
    pub loads: u64,
    /// Reads by ID that still answer from before the latest write.
    pub stale_reads: u32,
    pub notifier: Option<ChangeNotifier>,
    pub assets: HashMap<String, crate::Asset>,
}
/// Every clone is another handle to the same store, as a second connection is.
#[derive(Clone, Default)]
pub struct MemoryBackend(pub Arc<Mutex<MemoryState>>);
impl MemoryBackend {
    pub fn state(&self) -> std::sync::MutexGuard<'_, MemoryState> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
    /// Every stored note, in the order of their IDs.
    pub fn notes(&self) -> Vec<BackendNote> {
        let mut notes: Vec<_> = self.state().records.values().cloned().collect();
        notes.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
        notes
    }
    /// Store a note as another writer does, and return its ID.
    pub fn seed(&self, title: Option<&str>, markdown: &str) -> NoteId {
        let id = NoteId::new(uuid::Uuid::new_v4().to_string());
        let mut state = self.state();
        state.sequence += 1;
        let note = BackendNote {
            id: id.clone(),
            markdown: markdown.into(),
            title: title.map(str::to_owned),
            logical_key: None,
            revision: StorageRevision(state.sequence.to_string()),
            created_at: state.sequence,
            updated_at: state.sequence,
            pinned: false,
        };
        state.records.insert(id.to_string(), note);
        id
    }
    /// Replace a note's Markdown as another writer does. The session that holds the
    /// note learns of it at its next read.
    pub fn rewrite(&self, id: &NoteId, markdown: &str) {
        let mut state = self.state();
        state.sequence += 1;
        let revision = StorageRevision(state.sequence.to_string());
        if let Some(note) = state.records.get_mut(id.as_str()) {
            note.markdown = markdown.into();
            note.revision = revision;
            note.updated_at += 1;
        }
    }
}
impl NotesBackend for MemoryBackend {
    fn set_change_notifier(&mut self, notify: ChangeNotifier) {
        self.0.lock().unwrap().notifier = Some(notify);
    }
    fn capabilities(&self) -> crate::BackendCapabilities {
        crate::BackendCapabilities { assets: true }
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
