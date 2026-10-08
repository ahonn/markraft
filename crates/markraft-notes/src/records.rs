//! Converts backend records to the shared editor engine without filesystem paths.
use crate::{
    NewRecord, NoteId, NoteSaveOutcome,
    backend::*,
    doc,
    engine::WorkerBackend,
    fs::StoreError,
    persistence::Saved,
    storage::{Library, Note, Notices, Preferences},
    vault::{External, Sources},
};
use markraft_commonmark::{SourceDocument, SourceTrack};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

#[derive(Clone)]
struct Record {
    stored: BackendNote,
    note: Note,
}
pub(crate) struct RecordStore {
    backend: Box<dyn NotesBackend>,
    accepted: HashMap<String, Record>,
    visible: HashMap<String, Record>,
    sources: Arc<Sources>,
    house: markraft_commonmark::HouseStyleHandle,
    notices: Notices,
    notify: Option<ChangeNotifier>,
}
impl RecordStore {
    pub(crate) fn open(
        backend: Box<dyn NotesBackend>,
        house: markraft_commonmark::HouseStyleHandle,
    ) -> Result<(Self, Library), StoreError> {
        let mut store = Self {
            backend,
            accepted: HashMap::new(),
            visible: HashMap::new(),
            sources: Arc::default(),
            house,
            notices: Notices::default(),
            notify: None,
        };
        let library = store.reload()?;
        Ok((store, library))
    }
    fn parse(stored: BackendNote) -> Result<(Record, Arc<SourceTrack>), StoreError> {
        if stored.id.as_str().is_empty() {
            return Err(StoreError::Backend(BackendError::Invalid(
                "Note identity cannot be empty".into(),
            )));
        }
        let source = SourceDocument::parse(doc::schema(), &stored.markdown)
            .map_err(|e| StoreError::Backend(BackendError::Invalid(e.to_string())))?;
        let note = Note {
            id: stored.id.to_string(),
            document: source.document().clone(),
            title_override: stored.title.clone(),
            logical_key: stored.logical_key.clone(),
            created_at: stored.created_at,
            updated_at: stored.updated_at,
            deleted_at: None,
            pinned: stored.pinned,
            path: None,
            read_only: None,
            conflicted: false,
        };
        Ok((Record { stored, note }, Arc::new(SourceTrack::new(source))))
    }
    fn decode(&self, stored: BackendNote) -> Result<Record, StoreError> {
        let (record, track) = Self::parse(stored)?;
        self.sources.install(&record.note, track);
        Ok(record)
    }
    fn markdown(&self, note: &Note) -> Result<String, StoreError> {
        match self.sources.source(note)? {
            Some(track) => track
                .snapshot()
                .render(doc::schema(), &note.document)
                .map_err(|e| StoreError::Backend(BackendError::Invalid(e.to_string()))),
            None => Ok(format!(
                "{}\n",
                doc::to_markdown_in(&note.document, &self.house)
            )),
        }
    }
    fn put(&mut self, note: &Note, markdown: String) -> Result<StorageRevision, BackendError> {
        let id = NoteId::new(note.id.clone());
        let previous = self.accepted.get(&note.id);
        let committed = self.backend.commit(BackendMutation::Put {
            id: id.clone(),
            expected: previous.map(|r| r.stored.revision.clone()),
            markdown: markdown.clone(),
            title: note.title_override.clone(),
            logical_key: note.logical_key.clone(),
            created_at: note.created_at,
            updated_at: note.updated_at,
            pinned: note.pinned,
        });
        let revision = match committed {
            Ok(revision) => revision,
            // A commit can succeed after its reply is lost, and the retry then
            // carries a stale expectation. Storage that already holds exactly
            // this write is the outcome the retry asked for, not a conflict.
            Err(conflict @ BackendError::Conflict { .. }) => self
                .backend
                .read(std::slice::from_ref(&id))?
                .into_iter()
                .find(|stored| {
                    stored.id == id
                        && stored.markdown == markdown
                        && stored.title == note.title_override
                        && stored.logical_key == note.logical_key
                        && stored.pinned == note.pinned
                })
                .map(|stored| stored.revision)
                .ok_or(conflict)?,
            Err(error) => return Err(error),
        };
        let stored = BackendNote {
            id,
            markdown,
            title: note.title_override.clone(),
            logical_key: note.logical_key.clone(),
            revision: revision.clone(),
            created_at: note.created_at,
            updated_at: note.updated_at,
            pinned: note.pinned,
        };
        // Keep the editor's existing track across saves so undo can still address
        // the original source. Advance durability only after the backend commits.
        let track = self.sources.source(note).ok().flatten().unwrap_or_else(|| {
            Arc::new(SourceTrack::new(
                SourceDocument::parse(doc::schema(), &stored.markdown).expect("validated Markdown"),
            ))
        });
        self.sources.install(note, track);
        let record = Record {
            stored,
            note: note.clone(),
        };
        self.accepted.insert(note.id.clone(), record.clone());
        self.visible.insert(note.id.clone(), record);
        Ok(revision)
    }
    /// Open the stored note with this ID, if storage has one. Opening an existing
    /// note does not acknowledge a remote update on behalf of a dirty mounted editor.
    fn adopt(&mut self, id: &NoteId) -> Result<Option<Note>, StoreError> {
        let stored = self
            .backend
            .read(std::slice::from_ref(id))
            .map_err(StoreError::Backend)?
            .into_iter()
            .find(|stored| &stored.id == id);
        let Some(stored) = stored else {
            return Ok(None);
        };
        if let Some(known) = self.accepted.get(id.as_str()) {
            return Ok(Some(known.note.clone()));
        }
        let record = self.decode(stored)?;
        let note = record.note.clone();
        self.accepted.insert(note.id.clone(), record.clone());
        self.visible.insert(note.id.clone(), record);
        Ok(Some(note))
    }
    /// Compare what storage returned with what the editors last saw. `scope`
    /// limits the comparison to the notes that were read; None means all notes.
    fn reconcile(
        &mut self,
        stored: Vec<BackendNote>,
        scope: Option<&[NoteId]>,
    ) -> Result<Vec<External>, StoreError> {
        let in_scope = |id: &str| scope.is_none_or(|ids| ids.iter().any(|i| i.as_str() == id));
        let mut seen = HashSet::new();
        let mut updated = Vec::new();
        for stored in stored {
            let id = stored.id.to_string();
            if !in_scope(&id) {
                continue;
            }
            if !seen.insert(id.clone()) {
                return Err(StoreError::Backend(BackendError::Invalid(
                    "Duplicate note identity".into(),
                )));
            }
            if self
                .visible
                .get(&id)
                .is_some_and(|previous| previous.stored.revision == stored.revision)
            {
                continue;
            }
            updated.push(Self::parse(stored)?);
        }
        let mut changes = Vec::new();
        for (record, track) in updated {
            self.sources.install(&record.note, track);
            changes.push(External::Updated {
                previous: self.visible.get(&record.note.id).map(|r| r.note.clone()),
                note: record.note.clone(),
            });
            self.visible.insert(record.note.id.clone(), record);
        }
        let removed: Vec<String> = self
            .visible
            .keys()
            .filter(|id| in_scope(id) && !seen.contains(*id))
            .cloned()
            .collect();
        for id in removed {
            if let Some(previous) = self.visible.remove(&id) {
                self.sources.remove(&previous.note);
                changes.push(External::Removed(previous.note));
            }
        }
        Ok(changes)
    }
}
impl WorkerBackend for RecordStore {
    fn sources(&self) -> Arc<Sources> {
        self.sources.clone()
    }
    fn house(&self) -> markraft_commonmark::HouseStyleHandle {
        self.house.clone()
    }
    fn notices(&self) -> Notices {
        self.notices.clone()
    }
    fn capabilities(&self) -> BackendCapabilities {
        self.backend.capabilities()
    }
    fn persisted(&self) -> Vec<(String, StorageRevision)> {
        self.accepted
            .iter()
            .map(|(id, record)| (id.clone(), record.stored.revision.clone()))
            .collect()
    }
    fn set_change_notifier(&mut self, notify: ChangeNotifier) {
        self.notify = Some(notify.clone());
        self.backend.set_change_notifier(notify)
    }
    fn reload(&mut self) -> Result<Library, StoreError> {
        let snapshot = self.backend.load().map_err(StoreError::Backend)?;
        let mut records = HashMap::new();
        let mut tracks = Vec::new();
        for stored in snapshot.notes {
            if records.contains_key(stored.id.as_str()) {
                return Err(StoreError::Backend(BackendError::Invalid(
                    "Duplicate note identity".into(),
                )));
            }
            let (record, track) = Self::parse(stored)?;
            tracks.push((record.note.clone(), track));
            records.insert(record.note.id.clone(), record);
        }
        let mut library = Library::default();
        library.notes.clear();
        library.changes.clear();
        library.workspace = snapshot.workspace;
        library.notes = records.values().map(|r| r.note.clone()).collect();
        library.notes.sort_by(|a, b| a.id.cmp(&b.id));
        library.active_id = snapshot
            .active_id
            .map(|id| id.to_string())
            .filter(|id| records.contains_key(id))
            .or_else(|| library.notes.first().map(|n| n.id.clone()))
            .unwrap_or_default();
        if library.notes.is_empty() {
            library.new_note(doc::empty());
        }
        library.validate()?;
        self.sources.clear();
        for (note, track) in tracks {
            self.sources.install(&note, track);
        }
        self.accepted = records.clone();
        self.visible = records;
        Ok(library)
    }
    fn save(&mut self, revision: u64, library: &Library, _preferences: &Preferences) -> Saved {
        if let Err(error) = library.validate() {
            return Saved {
                revision,
                changes: Vec::new(),
                result: Err(error),
                paths: Vec::new(),
                conflicts: Vec::new(),
                trashed: Vec::new(),
                kept: Vec::new(),
                outcomes: Vec::new(),
            };
        }
        let mut changes = Vec::new();
        let mut errors = Vec::new();
        let mut conflicts = Vec::new();
        let mut conflict_titles = Vec::new();
        let mut outcomes = Vec::new();
        for (id, generation) in &library.changes {
            let deleted = library.deletions.get(id);
            let result = if let Some(note) = deleted {
                if let Some(previous) = self.accepted.get(id) {
                    self.backend
                        .commit(BackendMutation::Delete {
                            id: NoteId::new(id.clone()),
                            expected: previous.stored.revision.clone(),
                            deleted_at: note.deleted_at.unwrap_or_else(crate::storage::timestamp),
                        })
                        .inspect(|_| {
                            self.accepted.remove(id);
                            self.visible.remove(id);
                            self.sources.remove(note);
                        })
                } else {
                    changes.push((id.clone(), *generation));
                    continue;
                }
            } else if let Some(note) = library.note(id) {
                if note.document_is_empty()
                    && note.title_override.is_none()
                    && note.logical_key.is_none()
                    && !self.accepted.contains_key(id)
                {
                    changes.push((id.clone(), *generation));
                    continue;
                }
                if note.read_only.is_some() {
                    Err(BackendError::Invalid("Cannot save a read-only note".into()))
                } else {
                    self.markdown(note)
                        .map_err(|e| BackendError::Invalid(e.to_string()))
                        .and_then(|markdown| self.put(note, markdown))
                }
            } else {
                continue;
            };
            match &result {
                Ok(_) => changes.push((id.clone(), *generation)),
                // The refresh that follows keeps the local edits as a copy, so a
                // conflict is not a failure of the store.
                Err(BackendError::Conflict { .. }) => {
                    conflicts.push(id.clone());
                    conflict_titles.extend(
                        deleted
                            .or_else(|| library.note(id))
                            .map(Note::title_message),
                    );
                }
                Err(error) => errors.push(StoreError::Backend(error.clone())),
            }
            outcomes.push(NoteSaveOutcome {
                id: NoteId::new(id.clone()),
                result,
                deleted: deleted.is_some(),
            });
        }
        if let Err(error) = self.backend.save_workspace(
            Some(&NoteId::new(library.active_id.clone())),
            &library.workspace,
        ) {
            errors.push(StoreError::Backend(error));
        }
        if !conflict_titles.is_empty() {
            errors.push(StoreError::Conflict(conflict_titles));
        }
        Saved {
            revision,
            changes,
            result: if errors.is_empty() {
                Ok(())
            } else {
                Err(StoreError::several(errors))
            },
            paths: Vec::new(),
            conflicts,
            trashed: Vec::new(),
            kept: Vec::new(),
            outcomes,
        }
    }
    fn refresh(&mut self) -> Result<Vec<External>, StoreError> {
        let snapshot = self.backend.load().map_err(StoreError::Backend)?;
        self.reconcile(snapshot.notes, None)
    }
    fn refresh_notes(&mut self, ids: &[NoteId]) -> Result<Vec<External>, StoreError> {
        let notes = self.backend.read(ids).map_err(StoreError::Backend)?;
        self.reconcile(notes, Some(ids))
    }
    fn acknowledge_changes(&mut self, changes: &[External]) {
        for change in changes {
            if !self.sources.is_current_external(change) {
                continue;
            }
            match change {
                External::Updated { note, .. } => {
                    if let Some(record) = self.visible.get(&note.id) {
                        self.accepted.insert(note.id.clone(), record.clone());
                        self.sources.accept_current(&note.id);
                    }
                }
                External::Removed(note) => {
                    self.accepted.remove(&note.id);
                }
            }
        }
    }
    fn recover(&mut self, note: &Note) -> Result<(), StoreError> {
        let markdown = self.markdown(note)?;
        let copy = self.create_record(NewRecord {
            markdown,
            title: Some(crate::vault::conflicted_copy_name(&note.title())),
            logical_key: None,
        })?;
        // The next refresh publishes the durable copy to every mounted editor.
        self.visible.remove(&copy.id);
        if let Some(notify) = &self.notify {
            notify.changed(vec![NoteId::new(copy.id)]);
        }
        Ok(())
    }
    fn add_file(&mut self, path: std::path::PathBuf) -> Result<Note, StoreError> {
        use std::io::Read;
        const MAX_IMPORT_BYTES: u64 = 64 * 1024 * 1024;
        let file = std::fs::File::open(&path).map_err(|e| crate::fs::describe(&path, &e))?;
        let mut bytes = Vec::new();
        file.take(MAX_IMPORT_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| crate::fs::describe(&path, &e))?;
        if bytes.len() as u64 > MAX_IMPORT_BYTES {
            return Err(StoreError::Backend(BackendError::Invalid(
                "Markdown import exceeds 64 MiB".into(),
            )));
        }
        let markdown = String::from_utf8(bytes).map_err(|_| {
            StoreError::Backend(BackendError::Invalid(
                "Markdown import must be UTF-8".into(),
            ))
        })?;
        let source = SourceDocument::parse(doc::schema(), &markdown)
            .map_err(|e| StoreError::Backend(BackendError::Invalid(e.to_string())))?;
        let portable = |url: &str| {
            url.starts_with('#')
                || url.starts_with("https://")
                || url.starts_with("http://")
                || url.starts_with("mailto:")
                || url.starts_with("markraft-asset:")
                || url.starts_with("data:")
        };
        let mut relative = false;
        source.document().descendants(&mut |node, _, _, _| {
            if let Some(url) = node.attrs().get("src").and_then(|v| v.as_str()) {
                relative |= !portable(url);
            }
            for mark in node.marks().iter() {
                if let Some(url) = mark.attrs.get("href").and_then(|v| v.as_str()) {
                    relative |= !portable(url);
                }
            }
            true
        });
        if relative {
            return Err(crate::engine::unsupported(
                "importing relative file resources into a database; import attachments first",
            ));
        }
        self.create_record(NewRecord {
            markdown,
            title: path
                .file_stem()
                .map(|name| name.to_string_lossy().into_owned()),
            logical_key: None,
        })
    }
    fn create_record(&mut self, request: NewRecord) -> Result<Note, StoreError> {
        let id = match &request.logical_key {
            Some(key) => {
                let id = NoteId::for_logical_key(key);
                if let Some(existing) = self.adopt(&id)? {
                    return Ok(existing);
                }
                id
            }
            None => NoteId::new(uuid::Uuid::new_v4().to_string()),
        };
        let now = crate::storage::timestamp();
        let source = SourceDocument::parse(doc::schema(), &request.markdown)
            .map_err(|e| StoreError::Backend(BackendError::Invalid(e.to_string())))?;
        let note = Note {
            id: id.to_string(),
            document: source.document().clone(),
            title_override: request.title,
            logical_key: request.logical_key,
            created_at: now,
            updated_at: now,
            deleted_at: None,
            pinned: false,
            path: None,
            read_only: None,
            conflicted: false,
        };
        self.sources
            .install(&note, Arc::new(SourceTrack::new(source)));
        match self.put(&note, request.markdown) {
            Ok(_) => Ok(note),
            // Another writer created the logical note first. Open that note.
            Err(conflict @ BackendError::Conflict { .. }) if note.logical_key.is_some() => {
                self.adopt(&id)?.ok_or(StoreError::Backend(conflict))
            }
            Err(error) => Err(StoreError::Backend(error)),
        }
    }
    fn rename_record(&mut self, id: &str, name: &str) -> Result<Note, StoreError> {
        let mut note = self
            .accepted
            .get(id)
            .ok_or_else(|| {
                StoreError::Backend(BackendError::Invalid("Note does not exist".into()))
            })?
            .note
            .clone();
        note.title_override = Some(name.to_owned());
        note.updated_at = crate::storage::timestamp();
        let markdown = self.markdown(&note)?;
        self.put(&note, markdown).map_err(StoreError::Backend)?;
        Ok(note)
    }
    fn read_asset(&mut self, id: &AssetId) -> Result<Asset, StoreError> {
        let asset = self.backend.read_asset(id).map_err(StoreError::Backend)?;
        if &AssetId::for_content(&asset.bytes) != id {
            return Err(StoreError::Backend(BackendError::Invalid(
                "Stored asset does not match its ID".into(),
            )));
        }
        Ok(asset)
    }
    fn write_asset(&mut self, asset: Asset) -> Result<(), StoreError> {
        if AssetId::for_content(&asset.bytes) != asset.id {
            return Err(StoreError::Backend(BackendError::Invalid(
                "An asset ID must be the SHA-256 of its content".into(),
            )));
        }
        self.backend.write_asset(asset).map_err(StoreError::Backend)
    }
}
