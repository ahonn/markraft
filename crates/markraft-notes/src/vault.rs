//! The notes store: one copy of the rules for saving, for conflicts, and for changes
//! made outside the application, over any [`NotesBackend`]. A folder of Markdown
//! files is one backend; a host supplies the other kind.
use crate::locale::Message;
use crate::{
    Asset, AssetId, BackendCapabilities, BackendError, BackendMutation, BackendNote,
    BackendSnapshot, ChangeNotifier, NewRecord, NoteId, NoteSaveOutcome, NotesBackend,
    StorageRevision,
    directory::{MarkdownDirectory, ends_with_newline},
    doc,
    fs::StoreError,
    storage::{Library, Note, Notices, Preferences, Settings, WorkspaceSettings},
};
use markraft_commonmark::{SourceDocument, SourceTrack};
use std::{
    collections::{HashMap, HashSet},
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
};

/// One version of a note as storage holds it.
#[derive(Clone)]
struct Saved {
    /// What storage calls this version. A write names the one it replaces.
    revision: StorageRevision,
    /// The Markdown as storage holds it.
    text: String,
    /// The text as first read, and as the editor last wrote it: kept for the
    /// whole session, across saves and undo, and shared with the note's
    /// editor so that what it takes on a keystroke is what a save writes.
    /// `None` for a file that is not Markdown this store can read.
    track: Option<Arc<SourceTrack>>,
    note: Note,
}
#[derive(Clone, Debug, PartialEq)]
pub enum External {
    Updated { previous: Option<Note>, note: Note },
    Removed(Note),
}
/// Read-only editor baselines. The lock protects Arc lookups only; rendering and
/// parsing always happen after it is released.
#[derive(Default)]
pub(crate) struct Sources(Mutex<HashMap<String, SourceVersions>>);
#[derive(Clone)]
struct SourceVersion {
    document: markraft_core::Node,
    updated_at: u64,
    path: Option<PathBuf>,
    read_only: Option<Message>,
    track: Option<Arc<SourceTrack>>,
}
#[derive(Default)]
struct SourceVersions {
    current: Option<SourceVersion>,
    previous: Option<SourceVersion>,
    history: Vec<SourceVersion>,
    removed: Option<Note>,
}
impl Sources {
    pub(crate) fn install(&self, note: &Note, track: Arc<SourceTrack>) {
        let mut entries = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let versions = entries.entry(note.id.clone()).or_default();
        if let Some(current) = versions.current.take()
            && current
                .track
                .as_ref()
                .is_none_or(|old| !Arc::ptr_eq(old, &track))
        {
            if versions.previous.is_none() {
                versions.previous = Some(current.clone());
            }
            versions.history.push(current);
        }
        versions.removed = None;
        versions.current = Some(SourceVersion {
            document: note.document.clone(),
            updated_at: note.updated_at,
            path: note.path.clone(),
            read_only: note.read_only.clone(),
            track: Some(track),
        });
    }
    pub(crate) fn is_current_external(&self, change: &External) -> bool {
        let entries = self.0.lock().unwrap_or_else(|e| e.into_inner());
        match change {
            External::Updated { note, .. } => entries
                .get(&note.id)
                .and_then(|versions| versions.current.as_ref())
                .is_some_and(|version| {
                    version.document.ptr_eq(&note.document)
                        && version.updated_at == note.updated_at
                        && version.path == note.path
                        && version.read_only == note.read_only
                }),
            External::Removed(note) => entries.get(&note.id).is_some_and(|versions| {
                versions.current.is_none()
                    && versions
                        .removed
                        .as_ref()
                        .is_some_and(|removed| same_external_version(removed, note))
            }),
        }
    }

    pub(crate) fn source(&self, note: &Note) -> Result<Option<Arc<SourceTrack>>, StoreError> {
        let entries = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let baseline = entries.get(&note.id).and_then(|versions| {
            let exact = |version: &&SourceVersion| {
                version.document.ptr_eq(&note.document) && version.updated_at == note.updated_at
            };
            versions
                .current
                .as_ref()
                .filter(exact)
                .or_else(|| versions.history.iter().find(exact))
                .or_else(|| {
                    versions
                        .current
                        .as_ref()
                        .filter(|version| version.document == note.document)
                })
                .or_else(|| {
                    versions
                        .history
                        .iter()
                        .find(|version| version.document == note.document)
                })
                .or(versions.previous.as_ref())
                .or(versions.current.as_ref())
        });
        match baseline {
            Some(version) => version
                .track
                .clone()
                .map(Some)
                .ok_or_else(|| Message::new("error.unreadable-markdown").into()),
            None => Ok(None),
        }
    }
}

fn same_external_version(left: &Note, right: &Note) -> bool {
    left.id == right.id
        && left.document.ptr_eq(&right.document)
        && left.path == right.path
        && left.updated_at == right.updated_at
        && left.read_only == right.read_only
}

/// Whether two versions of a note say different things. A pin is where the
/// note sits in a list, not what it says, and never makes two versions conflict.
fn differs(left: &Note, right: &Note) -> bool {
    left.document != right.document
        || left.title_override != right.title_override
        || left.logical_key != right.logical_key
}

fn unsupported(operation: &'static str) -> StoreError {
    StoreError::Backend(BackendError::Unsupported(operation))
}
fn invalid(reason: &str) -> StoreError {
    StoreError::Backend(BackendError::Invalid(reason.into()))
}

/// Where the notes are kept. The store saves, refreshes and resolves conflicts
/// through [`NotesBackend`] alone. What only a file has, such as a path, a move to
/// another name and the Trash, it asks of the folder directly.
enum Storage {
    Directory(Box<MarkdownDirectory>),
    Host(Box<dyn NotesBackend>),
}
impl Storage {
    fn directory(&self) -> Option<&MarkdownDirectory> {
        match self {
            Self::Directory(directory) => Some(directory),
            Self::Host(_) => None,
        }
    }
    fn directory_mut(&mut self) -> Option<&mut MarkdownDirectory> {
        match self {
            Self::Directory(directory) => Some(directory),
            Self::Host(_) => None,
        }
    }
    fn capabilities(&self) -> BackendCapabilities {
        match self {
            Self::Directory(directory) => directory.capabilities(),
            Self::Host(backend) => backend.capabilities(),
        }
    }
    // The folder reports a failure in the reader's language and names the file.
    // A host backend reports what its own error says.
    fn load(&mut self) -> Result<BackendSnapshot, StoreError> {
        match self {
            Self::Directory(directory) => directory.snapshot(),
            Self::Host(backend) => backend.load().map_err(StoreError::Backend),
        }
    }
    fn read(&mut self, ids: &[NoteId]) -> Result<Vec<BackendNote>, StoreError> {
        match self {
            Self::Directory(directory) => directory.read_notes(ids),
            Self::Host(backend) => backend.read(ids).map_err(StoreError::Backend),
        }
    }
    fn commit(&mut self, mutation: BackendMutation) -> Result<StorageRevision, StoreError> {
        match self {
            Self::Directory(directory) => directory.commit_note(mutation),
            Self::Host(backend) => backend.commit(mutation).map_err(StoreError::Backend),
        }
    }
    fn save_workspace(
        &mut self,
        active_id: &NoteId,
        workspace: &WorkspaceSettings,
    ) -> Result<(), StoreError> {
        match self {
            Self::Directory(directory) => directory.save_folder_state(Some(active_id), workspace),
            Self::Host(backend) => backend
                .save_workspace(Some(active_id), workspace)
                .map_err(StoreError::Backend),
        }
    }
}

pub struct Store {
    storage: Storage,
    /// Every note as storage holds it now.
    files: HashMap<String, Saved>,
    /// Notes that storage changed and no editor has taken yet. Nothing is written
    /// over them until the change is acknowledged.
    pending: HashSet<String>,
    /// What the editors last saw of a pending note: the version their edits were made against.
    previous: HashMap<String, Saved>,
    removed: HashMap<String, Note>,
    /// Notes whose local edits were kept as a conflicted copy because storage won.
    disk_won: HashSet<String>,
    /// Where the last save's deletions landed in the Trash, for the window to reveal.
    trashed: Vec<PathBuf>,
    /// Notes the last save was asked to delete and left in place.
    kept: Vec<String>,
    sources: Arc<Sources>,
    notices: Notices,
    /// The style new Markdown is spelled in; the application hands over its own
    /// through [`Store::set_house`], so a saved note follows the preferences the
    /// editor does.
    house: markraft_commonmark::HouseStyleHandle,
    /// Asks the worker to read named notes again.
    notify: Option<ChangeNotifier>,
    /// The local versions already kept as a copy in a host backend, by note and
    /// text. One conflict seen twice leaves one copy.
    copies: HashSet<(String, String)>,
}
impl Store {
    /// The revision each note's editors last saw, for every note that storage holds.
    pub(crate) fn storage_revisions(&self) -> Vec<(String, StorageRevision)> {
        self.files
            .keys()
            .chain(self.previous.keys())
            .collect::<HashSet<_>>()
            .into_iter()
            .filter_map(|id| Some((id.clone(), self.baseline_of(id)?.revision.clone())))
            .collect()
    }
    /// Open the notes in `directory`, keeping the folder's state under the
    /// application's settings folder. `settings` is the settings file as the
    /// caller read it — the store keeps the copy it writes `settings.json` from.
    pub fn open(
        directory: PathBuf,
        settings_path: PathBuf,
        settings: Settings,
    ) -> Result<(Self, Library), StoreError> {
        let directory = MarkdownDirectory::open(directory, settings_path, settings)?;
        Self::start(Storage::Directory(Box::new(directory)), Default::default())
    }
    /// Open a headless library without reading or writing a host settings file.
    pub fn open_library(
        directory: PathBuf,
        state_directory: PathBuf,
    ) -> Result<(Self, Library), StoreError> {
        let directory = MarkdownDirectory::open(
            directory,
            state_directory.join("settings.json"),
            Settings::default(),
        )?
        .without_settings();
        Self::start(Storage::Directory(Box::new(directory)), Default::default())
    }
    /// Open the notes that a host keeps in its own storage.
    pub(crate) fn from_backend(
        backend: Box<dyn NotesBackend>,
        house: markraft_commonmark::HouseStyleHandle,
    ) -> Result<(Self, Library), StoreError> {
        Self::start(Storage::Host(backend), house)
    }
    fn start(
        storage: Storage,
        house: markraft_commonmark::HouseStyleHandle,
    ) -> Result<(Self, Library), StoreError> {
        let notices = storage
            .directory()
            .map(MarkdownDirectory::notices)
            .unwrap_or_default();
        let mut store = Self {
            storage,
            files: HashMap::new(),
            pending: HashSet::new(),
            previous: HashMap::new(),
            removed: HashMap::new(),
            disk_won: HashSet::new(),
            trashed: Vec::new(),
            kept: Vec::new(),
            sources: Arc::default(),
            notices,
            house,
            notify: None,
            copies: HashSet::new(),
        };
        let library = store.scan_library()?;
        Ok((store, library))
    }

    pub fn notices(&self) -> Notices {
        self.notices.clone()
    }
    #[cfg(test)]
    fn state(&self) -> &Path {
        self.storage.directory().expect("a notes folder").state()
    }
    #[cfg(test)]
    fn remembers_no_paths(&self) -> bool {
        self.storage
            .directory()
            .expect("a notes folder")
            .remembers_no_paths()
    }
    /// The notes folder, when the notes are files.
    pub fn directory(&self) -> Option<&Path> {
        self.storage.directory().map(MarkdownDirectory::directory)
    }
    pub fn extra_watch_directories(&self) -> Vec<PathBuf> {
        self.storage
            .directory()
            .map(MarkdownDirectory::extra_watch_directories)
            .unwrap_or_default()
    }
    pub(crate) fn capabilities(&self) -> BackendCapabilities {
        self.storage.capabilities()
    }
    pub(crate) fn set_change_notifier(&mut self, notify: ChangeNotifier) {
        self.notify = Some(notify.clone());
        if let Storage::Host(backend) = &mut self.storage {
            backend.set_change_notifier(notify);
        }
    }
    /// Notes whose local edits were kept as a conflicted copy because storage won.
    pub fn conflicts(&self) -> Vec<String> {
        self.disk_won.iter().cloned().collect()
    }
    /// Where the last save's deletions landed in the Trash. Empty when the platform
    /// did not say, which is not a failure: the file is still gone.
    pub fn trashed(&self) -> Vec<PathBuf> {
        self.trashed.clone()
    }
    /// Notes the last save was asked to delete and left in place: storage holds a
    /// version nobody here has seen, or refused the deletion. Storage still holds
    /// the note, so the deletion is dropped rather than retried.
    pub fn kept(&self) -> Vec<String> {
        self.kept.clone()
    }
    pub fn paths(&self) -> Vec<(String, PathBuf)> {
        self.storage
            .directory()
            .map(MarkdownDirectory::paths)
            .unwrap_or_default()
    }
    pub(crate) fn source_cache(&self) -> Arc<Sources> {
        self.sources.clone()
    }
    pub(crate) fn house(&self) -> markraft_commonmark::HouseStyleHandle {
        self.house.clone()
    }
    /// The version of a note that its editors last saw. While a change that storage
    /// made is pending, that is the version before the change.
    fn baseline_of(&self, id: &str) -> Option<&Saved> {
        self.previous.get(id).or_else(|| self.files.get(id))
    }
    fn update_source(&self, id: &str) {
        let version = |saved: &Saved| SourceVersion {
            document: saved.note.document.clone(),
            updated_at: saved.note.updated_at,
            path: saved.note.path.clone(),
            read_only: saved.note.read_only.clone(),
            track: saved.track.clone(),
        };
        let current = self.files.get(id).map(version);
        let previous = self.previous.get(id).map(version);
        let removed = self.removed.get(id).cloned();
        let mut entries = self.sources.0.lock().unwrap_or_else(|e| e.into_inner());
        let mut history = Vec::new();
        if self.pending.contains(id)
            && let Some(old) = entries.remove(id)
        {
            history = old.history;
            if let Some(old) = old.current {
                let same_track =
                    current
                        .as_ref()
                        .is_some_and(|current| match (&current.track, &old.track) {
                            (Some(current), Some(old)) => Arc::ptr_eq(current, old),
                            (None, None) => current.document == old.document,
                            _ => false,
                        });
                if !same_track {
                    history.push(old);
                }
            }
        }
        if current.is_none() && previous.is_none() {
            entries.remove(id);
        } else {
            entries.insert(
                id.to_owned(),
                SourceVersions {
                    current,
                    previous,
                    history,
                    removed,
                },
            );
        }
    }
    fn rebuild_sources(&self) {
        self.sources
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        for id in self.files.keys().chain(self.previous.keys()) {
            self.update_source(id);
        }
    }
    /// A note as storage returned it. An unchanged version is the one already
    /// known: its editors keep the document and the track they hold.
    fn decode(&self, stored: BackendNote, known: Option<&Saved>) -> Result<Saved, StoreError> {
        let id = stored.id.to_string();
        if id.is_empty() {
            return Err(invalid("Note identity cannot be empty"));
        }
        let (path, unwritable) = match self.storage.directory() {
            Some(directory) => (directory.path(&id), directory.read_only(&id)),
            None => (None, None),
        };
        if let Some(known) = known
            && known.revision == stored.revision
        {
            let mut saved = known.clone();
            let unparsed = known
                .note
                .read_only
                .as_ref()
                .filter(|reason| reason.is_key("error.markdown-parse"))
                .cloned();
            saved.note.read_only = unwritable.or(unparsed);
            if path.is_some() {
                saved.note.path = path;
            }
            return Ok(saved);
        }
        let unreadable = unwritable
            .as_ref()
            .is_some_and(|reason| reason.is_key("error.not-utf8"));
        let mut read_only = unwritable;
        let mut track = None;
        let document = if unreadable {
            doc::empty()
        } else {
            match SourceDocument::parse(doc::schema(), &stored.markdown) {
                Ok(source) => {
                    let document = source.document().clone();
                    track = Some(Arc::new(SourceTrack::new(source)));
                    document
                }
                Err(error) => {
                    read_only =
                        Some(Message::new("error.markdown-parse").arg("detail", error.to_string()));
                    doc::empty()
                }
            }
        };
        let note = Note {
            id,
            document,
            title_override: stored.title,
            logical_key: stored.logical_key,
            created_at: stored.created_at,
            updated_at: stored.updated_at,
            deleted_at: None,
            pinned: stored.pinned,
            path,
            read_only,
            conflicted: false,
        };
        Ok(Saved {
            revision: stored.revision,
            text: stored.markdown,
            track,
            note,
        })
    }
    fn decode_all(&self, stored: Vec<BackendNote>) -> Result<HashMap<String, Saved>, StoreError> {
        let mut files = HashMap::new();
        for stored in stored {
            let known = self.files.get(stored.id.as_str());
            let saved = self.decode(stored, known)?;
            if files.insert(saved.note.id.clone(), saved).is_some() {
                return Err(invalid("Duplicate note identity"));
            }
        }
        Ok(files)
    }
    /// The note that storage holds under `id` now.
    fn read_one(&mut self, id: &str, known: Option<&Saved>) -> Result<Option<Saved>, StoreError> {
        let id = NoteId::new(id);
        let stored = self
            .storage
            .read(std::slice::from_ref(&id))?
            .into_iter()
            .find(|stored| stored.id == id);
        stored.map(|stored| self.decode(stored, known)).transpose()
    }
    pub fn reload(&mut self) -> Result<Library, StoreError> {
        let library = self.scan_library()?;
        if let Some(directory) = self.storage.directory() {
            directory.drop_recovery_drafts();
        }
        // Nothing after adopting the new baseline can turn this into an error:
        // the UI must receive the library that the store will now save against.
        Ok(library)
    }
    fn scan_library(&mut self) -> Result<Library, StoreError> {
        let snapshot = self.storage.load()?;
        let files = self.decode_all(snapshot.notes)?;
        let mut library = Library {
            notes: files.values().map(|s| s.note.clone()).collect(),
            workspace: snapshot.workspace,
            ..Library::default()
        };
        library.notes.sort_by(|a, b| a.id.cmp(&b.id));
        if library.notes.is_empty() {
            library.new_note(doc::empty());
        }
        library.active_id = snapshot
            .active_id
            .map(|id| id.to_string())
            .filter(|id| library.note(id).is_some())
            .unwrap_or_else(|| library.notes[0].id.clone());
        library.validate()?;
        library.changes.clear();
        library.deletions.clear();
        // Only now does the store take the new baseline: a load that failed above
        // leaves the one the editors are working against.
        self.files = files;
        self.pending.clear();
        self.previous.clear();
        self.removed.clear();
        self.disk_won.clear();
        if let Some(directory) = self.storage.directory_mut() {
            directory.forget_missing();
        }
        self.rebuild_sources();
        Ok(library)
    }
    /// Give a note's file another name, in the folder it is already in.
    pub fn rename(&mut self, id: &str, name: &str) -> Result<PathBuf, StoreError> {
        if self.storage.directory().is_none() {
            return Err(unsupported("file rename"));
        }
        let saved = self
            .files
            .get(id)
            .ok_or(Message::new("error.rename-unsaved"))?
            .clone();
        if self.pending.contains(id) {
            return Err(Message::new("error.rename-pending").into());
        }
        let Some(directory) = self.storage.directory_mut() else {
            return Err(unsupported("file rename"));
        };
        let target = directory.rename(id, name, &saved.revision, saved.note.title_message())?;
        if let Some(current) = self.files.get_mut(id) {
            current.note.path = Some(target.clone());
        }
        Ok(target)
    }
    /// Give a note another name: its file's name in a folder, and the title its
    /// host keeps for it elsewhere. The Markdown is not rewritten.
    pub(crate) fn rename_note(&mut self, id: &str, name: &str) -> Result<Note, StoreError> {
        if self.storage.directory().is_some() {
            self.rename(id, name)?;
            return self
                .files
                .get(id)
                .map(|saved| saved.note.clone())
                .ok_or_else(|| invalid("Note does not exist"));
        }
        let base = self
            .baseline_of(id)
            .ok_or_else(|| invalid("Note does not exist"))?
            .clone();
        let mut note = base.note.clone();
        note.title_override = Some(name.to_owned());
        note.updated_at = crate::storage::timestamp();
        let revision = self.put(&note, &base.text, Some(&base.revision))?;
        self.files.insert(
            id.to_owned(),
            Saved {
                revision,
                note: note.clone(),
                ..base
            },
        );
        self.update_source(id);
        Ok(note)
    }
    /// Open the stored note with this ID, if storage has one. Opening an existing
    /// note does not acknowledge a remote update on behalf of a dirty mounted editor.
    fn adopt(&mut self, id: &NoteId) -> Result<Option<Note>, StoreError> {
        let Some(stored) = self
            .storage
            .read(std::slice::from_ref(id))?
            .into_iter()
            .find(|stored| &stored.id == id)
        else {
            return Ok(None);
        };
        if let Some(known) = self.baseline_of(id.as_str()) {
            return Ok(Some(known.note.clone()));
        }
        let saved = self.decode(stored, None)?;
        let note = saved.note.clone();
        self.files.insert(note.id.clone(), saved);
        self.update_source(&note.id);
        Ok(Some(note))
    }
    /// Take a Markdown file as a note. A folder opens the file where it is. A
    /// host backend keeps a copy of its text.
    pub fn add_file(&mut self, path: PathBuf) -> Result<Note, StoreError> {
        match &mut self.storage {
            Storage::Directory(directory) => {
                let id = directory.add_loose(path)?;
                self.adopt(&id)?
                    .ok_or_else(|| Message::new("error.file-gone").into())
            }
            Storage::Host(_) => self.import(&path),
        }
    }
    fn import(&mut self, path: &Path) -> Result<Note, StoreError> {
        use std::io::Read;
        const MAX_IMPORT_BYTES: u64 = 64 * 1024 * 1024;
        let file = std::fs::File::open(path).map_err(|e| crate::fs::describe(path, &e))?;
        let mut bytes = Vec::new();
        file.take(MAX_IMPORT_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| crate::fs::describe(path, &e))?;
        if bytes.len() as u64 > MAX_IMPORT_BYTES {
            return Err(invalid("Markdown import exceeds 64 MiB"));
        }
        let markdown =
            String::from_utf8(bytes).map_err(|_| invalid("Markdown import must be UTF-8"))?;
        let source =
            SourceDocument::parse(doc::schema(), &markdown).map_err(|e| invalid(&e.to_string()))?;
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
            return Err(unsupported(
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
    /// The text of the file at `relative` in the notes folder, or `None` when there is
    /// no such readable text file there.
    pub fn read_text(&self, relative: &Path) -> Option<String> {
        self.storage.directory()?.read_text(relative)
    }
    /// Make the note at `relative` with `contents`, or take the one already there.
    ///
    /// The file is written without replacing anything, so a note another program made
    /// at that path first — or makes while this runs — is the one returned, as it is.
    /// It is tracked before this returns, so the watcher reading it afterwards finds
    /// nothing new.
    pub fn create_note(&mut self, relative: &Path, contents: &str) -> Result<Note, StoreError> {
        let Some(directory) = self.storage.directory_mut() else {
            return Err(unsupported("file creation"));
        };
        let id = directory.create_at(relative, contents)?;
        self.adopt(&id)?
            .ok_or_else(|| Message::new("error.file-gone").into())
    }
    /// Make a note in a host backend, or open the one that already has its logical key.
    pub(crate) fn create_record(&mut self, request: NewRecord) -> Result<Note, StoreError> {
        if self.storage.directory().is_some() {
            return Err(unsupported("logical note creation"));
        }
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
            .map_err(|e| invalid(&e.to_string()))?;
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
        match self.put(&note, &request.markdown, None) {
            Ok(revision) => {
                self.files.insert(
                    note.id.clone(),
                    Saved {
                        revision,
                        text: request.markdown,
                        track: Some(Arc::new(SourceTrack::new(source))),
                        note: note.clone(),
                    },
                );
                self.update_source(&note.id);
                Ok(note)
            }
            // Another writer created the logical note first. Open that note.
            Err(conflict @ StoreError::Backend(BackendError::Conflict { .. }))
                if note.logical_key.is_some() =>
            {
                self.adopt(&id)?.ok_or(conflict)
            }
            Err(error) => Err(error),
        }
    }
    pub(crate) fn read_asset(&mut self, id: &AssetId) -> Result<Asset, StoreError> {
        let Storage::Host(backend) = &mut self.storage else {
            return Err(unsupported("attachments"));
        };
        let asset = backend.read_asset(id).map_err(StoreError::Backend)?;
        if &AssetId::for_content(&asset.bytes) != id {
            return Err(invalid("Stored asset does not match its ID"));
        }
        Ok(asset)
    }
    pub(crate) fn write_asset(&mut self, asset: Asset) -> Result<(), StoreError> {
        let Storage::Host(backend) = &mut self.storage else {
            return Err(unsupported("attachments"));
        };
        if AssetId::for_content(&asset.bytes) != asset.id {
            return Err(invalid("An asset ID must be the SHA-256 of its content"));
        }
        backend.write_asset(asset).map_err(StoreError::Backend)
    }
    /// Read every note again and report what storage changed.
    pub fn refresh(&mut self) -> Result<Vec<External>, StoreError> {
        let stored = self.storage.load()?.notes;
        let found = self.decode_all(stored)?;
        Ok(self.reconcile(found, None))
    }
    /// Read only these notes again. A note that storage no longer returns has left.
    pub(crate) fn refresh_notes(&mut self, ids: &[NoteId]) -> Result<Vec<External>, StoreError> {
        let mut stored = self.storage.read(ids)?;
        stored.retain(|stored| ids.contains(&stored.id));
        let found = self.decode_all(stored)?;
        Ok(self.reconcile(found, Some(ids)))
    }
    /// File events only read affected paths. Directory events and explicit refreshes rescan.
    pub fn refresh_paths(&mut self, paths: &[PathBuf]) -> Result<Vec<External>, StoreError> {
        let Some(directory) = self.storage.directory_mut() else {
            return self.refresh();
        };
        let ids = directory.ids_for_paths(paths)?;
        self.refresh_notes(&ids)
    }
    /// Compare what storage returned with what it held at the last read. `scope`
    /// limits the comparison to the notes that were read; `None` means all notes.
    /// Each difference is held back from saves until an editor acknowledges it.
    fn reconcile(
        &mut self,
        found: HashMap<String, Saved>,
        scope: Option<&[NoteId]>,
    ) -> Vec<External> {
        let in_scope = |id: &str| scope.is_none_or(|ids| ids.iter().any(|i| i.as_str() == id));
        let mut changes = Vec::new();
        for (id, saved) in &found {
            // A version that storage won a save with was taken without saying so.
            // The editors still hold the one before it, and are told of this one now.
            let old = if self.disk_won.contains(id) {
                self.baseline_of(id)
            } else {
                self.files.get(id)
            };
            if old.is_none_or(|old| {
                old.revision != saved.revision || old.note.read_only != saved.note.read_only
            }) {
                let old = old.cloned();
                if let Some(old) = &old {
                    self.previous
                        .entry(id.clone())
                        .or_insert_with(|| old.clone());
                }
                changes.push(External::Updated {
                    previous: old.map(|s| s.note),
                    note: saved.note.clone(),
                });
            }
        }
        let gone: Vec<String> = self
            .files
            .keys()
            .filter(|id| in_scope(id) && !found.contains_key(*id))
            .cloned()
            .collect();
        let mut left = Vec::new();
        for id in &gone {
            let Some(saved) = self.files.remove(id) else {
                continue;
            };
            left.extend(saved.note.path.clone());
            changes.push(External::Removed(saved.note.clone()));
            self.previous.entry(id.clone()).or_insert(saved);
        }
        for change in &changes {
            let (External::Updated { note, .. } | External::Removed(note)) = change;
            self.pending.insert(note.id.clone());
            match change {
                External::Updated { .. } => {
                    self.removed.remove(&note.id);
                }
                External::Removed(_) => {
                    self.removed.insert(note.id.clone(), note.clone());
                }
            }
        }
        match scope {
            None => self.files = found,
            Some(_) => self.files.extend(found),
        }
        if let Some(directory) = self.storage.directory_mut()
            && !left.is_empty()
        {
            directory.mark_gone(&left);
        }
        for change in &changes {
            let (External::Updated { note, .. } | External::Removed(note)) = change;
            self.update_source(&note.id);
        }
        changes
    }
    pub fn acknowledge_changes(&mut self, changes: &[External]) {
        for change in changes {
            let (note, current) = match change {
                External::Updated { note, .. } => {
                    (note, self.files.get(&note.id).map(|saved| &saved.note))
                }
                External::Removed(note) => (
                    note,
                    self.removed
                        .get(&note.id)
                        .filter(|_| !self.files.contains_key(&note.id)),
                ),
            };
            if current.is_some_and(|current| same_external_version(current, note)) {
                self.clear_pending(&note.id);
            }
        }
    }
    fn clear_pending(&mut self, id: &str) {
        self.pending.remove(id);
        self.previous.remove(id);
        self.removed.remove(id);
        self.disk_won.remove(id);
        self.update_source(id);
    }
    #[cfg(any(test, feature = "test-support"))]
    #[cfg_attr(coverage_nightly, coverage(off))]
    pub fn acknowledge(&mut self, ids: &[String]) {
        for id in ids {
            self.clear_pending(id);
        }
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn markdown(&self, note: &Note) -> Result<String, StoreError> {
        render(self.baseline(note), note, &self.house)
    }
    /// The version `note`'s edits are written against. A note that already says
    /// what storage holds — the version an external change was just adopted as —
    /// takes that version as its baseline. The version before the change is kept
    /// only for edits made against it until the change is acknowledged.
    #[cfg(any(test, feature = "test-support"))]
    fn baseline(&self, note: &Note) -> Option<&Saved> {
        let current = self
            .files
            .get(&note.id)
            .filter(|saved| saved.note.document == note.document);
        current
            .or_else(|| self.previous.get(&note.id))
            .or_else(|| self.files.get(&note.id))
    }
    /// The Markdown that says `note`, written against the version `base`. A note
    /// that storage has never held is written against the source it was made from.
    fn text_of(&self, note: &Note, base: Option<&Saved>) -> Result<String, StoreError> {
        if base.is_some() {
            return render(base, note, &self.house);
        }
        match self.sources.source(note)? {
            Some(track) => track
                .snapshot()
                .render(doc::schema(), &note.document)
                .map_err(|error| StoreError::from(error.to_string())),
            None => render(None, note, &self.house),
        }
    }
    /// Keep the note's local text as a conflicted copy: a file beside the note's
    /// own in a folder, a note of its own in a host backend. Used when storage wins.
    pub fn recover(&mut self, note: &Note) -> Result<(), StoreError> {
        let original = self.baseline_of(&note.id).cloned();
        let local = ends_with_newline(
            self.text_of(note, original.as_ref())
                .unwrap_or_else(|_| doc::to_markdown_in(&note.document, &self.house)),
        );
        if let Some(directory) = self.storage.directory() {
            let path = original
                .as_ref()
                .and_then(|saved| saved.note.path.clone())
                .or_else(|| note.path.clone());
            let Some(path) = path else {
                // Nothing to stand beside. Saying so is what keeps the caller from
                // reporting a copy that was never written.
                return Err(Message::new("error.no-recovery-path").into());
            };
            directory.keep_beside(&path, &local)?;
            directory.clear_recovery(&note.id);
            return Ok(());
        }
        let kept = (note.id.clone(), local.clone());
        if !self.copies.insert(kept.clone()) {
            return Ok(());
        }
        let copy = self
            .create_record(NewRecord {
                markdown: local,
                title: Some(conflicted_copy_name(&note.title())),
                logical_key: None,
            })
            .inspect_err(|_| {
                self.copies.remove(&kept);
            })?;
        // The copy reaches the mounted editors the way any other new note does:
        // as a change that the next read of storage reports.
        self.files.remove(&copy.id);
        self.update_source(&copy.id);
        if let Some(notify) = &self.notify {
            notify.changed(vec![NoteId::new(copy.id)]);
        }
        Ok(())
    }
    /// Write the library out, and say whether every note in it reached storage.
    pub fn save(&mut self, library: &Library, preferences: &Preferences) -> Result<(), StoreError> {
        self.write(0, library, preferences).result
    }
    /// Write the library out. A new note that says something is stored at once; in a
    /// folder, under the name the folder's naming setting gives it. When storage
    /// holds a version of a note that no editor here has seen, that version wins,
    /// and the local edits are kept as a conflicted copy. Each note is reported on
    /// its own: a failure of one does not undo the others.
    pub(crate) fn write(
        &mut self,
        revision: u64,
        library: &Library,
        preferences: &Preferences,
    ) -> crate::persistence::Saved {
        let saved = |store: &Self, result, changes, outcomes| crate::persistence::Saved {
            revision,
            changes,
            result,
            paths: store.paths(),
            conflicts: store.conflicts(),
            trashed: store.trashed(),
            kept: store.kept(),
            outcomes,
        };
        self.disk_won.clear();
        self.trashed.clear();
        self.kept.clear();
        if let Err(error) = library.validate().and_then(|()| preferences.validate()) {
            return saved(self, Err(error), Vec::new(), Vec::new());
        }
        if let Some(directory) = self.storage.directory_mut() {
            directory.set_workspace(&library.workspace);
        }
        let mut changes: Vec<(String, u64)> = Vec::new();
        let mut acknowledge = |id: &str| {
            if let Some(generation) = library.changes.get(id) {
                changes.push((id.to_owned(), *generation));
            }
        };
        let mut outcomes = Vec::new();
        let mut outcome = |id: &str, result: Result<StorageRevision, BackendError>, deleted| {
            if library.changes.contains_key(id) {
                outcomes.push(NoteSaveOutcome {
                    id: NoteId::new(id),
                    result,
                    deleted,
                });
            }
        };
        let mut errors: Vec<StoreError> = Vec::new();
        // The notes storage won over, by title: one conflict error at the end, so a
        // caller can tell it from a failure and still not take them as saved.
        let mut conflicts: Vec<Message> = Vec::new();
        for (id, note) in &library.deletions {
            let Some(saved) = self.files.get(id).cloned() else {
                acknowledge(id);
                continue;
            };
            // A stale snapshot cannot authorize deleting a version it has never seen.
            let deleted = if self.pending.contains(id) {
                Err(StoreError::Backend(BackendError::Conflict {
                    id: NoteId::new(id.clone()),
                    actual: Some(saved.revision.clone()),
                }))
            } else {
                self.storage.commit(BackendMutation::Delete {
                    id: NoteId::new(id.clone()),
                    expected: saved.revision.clone(),
                    deleted_at: note.deleted_at.unwrap_or_else(crate::storage::timestamp),
                })
            };
            match deleted {
                Ok(revision) => {
                    self.files.remove(id);
                    self.pending.remove(id);
                    self.previous.remove(id);
                    self.removed.remove(id);
                    self.update_source(id);
                    acknowledge(id);
                    outcome(id, Ok(revision), true);
                }
                Err(error) => {
                    if !matches!(error, StoreError::Backend(BackendError::Conflict { .. })) {
                        log::warn!("note {id} could not be deleted: {error}");
                    }
                    self.kept.push(id.clone());
                    outcome(id, Err(backend_error(error)), true);
                }
            }
        }
        if let Some(directory) = self.storage.directory_mut() {
            self.trashed = directory.take_trashed();
        }
        for note in &library.notes {
            let id = &note.id;
            if let Some(saved) = self.files.get_mut(id) {
                // A pin in a folder is the manifest's to remember, not the file's:
                // it is kept whether or not the note has anything to write.
                if let Some(directory) = self.storage.directory_mut() {
                    directory.identify(id, note.pinned, note.created_at);
                    saved.note.pinned = note.pinned;
                }
                if !library.changes.contains_key(id) {
                    continue;
                }
            }
            let saved = self.files.get(id).cloned();
            if self.pending.contains(id) {
                let adopted = saved
                    .as_ref()
                    .is_some_and(|saved| !differs(&saved.note, note));
                let locally_changed = !adopted
                    && self
                        .baseline_of(id)
                        .is_none_or(|base| differs(&base.note, note));
                if !locally_changed {
                    acknowledge(id);
                    if let Some(saved) = &saved {
                        outcome(id, Ok(saved.revision.clone()), false);
                    }
                    continue;
                }
                // Storage already won via refresh; keep local edits as a conflicted copy.
                match self.recover(note) {
                    Ok(()) => {
                        self.disk_won.insert(id.clone());
                        conflicts.push(note.title_message());
                    }
                    Err(error) => errors.push(error),
                }
                outcome(
                    id,
                    Err(BackendError::Conflict {
                        id: NoteId::new(id.clone()),
                        actual: saved.as_ref().map(|saved| saved.revision.clone()),
                    }),
                    false,
                );
                continue;
            }
            if let Some(saved) = &saved
                && !differs(&saved.note, note)
                && saved.note.pinned == note.pinned
            {
                acknowledge(id);
                outcome(id, Ok(saved.revision.clone()), false);
                continue;
            }
            if note.read_only.is_some() {
                let error = StoreError::from(
                    Message::new("error.note-read-only").arg("title", note.title_message()),
                );
                outcome(id, Err(BackendError::Invalid(error.to_string())), false);
                errors.push(error);
                continue;
            }
            if saved.is_none()
                && doc::is_blank(&note.document)
                && note.title_override.is_none()
                && note.logical_key.is_none()
            {
                if let Some(directory) = self.storage.directory() {
                    directory.clear_recovery(id);
                }
                acknowledge(id);
                continue;
            }
            match self.save_note(note, saved.as_ref()) {
                Ok(revision) => {
                    acknowledge(id);
                    outcome(id, Ok(revision), false);
                }
                // Storage holds a version that no editor here has seen.
                Err(StoreError::Backend(BackendError::Conflict { actual, .. })) => {
                    let conflicted = Err(BackendError::Conflict {
                        id: NoteId::new(id.clone()),
                        actual,
                    });
                    // Only where the note sits in a list changed here. There is no
                    // text to keep, so the version in storage is simply read again.
                    if saved
                        .as_ref()
                        .is_some_and(|saved| !differs(&saved.note, note))
                    {
                        if let Some(notify) = &self.notify {
                            notify.changed(vec![NoteId::new(id.clone())]);
                        }
                        outcome(id, conflicted, false);
                        continue;
                    }
                    // Preserve the local version before advancing any conflict
                    // state. Failed recovery must remain retryable against its base.
                    match self.recover(note) {
                        Ok(()) => {
                            if let Some(saved) = &saved {
                                self.previous
                                    .entry(id.clone())
                                    .or_insert_with(|| saved.clone());
                                if let Ok(Some(current)) = self.read_one(id, Some(saved)) {
                                    self.files.insert(id.clone(), current);
                                }
                            }
                            self.disk_won.insert(id.clone());
                            self.pending.insert(id.clone());
                            self.update_source(id);
                            conflicts.push(note.title_message());
                        }
                        Err(copy_error) => errors.push(
                            Message::new("error.conflicted-copy-failed")
                                .arg(
                                    "detail",
                                    Message::new("error.note-changed")
                                        .arg("title", note.title_message()),
                                )
                                .arg("copy_error", copy_error)
                                .into(),
                        ),
                    }
                    outcome(id, conflicted, false);
                }
                Err(error) => {
                    outcome(id, Err(backend_error(error.clone())), false);
                    errors.push(error);
                }
            }
        }
        if let Err(error) = self
            .storage
            .save_workspace(&NoteId::new(library.active_id.clone()), &library.workspace)
        {
            errors.push(error);
        }
        if let Some(directory) = self.storage.directory_mut()
            && let Err(error) = directory.save_settings(preferences)
        {
            errors.push(error);
        }
        if !conflicts.is_empty() {
            errors.push(StoreError::Conflict(conflicts));
        }
        let result = if errors.is_empty() {
            Ok(())
        } else {
            Err(StoreError::several(errors))
        };
        saved(self, result, changes, outcomes)
    }
    /// Write one note, naming the version it replaces.
    fn save_note(
        &mut self,
        note: &Note,
        saved: Option<&Saved>,
    ) -> Result<StorageRevision, StoreError> {
        let render_started = std::time::Instant::now();
        let rendered = self.text_of(note, saved);
        log::debug!(
            "save_render note={} elapsed_us={} success={}",
            note.id,
            render_started.elapsed().as_micros(),
            rendered.is_ok()
        );
        let text = rendered?;
        let mut stored = note.clone();
        stored.conflicted = false;
        stored.deleted_at = None;
        // A tree that writes what storage already holds — a picture spelled
        // out under the caret, which saves as the same characters as the
        // picture — is not written again: storage keeps its version, and only
        // the record follows the tree.
        if let Some(saved) = saved
            && saved.text == text
            && saved.note.title_override == note.title_override
            && saved.note.logical_key == note.logical_key
            && saved.note.pinned == note.pinned
        {
            stored.path.clone_from(&saved.note.path);
            let revision = saved.revision.clone();
            self.files.insert(
                note.id.clone(),
                Saved {
                    note: stored,
                    ..saved.clone()
                },
            );
            self.update_source(&note.id);
            return Ok(revision);
        }
        if saved.is_none()
            && let Some(path) = &note.path
            && let Some(directory) = self.storage.directory_mut()
        {
            directory.place(&note.id, path)?;
        }
        let revision = self.put(note, &text, saved.map(|saved| &saved.revision))?;
        if let Some(directory) = self.storage.directory() {
            stored.path = directory.path(&note.id);
        }
        // Keep the track the editor already holds across saves, so undo can still
        // address the source the note began as.
        let track = match saved {
            Some(saved) => saved.track.clone(),
            None => self
                .sources
                .source(note)
                .ok()
                .flatten()
                .or_else(|| track_of(&text)),
        };
        self.files.insert(
            note.id.clone(),
            Saved {
                revision: revision.clone(),
                text,
                track,
                note: stored,
            },
        );
        self.update_source(&note.id);
        Ok(revision)
    }
    /// Commit one note. A commit can succeed after its reply is lost, and the retry
    /// then names a version that storage has replaced. Storage that already holds
    /// exactly this write is the outcome the retry asked for, not a conflict.
    fn put(
        &mut self,
        note: &Note,
        text: &str,
        expected: Option<&StorageRevision>,
    ) -> Result<StorageRevision, StoreError> {
        let id = NoteId::new(note.id.clone());
        let committed = self.storage.commit(BackendMutation::Put {
            id: id.clone(),
            expected: expected.cloned(),
            markdown: text.to_owned(),
            title: note.title_override.clone(),
            logical_key: note.logical_key.clone(),
            created_at: note.created_at,
            updated_at: note.updated_at,
            pinned: note.pinned,
        });
        match committed {
            Err(conflict @ StoreError::Backend(BackendError::Conflict { .. })) => self
                .storage
                .read(std::slice::from_ref(&id))?
                .into_iter()
                .find(|stored| {
                    stored.id == id
                        && stored.markdown == text
                        && stored.title == note.title_override
                        && stored.logical_key == note.logical_key
                        && stored.pinned == note.pinned
                })
                .map(|stored| stored.revision)
                .ok_or(conflict),
            other => other,
        }
    }
    /// Spell new Markdown in `house`'s style from now on.
    pub fn set_house(&mut self, house: markraft_commonmark::HouseStyleHandle) {
        self.house = house;
    }
    /// Change the settings file that a notes folder writes. Notes that a host
    /// keeps have none.
    pub fn update_settings(
        &mut self,
        update: impl FnOnce(&mut Settings),
    ) -> Result<(), StoreError> {
        match self.storage.directory_mut() {
            Some(directory) => directory.update_settings(update),
            None => Ok(()),
        }
    }
}

/// A store failure as one note's outcome reports it.
fn backend_error(error: StoreError) -> BackendError {
    match error {
        StoreError::Backend(error) => error,
        other => BackendError::Unavailable(other.to_string()),
    }
}
fn render(
    saved: Option<&Saved>,
    note: &Note,
    house: &markraft_commonmark::HouseStyleHandle,
) -> Result<String, StoreError> {
    match saved {
        Some(saved) => saved
            .track
            .as_ref()
            .ok_or_else(|| StoreError::from(Message::new("error.unreadable-markdown")))?
            .save(doc::schema(), &note.document)
            .map_err(|e| {
                StoreError::from(
                    Message::new("error.markdown-preserve").arg("detail", e.to_string()),
                )
            }),
        None => Ok(format!("{}\n", doc::to_markdown_in(&note.document, house))),
    }
}

/// A track for a note whose text is `text`, when it is Markdown.
fn track_of(text: &str) -> Option<Arc<SourceTrack>> {
    let source = SourceDocument::parse(doc::schema(), text).ok()?;
    Some(Arc::new(SourceTrack::new(source)))
}

pub fn safe_relative(path: &Path) -> bool {
    path.components()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}

/// What a file may be called, with everything a name cannot carry taken out of it.
/// The rule lives here rather than in the one caller so that a name the user types
/// can be held to the same one.
///
/// A separator — `/`, `:`, `\` — and anything a control character interrupted become
/// `-`, which keeps `2026/09/21` readable as a date. Wiki-link syntax is dropped
/// instead: links resolve by file stem, and a stem holding `[`, `]`, `#`, `^` or `|`
/// is one no `[[link]]` can reach, because a target is cut at `#` and `^`, `|` begins
/// the alias, and the brackets end the link. Dropping a character closes the gap it
/// leaves, so `a | b` is not filed as `a  b`.
#[doc(hidden)]
pub fn safe_stem(title: &str) -> String {
    let mut name = String::with_capacity(title.len());
    let mut dropped = false;
    for character in title.chars() {
        match character {
            '[' | ']' | '#' | '^' | '|' => dropped = true,
            '/' | ':' | '\\' => {
                name.push('-');
                dropped = false;
            }
            _ if character.is_control() => {
                name.push('-');
                dropped = false;
            }
            // Only the run a dropped character sat in closes up; spacing the user
            // wrote elsewhere in the title is theirs.
            _ if character.is_whitespace() => {
                if !dropped || !name.ends_with(char::is_whitespace) {
                    name.push(character);
                }
            }
            _ => {
                name.push(character);
                dropped = false;
            }
        }
    }
    name.trim_matches(|c: char| c == '.' || c.is_whitespace())
        .to_owned()
}
/// The stem a user typed for a file, or why it cannot be one. A generated name is
/// quietly made safe; a typed one is refused instead, because silently filing the note
/// under something other than what was typed is its own surprise. The file's own
/// extension may be typed along with the name and is not part of it.
#[doc(hidden)]
pub fn typed_stem(name: &str, extension: &str) -> Result<String, StoreError> {
    let name = name.trim();
    let suffix = format!(".{extension}");
    let cut = name.len().saturating_sub(suffix.len());
    let stem = if cut > 0 && name.is_char_boundary(cut) && name[cut..].eq_ignore_ascii_case(&suffix)
    {
        name[..cut].trim_end()
    } else {
        name
    };
    if stem.is_empty() {
        return Err(Message::new("error.empty-name").into());
    }
    if safe_stem(stem) != stem {
        return Err(Message::new("error.invalid-name").into());
    }
    // The limit is the file system's, counted in bytes with the extension on.
    if stem.len() + suffix.len() > 255 {
        return Err(Message::new("error.long-name").into());
    }
    Ok(stem.to_owned())
}
/// Milliseconds since the epoch as (year, month, day, hour, minute, second, millisecond).
#[doc(hidden)]
pub fn civil(milliseconds: u64) -> (i64, u32, u32, u32, u32, u32, u32) {
    let seconds = milliseconds / 1000;
    let days = (seconds / 86_400) as i64;
    let second = (seconds % 86_400) as u32;
    // Days to civil date, after Howard Hinnant's `civil_from_days`.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * shifted_month + 2) / 5 + 1) as u32;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    } as u32;
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (
        year,
        month,
        day,
        second / 3600,
        second / 60 % 60,
        second % 60,
        (milliseconds % 1000) as u32,
    )
}

/// The name of a copy that keeps local edits when storage wins.
fn conflicted_copy_name(name: &str) -> String {
    let (year, month, day, ..) = civil(crate::storage::timestamp());
    format!("{name} (conflicted copy {year:04}-{month:02}-{day:02})")
}

/// [`Store::open`] over the settings file as it stands, the way a launch reads it.
#[cfg(any(test, feature = "test-support"))]
#[cfg_attr(coverage_nightly, coverage(off))]
#[doc(hidden)]
pub fn open_reading_settings(
    directory: PathBuf,
    settings_path: PathBuf,
) -> Result<(Store, Library), StoreError> {
    let settings = Settings::read(&settings_path).unwrap_or_default();
    Store::open(directory, settings_path, settings)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::directory::{collect_markdown, date_time_name, file_name, write_document};
    use std::fs::{self, File};
    use std::time::UNIX_EPOCH;
    fn open(root: &Path) -> (Store, Library) {
        let notes = root.join("notes");
        fs::create_dir_all(&notes).unwrap();
        open_reading_settings(notes, root.join("settings.json")).unwrap()
    }
    fn fixture(root: &Path, name: &str, text: &[u8]) -> PathBuf {
        let path = root.join("notes").join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        path
    }
    /// Every Markdown file in the notes folder, sorted.
    fn markdown_files(root: &Path) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        collect_markdown(&root.join("notes"), &mut paths).unwrap();
        paths.sort();
        paths
    }
    /// The folder's manifest, once `root` has been opened and closed.
    fn manifest_path(root: &Path) -> PathBuf {
        let (store, _) = open(root);
        store.state().join("manifest.json")
    }
    #[test]
    fn mid_save_conflict_is_delivered_on_the_next_refresh() {
        for incremental in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let path = fixture(root.path(), "note.md", b"Original");
            let (mut store, mut library) = open(root.path());
            let id = library.active_id.clone();
            library.set_document(&id, doc::from_markdown("Local"));
            fs::write(&path, b"External").unwrap();
            assert!(store.save(&library, &Preferences::default()).is_err());
            let changes = if incremental {
                store.refresh_paths(&[path])
            } else {
                store.refresh()
            }
            .unwrap();
            let note = changes
                .iter()
                .find_map(|change| match change {
                    External::Updated {
                        previous: Some(previous),
                        note,
                    } if note.id == id => {
                        assert_eq!(doc::plain_text(&previous.document), "Original");
                        Some(note)
                    }
                    _ => None,
                })
                .expect("the conflicted note must reach the UI");
            assert_eq!(doc::plain_text(&note.document), "External");
        }
    }

    #[test]
    fn reload_returns_the_adopted_library_when_auxiliary_metadata_cannot_be_written() {
        let root = tempfile::tempdir().unwrap();
        fixture(root.path(), "note.md", b"Original");
        let (mut store, _) = open(root.path());
        let manifest = store.state().join("manifest.json");
        fs::remove_file(&manifest).unwrap();
        fs::create_dir(&manifest).unwrap();
        fixture(root.path(), "added.md", b"Added");
        let reloaded = store.reload().unwrap();
        assert_eq!(reloaded.notes.len(), 2);
        assert_eq!(store.files.len(), 2);
        assert!(!store.notices().take().is_empty());
    }

    #[test]
    fn source_only_changes_with_the_same_timestamp_have_distinct_acknowledgements() {
        let root = tempfile::tempdir().unwrap();
        let path = fixture(root.path(), "note.md", b"# Heading\n");
        let (mut store, library) = open(root.path());
        let id = library.active_id.clone();
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        fs::write(&path, b"Heading\n=======\n").unwrap();
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(modified))
            .unwrap();
        let first = store.refresh_paths(std::slice::from_ref(&path)).unwrap();
        fs::write(&path, b"# Heading\n").unwrap();
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(modified))
            .unwrap();
        let second = store.refresh_paths(std::slice::from_ref(&path)).unwrap();
        let (External::Updated { note: a, .. }, External::Updated { note: b, .. }) =
            (&first[0], &second[0])
        else {
            panic!()
        };
        assert_eq!(a.document, b.document);
        assert_eq!(a.updated_at, b.updated_at);
        store.acknowledge_changes(&first);
        assert!(store.pending.contains(&id));
        assert_eq!(
            store
                .source_cache()
                .source(a)
                .unwrap()
                .unwrap()
                .origin()
                .source(),
            "Heading\n=======\n"
        );
        store.acknowledge_changes(&second);
        assert!(!store.pending.contains(&id));
    }

    #[test]
    fn only_latest_external_versions_are_current_and_removal_acks_its_own_version() {
        let root = tempfile::tempdir().unwrap();
        let path = fixture(root.path(), "note.md", b"First");
        let (mut store, library) = open(root.path());
        let id = library.active_id.clone();
        let sources = store.source_cache();
        fs::write(&path, b"Second").unwrap();
        let updated = store.refresh_paths(std::slice::from_ref(&path)).unwrap();
        assert!(sources.is_current_external(&updated[0]));
        fs::remove_file(&path).unwrap();
        let removed = store.refresh_paths(std::slice::from_ref(&path)).unwrap();
        assert!(!sources.is_current_external(&updated[0]));
        assert!(sources.is_current_external(&removed[0]));
        store.acknowledge_changes(&updated);
        assert!(store.pending.contains(&id));
        store.acknowledge_changes(&removed);
        assert!(!store.pending.contains(&id));
        assert!(!sources.is_current_external(&removed[0]));
    }

    #[test]
    fn reloading_rejects_deferred_events_from_the_previous_baseline() {
        let root = tempfile::tempdir().unwrap();
        let path = fixture(root.path(), "note.md", b"First");
        let (mut store, _) = open(root.path());
        fs::write(&path, b"Second").unwrap();
        let deferred = store.refresh_paths(std::slice::from_ref(&path)).unwrap();
        fs::write(&path, b"Latest").unwrap();
        let reloaded = store.reload().unwrap();
        assert_eq!(doc::plain_text(&reloaded.active_note().document), "Latest");
        assert!(!store.source_cache().is_current_external(&deferred[0]));
    }

    #[test]
    fn old_acknowledgement_cannot_release_a_newer_external_change() {
        let root = tempfile::tempdir().unwrap();
        let path = fixture(root.path(), "note.md", b"First");
        let (mut store, library) = open(root.path());
        let id = library.active_id.clone();
        fs::write(&path, b"Second").unwrap();
        let first = store.refresh_paths(std::slice::from_ref(&path)).unwrap();
        fs::write(&path, b"Third").unwrap();
        let second = store.refresh_paths(std::slice::from_ref(&path)).unwrap();
        store.acknowledge_changes(&first);
        assert!(store.pending.contains(&id));
        let External::Updated { note, .. } = &first[0] else {
            panic!()
        };
        assert_eq!(
            store
                .source_cache()
                .source(note)
                .unwrap()
                .unwrap()
                .origin()
                .source(),
            "Second"
        );
        store.acknowledge_changes(&second);
        assert!(!store.pending.contains(&id));
    }

    #[test]
    fn stale_snapshot_never_deletes_an_unacknowledged_external_file() {
        let root = tempfile::tempdir().unwrap();
        fixture(root.path(), "original.md", b"Original");
        let (mut store, library) = open(root.path());
        let added = fixture(root.path(), "dropped.md", b"External addition");
        let changes = store.refresh_paths(std::slice::from_ref(&added)).unwrap();
        assert_eq!(changes.len(), 1);
        store.save(&library, &Preferences::default()).unwrap();
        assert_eq!(fs::read(&added).unwrap(), b"External addition");
        assert_eq!(store.files.len(), 2);
    }

    #[test]
    fn explicit_delete_refuses_a_file_changed_since_its_baseline() {
        let root = tempfile::tempdir().unwrap();
        let path = fixture(root.path(), "note.md", b"Original");
        let (mut store, mut library) = open(root.path());
        let id = library.active_id.clone();
        assert!(library.delete(&id));
        fs::write(&path, b"Changed elsewhere").unwrap();
        // The refusal is reported as a kept note, not as a failed save: a failed
        // save would hold up every barrier until the deletion went through.
        store.save(&library, &Preferences::default()).unwrap();
        assert_eq!(store.kept(), std::slice::from_ref(&id));
        assert_eq!(fs::read(&path).unwrap(), b"Changed elsewhere");
        assert!(store.files.contains_key(&id));
    }

    #[test]
    fn failed_conflict_recovery_preserves_the_original_baseline_for_retry() {
        let root = tempfile::tempdir().unwrap();
        let path = fixture(root.path(), "sub/note.md", b"Original");
        let (mut store, mut library) = open(root.path());
        let id = library.active_id.clone();
        library.set_document(&id, doc::from_markdown("Local changes"));
        fs::remove_file(&path).unwrap();
        fs::remove_dir(path.parent().unwrap()).unwrap();
        assert!(store.save(&library, &Preferences::default()).is_err());
        assert_eq!(store.files[&id].text, "Original");
        assert!(!store.pending.contains(&id));
        assert!(!store.disk_won.contains(&id));
        assert_eq!(
            doc::plain_text(&library.note(&id).unwrap().document),
            "Local changes"
        );
    }

    #[test]
    fn manifest_failure_does_not_hide_external_changes_or_leave_the_source_cache_stale() {
        for incremental in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let path = fixture(root.path(), "note.md", b"Original");
            let (mut store, library) = open(root.path());
            let original = library.active_note().clone();
            let manifest = store.state().join("manifest.json");
            fs::remove_file(&manifest).unwrap();
            fs::create_dir(&manifest).unwrap();
            fs::write(&path, b"External content").unwrap();
            let added = fixture(root.path(), "added.md", b"Added");
            let changes = if incremental {
                store.refresh_paths(&[path, added])
            } else {
                store.refresh()
            }
            .unwrap();
            let note = changes
                .iter()
                .find_map(|change| match change {
                    External::Updated { note, .. } if note.id == original.id => Some(note),
                    _ => None,
                })
                .expect("change must reach the UI");
            let sources = store.source_cache();
            assert_eq!(
                sources
                    .source(&original)
                    .unwrap()
                    .unwrap()
                    .origin()
                    .source(),
                "Original"
            );
            assert_eq!(
                sources.source(note).unwrap().unwrap().origin().source(),
                "External content"
            );
            assert!(!store.notices().take().is_empty());
        }
    }

    #[test]
    fn an_unreadable_manifest_is_set_aside_and_the_folder_opens() {
        let root = tempfile::tempdir().unwrap();
        fixture(root.path(), "Kept.md", b"kept");
        let manifest = manifest_path(root.path());
        fs::write(&manifest, b"{\"paths\": [").unwrap();
        let (store, library) = open(root.path());
        assert_eq!(library.notes.len(), 1);
        assert_eq!(
            store
                .notices()
                .take()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            ["Folder settings were unreadable and have been reset."]
        );
        let aside: Vec<_> = fs::read_dir(manifest.parent().unwrap())
            .unwrap()
            .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
            .filter(|name| name.starts_with("manifest.unreadable-"))
            .collect();
        assert_eq!(aside.len(), 1, "{aside:?}");
    }
    #[test]
    fn a_manifest_from_a_newer_version_is_refused_not_reset() {
        for newer in [
            &b"{\"version\": 2}"[..],
            b"{\"version\": 2, \"paths\": \"a shape this build does not know\"}",
        ] {
            let root = tempfile::tempdir().unwrap();
            let manifest = manifest_path(root.path());
            fs::write(&manifest, newer).unwrap();
            let notes = root.path().join("notes");
            let Err(error) = open_reading_settings(notes, root.path().join("settings.json")) else {
                panic!("a newer manifest opened");
            };
            assert!(error.to_string().contains("newer version"), "{error}");
            assert_eq!(fs::read(&manifest).unwrap(), newer);
        }
    }
    #[test]
    fn a_file_removed_while_closed_is_named_once() {
        let root = tempfile::tempdir().unwrap();
        fixture(root.path(), "Kept.md", b"kept");
        let gone = fixture(root.path(), "Gone.md", b"gone");
        let (store, _) = open(root.path());
        assert!(store.notices().take().is_empty());
        drop(store);
        fs::remove_file(&gone).unwrap();
        let (store, library) = open(root.path());
        assert_eq!(library.notes.len(), 1);
        assert_eq!(
            store
                .notices()
                .take()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            ["“Gone.md” was deleted outside Markraft."]
        );
        drop(store);
        let (store, _) = open(root.path());
        assert!(store.notices().take().is_empty());
    }
    #[test]
    fn a_removal_the_window_reported_is_not_repeated_at_launch() {
        let root = tempfile::tempdir().unwrap();
        let _path = fixture(root.path(), "text.md", b"text");
        let (mut store, mut library) = open(root.path());
        let id = library.active_id.clone();
        library.notes[0].pinned = true;
        store.save(&library, &Preferences::default()).unwrap();
        let path = store.paths()[0].1.clone();
        fs::remove_file(&path).unwrap();
        assert!(matches!(
            store.refresh().unwrap().as_slice(),
            [External::Removed(note)] if note.id == id
        ));
        // An editor that deletes before it writes brings the same note back.
        fs::write(&path, b"text").unwrap();
        store.refresh().unwrap();
        assert!(store.files[&id].note.pinned);
        fs::remove_file(&path).unwrap();
        store.refresh().unwrap();
        drop(store);
        let (store, _) = open(root.path());
        assert!(store.notices().take().is_empty());
        assert!(store.remembers_no_paths());
    }
    #[test]
    fn a_missing_folder_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("notes");
        let error = match open_reading_settings(missing, root.path().join("settings.json")) {
            Err(error) => error,
            Ok(_) => panic!("expected missing folder to be refused"),
        };
        assert!(error.to_string().contains("no longer there"), "{error}");
    }
    #[test]
    fn opening_and_pinning_do_not_touch_user_files() {
        let root = tempfile::tempdir().unwrap();
        let path = fixture(
            root.path(),
            "nested/Title.md",
            b"---\nid: user-data\npinned: true\n---\n\nTitle\n=====\n",
        );
        let bytes = fs::read(&path).unwrap();
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        let (mut store, mut library) = open(root.path());
        let id = library.active_id.clone();
        library.notes[0].pinned = true;
        store.save(&library, &Preferences::default()).unwrap();
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), modified);
        assert!(!root.path().join("notes/.markraft").exists());
        assert!(!root.path().join("notes/.trash").exists());
        drop(store);
        let (_, library) = open(root.path());
        assert_eq!(library.active_id, id);
        assert!(library.active_note().pinned);
    }
    #[test]
    fn edit_preserves_path_frontmatter_crlf_and_reference_style() {
        let root = tempfile::tempdir().unwrap();
        let path = fixture(
            root.path(),
            "projects/Keep This Name.md",
            b"\xef\xbb\xbf---\r\nid: custom\r\n# comment\r\n---\r\n\r\nKeep This Name\r\n\r\nhello [site][s]\r\n\r\n[s]: https://example.com\r\n",
        );
        let (mut store, mut library) = open(root.path());
        let id = library.active_id.clone();
        // Only a word of the body changes. The reference link and its definition
        // are text and a block of the document, and are written back as they were.
        library.set_document(
            &id,
            doc::from_markdown("Keep This Name\n\nchanged [site][s]\n\n[s]: https://example.com"),
        );
        store.save(&library, &Preferences::default()).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "\u{feff}---\r\nid: custom\r\n# comment\r\n---\r\n\r\nKeep This Name\r\n\r\nchanged [site][s]\r\n\r\n[s]: https://example.com\r\n"
        );
        assert_eq!(store.paths()[0].1, fs::canonicalize(&path).unwrap());
    }
    #[test]
    fn recursive_same_named_notes_have_distinct_persistent_identities() {
        let root = tempfile::tempdir().unwrap();
        fixture(root.path(), "a/note.md", b"same");
        fixture(root.path(), "b/note.md", b"same");
        fixture(root.path(), ".git/ignored.md", b"ignore");
        let (store, library) = open(root.path());
        assert_eq!(library.notes.len(), 2);
        let ids: HashSet<_> = library.notes.iter().map(|n| n.id.clone()).collect();
        drop(store);
        let (_, again) = open(root.path());
        assert_eq!(ids, again.notes.iter().map(|n| n.id.clone()).collect());
    }
    #[test]
    fn external_source_only_changes_are_not_missed() {
        let root = tempfile::tempdir().unwrap();
        let path = fixture(root.path(), "note.md", b"# Heading\n");
        let (mut store, _) = open(root.path());
        fs::write(path, b"Heading\n=======\n").unwrap();
        assert_eq!(store.refresh().unwrap().len(), 1);
    }
    #[test]
    fn an_adopted_external_change_is_the_baseline_for_the_next_edit() {
        let root = tempfile::tempdir().unwrap();
        let path = fixture(
            root.path(),
            "note.md",
            b"Edit area\n\n| a | b |\n| - | - |\n| 1 | 2 |\n",
        );
        let (mut store, library) = open(root.path());
        let id = library.active_id.clone();
        fs::write(&path, b"Edit area\n").unwrap();
        let changes = store.refresh().unwrap();
        let External::Updated { note, .. } = &changes[0] else {
            panic!("the rewrite is an update");
        };
        // The editor opens the disk version before the change is acknowledged,
        // so its source baseline must already be the bytes now on disk.
        let source = store.markdown(note).unwrap();
        assert_eq!(source, "Edit area\n");
        let baseline = SourceDocument::parse(doc::schema(), &source).unwrap();
        let edited = doc::from_markdown("Edit area\n\ny");
        assert_eq!(
            baseline.render(doc::schema(), &edited).unwrap(),
            "Edit area\n\ny\n"
        );
        store.acknowledge(&[id]);
    }
    #[test]
    fn concurrent_edits_keep_base_local_disk_outside_workspace() {
        let root = tempfile::tempdir().unwrap();
        let path = fixture(root.path(), "note.md", b"Original\n");
        let (mut store, mut library) = open(root.path());
        let id = library.active_id.clone();
        library.set_document(&id, doc::from_markdown("Local"));
        fs::write(&path, b"External\n").unwrap();
        assert!(store.save(&library, &Preferences::default()).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"External\n");
        assert!(store.conflicts().contains(&id));
        let copies = conflicted_copies_beside(&path);
        assert_eq!(copies.len(), 1);
        assert_eq!(fs::read_to_string(&copies[0]).unwrap(), "Local\n");
        drop(store);
        let (_, again) = open(root.path());
        assert_eq!(again.active_note().id, id);
        assert!(!again.active_note().conflicted);
        assert_eq!(doc::plain_text(&again.active_note().document), "External");
    }
    #[test]
    fn deleted_dirty_document_is_never_recreated() {
        let root = tempfile::tempdir().unwrap();
        let path = fixture(root.path(), "note.md", b"Original");
        let (mut store, mut library) = open(root.path());
        let id = library.active_id.clone();
        library.set_document(&id, doc::from_markdown("Local"));
        fs::remove_file(&path).unwrap();
        store.refresh().unwrap();
        assert!(store.save(&library, &Preferences::default()).is_err());
        assert!(!path.exists());
        let copies = conflicted_copies_beside(&path);
        assert_eq!(copies.len(), 1);
        assert_eq!(fs::read_to_string(&copies[0]).unwrap().trim_end(), "Local");
    }
    #[test]
    fn read_only_encoding_and_links_are_protected() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let path = fixture(root.path(), "invalid.md", b"invalid\xff");
        let linked = fixture(root.path(), "linked.md", b"hardlink");
        fs::hard_link(&linked, root.path().join("hardlink.md")).unwrap();
        symlink(&linked, root.path().join("notes/symlink.md")).unwrap();
        let (mut store, mut library) = open(root.path());
        assert_eq!(library.notes.len(), 2);
        assert!(library.notes.iter().all(|n| n.read_only.is_some()));
        // Unchanged read-only notes are left alone on save.
        store.save(&library, &Preferences::default()).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"invalid\xff");
        // Edits are refused by the library before a write is attempted.
        let id = library.active_id.clone();
        assert!(!library.set_document(&id, doc::from_markdown("edited")));
        let note = store
            .add_file(root.path().join("notes/symlink.md"))
            .unwrap();
        assert!(note.read_only.is_some());
    }
    #[test]
    fn new_files_are_filed_under_their_title_once() {
        let root = tempfile::tempdir().unwrap();
        let (mut store, mut library) = open(root.path());
        let id = library.active_id.clone();
        library.workspace.new_note_directory = "inbox".into();
        library.set_document(&id, doc::from_markdown("Title"));
        store.save(&library, &Preferences::default()).unwrap();
        let path = store.paths()[0].1.clone();
        assert_eq!(fs::read_to_string(&path).unwrap(), "Title\n");
        assert!(path.ends_with("inbox/Title.md"));
        library.set_document(&id, doc::from_markdown("Changed"));
        store.save(&library, &Preferences::default()).unwrap();
        assert_eq!(store.paths()[0].1, path);
        assert_eq!(fs::read_to_string(&path).unwrap(), "Changed\n");
    }
    #[test]
    fn no_clobber_write_and_permissions_are_preserved() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let path = fixture(root.path(), "note.md", b"Original");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        assert!(write_document(&path, b"Wrong", None).is_err());
        assert_eq!(
            fs::read(&path).unwrap(),
            b"Original",
            "a refused write writes nothing"
        );
        write_document(&path, b"Edited", Some(b"Original")).unwrap();
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }
    #[test]
    fn opening_a_folder_loads_every_markdown_file() {
        let root = tempfile::tempdir().unwrap();
        fixture(root.path(), "note.md", b"Open me");
        fixture(root.path(), "other.md", b"Other");
        let (_, library) = open(root.path());
        assert_eq!(library.notes.len(), 2);
    }
    #[test]
    fn a_renamed_file_is_the_same_note_under_another_name() {
        let root = tempfile::tempdir().unwrap();
        let path = fixture(
            root.path(),
            "inbox/Meet.md",
            b"---\nid: x\n---\n\nMeeting notes\n",
        );
        let bytes = fs::read(&path).unwrap();
        let path = fs::canonicalize(&path).unwrap();
        let (mut store, mut library) = open(root.path());
        let id = library.active_id.clone();
        library.notes[0].pinned = true;
        store.save(&library, &Preferences::default()).unwrap();
        let renamed = store.rename(&id, "Meeting notes").unwrap();
        assert_eq!(renamed, path.with_file_name("Meeting notes.md"));
        assert!(!path.exists());
        assert_eq!(fs::read(&renamed).unwrap(), bytes);
        assert_eq!(store.paths(), [(id.clone(), renamed.clone())]);
        // The watcher sees the move too, and has nothing to report about it.
        assert!(store.refresh().unwrap().is_empty());
        assert!(
            store
                .refresh_paths(&[path, renamed.clone()])
                .unwrap()
                .is_empty()
        );
        // An edit afterwards keeps the filename; renaming is explicit.
        library.notes[0].path = Some(renamed.clone());
        library.set_document(&id, doc::from_markdown("Meeting notes, revised"));
        store.save(&library, &Preferences::default()).unwrap();
        assert_eq!(store.paths()[0].1, renamed);
        assert_eq!(
            fs::read_to_string(&renamed).unwrap().trim_end(),
            "---\nid: x\n---\n\nMeeting notes, revised"
        );
        drop(store);
        let (_, library) = open(root.path());
        assert_eq!(library.active_id, id);
        assert!(library.active_note().pinned);
    }
    #[test]
    fn a_rename_never_replaces_a_file_or_takes_a_name_no_link_can_reach() {
        let root = tempfile::tempdir().unwrap();
        let path = fixture(root.path(), "One.md", b"one\n");
        let other = fixture(root.path(), "Two.md", b"two\n");
        let (mut store, library) = open(root.path());
        let id = library
            .notes
            .iter()
            .find(|note| note.path.as_deref() == Some(fs::canonicalize(&path).unwrap().as_path()))
            .unwrap()
            .id
            .clone();
        assert!(
            store
                .rename(&id, "Two")
                .unwrap_err()
                .to_string()
                .contains("already exists")
        );
        assert!(
            store
                .rename(&id, "Two.md")
                .unwrap_err()
                .to_string()
                .contains("already exists")
        );
        for name in [
            "", "  ", ".md", "a/b", "a:b", "Q3 #plan", "[x]", "a|b", ".hidden", "v1.",
        ] {
            assert!(store.rename(&id, name).is_err(), "{name:?}");
        }
        assert_eq!(fs::read(&path).unwrap(), b"one\n");
        assert_eq!(fs::read(&other).unwrap(), b"two\n");
        // The same name is no rename at all, and a dot inside one is only a dot.
        assert_eq!(
            store.rename(&id, "One").unwrap(),
            fs::canonicalize(&path).unwrap()
        );
        let dotted = store.rename(&id, "One v1.2").unwrap();
        assert_eq!(dotted.file_name().unwrap(), "One v1.2.md");
        // Only the letters' case changes: on a file system that does not keep case
        // the target is the file itself, which is not a file in the way.
        let cased = store.rename(&id, "one V1.2").unwrap();
        assert_eq!(cased.file_name().unwrap(), "one V1.2.md");
        assert_eq!(fs::read(&cased).unwrap(), b"one\n");
        // Opening the file by its new name proves nothing on a file system that
        // ignores case: the name the folder lists is what changed.
        let listed: Vec<String> = fs::read_dir(cased.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.to_lowercase() == "one v1.2.md")
            .collect();
        assert_eq!(listed, ["one V1.2.md"]);
    }
    #[test]
    fn a_file_another_app_changed_is_not_renamed_under_it() {
        let root = tempfile::tempdir().unwrap();
        let path = fixture(root.path(), "One.md", b"one\n");
        let (mut store, library) = open(root.path());
        let id = library.active_id.clone();
        fs::write(&path, b"changed elsewhere\n").unwrap();
        assert!(
            store
                .rename(&id, "Uno")
                .unwrap_err()
                .to_string()
                .contains("changed on disk")
        );
        assert!(path.exists());
        assert!(store.rename("no-such-note", "Uno").is_err());
    }
    #[test]
    fn undo_after_save_restores_original_source() {
        let root = tempfile::tempdir().unwrap();
        let path = fixture(root.path(), "Title.md", b"Title\n=====\n\noriginal\n\n");
        let (mut store, mut library) = open(root.path());
        let id = library.active_id.clone();
        let original = library.active_note().document.clone();
        library.set_document(&id, doc::from_markdown("# Title\n\nchanged"));
        store.save(&library, &Preferences::default()).unwrap();
        library.set_document(&id, original);
        store.save(&library, &Preferences::default()).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"Title\n=====\n\noriginal\n\n");
    }
    /// A tree that differs from the saved one but writes the same characters —
    /// a picture the caret has spelled out — leaves the file alone: no write,
    /// so no backup of the version it would have replaced either.
    #[test]
    fn a_tree_that_writes_the_same_bytes_is_not_saved_again() {
        let root = tempfile::tempdir().unwrap();
        let text = b"intro\n\n![a](x.png)\n";
        let path = fixture(root.path(), "intro.md", text);
        let (mut store, mut library) = open(root.path());
        let id = library.active_id.clone();
        let schema = doc::schema();
        let spelled = schema
            .doc([
                schema
                    .node(
                        markraft_commonmark::schema::PARAGRAPH,
                        [schema.text("intro")],
                    )
                    .unwrap(),
                schema
                    .node(
                        markraft_commonmark::schema::PARAGRAPH,
                        [schema.text("![a](x.png)")],
                    )
                    .unwrap(),
            ])
            .unwrap();
        assert_ne!(library.active_note().document, spelled);
        library.set_document(&id, spelled);
        store.save(&library, &Preferences::default()).unwrap();
        assert_eq!(fs::read(&path).unwrap(), text);
        assert!(
            !store
                .state()
                .join("backups")
                .join(format!("{id}.md"))
                .exists(),
            "nothing was written over the file"
        );
    }
    #[test]
    fn disk_wins_keeps_local_work_as_a_conflicted_copy() {
        let root = tempfile::tempdir().unwrap();
        let path = fixture(root.path(), "note.md", b"Original");
        let (mut store, mut library) = open(root.path());
        let id = library.active_id.clone();
        library.set_document(&id, doc::from_markdown("Local"));
        fs::write(&path, b"External").unwrap();
        assert!(store.save(&library, &Preferences::default()).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"External");
        assert!(store.conflicts().contains(&id));
        let copies = conflicted_copies_beside(&path);
        assert_eq!(copies.len(), 1);
        assert_eq!(fs::read_to_string(&copies[0]).unwrap().trim_end(), "Local");
        assert!(
            !store
                .state()
                .join("recovery")
                .join(format!("{id}.json"))
                .exists()
        );
        // A later save with the disk text adopted writes cleanly.
        library.set_document(&id, doc::from_markdown("External"));
        store.save(&library, &Preferences::default()).unwrap();
        assert!(store.conflicts().is_empty());
    }
    /// A queued save and the flush behind it can both see one conflict. The second
    /// copy would hold exactly what the first does, so it is not written: what the
    /// user finds beside the file is one copy per conflict, not one per attempt.
    #[test]
    fn seeing_one_conflict_twice_leaves_one_copy() {
        let root = tempfile::tempdir().unwrap();
        let path = fixture(root.path(), "note.md", b"Original");
        let (mut store, mut library) = open(root.path());
        let id = library.active_id.clone();
        library.set_document(&id, doc::from_markdown("Local"));
        fs::write(&path, b"External").unwrap();
        assert!(store.save(&library, &Preferences::default()).is_err());
        assert!(store.save(&library, &Preferences::default()).is_err());
        let copies = conflicted_copies_beside(&path);
        assert_eq!(copies.len(), 1, "{copies:?}");
        assert_eq!(fs::read_to_string(&copies[0]).unwrap().trim_end(), "Local");
        // Different text is a different conflict, and gets its own copy.
        library.set_document(&id, doc::from_markdown("Local, revised"));
        assert!(store.save(&library, &Preferences::default()).is_err());
        assert_eq!(conflicted_copies_beside(&path).len(), 2);
    }
    /// A leftover draft nobody can read is discarded rather than kept forever as a
    /// folder that will not open: refusing here would refuse every later launch too.
    #[test]
    fn an_unreadable_recovery_draft_does_not_shut_the_folder() {
        let root = tempfile::tempdir().unwrap();
        fixture(root.path(), "Note.md", b"text");
        let (store, _) = open(root.path());
        let recovery = store.state().join("recovery");
        fs::create_dir_all(&recovery).unwrap();
        let damaged = recovery.join("broken.json");
        fs::write(&damaged, b"{ not json at all").unwrap();
        drop(store);
        let (store, library) = open(root.path());
        assert_eq!(library.notes.len(), 1);
        assert!(!damaged.exists());
        drop(store);
    }
    #[test]
    fn explicit_new_file_location_is_used_without_overwriting() {
        let root = tempfile::tempdir().unwrap();
        let (mut store, mut library) = open(root.path());
        let path = root.path().join("explicit.md");
        let id = library.active_id.clone();
        library.notes[0].path = Some(path.clone());
        library.set_document(&id, doc::from_markdown("Exact place"));
        store.save(&library, &Preferences::default()).unwrap();
        assert_eq!(fs::read_to_string(path).unwrap(), "Exact place\n");
        assert_eq!(fs::read_dir(root.path().join("notes")).unwrap().count(), 0);
    }
    /// Calls the attribute functions directly: a spawned child would share the
    /// open folder locks of tests running beside it until it execs.
    #[cfg(target_os = "macos")]
    #[test]
    fn atomic_replacement_preserves_extended_attributes() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let root = tempfile::tempdir().unwrap();
        let path = fixture(root.path(), "note.md", b"Original");
        let file = CString::new(path.as_os_str().as_bytes()).unwrap();
        let name = c"com.markraft.test";
        let value = b"retained";
        let written = unsafe {
            libc::setxattr(
                file.as_ptr(),
                name.as_ptr(),
                value.as_ptr().cast(),
                value.len(),
                0,
                0,
            )
        };
        assert_eq!(written, 0);
        write_document(&path, b"Edited", Some(b"Original")).unwrap();
        let mut read = [0u8; 16];
        let length = unsafe {
            libc::getxattr(
                file.as_ptr(),
                name.as_ptr(),
                read.as_mut_ptr().cast(),
                read.len(),
                0,
                0,
            )
        };
        assert_eq!(usize::try_from(length).ok(), Some(value.len()));
        assert_eq!(&read[..value.len()], value);
    }
    #[test]
    fn a_new_note_takes_the_permissions_of_the_folder_it_lands_in() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        fixture(root.path(), "existing.md", b"Existing\n");
        let folder = root.path().join("notes");
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o755)).unwrap();
        let (mut store, mut library) = open(root.path());
        let id = library.new_note(doc::from_markdown("Fresh note"));
        store.save(&library, &Preferences::default()).unwrap();
        let path = store.files.get(&id).unwrap().note.path.clone().unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644,
            "a new note sits in the folder like its neighbours"
        );
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o700)).unwrap();
        let private = library.new_note(doc::from_markdown("Private note"));
        store.save(&library, &Preferences::default()).unwrap();
        let path = store
            .files
            .get(&private)
            .unwrap()
            .note
            .path
            .clone()
            .unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600,
            "a private folder keeps its notes private"
        );
    }
    #[test]
    fn pending_dirty_flush_cannot_claim_the_original_is_saved() {
        let root = tempfile::tempdir().unwrap();
        let path = fixture(root.path(), "note.md", b"Original");
        let (mut store, mut library) = open(root.path());
        let id = library.active_id.clone();
        library.set_document(&id, doc::from_markdown("Dirty local"));
        fs::write(&path, b"Disk").unwrap();
        store.refresh().unwrap();
        assert!(store.save(&library, &Preferences::default()).is_err());
        assert_eq!(store.conflicts(), vec![id.clone()]);
        // Acknowledge the disk win, then adopt the store's disk note so a later save is clean.
        store.acknowledge(std::slice::from_ref(&id));
        let disk = store.files.get(&id).unwrap().note.clone();
        library.adopt(disk);
        store.save(&library, &Preferences::default()).unwrap();
        assert!(store.conflicts().is_empty());
        assert_eq!(fs::read_to_string(path).unwrap().trim(), "Disk");
    }
    #[test]
    fn invalid_encoding_stays_read_only_after_unchanged_refresh() {
        let root = tempfile::tempdir().unwrap();
        fixture(root.path(), "note.md", b"invalid\xff");
        let (mut store, _) = open(root.path());
        store.refresh().unwrap();
        assert!(store.files.values().all(|s| s.note.read_only.is_some()));
    }
    #[test]
    fn replacement_updates_mtime() {
        let root = tempfile::tempdir().unwrap();
        let path = fixture(root.path(), "note.md", b"Original");
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(UNIX_EPOCH))
            .unwrap();
        write_document(&path, b"Edited", Some(b"Original")).unwrap();
        assert!(fs::metadata(path).unwrap().modified().unwrap() > UNIX_EPOCH);
    }
    #[test]
    fn incremental_refresh_reads_only_reported_files() {
        let root = tempfile::tempdir().unwrap();
        let first = fixture(root.path(), "one.md", b"One");
        let second = fixture(root.path(), "two.md", b"Two");
        let (mut store, _) = open(root.path());
        fs::write(&first, b"First edited").unwrap();
        fs::write(&second, b"Second edited").unwrap();
        let changes = store.refresh_paths(&[first]).unwrap();
        assert_eq!(changes.len(), 1);
        assert!(store.files.values().any(|s| s.text == "Two"));
        assert_eq!(store.refresh().unwrap().len(), 1);
    }
    #[test]
    fn a_file_added_outside_the_folder_keeps_its_identity_after_restart() {
        let root = tempfile::tempdir().unwrap();
        fixture(root.path(), "inside.md", b"Inside");
        let outside = root.path().join("outside.md");
        fs::write(&outside, b"Outside").unwrap();
        let (mut store, _) = open(root.path());
        let note = store.add_file(outside.clone()).unwrap();
        let paths = store.paths();
        drop(store);
        let (store, library) = open(root.path());
        assert!(library.note(&note.id).is_some());
        let restored: HashSet<_> = store.paths().into_iter().collect();
        assert_eq!(restored, paths.into_iter().collect());
    }
    #[test]
    fn a_generated_name_keeps_nothing_a_wiki_link_cannot_reach() {
        let name = |source: &str| file_name(None, source, 0, Default::default());
        for (source, expected) in [
            // A wiki link is an atom, and the title reads it as the label the editor
            // draws — never as its brackets, which is what keeps the stem reachable.
            ("[[Link]] notes", "Link notes"),
            ("[[page|Alias]] notes", "Alias notes"),
            // A line that is only a link is named after it. The name may be the one
            // the link points at, and the note is filed beside it as "Link 2.md"
            // rather than over it.
            ("[[Link]]", "Link"),
            // Brackets the source only spells out do reach the title, and a stem
            // holding them is one no `[[link]]` can name.
            ("[TODO] Fix the bug", "TODO Fix the bug"),
            ("Q3 #planning", "Q3 planning"),
            ("a | b", "a b"),
            ("note^2 squared", "note2 squared"),
            // Spacing the title carries elsewhere is the user's own.
            ("a    b", "a    b"),
            // A separator is not dropped but replaced, so a date still reads as one.
            ("2026/09/21 log", "2026-09-21 log"),
            // Nothing readable is left of a line that was all syntax.
            ("[#^|]", "Untitled"),
        ] {
            assert_eq!(name(source), expected, "{source}");
        }
    }
    #[test]
    fn a_new_note_is_filed_under_a_name_a_link_can_reach() {
        let root = tempfile::tempdir().unwrap();
        let (mut store, mut library) = open(root.path());
        let id = library.active_id.clone();
        library.set_document(&id, doc::from_markdown("Q3 #planning\n\nbody"));
        store.save(&library, &Preferences::default()).unwrap();
        assert_eq!(
            markdown_files(root.path()),
            [root.path().join("notes/Q3 planning.md")]
        );
    }
    #[test]
    fn a_new_note_can_be_named_for_when_it_was_made() {
        assert_eq!(date_time_name(0), "1970-01-01 00.00");
        assert_eq!(date_time_name(1_790_165_100_000), "2026-09-23 12.05");
        let root = tempfile::tempdir().unwrap();
        let (mut store, mut library) = open(root.path());
        library.workspace.new_note_name = crate::storage::NoteNaming::DateTime;
        let id = library.new_note(doc::from_markdown("Meeting notes"));
        let created = library.note(&id).unwrap().created_at;
        store.save(&library, &Preferences::default()).unwrap();
        let path = store.files.get(&id).unwrap().note.path.clone().unwrap();
        let local = created.saturating_add_signed(crate::platform::local_utc_offset() * 1000);
        assert_eq!(
            path.file_name().unwrap().to_string_lossy(),
            format!("{}.md", date_time_name(local))
        );
    }
    #[test]
    fn an_explicit_path_is_used_without_renaming_to_the_title() {
        let root = tempfile::tempdir().unwrap();
        let (mut store, mut library) = open(root.path());
        let id = library.new_note(doc::from_markdown("Draft text"));
        let target = root.path().join("notes/draft.md");
        library.notes.iter_mut().find(|n| n.id == id).unwrap().path = Some(target.clone());
        store.save(&library, &Preferences::default()).unwrap();
        let path = store.files.get(&id).unwrap().note.path.clone().unwrap();
        assert_eq!(path, fs::canonicalize(&target).unwrap());
        assert_eq!(fs::read_to_string(&path).unwrap(), "Draft text\n");
    }
    #[test]
    fn only_explicit_deletion_discards_a_filed_draft() {
        for discard in ["trash", "remove"] {
            let root = tempfile::tempdir().unwrap();
            fixture(root.path(), "one.md", b"One");
            let (mut store, mut library) = open(root.path());
            let id = library.new_note(doc::from_markdown("Draft text"));
            let target = root.path().join("notes/Draft text.md");
            library.notes.iter_mut().find(|n| n.id == id).unwrap().path = Some(target.clone());
            store.save(&library, &Preferences::default()).unwrap();
            match discard {
                "trash" => assert!(library.delete(&id)),
                _ => library.remove(&id),
            }
            store.save(&library, &Preferences::default()).unwrap();
            drop(store);
            let (_, library) = open(root.path());
            if discard == "trash" {
                assert!(library.note(&id).is_none(), "a discarded draft came back");
                assert!(
                    !target.exists(),
                    "the explicitly deleted draft was left on disk"
                );
            } else {
                assert!(library.note(&id).is_some());
                assert!(
                    target.exists(),
                    "forgetting a note must not authorize deleting its file"
                );
            }
        }
    }
    #[test]
    fn deleting_a_note_removes_it_from_the_library_and_trashes_its_file() {
        let root = tempfile::tempdir().unwrap();
        let path = fixture(root.path(), "Keep.md", b"Keep");
        let gone = fixture(root.path(), "Gone.md", b"Gone");
        let (mut store, mut library) = open(root.path());
        let id = library
            .notes
            .iter()
            .find(|note| note.path.as_ref().is_some_and(|p| p.ends_with("Gone.md")))
            .unwrap()
            .id
            .clone();
        assert!(library.delete(&id));
        assert!(library.note(&id).is_none());
        store.save(&library, &Preferences::default()).unwrap();
        assert!(!gone.exists());
        assert!(path.exists());
        assert_eq!(library.notes.len(), 1);
        assert!(!store.files.contains_key(&id));
    }
    #[test]
    fn a_new_file_outside_the_folder_is_remembered_after_restart() {
        let root = tempfile::tempdir().unwrap();
        fixture(root.path(), "one.md", b"One");
        let (mut store, mut library) = open(root.path());
        let id = library.new_note(doc::from_markdown("New note"));
        let target = root.path().join("elsewhere/New note.md");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        library.notes.iter_mut().find(|n| n.id == id).unwrap().path = Some(target.clone());
        store.save(&library, &Preferences::default()).unwrap();
        drop(store);
        let (_, library) = open(root.path());
        assert!(library.note(&id).is_some());
        assert_eq!(fs::read_to_string(target).unwrap(), "New note\n");
    }
    #[test]
    fn failed_write_is_retryable_without_fake_external_conflict() {
        let root = tempfile::tempdir().unwrap();
        let (mut store, mut library) = open(root.path());
        let id = library.active_id.clone();
        let path = fixture(root.path(), "occupied.md", b"Existing");
        library.notes[0].path = Some(path.clone());
        library.set_document(&id, doc::from_markdown("Draft"));
        assert!(store.save(&library, &Preferences::default()).is_err());
        assert!(store.conflicts().is_empty());
        assert!(!store.pending.contains(&id));
        library.notes[0].path = Some(root.path().join("notes/free.md"));
        store.save(&library, &Preferences::default()).unwrap();
        assert_eq!(fs::read(path).unwrap(), b"Existing");
        assert!(
            !store
                .state()
                .join("recovery")
                .join(format!("{id}.json"))
                .exists()
        );
    }
    #[test]
    #[ignore = "filesystem scale measurement; run explicitly"]
    fn workspace_scale_measurement() {
        for count in [1000, 10000] {
            let root = tempfile::tempdir().unwrap();
            for index in 0..count {
                fixture(
                    root.path(),
                    &format!("group-{}/note-{index}.md", index % 10),
                    format!("# Note {index}\n\nText with [a link](https://example.com).\n")
                        .as_bytes(),
                );
            }
            let start = std::time::Instant::now();
            let (mut store, mut library) = open(root.path());
            let cold = start.elapsed();
            assert_eq!(library.notes.len(), count);
            let path = store.paths()[0].1.clone();
            fs::write(&path, b"External text\n").unwrap();
            let start = std::time::Instant::now();
            let changes = store.refresh_paths(&[path]).unwrap();
            let incremental = start.elapsed();
            assert_eq!(changes.len(), 1);
            let editable = library
                .notes
                .iter()
                .find(|n| !store.pending.contains(&n.id))
                .unwrap()
                .id
                .clone();
            library.set_document(
                &editable,
                doc::from_markdown("# Changed\n\nText with [a link](https://example.com)."),
            );
            let start = std::time::Instant::now();
            store.save(&library, &Preferences::default()).unwrap();
            let save = start.elapsed();
            eprintln!(
                "workspace_count={count} cold_ms={} incremental_ms={} save_ms={}",
                cold.as_millis(),
                incremental.as_millis(),
                save.as_millis()
            );
        }
    }
    #[test]
    fn explicit_reload_drops_leftover_recovery_json() {
        let root = tempfile::tempdir().unwrap();
        let path = fixture(root.path(), "note.md", b"Original");
        let (mut store, mut library) = open(root.path());
        let id = library.active_id.clone();
        library.set_document(&id, doc::from_markdown("Local"));
        fs::write(&path, b"Disk").unwrap();
        assert!(store.save(&library, &Preferences::default()).is_err());
        let copies = conflicted_copies_beside(&path);
        assert_eq!(copies.len(), 1);
        store.reload().unwrap();
        drop(store);
        let (_, again) = open(root.path());
        assert!(!again.active_note().conflicted);
        assert_eq!(doc::plain_text(&again.active_note().document), "Disk");
    }

    /// Markdown files beside `path` whose name contains "conflicted copy".
    fn conflicted_copies_beside(path: &Path) -> Vec<PathBuf> {
        let parent = path.parent().unwrap();
        let mut copies: Vec<_> = fs::read_dir(parent)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|p| {
                p.extension().is_some_and(|e| e.eq_ignore_ascii_case("md"))
                    && p.file_name()
                        .is_some_and(|n| n.to_string_lossy().contains("conflicted copy"))
            })
            .collect();
        copies.sort();
        copies
    }
}
