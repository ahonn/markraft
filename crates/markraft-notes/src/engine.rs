//! What a host hands to, and receives from, the notes store.
use crate::{backend::*, fs::StoreError, storage::Library, vault::Store};

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
    #[cfg(feature = "unstable-internals")]
    #[doc(hidden)]
    pub fn into_parts(self) -> (Library, Option<crate::persistence::Persistence>) {
        (self.library, self.persistence)
    }
    pub fn from_backend(
        backend: Box<dyn NotesBackend>,
        house: markraft_commonmark::HouseStyleHandle,
    ) -> Result<Self, StoreError> {
        let (store, library) = Store::from_backend(backend, house)?;
        Ok(Self::from_parts(
            library,
            Some(crate::persistence::Persistence::from_store(store)),
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
#[non_exhaustive]
pub struct NoteSaveOutcome {
    pub id: crate::NoteId,
    pub result: Result<StorageRevision, BackendError>,
    pub deleted: bool,
}
