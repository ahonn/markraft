//! In-place Markdown persistence. Application metadata never enters the workspace.
use crate::{
    doc,
    storage::{Library, Note, Notices, Settings, WorkspaceSettings},
};
use markraft_commonmark::SourceDocument;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Component, Path, PathBuf},
    time::UNIX_EPOCH,
};
use uuid::Uuid;

#[derive(Clone)]
struct Saved {
    path: PathBuf,
    bytes: Vec<u8>,
    /// Keep the original source throughout this session, including after saves/undo.
    source: Vec<u8>,
    note: Note,
}
#[derive(Clone, Debug, PartialEq)]
pub enum External {
    Updated { previous: Option<Note>, note: Note },
    Removed(Note),
}
#[derive(Clone, Default, Serialize, Deserialize)]
struct Identity {
    id: String,
    pinned: bool,
    created: u64,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Manifest {
    version: u32,
    paths: HashMap<PathBuf, Identity>,
    workspace: WorkspaceSettings,
    active_id: String,
    loose: Vec<PathBuf>,
}
#[derive(Serialize, Deserialize)]
struct Recovery {
    id: String,
    path: Option<PathBuf>,
    base: Vec<u8>,
    source: Vec<u8>,
    local: String,
    disk: Option<Vec<u8>>,
    conflict: bool,
}
#[derive(Serialize, Deserialize)]
struct Tombstone {
    id: String,
    path: PathBuf,
    source: Vec<u8>,
    bytes: Vec<u8>,
    created: u64,
    updated: u64,
    deleted: u64,
    pinned: bool,
    trashed: bool,
    trash_path: Option<PathBuf>,
}
pub struct Store {
    directory: PathBuf,
    state: PathBuf,
    settings_path: PathBuf,
    _lock: File,
    files: HashMap<String, Saved>,
    deleted: HashMap<String, Saved>,
    pending: HashSet<String>,
    previous: HashMap<String, Saved>,
    reviewed: HashMap<String, Option<Vec<u8>>>,
    manifest: Manifest,
    settings: Settings,
    notices: Notices,
    loose: HashSet<PathBuf>,
    standalone: bool,
    /// Whether the notes folder had to be made because the one the settings named
    /// was no longer there.
    created: bool,
}
impl Store {
    pub fn open(directory: PathBuf, settings_path: PathBuf) -> Result<(Self, Library), String> {
        Self::open_mode(directory, settings_path, None)
    }
    pub fn open_file(path: PathBuf, settings_path: PathBuf) -> Result<(Self, Library), String> {
        let parent = path
            .parent()
            .ok_or("The file has no parent folder")?
            .to_owned();
        Self::open_mode(parent, settings_path, Some(path))
    }
    fn open_mode(
        directory: PathBuf,
        settings_path: PathBuf,
        file: Option<PathBuf>,
    ) -> Result<(Self, Library), String> {
        // A folder the settings still name but the disk no longer has is made
        // again rather than refused, so the app keeps working; that it was made is
        // worth saying, because the notes that were in it are not coming back.
        let created = !directory.exists();
        fs::create_dir_all(&directory).map_err(|e| describe(&directory, &e))?;
        let directory = fs::canonicalize(&directory).map_err(|e| describe(&directory, &e))?;
        let mut hash = 0xcbf29ce484222325u64;
        let key = if file.is_some() {
            b"standalone-files-v1".as_slice()
        } else {
            directory.as_os_str().as_encoded_bytes()
        };
        for byte in key {
            hash = (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
        }
        let settings_directory = settings_path.parent().unwrap_or(Path::new("."));
        fs::create_dir_all(settings_directory).map_err(|e| describe(settings_directory, &e))?;
        let state = fs::canonicalize(settings_directory)
            .map_err(|e| describe(settings_directory, &e))?
            .join("workspaces")
            .join(format!("{hash:016x}"));
        if state.starts_with(&directory) {
            return Err("Application settings must be stored outside the Markdown folder.".into());
        }
        fs::create_dir_all(&state).map_err(|e| describe(&state, &e))?;
        let lock_path = state.join("lock");
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|e| describe(&lock_path, &e))?;
        lock.try_lock()
            .map_err(|_| "Another Markraft instance is already using this folder.".to_owned())?;
        let manifest_path = state.join("manifest.json");
        let manifest =
            match read_optional(&manifest_path).map_err(|e| describe(&manifest_path, &e))? {
                Some(bytes) => serde_json::from_slice(&bytes)
                    .map_err(|e| format!("Cannot read the folder's saved state: {e}"))?,
                None => Manifest::default(),
            };
        let settings = Settings::read(&settings_path)?;
        let mut loose: HashSet<PathBuf> = manifest.loose.iter().cloned().collect();
        let standalone = file.is_some();
        if let Some(file) = file {
            loose.insert(absolute_file(&file)?);
        }
        let mut store = Self {
            directory,
            state,
            settings_path,
            _lock: lock,
            files: HashMap::new(),
            deleted: HashMap::new(),
            pending: HashSet::new(),
            previous: HashMap::new(),
            reviewed: HashMap::new(),
            manifest,
            settings,
            notices: Notices::default(),
            loose,
            created,
            standalone,
        };
        let mut library = store.scan_library()?;
        store.restore_recovery(&mut library)?;
        Ok((store, library))
    }
    /// Whether the notes folder was made on open because the one the settings named
    /// was gone. What was in the old one is not coming back, so the app says so.
    pub fn created_folder(&self) -> bool {
        self.created
    }
    pub fn notices(&self) -> Notices {
        self.notices.clone()
    }
    pub fn directory(&self) -> &Path {
        &self.directory
    }
    pub fn extra_watch_directories(&self) -> Vec<PathBuf> {
        self.loose
            .iter()
            .filter(|p| !p.starts_with(&self.directory))
            .filter_map(|p| p.parent().map(Path::to_owned))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect()
    }
    pub fn conflicts(&self) -> Vec<String> {
        self.reviewed.keys().cloned().collect()
    }
    pub fn paths(&self) -> Vec<(String, PathBuf)> {
        self.files
            .iter()
            .map(|(id, s)| (id.clone(), s.path.clone()))
            .collect()
    }
    fn persist_manifest(&mut self) -> Result<(), String> {
        self.manifest.version = 1;
        self.manifest.loose = self.loose.iter().cloned().collect();
        self.manifest.loose.sort();
        let bytes = serde_json::to_vec_pretty(&self.manifest).map_err(|e| e.to_string())?;
        atomic_write(&self.state.join("manifest.json"), &bytes)
    }
    fn read_path(&self, path: &Path, previous: Option<&Saved>) -> Result<Option<Saved>, String> {
        let Some(bytes) = read_optional(path).map_err(|e| describe(path, &e))? else {
            return Ok(None);
        };
        if let Some(previous) = previous
            && previous.bytes == bytes
        {
            let mut saved = previous.clone();
            let content_error = previous
                .note
                .read_only
                .as_ref()
                .filter(|reason| {
                    reason.starts_with("This file is not UTF-8")
                        || reason.starts_with("Markdown could not be parsed")
                })
                .cloned();
            saved.note.read_only = unsafe_file(path)?.or(content_error);
            return Ok(Some(saved));
        }
        let metadata = fs::symlink_metadata(path).map_err(|e| describe(path, &e))?;
        let modified = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_millis() as u64);
        let identity = self.manifest.paths.get(path);
        let mut read_only = unsafe_file(path)?;
        let text = std::str::from_utf8(&bytes);
        let document = match text {
            Ok(text) => match SourceDocument::parse(doc::schema(), text) {
                Ok(source) => source.document().clone(),
                Err(error) => {
                    read_only = Some(format!("Markdown could not be parsed: {error}"));
                    doc::empty()
                }
            },
            Err(_) => {
                read_only = Some("This file is not UTF-8 text. Open it in another editor to convert its encoding.".into());
                doc::empty()
            }
        };
        let note = Note {
            id: previous
                .map(|s| s.note.id.clone())
                .or_else(|| identity.map(|i| i.id.clone()))
                .unwrap_or_else(|| Uuid::new_v4().to_string()),
            document,
            created_at: identity.map_or(modified, |i| i.created),
            updated_at: modified,
            deleted_at: None,
            pinned: identity.is_some_and(|i| i.pinned),
            path: Some(path.to_owned()),
            read_only,
            conflicted: false,
        };
        Ok(Some(Saved {
            path: path.to_owned(),
            source: bytes.clone(),
            bytes,
            note,
        }))
    }
    fn read_folder(&self) -> Result<HashMap<String, Saved>, String> {
        let mut paths = self.loose.iter().cloned().collect::<Vec<_>>();
        if !self.standalone {
            collect_markdown(&self.directory, &mut paths)?;
        }
        paths.sort();
        paths.dedup();
        let known: HashMap<_, _> = self.files.values().map(|s| (&s.path, s)).collect();
        let mut files = HashMap::new();
        for path in paths {
            if let Some(saved) = self.read_path(&path, known.get(&path).copied())? {
                files.insert(saved.note.id.clone(), saved);
            }
        }
        Ok(files)
    }
    pub fn reload(&mut self) -> Result<Library, String> {
        let library = self.scan_library()?;
        let folder = self.state.join("recovery");
        if folder.exists() {
            for entry in fs::read_dir(&folder).map_err(|e| describe(&folder, &e))? {
                let path = entry.map_err(|e| e.to_string())?.path();
                if path.extension().is_some_and(|e| e == "json")
                    && let Some(id) = path.file_stem().and_then(|s| s.to_str())
                {
                    self.archive_recovery(id)?;
                }
            }
        }
        Ok(library)
    }
    fn scan_library(&mut self) -> Result<Library, String> {
        self.files = self.read_folder()?;
        self.load_deleted()?;
        self.pending.clear();
        self.previous.clear();
        self.reviewed.clear();
        let mut library = Library {
            notes: self
                .files
                .values()
                .chain(self.deleted.values())
                .map(|s| s.note.clone())
                .collect(),
            preferences: self.settings.preferences.clone(),
            workspace: self.manifest.workspace.clone(),
            ..Library::default()
        };
        if library.notes.iter().all(|note| note.deleted_at.is_some()) {
            library.new_note(doc::empty());
        }
        library.active_id = library
            .note(&self.manifest.active_id)
            .filter(|n| n.deleted_at.is_none())
            .map(|n| n.id.clone())
            .unwrap_or_else(|| {
                library
                    .notes
                    .iter()
                    .find(|n| n.deleted_at.is_none())
                    .unwrap()
                    .id
                    .clone()
            });
        for saved in self.files.values() {
            self.manifest
                .paths
                .insert(saved.path.clone(), identity(&saved.note));
        }
        self.persist_manifest()?;
        Ok(library)
    }
    pub fn add_file(&mut self, path: PathBuf) -> Result<Note, String> {
        let path = absolute_file(&path)?;
        if let Some(saved) = self
            .files
            .values()
            .find(|s| s.path == path || same_regular_file(&s.path, &path))
        {
            return Ok(saved.note.clone());
        }
        let saved = self
            .read_path(&path, None)?
            .ok_or("The file no longer exists")?;
        self.loose.insert(path.clone());
        self.settings.open_files = self.loose.iter().cloned().collect();
        self.settings.open_files.sort();
        self.settings.write(&self.settings_path)?;
        self.manifest.paths.insert(path, identity(&saved.note));
        let note = saved.note.clone();
        self.files.insert(note.id.clone(), saved);
        self.persist_manifest()?;
        Ok(note)
    }
    pub fn refresh(&mut self) -> Result<Vec<External>, String> {
        let files = self.read_folder()?;
        let mut changes = Vec::new();
        for (id, saved) in &files {
            let old = self.files.get(id);
            if old.is_none_or(|old| {
                old.bytes != saved.bytes || old.note.read_only != saved.note.read_only
            }) {
                if let Some(old) = old {
                    self.previous
                        .entry(id.clone())
                        .or_insert_with(|| old.clone());
                }
                changes.push(External::Updated {
                    previous: old.map(|s| s.note.clone()),
                    note: saved.note.clone(),
                });
            }
        }
        for (id, saved) in &self.files {
            if !files.contains_key(id) {
                self.previous
                    .entry(id.clone())
                    .or_insert_with(|| saved.clone());
                changes.push(External::Removed(saved.note.clone()));
            }
        }
        for change in &changes {
            let (External::Updated { note, .. } | External::Removed(note)) = change;
            self.pending.insert(note.id.clone());
        }
        self.files = files;
        for saved in self.files.values() {
            self.manifest
                .paths
                .insert(saved.path.clone(), identity(&saved.note));
        }
        self.persist_manifest()?;
        Ok(changes)
    }
    /// File events only read affected paths. Directory events and explicit refreshes rescan.
    pub fn refresh_paths(&mut self, paths: &[PathBuf]) -> Result<Vec<External>, String> {
        let mut staged = Vec::new();
        let mut seen = HashSet::new();
        for path in paths {
            let path = absolute_file(path)?;
            if !seen.insert(path.clone()) {
                continue;
            }
            let relative = path.strip_prefix(&self.directory).ok();
            if !self.loose.contains(&path) && (self.standalone || relative.is_none() || relative.is_some_and(|p|p.components().any(|c| matches!(c,Component::Normal(name) if matches!(name.to_str(),Some(".git"|".obsidian"|".markraft"|".trash"|"node_modules")))))) { continue; }
            let old = self.files.values().find(|s| s.path == path).cloned();
            if old.is_none()
                && !self.loose.contains(&path)
                && fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink())
            {
                continue;
            }
            let next = self.read_path(&path, old.as_ref())?;
            staged.push((old, next));
        }
        let mut changes = Vec::new();
        for (old, next) in staged {
            match (old, next) {
                (Some(old), Some(next))
                    if old.bytes == next.bytes && old.note.read_only == next.note.read_only => {}
                (old, Some(next)) => {
                    let id = next.note.id.clone();
                    if let Some(old) = &old {
                        self.previous
                            .entry(id.clone())
                            .or_insert_with(|| old.clone());
                    }
                    changes.push(External::Updated {
                        previous: old.map(|s| s.note),
                        note: next.note.clone(),
                    });
                    self.pending.insert(id.clone());
                    self.manifest
                        .paths
                        .insert(next.path.clone(), identity(&next.note));
                    self.files.insert(id, next);
                }
                (Some(old), None) => {
                    let id = old.note.id.clone();
                    changes.push(External::Removed(old.note.clone()));
                    self.pending.insert(id.clone());
                    self.files.remove(&id);
                    self.previous.entry(id).or_insert(old);
                }
                (None, None) => {}
            }
        }
        if !changes.is_empty() {
            self.persist_manifest()?;
        }
        Ok(changes)
    }
    pub fn acknowledge(&mut self, ids: &[String]) {
        for id in ids {
            if !self.reviewed.contains_key(id) {
                self.pending.remove(id);
                self.previous.remove(id);
            }
        }
    }
    pub fn markdown(&self, note: &Note) -> Result<String, String> {
        render(
            self.previous
                .get(&note.id)
                .or_else(|| self.files.get(&note.id))
                .or_else(|| self.deleted.get(&note.id)),
            note,
        )
    }
    pub fn recover(&mut self, note: &Note) -> Result<(), String> {
        self.write_recovery(note, true)
    }
    fn write_recovery(&mut self, note: &Note, conflict: bool) -> Result<(), String> {
        let original = self
            .previous
            .get(&note.id)
            .or_else(|| self.files.get(&note.id));
        let path = original
            .map(|s| s.path.clone())
            .or_else(|| note.path.clone());
        let disk = path
            .as_deref()
            .map(read_optional)
            .transpose()
            .map_err(|e| e.to_string())?
            .flatten();
        let base = original.map(|s| s.bytes.clone()).unwrap_or_default();
        let source = original.map(|s| s.source.clone()).unwrap_or_default();
        let local = render(original, note).unwrap_or_else(|_| doc::to_markdown(&note.document));
        let record = Recovery {
            id: note.id.clone(),
            path,
            base,
            source,
            local,
            conflict,
            disk: self
                .reviewed
                .get(&note.id)
                .cloned()
                .unwrap_or_else(|| disk.clone()),
        };
        atomic_write(
            &self
                .state
                .join("recovery")
                .join(format!("{}.json", note.id)),
            &serde_json::to_vec_pretty(&record).map_err(|e| e.to_string())?,
        )?;
        if conflict {
            self.reviewed.entry(note.id.clone()).or_insert(disk);
            self.pending.insert(note.id.clone());
        }
        Ok(())
    }
    fn restore_recovery(&mut self, library: &mut Library) -> Result<(), String> {
        let dir = self.state.join("recovery");
        if !dir.exists() {
            return Ok(());
        }
        for entry in fs::read_dir(&dir).map_err(|e| describe(&dir, &e))? {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let record: Recovery =
                serde_json::from_slice(&fs::read(&path).map_err(|e| describe(&path, &e))?)
                    .map_err(|e| format!("Cannot read recovery draft: {e}"))?;
            let source =
                SourceDocument::parse(doc::schema(), &record.local).map_err(|e| e.to_string())?;
            let id = library.new_note(source.document().clone());
            let note = library.notes.iter_mut().find(|n| n.id == id).unwrap();
            note.id = record.id.clone();
            note.path = record.path.clone();
            note.conflicted = record.conflict;
            library.active_id = note.id.clone();
            let recovered = note.clone();
            library.notes.retain(|n| n.id != record.id);
            library.notes.push(recovered.clone());
            if let Some(path) = record.path {
                self.previous.insert(
                    record.id.clone(),
                    Saved {
                        path,
                        bytes: record.base.clone(),
                        source: record.source,
                        note: recovered,
                    },
                );
            }
            if record.conflict {
                self.pending.insert(record.id.clone());
                self.reviewed.insert(record.id, record.disk);
            }
        }
        if !self.pending.is_empty() {
            self.notices.raise(format!(
                "Recovered unsaved changes. Resolve them before saving. Recovery files: {}",
                dir.display()
            ));
        }
        Ok(())
    }
    /// Capture precisely the disk version displayed by the resolution prompt.
    pub fn review_conflict(&mut self, note: &Note) -> Result<Option<String>, String> {
        self.recover(note)?;
        let path = self
            .previous
            .get(&note.id)
            .or_else(|| self.files.get(&note.id))
            .map(|s| &s.path)
            .or(note.path.as_ref())
            .ok_or("Choose Save As for this draft.")?;
        let disk = read_optional(path).map_err(|e| describe(path, &e))?;
        let text = disk
            .as_ref()
            .map(|bytes| {
                String::from_utf8(bytes.clone()).map_err(|_| {
                    "The disk version is not UTF-8; use another editor to inspect it.".to_owned()
                })
            })
            .transpose()?;
        self.archive_recovery(&note.id)?;
        self.reviewed.insert(note.id.clone(), disk);
        self.recover(note)?;
        Ok(text)
    }
    fn archive_recovery(&self, id: &str) -> Result<(), String> {
        let current = self.state.join("recovery").join(format!("{id}.json"));
        if !current.exists() {
            return Ok(());
        }
        let history = self.state.join("recovery-history");
        fs::create_dir_all(&history).map_err(|e| describe(&history, &e))?;
        let target = history.join(format!("{id}-{}.json", Uuid::new_v4()));
        fs::rename(&current, &target).map_err(|e| describe(&current, &e))
    }
    pub fn resolve_conflict(&mut self, note: &Note) -> Result<Option<Note>, String> {
        let original = self
            .previous
            .get(&note.id)
            .or_else(|| self.files.get(&note.id))
            .cloned();
        let path = original
            .as_ref()
            .map(|s| s.path.clone())
            .or_else(|| note.path.clone())
            .ok_or("Choose Save As for this recovery draft.")?;
        let disk = read_optional(&path).map_err(|e| describe(&path, &e))?;
        let reviewed = self
            .reviewed
            .get(&note.id)
            .ok_or("The conflict has not been reviewed yet.")?;
        if &disk != reviewed {
            return Err("The file changed again. Refresh and review the latest disk version before resolving it.".into());
        }
        let result = self.read_path(&path, None)?.map(|mut saved| {
            saved.note.id = note.id.clone();
            saved.note.conflicted = false;
            let note = saved.note.clone();
            self.files.insert(note.id.clone(), saved);
            note
        });
        if result.is_none() {
            self.files.remove(&note.id);
        }
        self.pending.remove(&note.id);
        self.previous.remove(&note.id);
        self.reviewed.remove(&note.id);
        self.archive_recovery(&note.id)?;
        Ok(result)
    }
    pub fn save(&mut self, library: &Library) -> Result<(), String> {
        library.validate()?;
        self.manifest.workspace = library.workspace.clone();
        let mut errors = Vec::new();
        for note in &library.notes {
            let saved = self
                .files
                .get(&note.id)
                .or_else(|| self.deleted.get(&note.id))
                .cloned();
            if let Some(saved) = &saved
                && saved.note.deleted_at.is_none()
            {
                self.manifest
                    .paths
                    .insert(saved.path.clone(), identity(note));
                if let Some(current) = self.files.get_mut(&note.id) {
                    current.note.pinned = note.pinned;
                }
            }
            if note.conflicted {
                self.recover(note)?;
                continue;
            }
            if self.pending.contains(&note.id) {
                let locally_changed = self
                    .previous
                    .get(&note.id)
                    .or_else(|| self.files.get(&note.id))
                    .is_none_or(|base| {
                        base.note.document != note.document
                            || base.note.deleted_at != note.deleted_at
                    });
                if self.reviewed.contains_key(&note.id) || locally_changed {
                    self.recover(note)?;
                    // The quotes around the title are what the status card's own
                    // sentence is built to sit beside; keep them when rewording.
                    errors.push(format!(
                        "Unsaved changes for “{}” are held in recovery.",
                        note.title()
                    ));
                }
                continue;
            }
            if saved.as_ref().is_some_and(|s| {
                s.note.document == note.document && s.note.deleted_at == note.deleted_at
            }) {
                continue;
            }
            if note.read_only.is_some() {
                errors.push(format!("{} is read-only", note.title()));
                continue;
            }
            if saved.is_none() && (doc::is_blank(&note.document) || note.deleted_at.is_some()) {
                // A note that was never filed has no file to remove, but an earlier
                // save may have left it a private copy. Emptied or thrown away, that
                // copy is no longer what the note says, so it is retired to history
                // rather than restored on the next launch. Conflicted and pending
                // notes continue above and never reach here, and a note that has
                // been given a path keeps its copy for the write that is still due.
                if note.path.is_none() {
                    self.archive_recovery(&note.id)?;
                }
                continue;
            }
            // Without a folder there is nowhere to file a new note, so a standalone
            // draft is held in recovery until the user names a file for it. It is
            // waiting for a location, not failing to be written.
            if saved.is_none() && note.path.is_none() && self.standalone {
                self.write_recovery(note, false)?;
                continue;
            }
            let result = self.save_note(note, saved.as_ref(), library);
            if let Err(error) = result {
                if saved.as_ref().is_some_and(|s| s.note.deleted_at.is_some()) {
                    errors.push(error);
                    continue;
                }
                let external = saved.as_ref().is_some_and(|s| {
                    read_optional(&s.path).is_ok_and(|disk| disk.as_ref() != Some(&s.bytes))
                });
                let recovery = self.write_recovery(note, external);
                errors.push(match recovery {
                    Ok(()) => error,
                    Err(e) => format!("{error}; recovery failed: {e}"),
                });
            }
        }
        self.manifest.active_id.clone_from(&library.active_id);
        self.persist_manifest()?;
        let settings = Settings {
            active_id: library.active_id.clone(),
            preferences: library.preferences.clone(),
            open_files: {
                let mut paths: Vec<_> = self.loose.iter().cloned().collect();
                paths.sort();
                paths
            },
            ..self.settings.clone()
        };
        if settings != self.settings || !self.settings_path.exists() {
            settings.write(&self.settings_path)?;
            self.settings = settings;
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("\n"))
        }
    }
    fn save_note(
        &mut self,
        note: &Note,
        saved: Option<&Saved>,
        library: &Library,
    ) -> Result<(), String> {
        let restoring =
            saved.is_some_and(|s| s.note.deleted_at.is_some()) && note.deleted_at.is_none();
        if let Some(saved) = saved
            && !restoring
        {
            let current = read_optional(&saved.path).map_err(|e| describe(&saved.path, &e))?;
            if current.as_ref() != Some(&saved.bytes) {
                // The quotes around the title are what the status card's own
                // sentence is built to sit beside; keep them when rewording.
                return Err(format!(
                    "“{}” changed on disk. Your changes are preserved in recovery; resolve the conflict before saving.",
                    note.title()
                ));
            }
            if note.deleted_at.is_some() {
                return self.trash_note(note, saved, move_to_trash);
            }
        }
        let bytes = render(saved, note)?.into_bytes();
        let path = match saved {
            Some(s) if restoring => {
                if s.path.exists() {
                    return Err("The original path is occupied. Choose another location with Save As, or restore from the system Trash in Finder.".into());
                }
                s.path.clone()
            }
            Some(s) => s.path.clone(),
            None if note.path.is_some() => {
                let path = note.path.as_ref().unwrap();
                if !path.is_absolute() {
                    return Err("Choose an absolute file location.".into());
                }
                absolute_file(path)?
            }
            None => {
                if self.standalone {
                    // `save` holds standalone drafts in recovery instead of coming
                    // here, so this only catches one that slipped past that check.
                    return Err("Choose a location with Save As before saving this draft.".into());
                }
                let relative = &library.workspace.new_note_directory;
                if !safe_relative(relative) {
                    return Err("The new-note location must stay inside the notes folder.".into());
                }
                let folder = self.directory.join(relative);
                reject_symlink_components(&self.directory, &folder)?;
                fs::create_dir_all(&folder).map_err(|e| describe(&folder, &e))?;
                let name = file_name(note);
                (1..)
                    .map(|n| {
                        folder.join(if n == 1 {
                            format!("{name}.md")
                        } else {
                            format!("{name} {n}.md")
                        })
                    })
                    .find(|p| !p.exists())
                    .unwrap()
            }
        };
        if let Some(saved) = saved {
            atomic_write(
                &self.state.join("backups").join(format!("{}.md", note.id)),
                &saved.bytes,
            )?;
        }
        if path.starts_with(&self.directory) {
            reject_symlink_components(&self.directory, path.parent().unwrap())?;
        }
        match restoring.then(|| self.trashed_file(&note.id)).flatten() {
            // Putting the file back is what restoring means. Writing a fresh one
            // instead would leave the original sitting in the Trash for the user to
            // find later, and would lose the permissions and extended attributes it
            // went in with. An emptied Trash falls through to writing the bytes.
            Some(from) => {
                move_back(&from, &path)?;
                // It comes back as it went in, so only a file someone edited while it
                // sat in the Trash still needs the note's own bytes written over it.
                let current = read_optional(&path).map_err(|e| describe(&path, &e))?;
                if current.as_deref() != Some(bytes.as_slice()) {
                    write_document(&path, &bytes, current.as_deref())?;
                }
            }
            None => write_document(
                &path,
                &bytes,
                if restoring {
                    None
                } else {
                    saved.map(|s| s.bytes.as_slice())
                },
            )?,
        }
        let mut stored = note.clone();
        stored.path = Some(path.clone());
        stored.conflicted = false;
        self.manifest.paths.insert(path.clone(), identity(&stored));
        if restoring {
            let tombstone = self.state.join("deleted").join(format!("{}.json", note.id));
            fs::remove_file(&tombstone).map_err(|e| describe(&tombstone, &e))?;
            self.deleted.remove(&note.id);
        }
        if self.standalone || !path.starts_with(&self.directory) {
            self.loose.insert(path.clone());
        }
        if !self.reviewed.contains_key(&note.id) {
            self.archive_recovery(&note.id)?;
        }
        self.files.insert(
            note.id.clone(),
            Saved {
                path,
                source: saved
                    .map(|s| s.source.clone())
                    .unwrap_or_else(|| bytes.clone()),
                bytes,
                note: stored,
            },
        );
        Ok(())
    }
    fn load_deleted(&mut self) -> Result<(), String> {
        self.deleted.clear();
        let folder = self.state.join("deleted");
        if !folder.exists() {
            return Ok(());
        }
        for entry in fs::read_dir(&folder).map_err(|e| describe(&folder, &e))? {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let record: Tombstone =
                serde_json::from_slice(&fs::read(&path).map_err(|e| describe(&path, &e))?)
                    .map_err(|e| e.to_string())?;
            if !record.trashed && record.path.exists() {
                continue;
            }
            let source = SourceDocument::parse(
                doc::schema(),
                std::str::from_utf8(&record.bytes).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
            let note = Note {
                id: record.id.clone(),
                document: source.document().clone(),
                created_at: record.created,
                updated_at: record.updated,
                deleted_at: Some(record.deleted),
                pinned: record.pinned,
                path: Some(record.path.clone()),
                read_only: None,
                conflicted: false,
            };
            self.deleted.insert(
                record.id,
                Saved {
                    path: record.path,
                    bytes: record.bytes,
                    source: record.source,
                    note,
                },
            );
        }
        Ok(())
    }
    /// Where a deleted note's file went, while it is still there. A Trash the user has
    /// emptied, a record from before the file was moved, and an unreadable tombstone
    /// all read the same way: there is nothing to put back.
    fn trashed_file(&self, id: &str) -> Option<PathBuf> {
        let location = self.state.join("deleted").join(format!("{id}.json"));
        let record: Tombstone = serde_json::from_slice(&fs::read(location).ok()?).ok()?;
        record.trash_path.filter(|path| path.exists())
    }
    fn trash_note(
        &mut self,
        note: &Note,
        saved: &Saved,
        trash: impl FnOnce(&Path) -> Result<PathBuf, String>,
    ) -> Result<(), String> {
        let bytes = render(Some(saved), note)?.into_bytes();
        let mut record = Tombstone {
            id: note.id.clone(),
            path: saved.path.clone(),
            source: saved.source.clone(),
            bytes: bytes.clone(),
            created: note.created_at,
            updated: note.updated_at,
            deleted: note.deleted_at.unwrap_or_else(crate::storage::timestamp),
            pinned: note.pinned,
            trashed: false,
            trash_path: None,
        };
        let location = self.state.join("deleted").join(format!("{}.json", note.id));
        atomic_write(
            &location,
            &serde_json::to_vec_pretty(&record).map_err(|e| e.to_string())?,
        )?;
        record.trash_path = Some(trash(&saved.path)?);
        record.trashed = true;
        atomic_write(
            &location,
            &serde_json::to_vec_pretty(&record).map_err(|e| e.to_string())?,
        )?;
        let mut deleted = note.clone();
        deleted.deleted_at = Some(record.deleted);
        self.deleted.insert(
            note.id.clone(),
            Saved {
                path: saved.path.clone(),
                bytes,
                source: saved.source.clone(),
                note: deleted,
            },
        );
        self.files.remove(&note.id);
        self.manifest.paths.remove(&saved.path);
        self.pending.remove(&note.id);
        Ok(())
    }
    pub fn purge(&mut self, ids: &[String]) -> (Vec<String>, Result<(), String>) {
        let mut removed = Vec::new();
        for id in ids {
            if self.deleted.contains_key(id) {
                let path = self.state.join("deleted").join(format!("{id}.json"));
                if let Err(error) = fs::remove_file(&path) {
                    return (removed, Err(describe(&path, &error)));
                }
                self.deleted.remove(id);
            }
            // Throwing the note away for good retires whatever private copy it still
            // has, or the next launch reads that copy back as a live note. A note
            // whose conflict is still open is not one of these.
            if !self.files.contains_key(id)
                && !self.pending.contains(id)
                && !self.reviewed.contains_key(id)
                && let Err(error) = self.archive_recovery(id)
            {
                return (removed, Err(error));
            }
            removed.push(id.clone());
        }
        (removed, Ok(()))
    }
    pub fn update_settings(&mut self, update: impl FnOnce(&mut Settings)) -> Result<(), String> {
        let mut settings = self.settings.clone();
        update(&mut settings);
        settings.write(&self.settings_path)?;
        self.settings = settings;
        Ok(())
    }
}
fn identity(note: &Note) -> Identity {
    Identity {
        id: note.id.clone(),
        pinned: note.pinned,
        created: note.created_at,
    }
}
fn render(saved: Option<&Saved>, note: &Note) -> Result<String, String> {
    match saved {
        Some(saved) => SourceDocument::parse(
            doc::schema(),
            std::str::from_utf8(&saved.source).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?
        .render(doc::schema(), &note.document)
        .map_err(|e| format!("This edit cannot preserve the original Markdown safely: {e}")),
        None => Ok(format!("{}\n", doc::to_markdown(&note.document))),
    }
}
fn collect_markdown(folder: &Path, paths: &mut Vec<PathBuf>) -> Result<(), String> {
    for entry in fs::read_dir(folder).map_err(|e| describe(folder, &e))? {
        let entry = entry.map_err(|e| describe(folder, &e))?;
        let path = entry.path();
        let kind = entry.file_type().map_err(|e| describe(&path, &e))?;
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            if !matches!(
                entry.file_name().to_str(),
                Some(".git" | ".obsidian" | ".markraft" | ".trash" | "node_modules")
            ) {
                collect_markdown(&path, paths)?
            }
        } else if kind.is_file()
            && path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"))
        {
            paths.push(path)
        }
    }
    Ok(())
}
fn same_regular_file(left: &Path, right: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (fs::symlink_metadata(left), fs::symlink_metadata(right)) {
        (Ok(a), Ok(b)) => {
            a.file_type().is_file()
                && b.file_type().is_file()
                && a.nlink() == 1
                && b.nlink() == 1
                && a.dev() == b.dev()
                && a.ino() == b.ino()
        }
        _ => false,
    }
}
fn absolute_file(path: &Path) -> Result<PathBuf, String> {
    if fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_file()) {
        return fs::canonicalize(path).map_err(|e| describe(path, &e));
    }
    let parent = path.parent().ok_or("The file has no parent")?;
    let parent = fs::canonicalize(parent).map_err(|e| describe(parent, &e))?;
    Ok(parent.join(path.file_name().ok_or("The file has no name")?))
}
pub fn safe_relative(path: &Path) -> bool {
    path.components()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}
fn reject_symlink_components(root: &Path, path: &Path) -> Result<(), String> {
    let mut current = root.to_owned();
    for part in path
        .strip_prefix(root)
        .map_err(|_| "Path is outside the notes folder")?
        .components()
    {
        current.push(part);
        if fs::symlink_metadata(&current).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(
                "A destination directory is a symbolic link; choose a real directory.".into(),
            );
        }
    }
    Ok(())
}
fn unsafe_file(path: &Path) -> Result<Option<String>, String> {
    use std::os::unix::fs::MetadataExt;
    let m = fs::symlink_metadata(path).map_err(|e| describe(path, &e))?;
    Ok(if m.file_type().is_symlink() {
        Some("Symbolic links are read-only in Markraft.".into())
    } else if m.nlink() > 1 {
        Some("Hard-linked files are read-only in Markraft.".into())
    } else if m.permissions().readonly() {
        Some("This file is read-only.".into())
    } else {
        None
    })
}
fn file_name(note: &Note) -> String {
    let title: String = note
        .title()
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | ':' | '\\') {
                '-'
            } else {
                c
            }
        })
        .collect();
    let mut name = title
        .trim_matches(|c: char| c == '.' || c.is_whitespace())
        .to_owned();
    while name.len() > 180 {
        name.pop();
    }
    if name.is_empty() {
        "Untitled".into()
    } else {
        name
    }
}
fn write_document(path: &Path, bytes: &[u8], expected: Option<&[u8]>) -> Result<(), String> {
    if expected.is_some()
        && let Some(reason) = unsafe_file(path)?
    {
        return Err(reason);
    }
    let parent = path.parent().ok_or("The file has no parent")?;
    let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(|e| describe(parent, &e))?;
    temp.write_all(bytes).map_err(|e| describe(path, &e))?;
    if expected.is_some() {
        copy_metadata(path, temp.path())?;
        temp.as_file()
            .set_times(fs::FileTimes::new().set_modified(std::time::SystemTime::now()))
            .map_err(|e| describe(path, &e))?;
    } else {
        // A file that does not exist yet has no permissions of its own to keep, and
        // the temporary file it is written through is private. A note in a folder
        // shared with other tools should sit there like its neighbours, so it takes
        // the folder's own permissions: a 755 folder gives a 644 note.
        inherit_folder_mode(parent, temp.path())?;
    }
    temp.as_file().sync_all().map_err(|e| describe(path, &e))?;
    if read_optional(path)
        .map_err(|e| describe(path, &e))?
        .as_deref()
        != expected
    {
        return Err("The file changed while saving. Your changes were not written.".into());
    }
    if expected.is_some() {
        temp.persist(path).map_err(|e| describe(path, &e.error))?;
    } else {
        temp.persist_noclobber(path)
            .map_err(|e| describe(path, &e.error))?;
    }
    File::open(parent)
        .and_then(|f| f.sync_all())
        .map_err(|e| describe(parent, &e))?;
    Ok(())
}
/// Moves a trashed file back to the place it was deleted from, which the caller has
/// already found unoccupied. macOS trashes to the file's own volume, so a rename is
/// normally enough; a Trash that turns out to be elsewhere is copied across instead.
fn move_back(from: &Path, to: &Path) -> Result<(), String> {
    if fs::rename(from, to).is_ok() {
        return Ok(());
    }
    fs::copy(from, to).map_err(|e| describe(from, &e))?;
    fs::remove_file(from).map_err(|e| describe(from, &e))
}
/// The permissions a new file in `folder` should have: the folder's own, without the
/// execute bits a Markdown file has no use for.
#[cfg(unix)]
fn inherit_folder_mode(folder: &Path, file: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::metadata(folder)
        .map_err(|e| describe(folder, &e))?
        .permissions()
        .mode()
        & 0o666;
    fs::set_permissions(file, fs::Permissions::from_mode(mode)).map_err(|e| describe(file, &e))
}
#[cfg(not(unix))]
fn inherit_folder_mode(_folder: &Path, _file: &Path) -> Result<(), String> {
    Ok(())
}
#[cfg(target_os = "macos")]
fn copy_metadata(from: &Path, to: &Path) -> Result<(), String> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    unsafe extern "C" {
        fn copyfile(
            from: *const std::ffi::c_char,
            to: *const std::ffi::c_char,
            state: *mut std::ffi::c_void,
            flags: u32,
        ) -> i32;
    }
    let source = CString::new(from.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    let target = CString::new(to.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    // COPYFILE_ACL | COPYFILE_STAT | COPYFILE_XATTR, without copying file data.
    if unsafe { copyfile(source.as_ptr(), target.as_ptr(), std::ptr::null_mut(), 7) } != 0 {
        return Err(describe(from, &io::Error::last_os_error()));
    }
    Ok(())
}
#[cfg(not(target_os = "macos"))]
fn copy_metadata(from: &Path, to: &Path) -> Result<(), String> {
    fs::set_permissions(
        to,
        fs::metadata(from)
            .map_err(|e| describe(from, &e))?
            .permissions(),
    )
    .map_err(|e| describe(to, &e))
}
#[cfg(target_os = "macos")]
fn move_to_trash(path: &Path) -> Result<PathBuf, String> {
    use objc2::rc::Retained;
    use objc2_foundation::{NSFileManager, NSURL};
    let url = NSURL::fileURLWithPath(&objc2_foundation::NSString::from_str(
        &path.to_string_lossy(),
    ));
    let manager = NSFileManager::defaultManager();
    let mut result: Option<Retained<NSURL>> = None;
    manager
        .trashItemAtURL_resultingItemURL_error(&url, Some(&mut result))
        .map_err(|e| e.to_string())?;
    result
        .and_then(|url| url.path())
        .map(|path| PathBuf::from(path.to_string()))
        .ok_or_else(|| "The system did not return the Trash location.".into())
}
#[cfg(not(target_os = "macos"))]
fn move_to_trash(_path: &Path) -> Result<PathBuf, String> {
    Err("System Trash is not available on this platform.".into())
}
pub fn backup(library: &Library) -> Result<Vec<u8>, String> {
    serde_json::to_vec_pretty(&serde_json::json!({"format":"markraft-recovery-v1","notes":library.notes.iter().map(|n|serde_json::json!({"id":n.id,"path":n.path,"markdown":doc::to_markdown(&n.document)})).collect::<Vec<_>>()})).map_err(|e|e.to_string())
}
/// Milliseconds since the epoch as (year, month, day, hour, minute, second, millisecond).
pub(crate) fn civil(milliseconds: u64) -> (i64, u32, u32, u32, u32, u32, u32) {
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

/// The one place where a file-system failure becomes something a person can act
/// on. An `io::Error` reads as the operating system's own report of a system
/// call — the kind of sentence that belongs in a log, not in a window — so the
/// raw text is printed for a bug report and reaches the interface only inside
/// the parentheses of the last resort.
pub(crate) fn describe(path: &Path, error: &io::Error) -> String {
    eprintln!("Markraft: {}: {error} ({:?})", path.display(), error.kind());
    message(path, error)
}

/// The half of [`describe`] the user reads, separated so it can be tested
/// without the log.
fn message(path: &Path, error: &io::Error) -> String {
    let name = file_label(path);
    match error.kind() {
        io::ErrorKind::NotFound => format!(
            "“{name}” is no longer there. It may have been renamed, moved or deleted; \
             choose the notes folder again."
        ),
        io::ErrorKind::PermissionDenied => format!(
            "Markraft is not allowed to use “{name}”. Check its permissions in Finder, \
             or choose another notes folder."
        ),
        io::ErrorKind::AlreadyExists => {
            format!("“{name}” already exists. Rename or move it, then try again.")
        }
        io::ErrorKind::InvalidFilename => format!(
            "“{name}” is not a name this disk accepts. Shorten the note's first line, \
             then try again."
        ),
        io::ErrorKind::StorageFull => {
            format!("The disk has no room left for “{name}”. Free some space, then try again.")
        }
        io::ErrorKind::ReadOnlyFilesystem => format!(
            "“{name}” is on a disk that cannot be written to. Choose a notes folder \
             on a disk you can write to."
        ),
        io::ErrorKind::TimedOut => format!(
            "“{name}” did not respond in time. If it is on a network drive or in iCloud, \
             check the connection and try again."
        ),
        _ => format!("Markraft could not use “{name}” ({error})."),
    }
}

/// A file or folder as the user knows it. The whole path belongs in the log; in
/// a message it would bury the sentence.
pub(crate) fn file_label(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn read_optional(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(parent).map_err(|error| describe(parent, &error))?;
    let mut output =
        tempfile::NamedTempFile::new_in(parent).map_err(|error| describe(parent, &error))?;
    output
        .write_all(bytes)
        .and_then(|_| output.as_file().sync_all())
        .map_err(|error| describe(path, &error))?;
    output
        .persist(path)
        .map_err(|error| describe(path, &error.error))?;
    // Sync the directory entry as well as the file contents when the platform supports it.
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| describe(parent, &error))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn open(root: &Path) -> (Store, Library) {
        Store::open(root.join("notes"), root.join("settings.json")).unwrap()
    }
    fn fixture(root: &Path, name: &str, text: &[u8]) -> PathBuf {
        let path = root.join("notes").join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        path
    }
    #[test]
    fn opening_and_pinning_do_not_touch_user_files() {
        let root = tempfile::tempdir().unwrap();
        let path = fixture(
            root.path(),
            "nested/source.md",
            b"---\nid: user-data\npinned: true\n---\n\nTitle\n=====\n",
        );
        let bytes = fs::read(&path).unwrap();
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        let (mut store, mut library) = open(root.path());
        let id = library.active_id.clone();
        library.notes[0].pinned = true;
        store.save(&library).unwrap();
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
        let path=fixture(root.path(),"projects/Keep This Name.md",b"\xef\xbb\xbf---\r\nid: custom\r\n# comment\r\n---\r\n\r\nhello [site][s]\r\n\r\n[s]: https://example.com\r\n");
        let (mut store, mut library) = open(root.path());
        let id = library.active_id.clone();
        library.set_document(
            &id,
            doc::from_markdown("changed [site](https://example.com)"),
        );
        store.save(&library).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "\u{feff}---\r\nid: custom\r\n# comment\r\n---\r\n\r\nchanged [site][s]\r\n\r\n[s]: https://example.com\r\n"
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
    fn concurrent_edits_keep_base_local_disk_outside_workspace() {
        let root = tempfile::tempdir().unwrap();
        let path = fixture(root.path(), "note.md", b"Original\n");
        let (mut store, mut library) = open(root.path());
        let id = library.active_id.clone();
        library.set_document(&id, doc::from_markdown("Local"));
        fs::write(&path, b"External\n").unwrap();
        assert!(store.save(&library).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"External\n");
        let record: Recovery = serde_json::from_slice(
            &fs::read(store.state.join("recovery").join(format!("{id}.json"))).unwrap(),
        )
        .unwrap();
        assert_eq!(record.base, b"Original\n");
        assert!(record.local.contains("Local"));
        assert_eq!(record.disk, Some(b"External\n".to_vec()));
        drop(store);
        let (_, again) = open(root.path());
        assert_eq!(again.active_note().id, id);
        assert!(again.active_note().conflicted);
        assert_eq!(doc::plain_text(&again.active_note().document), "Local");
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
        store.recover(library.active_note()).unwrap();
        assert!(store.save(&library).is_err());
        assert!(!path.exists());
        assert_eq!(fs::read_dir(root.path().join("notes")).unwrap().count(), 0);
    }
    #[test]
    fn resolution_rejects_an_unreviewed_disk_version() {
        let root = tempfile::tempdir().unwrap();
        let path = fixture(root.path(), "note.md", b"Original");
        let (mut store, mut library) = open(root.path());
        let id = library.active_id.clone();
        library.set_document(&id, doc::from_markdown("Local"));
        fs::write(&path, b"External").unwrap();
        store.recover(library.active_note()).unwrap();
        fs::write(&path, b"New external").unwrap();
        assert!(store.resolve_conflict(library.active_note()).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"New external");
    }
    #[test]
    fn read_only_encoding_and_links_are_protected() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let path = fixture(root.path(), "invalid.md", b"invalid\xff");
        let linked = fixture(root.path(), "linked.md", b"hardlink");
        fs::hard_link(&linked, root.path().join("hardlink.md")).unwrap();
        symlink(&linked, root.path().join("notes/symlink.md")).unwrap();
        let (mut store, library) = open(root.path());
        assert_eq!(library.notes.len(), 2);
        assert!(library.notes.iter().all(|n| n.read_only.is_some()));
        store.save(&library).unwrap();
        assert_eq!(fs::read(path).unwrap(), b"invalid\xff");
        let note = store
            .add_file(root.path().join("notes/symlink.md"))
            .unwrap();
        assert!(note.read_only.is_some());
    }
    #[test]
    fn new_files_have_no_metadata_and_names_stay_stable() {
        let root = tempfile::tempdir().unwrap();
        let (mut store, mut library) = open(root.path());
        let id = library.active_id.clone();
        library.workspace.new_note_directory = "inbox".into();
        library.set_document(&id, doc::from_markdown("Title"));
        store.save(&library).unwrap();
        let path = store.paths()[0].1.clone();
        assert_eq!(fs::read_to_string(&path).unwrap(), "Title\n");
        library.set_document(&id, doc::from_markdown("Changed"));
        store.save(&library).unwrap();
        assert_eq!(store.paths()[0].1, fs::canonicalize(&path).unwrap());
        assert!(path.ends_with("inbox/Title.md"));
    }
    #[test]
    fn no_clobber_write_and_permissions_are_preserved() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let path = fixture(root.path(), "note.md", b"Original");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        assert!(write_document(&path, b"Wrong", None).is_err());
        write_document(&path, b"Edited", Some(b"Original")).unwrap();
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }
    #[test]
    fn standalone_opens_only_requested_file() {
        let root = tempfile::tempdir().unwrap();
        let path = fixture(root.path(), "note.md", b"Open me");
        fixture(root.path(), "other.md", b"Other");
        let (_, library) = Store::open_file(path, root.path().join("settings.json")).unwrap();
        assert_eq!(library.notes.len(), 1);
    }
    #[test]
    fn undo_after_save_restores_original_source() {
        let root = tempfile::tempdir().unwrap();
        let path = fixture(root.path(), "note.md", b"Title\n=====\n\noriginal\n\n");
        let (mut store, mut library) = open(root.path());
        let id = library.active_id.clone();
        let original = library.active_note().document.clone();
        library.set_document(&id, doc::from_markdown("# Title\n\nchanged"));
        store.save(&library).unwrap();
        library.set_document(&id, original);
        store.save(&library).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"Title\n=====\n\noriginal\n\n");
    }
    #[test]
    fn resolved_conflicts_archive_local_work_and_allow_fresh_review() {
        let root = tempfile::tempdir().unwrap();
        let path = fixture(root.path(), "note.md", b"Original");
        let (mut store, mut library) = open(root.path());
        let id = library.active_id.clone();
        library.set_document(&id, doc::from_markdown("Local"));
        fs::write(&path, b"External").unwrap();
        assert_eq!(
            store.review_conflict(library.active_note()).unwrap(),
            Some("External".into())
        );
        fs::write(&path, b"New external").unwrap();
        assert!(store.resolve_conflict(library.active_note()).is_err());
        assert_eq!(
            store.review_conflict(library.active_note()).unwrap(),
            Some("New external".into())
        );
        let disk = store
            .resolve_conflict(library.active_note())
            .unwrap()
            .unwrap();
        assert_eq!(doc::plain_text(&disk.document), "New external");
        assert!(
            fs::read_dir(store.state.join("recovery-history"))
                .unwrap()
                .count()
                >= 2
        );
        assert!(
            !store
                .state
                .join("recovery")
                .join(format!("{id}.json"))
                .exists()
        );
    }
    #[test]
    fn explicit_new_file_location_is_used_without_overwriting() {
        let root = tempfile::tempdir().unwrap();
        let (mut store, mut library) = open(root.path());
        let path = root.path().join("explicit.md");
        let id = library.active_id.clone();
        library.notes[0].path = Some(path.clone());
        library.set_document(&id, doc::from_markdown("Exact place"));
        store.save(&library).unwrap();
        assert_eq!(fs::read_to_string(path).unwrap(), "Exact place\n");
        assert_eq!(fs::read_dir(root.path().join("notes")).unwrap().count(), 0);
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn atomic_replacement_preserves_extended_attributes() {
        let root = tempfile::tempdir().unwrap();
        let path = fixture(root.path(), "note.md", b"Original");
        let status = std::process::Command::new("/usr/bin/xattr")
            .args(["-w", "com.markraft.test", "retained"])
            .arg(&path)
            .status()
            .unwrap();
        assert!(status.success());
        write_document(&path, b"Edited", Some(b"Original")).unwrap();
        let output = std::process::Command::new("/usr/bin/xattr")
            .args(["-p", "com.markraft.test"])
            .arg(path)
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"retained\n");
    }
    #[test]
    fn deleted_notes_restore_source_after_restart_without_clobbering() {
        let root = tempfile::tempdir().unwrap();
        let original = b"\xef\xbb\xbf---\r\ncustom: true\r\n---\r\n\r\nTitle\r\n=====\r\n";
        let path = fixture(root.path(), "note.md", original);
        let (mut store, mut library) = open(root.path());
        let id = library.active_id.clone();
        library.delete(&id);
        let note = library.note(&id).unwrap().clone();
        let saved = store.files.get(&id).unwrap().clone();
        let trash = root.path().join("test-trash.md");
        store
            .trash_note(&note, &saved, |source| {
                fs::rename(source, &trash).map_err(|e| e.to_string())?;
                Ok(trash.clone())
            })
            .unwrap();
        store.persist_manifest().unwrap();
        assert!(!path.exists());
        drop(store);
        fixture(root.path(), "note.md", b"Another document");
        let (mut store, mut library) = open(root.path());
        assert!(library.note(&id).unwrap().deleted_at.is_some());
        assert!(library.restore(&id));
        assert!(store.save(&library).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"Another document");
        fs::remove_file(&path).unwrap();
        store.save(&library).unwrap();
        let restored = store.files.get(&id).unwrap();
        assert_eq!(restored.bytes, original);
        assert!(restored.path.ends_with("note.md"));
    }
    #[test]
    fn restoring_a_note_puts_the_trashed_file_back_rather_than_copying_it() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let original = b"# Note\n\nBody.\n";
        let path = fixture(root.path(), "note.md", original);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let (mut store, mut library) = open(root.path());
        let id = library.active_id.clone();
        library.delete(&id);
        let note = library.note(&id).unwrap().clone();
        let saved = store.files.get(&id).unwrap().clone();
        let trash = root.path().join("trash/note.md");
        fs::create_dir_all(trash.parent().unwrap()).unwrap();
        store
            .trash_note(&note, &saved, |source| {
                fs::rename(source, &trash).map_err(|e| e.to_string())?;
                Ok(trash.clone())
            })
            .unwrap();
        assert!(!path.exists() && trash.exists());
        assert!(library.restore(&id));
        store.save(&library).unwrap();
        assert!(!trash.exists(), "the trashed file was left behind");
        assert_eq!(fs::read(&path).unwrap(), original);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644,
            "the file came back as it went in"
        );
    }
    #[test]
    fn a_restore_falls_back_to_writing_the_bytes_once_the_trash_is_emptied() {
        let root = tempfile::tempdir().unwrap();
        let original = b"# Note\n\nBody.\n";
        let path = fixture(root.path(), "note.md", original);
        let (mut store, mut library) = open(root.path());
        let id = library.active_id.clone();
        library.delete(&id);
        let note = library.note(&id).unwrap().clone();
        let saved = store.files.get(&id).unwrap().clone();
        let trash = root.path().join("trash/note.md");
        fs::create_dir_all(trash.parent().unwrap()).unwrap();
        store
            .trash_note(&note, &saved, |source| {
                fs::rename(source, &trash).map_err(|e| e.to_string())?;
                Ok(trash.clone())
            })
            .unwrap();
        fs::remove_file(&trash).unwrap();
        assert!(library.restore(&id));
        store.save(&library).unwrap();
        assert_eq!(fs::read(&path).unwrap(), original);
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
        store.save(&library).unwrap();
        let path = store.files.get(&id).unwrap().path.clone();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644,
            "a new note sits in the folder like its neighbours"
        );
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o700)).unwrap();
        let private = library.new_note(doc::from_markdown("Private note"));
        store.save(&library).unwrap();
        let path = store.files.get(&private).unwrap().path.clone();
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
        fs::write(path, b"Disk").unwrap();
        store.refresh().unwrap();
        assert!(store.save(&library).is_err());
        assert_eq!(store.conflicts(), vec![id.clone()]);
        library
            .notes
            .iter_mut()
            .find(|n| n.id == id)
            .unwrap()
            .conflicted = true;
        store.save(&library).unwrap();
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
        assert!(store.files.values().any(|s| s.bytes == b"Two"));
        assert_eq!(store.refresh().unwrap().len(), 1);
    }
    #[test]
    fn standalone_identity_does_not_depend_on_first_file_parent() {
        let root = tempfile::tempdir().unwrap();
        let first = fixture(root.path(), "z/Z.md", b"Z");
        let second = fixture(root.path(), "a/A.md", b"A");
        let settings = root.path().join("settings.json");
        let (mut store, _) = Store::open_file(first.clone(), settings.clone()).unwrap();
        let note = store.add_file(second.clone()).unwrap();
        let paths = store.paths();
        drop(store);
        let (store, library) = Store::open_file(second, settings).unwrap();
        assert_eq!(library.notes.len(), 2);
        assert!(library.note(&note.id).is_some());
        let restored: HashSet<_> = store.paths().into_iter().collect();
        assert_eq!(restored, paths.into_iter().collect());
    }
    #[test]
    fn a_standalone_draft_survives_until_it_is_given_a_file() {
        let root = tempfile::tempdir().unwrap();
        let first = fixture(root.path(), "one.md", b"One");
        let settings = root.path().join("settings.json");
        let (mut store, mut library) = Store::open_file(first.clone(), settings.clone()).unwrap();
        let id = library.new_note(doc::from_markdown("Draft text"));
        // There is no folder to file it in, which is not a failure to write it.
        store.save(&library).unwrap();
        assert!(
            store
                .state
                .join("recovery")
                .join(format!("{id}.json"))
                .exists()
        );
        drop(store);
        let (mut store, mut library) = Store::open_file(first, settings).unwrap();
        let draft = library.note(&id).expect("the draft came back");
        assert_eq!(draft.path, None);
        assert_eq!(doc::plain_text(&draft.document), "Draft text");
        let target = root.path().join("notes/draft.md");
        library.notes.iter_mut().find(|n| n.id == id).unwrap().path = Some(target.clone());
        store.save(&library).unwrap();
        assert_eq!(fs::read_to_string(target).unwrap(), "Draft text\n");
        assert!(
            !store
                .state
                .join("recovery")
                .join(format!("{id}.json"))
                .exists()
        );
    }
    #[test]
    fn a_discarded_standalone_draft_does_not_come_back() {
        for discard in ["trash", "blank", "purge"] {
            let root = tempfile::tempdir().unwrap();
            let first = fixture(root.path(), "one.md", b"One");
            let settings = root.path().join("settings.json");
            let (mut store, mut library) =
                Store::open_file(first.clone(), settings.clone()).unwrap();
            let id = library.new_note(doc::from_markdown("Draft text"));
            store.save(&library).unwrap();
            match discard {
                "trash" => assert!(library.delete(&id)),
                "blank" => assert!(library.set_document(&id, doc::empty())),
                _ => library.remove(&id),
            }
            if discard == "purge" {
                assert!(store.purge(std::slice::from_ref(&id)).1.is_ok());
            }
            store.save(&library).unwrap();
            drop(store);
            let (_, library) = Store::open_file(first, settings).unwrap();
            assert!(
                library
                    .note(&id)
                    .is_none_or(|note| note.deleted_at.is_some() || doc::is_blank(&note.document)),
                "a {discard}ed draft came back"
            );
        }
    }
    #[test]
    fn a_conflicted_note_keeps_its_recovery_through_a_purge() {
        let root = tempfile::tempdir().unwrap();
        let path = fixture(root.path(), "note.md", b"Original");
        let (mut store, mut library) = open(root.path());
        let id = library.active_id.clone();
        library.set_document(&id, doc::from_markdown("Local"));
        fs::write(&path, b"External").unwrap();
        assert!(store.save(&library).is_err());
        let record = store.state.join("recovery").join(format!("{id}.json"));
        assert!(record.exists());
        // An unrelated purge never reaches a note whose conflict is still open.
        assert!(store.purge(std::slice::from_ref(&id)).1.is_ok());
        assert!(record.exists());
        library
            .notes
            .iter_mut()
            .find(|n| n.id == id)
            .unwrap()
            .conflicted = true;
        store.save(&library).unwrap();
        assert!(record.exists());
    }
    #[test]
    fn standalone_new_file_is_remembered_after_restart() {
        let root = tempfile::tempdir().unwrap();
        let first = fixture(root.path(), "one.md", b"One");
        let settings = root.path().join("settings.json");
        let (mut store, mut library) = Store::open_file(first.clone(), settings.clone()).unwrap();
        let id = library.new_note(doc::from_markdown("New note"));
        let target = root.path().join("notes/new.md");
        library.notes.iter_mut().find(|n| n.id == id).unwrap().path = Some(target.clone());
        store.save(&library).unwrap();
        drop(store);
        let (_, library) = Store::open_file(first, settings).unwrap();
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
        assert!(store.save(&library).is_err());
        assert!(store.conflicts().is_empty());
        assert!(!store.pending.contains(&id));
        library.notes[0].path = Some(root.path().join("notes/free.md"));
        store.save(&library).unwrap();
        assert_eq!(fs::read(path).unwrap(), b"Existing");
        assert!(
            !store
                .state
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
            store.save(&library).unwrap();
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
    fn explicit_reload_archives_the_recovery_instead_of_reopening_it_later() {
        let root = tempfile::tempdir().unwrap();
        let path = fixture(root.path(), "note.md", b"Original");
        let (mut store, mut library) = open(root.path());
        let id = library.active_id.clone();
        library.set_document(&id, doc::from_markdown("Local"));
        fs::write(path, b"Disk").unwrap();
        assert!(store.save(&library).is_err());
        store.reload().unwrap();
        drop(store);
        let (_, again) = open(root.path());
        assert!(!again.active_note().conflicted);
        assert_eq!(doc::plain_text(&again.active_note().document), "Disk");
    }
}
