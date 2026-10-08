//! Shared worker adapter for file and host-owned persistence.
use crate::{
    backend::*,
    fs::StoreError,
    persistence::Saved,
    storage::{Library, Note, Notices, Preferences},
    vault::{External, Sources, Store},
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

pub(crate) fn unsupported(operation: &'static str) -> StoreError {
    StoreError::Backend(BackendError::Unsupported(operation))
}
pub(crate) trait WorkerBackend: Send {
    fn sources(&self) -> Arc<Sources>;
    fn house(&self) -> markraft_commonmark::HouseStyleHandle;
    fn notices(&self) -> Notices;
    fn capabilities(&self) -> BackendCapabilities;
    fn persisted(&self) -> Vec<(String, StorageRevision)>;
    fn set_change_notifier(&mut self, _notify: ChangeNotifier) {}
    fn watch_root(&self) -> Option<PathBuf> {
        None
    }
    fn extra_watch_directories(&self) -> Vec<PathBuf> {
        Vec::new()
    }
    fn save(&mut self, revision: u64, library: &Library, preferences: &Preferences) -> Saved;
    fn reload(&mut self) -> Result<Library, StoreError>;
    fn refresh(&mut self) -> Result<Vec<External>, StoreError>;
    fn refresh_paths(&mut self, _paths: &[PathBuf]) -> Result<Vec<External>, StoreError> {
        self.refresh()
    }
    fn refresh_notes(&mut self, _ids: &[crate::NoteId]) -> Result<Vec<External>, StoreError> {
        self.refresh()
    }
    fn acknowledge_changes(&mut self, _changes: &[External]) {}
    #[cfg(any(test, feature = "test-support"))]
    fn acknowledge(&mut self, _ids: &[String]) {}
    fn recover(&mut self, _note: &Note) -> Result<(), StoreError> {
        Err(unsupported("recovery files"))
    }
    fn add_file(&mut self, _path: PathBuf) -> Result<Note, StoreError> {
        Err(unsupported("opening files in place"))
    }
    fn read_text(&self, _relative: &Path) -> Option<String> {
        None
    }
    fn create_note(&mut self, _relative: &Path, _text: &str) -> Result<Note, StoreError> {
        Err(unsupported("file creation"))
    }
    fn rename(&mut self, _id: &str, _name: &str) -> Result<PathBuf, StoreError> {
        Err(unsupported("file rename"))
    }
    fn create_record(&mut self, _request: crate::NewRecord) -> Result<Note, StoreError> {
        Err(unsupported("logical note creation"))
    }
    fn rename_record(&mut self, _id: &str, _name: &str) -> Result<Note, StoreError> {
        Err(unsupported("logical note rename"))
    }
    fn read_asset(&mut self, _id: &AssetId) -> Result<Asset, StoreError> {
        Err(unsupported("attachments"))
    }
    fn write_asset(&mut self, _asset: Asset) -> Result<(), StoreError> {
        Err(unsupported("attachments"))
    }
}

impl WorkerBackend for Store {
    fn sources(&self) -> Arc<Sources> {
        self.source_cache()
    }
    fn house(&self) -> markraft_commonmark::HouseStyleHandle {
        Store::house(self)
    }
    fn notices(&self) -> Notices {
        Store::notices(self)
    }
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            file_operations: true,
            assets: false,
        }
    }
    fn persisted(&self) -> Vec<(String, StorageRevision)> {
        self.storage_revisions()
    }
    fn watch_root(&self) -> Option<PathBuf> {
        Some(self.directory().to_owned())
    }
    fn extra_watch_directories(&self) -> Vec<PathBuf> {
        Store::extra_watch_directories(self)
    }
    fn save(&mut self, revision: u64, library: &Library, preferences: &Preferences) -> Saved {
        let result = Store::save(self, library, preferences);
        let changes = if result.is_ok() {
            library
                .changes
                .iter()
                .map(|(id, generation)| (id.clone(), *generation))
                .collect()
        } else {
            Vec::new()
        };
        let versions: std::collections::HashMap<_, _> =
            self.storage_revisions().into_iter().collect();
        let kept = self.kept();
        let conflicts = self.conflicts();
        let outcomes = library
            .changes
            .keys()
            .filter_map(|id| {
                let deleted = library.deletions.contains_key(id);
                let version = if deleted && !kept.contains(id) {
                    Some(StorageRevision(format!(
                        "deleted:{id}:{}",
                        library.deletions[id].deleted_at.unwrap_or(0)
                    )))
                } else {
                    versions.get(id).cloned()
                };
                let status = match (&result, version) {
                    (_, _) if deleted && kept.contains(id) => Err(BackendError::Conflict {
                        id: crate::NoteId::new(id.clone()),
                        actual: versions.get(id).cloned(),
                    }),
                    (Ok(()), Some(version)) => Ok(version),
                    (Ok(()), None) => return None,
                    (Err(_), _) if conflicts.contains(id) => Err(BackendError::Conflict {
                        id: crate::NoteId::new(id.clone()),
                        actual: versions.get(id).cloned(),
                    }),
                    (Err(error), _) => Err(BackendError::Unavailable(error.to_string())),
                };
                Some(crate::NoteSaveOutcome {
                    id: crate::NoteId::new(id.clone()),
                    result: status,
                    deleted,
                })
            })
            .collect();
        Saved {
            revision,
            changes,
            result,
            paths: self.paths(),
            conflicts,
            trashed: self.trashed(),
            kept,
            outcomes,
        }
    }
    fn reload(&mut self) -> Result<Library, StoreError> {
        Store::reload(self)
    }
    fn refresh(&mut self) -> Result<Vec<External>, StoreError> {
        Store::refresh(self)
    }
    fn refresh_paths(&mut self, paths: &[PathBuf]) -> Result<Vec<External>, StoreError> {
        Store::refresh_paths(self, paths)
    }
    fn acknowledge_changes(&mut self, changes: &[External]) {
        Store::acknowledge_changes(self, changes)
    }
    #[cfg(any(test, feature = "test-support"))]
    fn acknowledge(&mut self, ids: &[String]) {
        Store::acknowledge(self, ids)
    }
    fn recover(&mut self, note: &Note) -> Result<(), StoreError> {
        Store::recover(self, note)
    }
    fn add_file(&mut self, path: PathBuf) -> Result<Note, StoreError> {
        Store::add_file(self, path)
    }
    fn read_text(&self, relative: &Path) -> Option<String> {
        Store::read_text(self, relative)
    }
    fn create_note(&mut self, relative: &Path, text: &str) -> Result<Note, StoreError> {
        Store::create_note(self, relative, text)
    }
    fn rename(&mut self, id: &str, name: &str) -> Result<PathBuf, StoreError> {
        Store::rename(self, id, name)
    }
    fn rename_record(&mut self, id: &str, name: &str) -> Result<Note, StoreError> {
        Store::rename(self, id, name)?;
        Store::reload(self)?
            .note(id)
            .cloned()
            .ok_or_else(|| unsupported("missing renamed note"))
    }
}

/// A loaded notes store and its storage worker. A host prepares one off the UI
/// thread and hands it to the workspace view. Headless hosts use NotesLibrary.
pub struct NotesSession {
    pub(crate) library: Library,
    pub(crate) persistence: Option<crate::persistence::Persistence>,
}
impl NotesSession {
    pub fn house(&self) -> markraft_commonmark::HouseStyleHandle {
        self.persistence
            .as_ref()
            .map(|p| p.house())
            .unwrap_or_default()
    }
    pub(crate) fn from_parts(
        library: Library,
        persistence: Option<crate::persistence::Persistence>,
    ) -> Self {
        Self {
            library,
            persistence,
        }
    }
    /// For the workspace view, which drives the model and the worker directly.
    #[doc(hidden)]
    pub fn into_parts(self) -> (Library, Option<crate::persistence::Persistence>) {
        (self.library, self.persistence)
    }
    pub fn from_backend(
        backend: Box<dyn NotesBackend>,
        house: markraft_commonmark::HouseStyleHandle,
    ) -> Result<Self, StoreError> {
        let (store, library) = crate::records::RecordStore::open(backend, house)?;
        Ok(Self::from_parts(
            library,
            Some(crate::persistence::Persistence::from_worker(Box::new(
                store,
            ))),
        ))
    }
}

/// Exact source and optional backend-owned logical identity for a new note.
#[derive(Clone, Debug)]
pub struct NewRecord {
    pub markdown: String,
    pub title: Option<String>,
    pub logical_key: Option<String>,
}
impl NewRecord {
    pub fn markdown(markdown: impl Into<String>) -> Self {
        Self {
            markdown: markdown.into(),
            title: None,
            logical_key: None,
        }
    }
}

/// The outcome for one note. Partial saves never imply the failed notes are durable.
#[derive(Clone, Debug)]
pub struct NoteSaveOutcome {
    pub id: crate::NoteId,
    pub result: Result<StorageRevision, BackendError>,
    pub deleted: bool,
}
