//! Public, headless library boundary. Mutable access is the single write authority;
//! snapshots are immutable and edits compare revisions before replacing content.
use crate::{doc, fs::StoreError, persistence::Persistence, storage::Preferences, vault::Store};
use std::{collections::HashMap, fmt, path::PathBuf};

/// Explicit locations owned by this library, independent of application settings.
/// `state_dir` must be outside `notes_dir`; manifests and recovery data never
/// become user Markdown content. A directory lock coordinates all hosts.
#[derive(Clone, Debug)]
pub struct NotesConfig {
    pub notes_dir: PathBuf,
    pub state_dir: PathBuf,
}
impl NotesConfig {
    pub fn new(notes_dir: impl Into<PathBuf>, state_dir: impl Into<PathBuf>) -> Self {
        Self {
            notes_dir: notes_dir.into(),
            state_dir: state_dir.into(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct NoteId(String);
impl NoteId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    /// The ID of the note that owns `key`. Every session and device derives the
    /// same ID, so two writers that create the note independently address one
    /// record, and its storage identity keeps the key unique.
    pub fn for_logical_key(key: &str) -> Self {
        const NAMESPACE: uuid::Uuid =
            uuid::Uuid::from_u128(0x3f0c_8a52_9b7e_4d16_a1c4_6e2d_5b90_73af);
        Self(uuid::Uuid::new_v5(&NAMESPACE, key.as_bytes()).to_string())
    }
}
impl fmt::Display for NoteId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Lightweight list/search metadata. Unreadable files remain visible without
/// requiring their contents to be decoded or rendered as part of a search.
#[derive(Clone, Debug)]
pub struct NoteSummary {
    pub id: NoteId,
    pub title: String,
    pub path: Option<PathBuf>,
    pub read_only: Option<crate::locale::Message>,
    /// Compare-and-replace token scoped to this opened library instance.
    pub revision: u64,
    /// Unix milliseconds.
    pub created_at: u64,
    /// Unix milliseconds.
    pub updated_at: u64,
    pub storage_revision: Option<crate::StorageRevision>,
}

/// An immutable committed document. It contains no view or input-composition state.
#[derive(Clone, Debug)]
pub struct NoteSnapshot {
    pub id: NoteId,
    pub title: String,
    pub markdown: String,
    pub document: markraft_core::Node,
    /// Unix milliseconds.
    pub created_at: u64,
    /// Unix milliseconds.
    pub updated_at: u64,
    /// Compare-and-replace token scoped to this opened `NotesLibrary` instance.
    /// After reopening, obtain a fresh snapshot; this is not a persisted file version.
    pub revision: u64,
    pub path: Option<PathBuf>,
    pub storage_revision: Option<crate::StorageRevision>,
}

/// A local durability barrier. Success means the backend committed the requested
/// snapshot. It does not promise network sync or treat recovery copies as a save.
#[derive(Clone, Debug)]
pub struct SaveReceipt {
    /// Save barrier revision within this opened library, not across restarts.
    pub revision: u64,
    pub saved: Vec<NoteId>,
    /// Documents with no persisted storage identity. A missing file path does
    /// not make a database note a draft.
    pub drafts: Vec<NoteId>,
    pub removed: Vec<NoteId>,
    pub conflicts: Vec<NoteId>,
    pub outcomes: Vec<crate::NoteSaveOutcome>,
}

#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum NotesError {
    Storage(StoreError),
    Closed,
    NotFound(NoteId),
    ReadOnly(NoteId),
    StaleRevision {
        expected: u64,
        actual: u64,
    },
    InvalidMarkdown(String),
    UnsupportedEdit,
    UnsavedChanges,
    SaveFailed {
        receipt: Box<SaveReceipt>,
        error: StoreError,
    },
}
impl From<StoreError> for NotesError {
    fn from(error: StoreError) -> Self {
        Self::Storage(error)
    }
}
impl fmt::Display for NotesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Storage(error) | Self::SaveFailed { error, .. } => error.fmt(f),
            Self::Closed => f.write_str("The notes library is closed"),
            Self::NotFound(id) => write!(f, "Note {id} does not exist"),
            Self::ReadOnly(id) => write!(f, "Note {id} is read-only"),
            Self::StaleRevision { expected, actual } => {
                write!(f, "Expected revision {expected}, found {actual}")
            }
            Self::InvalidMarkdown(error) => write!(f, "Invalid Markdown: {error}"),
            Self::UnsupportedEdit => {
                f.write_str("This edit cannot be represented without losing Markdown source")
            }
            Self::UnsavedChanges => f.write_str("Save local changes before refreshing the library"),
        }
    }
}
impl std::error::Error for NotesError {}

/// A library owns committed documents and the persistence worker, never a window.
///
/// Startup and search are synchronous and should run on the host's background
/// executor for large folders. Save, refresh and close are runtime-independent
/// futures. Dropping is best-effort cleanup; call [`Self::close`] for durability.
pub struct NotesLibrary {
    session: crate::NotesSession,
    revisions: HashMap<String, u64>,
    revision: u64,
    root: Option<PathBuf>,
    last_receipt: Option<SaveReceipt>,
    closing: bool,
}
impl NotesLibrary {
    pub fn open(config: NotesConfig) -> Result<Self, NotesError> {
        let directory = crate::storage::ensure_notes_folder(&config.notes_dir)?;
        let (store, library) = Store::open_library(directory.clone(), config.state_dir)?;
        let revisions = library
            .notes
            .iter()
            .map(|note| (note.id.clone(), 0))
            .collect();
        Ok(Self {
            session: crate::NotesSession::from_parts(
                library,
                Some(Persistence::new(store, Default::default())),
            ),
            revisions,
            revision: 0,
            root: Some(directory),
            last_receipt: None,
            closing: false,
        })
    }

    pub fn from_backend(backend: Box<dyn crate::NotesBackend>) -> Result<Self, NotesError> {
        let session = crate::NotesSession::from_backend(backend, Default::default())?;
        let revisions = session
            .library
            .notes
            .iter()
            .map(|note| (note.id.clone(), 0))
            .collect();
        Ok(Self {
            session,
            revisions,
            revision: 0,
            root: None,
            last_receipt: None,
            closing: false,
        })
    }

    pub fn capabilities(&self) -> Result<crate::BackendCapabilities, NotesError> {
        Ok(self.worker()?.capabilities())
    }

    /// Create or retrieve a note using a stable logical key. The backend commits
    /// its exact source before this call returns; no file path is required.
    pub async fn create_record(&mut self, record: crate::NewRecord) -> Result<NoteId, NotesError> {
        let note = self.worker()?.create_record_async(record).await?;
        let id = NoteId::new(note.id.clone());
        if !self.session.library.changes.contains_key(id.as_str()) {
            self.session.library.adopt(note);
        }
        self.revision += 1;
        self.revisions.insert(id.0.clone(), self.revision);
        Ok(id)
    }

    pub async fn rename_note(&mut self, id: &NoteId, name: &str) -> Result<(), NotesError> {
        self.note(id)?;
        self.flush().await?;
        let note = self
            .worker()?
            .rename_note_async(id.to_string(), name.to_owned())
            .await?;
        self.session.library.adopt(note);
        self.revision += 1;
        self.revisions.insert(id.0.clone(), self.revision);
        Ok(())
    }

    pub async fn read_asset(&self, id: crate::AssetId) -> Result<crate::Asset, NotesError> {
        Ok(self.worker()?.read_asset_async(id).await?)
    }
    pub async fn write_asset(&self, asset: crate::Asset) -> Result<(), NotesError> {
        Ok(self.worker()?.write_asset_async(asset).await?)
    }

    fn worker(&self) -> Result<&Persistence, NotesError> {
        if self.closing {
            return Err(NotesError::Closed);
        }
        self.session.persistence.as_ref().ok_or(NotesError::Closed)
    }

    /// Create an unsaved document with its exact Markdown baseline. Front matter
    /// and original whitespace remain available to both file and database saves.
    pub fn create(&mut self, markdown: &str) -> Result<NoteId, NotesError> {
        self.worker()?;
        let document = parse(markdown)?;
        let id = self.session.library.new_note(document);
        self.worker()?
            .register_source(self.session.library.note(&id).expect("new note"), markdown)?;
        self.revision += 1;
        self.revisions.insert(id.clone(), self.revision);
        Ok(NoteId(id))
    }

    pub fn note(&self, id: &NoteId) -> Result<NoteSnapshot, NotesError> {
        let worker = self.worker()?;
        let note = self
            .session
            .library
            .note(&id.0)
            .ok_or_else(|| NotesError::NotFound(id.clone()))?;
        let markdown = match worker.source(note.clone())? {
            Some(track) => track
                .snapshot()
                .render(doc::schema(), &note.document)
                .map_err(|_| NotesError::UnsupportedEdit)?,
            None => format!(
                "{}\n",
                doc::to_markdown_in(&note.document, &Default::default())
            ),
        };
        Ok(NoteSnapshot {
            id: id.clone(),
            title: note.title(),
            markdown,
            document: note.document.clone(),
            created_at: note.created_at,
            updated_at: note.updated_at,
            revision: self.revisions.get(&id.0).copied().unwrap_or(0),
            path: note.path.clone(),
            storage_revision: worker.storage_revision(&note.id),
        })
    }

    pub fn search(&self, query: &str) -> Result<Vec<NoteSummary>, NotesError> {
        self.worker()?;
        Ok(self
            .session
            .library
            .search(query, self.root.as_deref())
            .into_iter()
            .map(|note| NoteSummary {
                id: NoteId(note.id.clone()),
                title: note.title(),
                path: note.path.clone(),
                read_only: note.read_only.clone(),
                revision: self.revisions.get(&note.id).copied().unwrap_or(0),
                created_at: note.created_at,
                updated_at: note.updated_at,
                storage_revision: self
                    .worker()
                    .ok()
                    .and_then(|worker| worker.storage_revision(&note.id)),
            })
            .collect())
    }

    /// Compare-and-replace prevents two independently mounted views from silently
    /// overwriting each other's committed edits. IME composition stays in the view.
    pub fn edit(
        &mut self,
        id: &NoteId,
        expected_revision: u64,
        markdown: &str,
    ) -> Result<u64, NotesError> {
        self.edit_document(id, expected_revision, parse(markdown)?)
    }

    /// Commit a structured editor document without a Markdown round trip.
    pub fn edit_document(
        &mut self,
        id: &NoteId,
        expected_revision: u64,
        document: markraft_core::Node,
    ) -> Result<u64, NotesError> {
        self.worker()?;
        let note = self
            .session
            .library
            .note(&id.0)
            .ok_or_else(|| NotesError::NotFound(id.clone()))?;
        if note.read_only.is_some() {
            return Err(NotesError::ReadOnly(id.clone()));
        }
        let actual = self.revisions.get(&id.0).copied().unwrap_or(0);
        if actual != expected_revision {
            return Err(NotesError::StaleRevision {
                expected: expected_revision,
                actual,
            });
        }
        document
            .check(doc::schema())
            .map_err(|error| NotesError::InvalidMarkdown(error.to_string()))?;
        // Validate against an immutable baseline before accepting content or
        // incrementing its revision. SourceTrack::save may advance a live track,
        // so it is deliberately not used as a speculative validation operation.
        match self.worker()?.source(note.clone())? {
            Some(track) => track.snapshot().render(doc::schema(), &document),
            None => markraft_commonmark::SourceDocument::parse(doc::schema(), "")
                .expect("empty Markdown")
                .render(doc::schema(), &document),
        }
        .map_err(|_| NotesError::UnsupportedEdit)?;
        if self.session.library.set_document(&id.0, document) {
            self.revision += 1;
            self.revisions.insert(id.0.clone(), self.revision);
        }
        Ok(self.revisions.get(&id.0).copied().unwrap_or(0))
    }

    /// Schedule a writable file for the system Trash on the next flush. This is
    /// deletion, not detaching an external file from a view. Read-only notes are refused.
    pub fn delete(&mut self, id: &NoteId, expected_revision: u64) -> Result<(), NotesError> {
        self.worker()?;
        let note = self
            .session
            .library
            .note(&id.0)
            .ok_or_else(|| NotesError::NotFound(id.clone()))?;
        if note.read_only.is_some() {
            return Err(NotesError::ReadOnly(id.clone()));
        }
        let actual = self.revisions.get(&id.0).copied().unwrap_or(0);
        if actual != expected_revision {
            return Err(NotesError::StaleRevision {
                expected: expected_revision,
                actual,
            });
        }
        self.session.library.delete(&id.0);
        self.revision += 1;
        Ok(())
    }

    /// Create a file with its exact UTF-8 source, or open the file already at that
    /// relative path. Existing files are never overwritten by this operation.
    pub async fn create_file(
        &mut self,
        relative: PathBuf,
        markdown: String,
    ) -> Result<NoteId, NotesError> {
        let created = self
            .worker()?
            .create_note_async(crate::persistence::NewNote {
                relative,
                template: None,
                fill: Box::new(move |_| markdown),
            })
            .await?;
        let id = NoteId(created.note.id.clone());
        if !self.session.library.changes.contains_key(id.as_str()) {
            self.session.library.adopt(created.note);
            self.revision += 1;
            self.revisions.insert(id.0.clone(), self.revision);
        }
        Ok(id)
    }

    /// Open an existing Markdown file, including a file outside the library root.
    pub async fn open_file(&mut self, path: PathBuf) -> Result<NoteId, NotesError> {
        let note = self.worker()?.open_file_async(path).await?;
        let id = NoteId(note.id.clone());
        if !self.session.library.changes.contains_key(id.as_str()) {
            self.session.library.adopt(note);
            self.revision += 1;
            self.revisions.insert(id.0.clone(), self.revision);
        }
        Ok(id)
    }

    /// Finish local writes before changing a file's name. The store performs a
    /// no-replace rename so an existing destination is never silently overwritten.
    pub async fn rename(&mut self, id: &NoteId, name: &str) -> Result<PathBuf, NotesError> {
        self.note(id)?;
        self.flush().await?;
        let path = self
            .worker()?
            .rename_async(id.0.clone(), name.to_owned())
            .await?;
        if let Some(note) = self
            .session
            .library
            .notes
            .iter_mut()
            .find(|note| note.id == id.0)
        {
            note.path = Some(path.clone());
        }
        self.revision += 1;
        self.revisions.insert(id.0.clone(), self.revision);
        Ok(path)
    }

    /// Drain recoverable diagnostics such as restored conflicts or unavailable
    /// file watching. Messages remain structured until the host chooses a locale.
    pub fn take_notices(&self) -> Result<Vec<crate::locale::Message>, NotesError> {
        Ok(self.worker()?.notices())
    }

    /// Integrate file-watcher notifications. Dirty local documents retain their
    /// version until flush resolves or preserves a conflict. Hosts can call this
    /// from their own tick/subscription; it does not require a UI runtime.
    pub fn poll_changes(&mut self) -> Result<bool, NotesError> {
        let events = self.worker()?.poll();
        let mut adopted = Vec::new();
        for event in events {
            let crate::persistence::Event::External(changes) = event else {
                continue;
            };
            for change in changes {
                if !self.worker()?.is_current_external(&change) {
                    continue;
                }
                let id = match &change {
                    crate::vault::External::Updated { note, .. } => &note.id,
                    crate::vault::External::Removed(note) => &note.id,
                };
                if self.session.library.changes.contains_key(id) {
                    continue;
                }
                self.revision += 1;
                self.revisions.insert(id.clone(), self.revision);
                match &change {
                    crate::vault::External::Updated { note, .. } => {
                        self.session.library.adopt(note.clone())
                    }
                    crate::vault::External::Removed(note) => self.session.library.remove(&note.id),
                }
                adopted.push(change);
            }
        }
        let changed = !adopted.is_empty();
        self.worker()?.acknowledge_changes(adopted);
        Ok(changed)
    }

    pub async fn flush(&mut self) -> Result<SaveReceipt, NotesError> {
        self.poll_changes()?;
        let saved = self
            .worker()?
            .flush_async(
                self.revision,
                self.session.library.clone(),
                Preferences::default(),
            )
            .await?;
        for (id, path) in &saved.paths {
            if let Some(note) = self
                .session
                .library
                .notes
                .iter_mut()
                .find(|note| note.id == *id)
            {
                note.path = Some(path.clone());
            }
        }
        let receipt = SaveReceipt {
            revision: saved.revision,
            saved: saved
                .changes
                .iter()
                .filter(|(id, _)| {
                    self.session.library.note(id).is_some()
                        && self.worker().is_ok_and(|worker| worker.is_persisted(id))
                })
                .map(|(id, _)| NoteId(id.clone()))
                .collect(),
            drafts: self
                .session
                .library
                .notes
                .iter()
                .filter(|note| {
                    !self
                        .worker()
                        .is_ok_and(|worker| worker.is_persisted(&note.id))
                })
                .map(|note| NoteId(note.id.clone()))
                .collect(),
            removed: if saved.outcomes.is_empty() && saved.result.is_ok() {
                self.session
                    .library
                    .deletions
                    .keys()
                    .filter(|id| !saved.kept.contains(id))
                    .cloned()
                    .map(NoteId)
                    .collect()
            } else {
                saved
                    .outcomes
                    .iter()
                    .filter(|outcome| outcome.deleted && outcome.result.is_ok())
                    .map(|outcome| outcome.id.clone())
                    .collect()
            },
            conflicts: saved.conflicts.iter().cloned().map(NoteId).collect(),
            outcomes: saved.outcomes.clone(),
        };
        for id in &saved.kept {
            self.session.library.restore_deleted(id);
        }
        self.session.library.acknowledge_saved(&saved.changes);
        match saved.result {
            Ok(()) => {
                self.last_receipt = Some(receipt.clone());
                Ok(receipt)
            }
            Err(error) => Err(NotesError::SaveFailed {
                receipt: Box::new(receipt),
                error,
            }),
        }
    }

    /// Adopt outside edits only at a clean boundary. Dirty documents remain local
    /// and the next flush uses the store's existing conflict-preservation rules.
    pub async fn refresh(&mut self) -> Result<(), NotesError> {
        self.worker()?;
        if !self.session.library.changes.is_empty() {
            return Err(NotesError::UnsavedChanges);
        }
        self.discard_local_changes_and_reload().await
    }

    /// Explicitly discard in-memory changes and adopt disk. Use only after the
    /// host has obtained the user's discard decision, or after a conflict receipt
    /// confirms the local version exists in a recovery copy.
    pub async fn discard_local_changes_and_reload(&mut self) -> Result<(), NotesError> {
        self.session.library = self.worker()?.reload_async().await?;
        self.revision += 1;
        self.revisions = self
            .session
            .library
            .notes
            .iter()
            .map(|note| (note.id.clone(), self.revision))
            .collect();
        Ok(())
    }

    /// On save failure the library remains open and can be edited or retried.
    /// On success storage has stopped and another host may open the directory.
    /// Native watch callbacks are cancelled; OS watcher cleanup can finish later.
    pub async fn close(&mut self) -> Result<SaveReceipt, NotesError> {
        if self.session.persistence.is_none() {
            return self.last_receipt.clone().ok_or(NotesError::Closed);
        }
        if !self.closing {
            self.flush().await?;
            self.closing = true;
        }
        // The worker retains the shared completion, so cancellation of this
        // future does not lose shutdown progress. Repeating close resumes it.
        self.session
            .persistence
            .as_ref()
            .expect("open while closing")
            .shutdown_async()
            .await?;
        self.session.persistence.take();
        self.last_receipt.clone().ok_or(NotesError::Closed)
    }
}

fn parse(markdown: &str) -> Result<markraft_core::Node, NotesError> {
    markraft_commonmark::SourceDocument::parse(doc::schema(), markdown)
        .map(|source| source.document().clone())
        .map_err(|error| NotesError::InvalidMarkdown(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;

    fn config(root: &std::path::Path) -> NotesConfig {
        NotesConfig::new(root.join("notes"), root.join("state"))
    }

    #[test]
    fn headless_save_never_writes_host_settings_and_close_releases_directory() {
        block_on(async {
            let root = tempfile::tempdir().unwrap();
            let cfg = config(root.path());
            std::fs::create_dir_all(&cfg.state_dir).unwrap();
            let settings = cfg.state_dir.join("settings.json");
            std::fs::write(&settings, b"host owns these bytes").unwrap();
            let mut notes = NotesLibrary::open(cfg.clone()).unwrap();
            let external = root.path().join("external.md");
            std::fs::write(&external, "External").unwrap();
            let imported = notes.open_file(external).await.unwrap();
            notes.rename(&imported, "Renamed external").await.unwrap();
            let id = notes.create("# Hello\n\nWorld").unwrap();
            let before = notes.note(&id).unwrap();
            notes
                .edit(&id, before.revision, "# Hello\n\nSaved")
                .unwrap();
            notes.close().await.unwrap();
            notes.close().await.unwrap();
            assert_eq!(std::fs::read(&settings).unwrap(), b"host owns these bytes");
            let mut reopened = NotesLibrary::open(cfg).unwrap();
            assert!(reopened.note(&id).unwrap().markdown.contains("Saved"));
            reopened.close().await.unwrap();
        });
    }

    #[test]
    fn different_host_state_directories_cannot_write_the_same_notes() {
        block_on(async {
            let root = tempfile::tempdir().unwrap();
            let mut first = NotesLibrary::open(config(root.path())).unwrap();
            let other = NotesConfig::new(root.path().join("notes"), root.path().join("other-host"));
            assert!(matches!(
                NotesLibrary::open(other.clone()),
                Err(NotesError::Storage(StoreError::Locked(_)))
            ));
            first.close().await.unwrap();
            NotesLibrary::open(other).unwrap().close().await.unwrap();
        });
    }

    #[test]
    fn search_lists_unreadable_files_without_rendering_their_contents() {
        block_on(async {
            let root = tempfile::tempdir().unwrap();
            let cfg = config(root.path());
            std::fs::create_dir_all(&cfg.notes_dir).unwrap();
            std::fs::write(cfg.notes_dir.join("invalid.md"), b"invalid\xff").unwrap();
            std::fs::write(cfg.notes_dir.join("valid.md"), "Visible").unwrap();
            let mut notes = NotesLibrary::open(cfg).unwrap();
            let results = notes.search("").unwrap();
            assert_eq!(results.len(), 2);
            assert_eq!(
                results
                    .iter()
                    .filter(|note| note.read_only.is_some())
                    .count(),
                1
            );
            let protected = results
                .iter()
                .find(|note| note.read_only.is_some())
                .unwrap();
            assert!(matches!(
                notes.delete(&protected.id, protected.revision),
                Err(NotesError::ReadOnly(_))
            ));
            let valid = results
                .iter()
                .find(|note| note.read_only.is_none())
                .unwrap();
            assert!(notes.note(&valid.id).unwrap().markdown.contains("Visible"));
            notes.close().await.unwrap();
        });
    }

    #[test]
    fn stale_view_cannot_overwrite_another_views_edit() {
        block_on(async {
            let root = tempfile::tempdir().unwrap();
            let mut notes = NotesLibrary::open(config(root.path())).unwrap();
            let id = notes.create("One").unwrap();
            let revision = notes.note(&id).unwrap().revision;
            notes.edit(&id, revision, "Two").unwrap();
            assert!(matches!(
                notes.edit(&id, revision, "Stale"),
                Err(NotesError::StaleRevision { .. })
            ));
            assert!(notes.note(&id).unwrap().markdown.contains("Two"));
            notes.close().await.unwrap();
        });
    }

    #[test]
    fn failed_close_retains_edits_and_can_be_retried() {
        block_on(async {
            let root = tempfile::tempdir().unwrap();
            let cfg = config(root.path());
            let mut notes = NotesLibrary::open(cfg.clone()).unwrap();
            let id = notes.create("Will survive failure").unwrap();
            let fault = crate::fs::faults::inject(
                &cfg.notes_dir,
                crate::fs::faults::Stage::Write,
                std::io::ErrorKind::StorageFull,
            );
            assert!(notes.close().await.is_err());
            assert!(notes.note(&id).unwrap().markdown.contains("survive"));
            drop(fault);
            notes.close().await.unwrap();
            let mut reopened = NotesLibrary::open(cfg).unwrap();
            assert!(reopened.note(&id).unwrap().markdown.contains("survive"));
            reopened.close().await.unwrap();
        });
    }

    #[test]
    fn an_unsavable_structured_edit_is_rejected_before_changing_the_session() {
        block_on(async {
            let root = tempfile::tempdir().unwrap();
            let mut notes = NotesLibrary::open(config(root.path())).unwrap();
            let id = notes
                .create_file(
                    "raw.md".into(),
                    "---\ntitle: Kept\n---\n\n<div>original</div>\n".into(),
                )
                .await
                .unwrap();
            let before = notes.note(&id).unwrap();
            let schema = doc::schema();
            // This is schema-valid, but the raw setext spelling parses back to
            // a heading whose canonical spelling differs from this raw node.
            let raw = schema
                .node(
                    markraft_commonmark::schema::RAW_BLOCK,
                    [schema.text("Heading\n=======")],
                )
                .unwrap();
            let candidate = schema.doc([raw]).unwrap();
            candidate.check(schema).unwrap();
            assert!(matches!(
                notes.edit_document(&id, before.revision, candidate),
                Err(NotesError::UnsupportedEdit)
            ));
            let after = notes.note(&id).unwrap();
            assert_eq!(after.revision, before.revision);
            assert_eq!(after.markdown, before.markdown);
            assert_eq!(after.document, before.document);
            notes
                .session
                .library
                .update_read_only(id.as_str(), Some("Protected".into()));
            assert!(matches!(
                notes.delete(&id, before.revision),
                Err(NotesError::ReadOnly(_))
            ));
            notes.close().await.unwrap();
        });
    }

    #[test]
    fn imported_markdown_keeps_front_matter_and_original_spelling() {
        block_on(async {
            let root = tempfile::tempdir().unwrap();
            let cfg = config(root.path());
            std::fs::create_dir_all(&cfg.notes_dir).unwrap();
            let path = cfg.notes_dir.join("original.md");
            let original = "---\ntitle: Example\n---\n\nHeading\n=======\n\nOriginal\n";
            std::fs::write(&path, original).unwrap();
            let mut notes = NotesLibrary::open(cfg).unwrap();
            let found = notes.search("Original").unwrap().remove(0);
            let snapshot = notes.note(&found.id).unwrap();
            notes
                .edit(
                    &snapshot.id,
                    snapshot.revision,
                    &snapshot.markdown.replace("Original", "Updated"),
                )
                .unwrap();
            notes.close().await.unwrap();
            let saved = std::fs::read_to_string(path).unwrap();
            assert!(saved.starts_with("---\ntitle: Example\n---"));
            assert!(saved.contains("Heading\n======="));
            assert!(saved.contains("Updated"));
        });
    }

    #[test]
    fn dropping_a_close_future_does_not_lose_the_save_or_lock_release() {
        block_on(async {
            use futures::FutureExt;
            let root = tempfile::tempdir().unwrap();
            let cfg = config(root.path());
            let mut notes = NotesLibrary::open(cfg.clone()).unwrap();
            let id = notes.create("Still committed").unwrap();
            // Poll once then cancel the waiting host task. The library remains
            // its lifecycle owner and can resume close without losing the receipt.
            let _ = notes.close().now_or_never();
            notes.close().await.unwrap();
            let mut reopened = NotesLibrary::open(cfg).unwrap();
            assert!(
                reopened
                    .note(&id)
                    .unwrap()
                    .markdown
                    .contains("Still committed")
            );
            reopened.close().await.unwrap();
        });
    }

    #[test]
    fn independent_libraries_keep_search_and_edits_separate() {
        block_on(async {
            let left = tempfile::tempdir().unwrap();
            let right = tempfile::tempdir().unwrap();
            let mut a = NotesLibrary::open(config(left.path())).unwrap();
            let mut b = NotesLibrary::open(config(right.path())).unwrap();
            a.create("# Private alpha").unwrap();
            b.create("# Private beta").unwrap();
            assert_eq!(a.search("alpha").unwrap().len(), 1);
            assert!(b.search("alpha").unwrap().is_empty());
            a.close().await.unwrap();
            b.close().await.unwrap();
        });
    }
}

#[cfg(test)]
#[path = "backend_tests.rs"]
mod backend_tests;
