//! In-place Markdown persistence. Application metadata never enters the workspace.
use crate::{
    doc,
    fs::{
        StoreError, atomic_write, copy_metadata, describe, inherit_folder_mode, move_to_trash,
        move_without_replacing, read_optional, same_regular_file,
    },
    storage::{Library, Note, Notices, Preferences, Settings, WorkspaceSettings},
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
#[serde(default)]
struct Identity {
    id: String,
    pinned: bool,
    created: u64,
    /// The file left while Markraft was watching, which the window reported then.
    /// The identity stays for a file that comes straight back, as one does when an
    /// editor replaces it; the next launch drops it without saying so twice.
    gone: bool,
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
/// A leftover draft from a version that kept them. Only read now, and read
/// forgivingly: a field this version does not know is not worth losing the text over.
#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct Recovery {
    id: String,
    path: Option<PathBuf>,
    base: Vec<u8>,
    source: Vec<u8>,
    local: String,
    disk: Option<Vec<u8>>,
    conflict: bool,
}
pub struct Store {
    directory: PathBuf,
    state: PathBuf,
    settings_path: PathBuf,
    _lock: File,
    files: HashMap<String, Saved>,
    pending: HashSet<String>,
    previous: HashMap<String, Saved>,
    /// Notes whose local edits were kept as a conflicted copy because disk won.
    disk_won: HashSet<String>,
    /// Where the last save's deletions landed in the Trash, for the window to reveal.
    trashed: Vec<PathBuf>,
    manifest: Manifest,
    settings: Settings,
    notices: Notices,
    loose: HashSet<PathBuf>,
    /// The style new Markdown is spelled in; the application hands over its own
    /// through [`Store::set_house`], so a saved file follows the preferences the
    /// editor does.
    house: markraft_commonmark::HouseStyleHandle,
}
impl Store {
    /// Open the notes in `directory`, keeping the folder's state under the
    /// application's settings folder. `settings` is the settings file as the
    /// caller read it — the store keeps the copy it writes `settings.json` from.
    pub fn open(
        directory: PathBuf,
        settings_path: PathBuf,
        settings: Settings,
    ) -> Result<(Self, Library), StoreError> {
        if !directory.exists() {
            return Err(describe(
                &directory,
                &io::Error::new(io::ErrorKind::NotFound, "not found"),
            ));
        }
        let directory = fs::canonicalize(&directory).map_err(|e| describe(&directory, &e))?;
        let mut hash = 0xcbf29ce484222325u64;
        for byte in directory.as_os_str().as_encoded_bytes() {
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
        lock.try_lock().map_err(|_| {
            StoreError::Locked("Another Markraft instance is already using this folder.".into())
        })?;
        let manifest_path = state.join("manifest.json");
        let manifest =
            match read_optional(&manifest_path).map_err(|e| describe(&manifest_path, &e))? {
                Some(bytes) => serde_json::from_slice(&bytes).map_err(|e| StoreError::Json {
                    path: manifest_path.clone(),
                    detail: e.to_string(),
                })?,
                None => Manifest::default(),
            };
        let loose: HashSet<PathBuf> = manifest.loose.iter().cloned().collect();
        let mut store = Self {
            directory,
            state,
            settings_path,
            _lock: lock,
            files: HashMap::new(),
            pending: HashSet::new(),
            previous: HashMap::new(),
            disk_won: HashSet::new(),
            trashed: Vec::new(),
            manifest,
            settings,
            notices: Notices::default(),
            loose,
            house: Default::default(),
        };
        let mut library = store.scan_library()?;
        store.restore_recovery(&mut library);
        Ok((store, library))
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
    /// Notes whose local edits were kept as a conflicted copy because disk won.
    pub fn conflicts(&self) -> Vec<String> {
        self.disk_won.iter().cloned().collect()
    }
    /// Where the last save's deletions landed in the Trash. Empty when the platform
    /// did not say, which is not a failure: the file is still gone.
    pub fn trashed(&self) -> Vec<PathBuf> {
        self.trashed.clone()
    }
    pub fn paths(&self) -> Vec<(String, PathBuf)> {
        self.files
            .iter()
            .map(|(id, s)| (id.clone(), s.path.clone()))
            .collect()
    }
    fn persist_manifest(&mut self) -> Result<(), StoreError> {
        self.manifest.version = 1;
        self.manifest.loose = self.loose.iter().cloned().collect();
        self.manifest.loose.sort();
        let path = self.state.join("manifest.json");
        let bytes = serde_json::to_vec_pretty(&self.manifest).map_err(|e| StoreError::Json {
            path: path.clone(),
            detail: e.to_string(),
        })?;
        atomic_write(&path, &bytes)
    }
    fn read_path(
        &self,
        path: &Path,
        previous: Option<&Saved>,
    ) -> Result<Option<Saved>, StoreError> {
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
    fn read_folder(&self) -> Result<HashMap<String, Saved>, StoreError> {
        let mut paths = self.loose.iter().cloned().collect::<Vec<_>>();
        collect_markdown(&self.directory, &mut paths)?;
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
    pub fn reload(&mut self) -> Result<Library, StoreError> {
        let library = self.scan_library()?;
        let folder = self.state.join("recovery");
        if folder.exists() {
            for entry in fs::read_dir(&folder).map_err(|e| describe(&folder, &e))? {
                let path = entry.map_err(|e| e.to_string())?.path();
                if path.extension().is_some_and(|e| e == "json") {
                    let _ = fs::remove_file(&path);
                }
            }
        }
        Ok(library)
    }
    fn scan_library(&mut self) -> Result<Library, StoreError> {
        self.files = self.read_folder()?;
        self.pending.clear();
        self.previous.clear();
        self.disk_won.clear();
        let mut library = Library {
            notes: self.files.values().map(|s| s.note.clone()).collect(),
            workspace: self.manifest.workspace.clone(),
            ..Library::default()
        };
        if library.notes.is_empty() {
            library.new_note(doc::empty());
        }
        library.active_id = library
            .note(&self.manifest.active_id)
            .map(|n| n.id.clone())
            .unwrap_or_else(|| library.notes[0].id.clone());
        self.forget_missing();
        for saved in self.files.values() {
            self.manifest
                .paths
                .insert(saved.path.clone(), identity(&saved.note));
        }
        self.persist_manifest()?;
        Ok(library)
    }
    /// A file that left while Markraft was not watching has no note to vanish from
    /// the window, so the list would simply be shorter with nothing said. Name what
    /// went, once, and stop expecting it.
    fn forget_missing(&mut self) {
        let known: HashSet<_> = self.files.values().map(|saved| &saved.path).collect();
        let mut missing: Vec<_> = self
            .manifest
            .paths
            .iter()
            .filter(|(path, _)| {
                !known.contains(path)
                    && fs::symlink_metadata(path)
                        .is_err_and(|error| error.kind() == io::ErrorKind::NotFound)
            })
            .map(|(path, identity)| (path.clone(), identity.gone))
            .collect();
        missing.sort();
        for (path, _) in &missing {
            self.manifest.paths.remove(path);
        }
        // One the window already reported leaving is dropped without a second notice.
        let missing: Vec<_> = missing
            .into_iter()
            .filter_map(|(path, reported)| (!reported).then_some(path))
            .collect();
        match missing.as_slice() {
            [] => {}
            [path] => self.notices.raise(format!(
                "“{}” was removed outside Markraft, so the note is gone too.",
                path.file_name()
                    .map(|name| name.to_string_lossy())
                    .unwrap_or_default()
            )),
            _ => self.notices.raise(format!(
                "{} notes' files were removed outside Markraft, so those notes are gone too.",
                missing.len()
            )),
        }
    }
    /// Give a note's file another name, in the folder it is already in.
    ///
    /// The file is moved rather than rewritten, so it keeps its bytes, permissions and
    /// extended attributes, and the note keeps its identity: the manifest follows the
    /// file to its new path, which is what stops the watcher from reading the move as
    /// one note leaving and a stranger arriving.
    pub fn rename(&mut self, id: &str, name: &str) -> Result<PathBuf, StoreError> {
        let saved = self
            .files
            .get(id)
            .ok_or("Save this note to a file before renaming it.")?
            .clone();
        if self.pending.contains(id) {
            return Err("A note changed on disk. Wait a moment, then try renaming again.".into());
        }
        if let Some(reason) = unsafe_file(&saved.path)? {
            return Err(reason.into());
        }
        let extension = saved
            .path
            .extension()
            .map(|e| e.to_string_lossy().into_owned())
            .unwrap_or_else(|| "md".into());
        let stem = typed_stem(name, &extension)?;
        let target = saved.path.with_file_name(format!("{stem}.{extension}"));
        if target == saved.path {
            return Ok(target);
        }
        let disk = read_optional(&saved.path).map_err(|e| describe(&saved.path, &e))?;
        if disk.as_ref() != Some(&saved.bytes) {
            return Err(format!(
                "“{}” changed on disk. Refresh before renaming it.",
                saved.note.title()
            )
            .into());
        }
        move_without_replacing(&saved.path, &target)?;
        let parent = target.parent().ok_or("The file has no parent")?;
        File::open(parent)
            .and_then(|f| f.sync_all())
            .map_err(|e| describe(parent, &e))?;
        self.manifest.paths.remove(&saved.path);
        if self.loose.remove(&saved.path) {
            self.loose.insert(target.clone());
            self.settings.open_files = self.loose.iter().cloned().collect();
            self.settings.open_files.sort();
            self.settings.write(&self.settings_path)?;
        }
        if let Some(current) = self.files.get_mut(id) {
            current.path.clone_from(&target);
            current.note.path = Some(target.clone());
            self.manifest
                .paths
                .insert(target.clone(), identity(&current.note));
        }
        self.persist_manifest()?;
        Ok(target)
    }
    pub fn add_file(&mut self, path: PathBuf) -> Result<Note, StoreError> {
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
    pub fn refresh(&mut self) -> Result<Vec<External>, StoreError> {
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
                if let Some(identity) = self.manifest.paths.get_mut(&saved.path) {
                    identity.gone = true;
                }
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
    pub fn refresh_paths(&mut self, paths: &[PathBuf]) -> Result<Vec<External>, StoreError> {
        let mut staged = Vec::new();
        let mut seen = HashSet::new();
        for path in paths {
            let path = absolute_file(path)?;
            if !seen.insert(path.clone()) {
                continue;
            }
            let relative = path.strip_prefix(&self.directory).ok();
            if !self.loose.contains(&path) && (relative.is_none() || relative.is_some_and(|p|p.components().any(|c| matches!(c,Component::Normal(name) if matches!(name.to_str(),Some(".git"|".obsidian"|".markraft"|".trash"|"node_modules")))))) { continue; }
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
                    if let Some(identity) = self.manifest.paths.get_mut(&old.path) {
                        identity.gone = true;
                    }
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
            self.pending.remove(id);
            self.previous.remove(id);
            self.disk_won.remove(id);
        }
    }
    pub fn markdown(&self, note: &Note) -> Result<String, StoreError> {
        // A note that already says what the file on disk says — the disk version
        // an external change was just adopted as — takes those bytes as its
        // baseline. The pre-change copy is kept only for edits made against it
        // until the change is acknowledged.
        let current = self
            .files
            .get(&note.id)
            .filter(|saved| saved.note.document == note.document);
        render(
            current
                .or_else(|| self.previous.get(&note.id))
                .or_else(|| self.files.get(&note.id)),
            note,
            &self.house,
        )
    }
    /// Keep the note's local text as a conflicted copy beside the file. Used when disk wins.
    pub fn recover(&mut self, note: &Note) -> Result<(), StoreError> {
        let original = self
            .previous
            .get(&note.id)
            .or_else(|| self.files.get(&note.id));
        let path = original
            .map(|s| s.path.clone())
            .or_else(|| note.path.clone());
        let Some(path) = path else {
            // Nothing to stand beside. Saying so is what keeps the caller from
            // reporting a copy that was never written.
            return Err("This note has no file to keep a copy beside.".into());
        };
        let parent = path.parent().ok_or("The file has no parent")?;
        if !parent.exists() {
            return Err(describe(
                parent,
                &io::Error::new(io::ErrorKind::NotFound, "not found"),
            ));
        }
        // Whichever way the text was rendered, the copy is a text file and ends in a
        // newline. It also makes two sightings of one conflict compare equal, which
        // is what keeps the second from becoming a second file.
        let local = ends_with_newline(
            render(original, note, &self.house)
                .unwrap_or_else(|_| doc::to_markdown_in(&note.document, &self.house)),
        );
        if !already_kept_beside(&path, local.as_bytes()) {
            let target = conflicted_copy_path(&path);
            write_document(&target, local.as_bytes(), None)?;
        }
        self.clear_recovery(&note.id);
        Ok(())
    }
    /// On open: migrate leftover recovery JSON from the versions that kept it.
    /// When the file is still there the local text is kept beside it as a conflicted
    /// copy; when the file is gone it is written back once.
    ///
    /// Best effort throughout. A draft nobody can read is discarded, because there is
    /// no text left in it to keep; one that cannot be written yet is left where it is
    /// for the next launch. Neither is worth refusing to open the whole folder over,
    /// which is what a leftover file that always fails would otherwise do forever.
    fn restore_recovery(&mut self, library: &mut Library) {
        let history = self.state.join("recovery-history");
        if history.exists() {
            let _ = fs::remove_dir_all(&history);
        }
        let dir = self.state.join("recovery");
        let Ok(entries) = fs::read_dir(&dir) else {
            return;
        };
        let (mut conflicted, mut held) = (0, 0);
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let record: Recovery =
                match fs::read(&path)
                    .map_err(|e| describe(&path, &e))
                    .and_then(|bytes| {
                        serde_json::from_slice(&bytes).map_err(|e| StoreError::Json {
                            path: path.clone(),
                            detail: format!("unreadable draft: {e}"),
                        })
                    }) {
                    Ok(record) => record,
                    Err(error) => {
                        eprintln!("Markraft: {} was discarded: {error}", path.display());
                        let _ = fs::remove_file(&path);
                        continue;
                    }
                };
            let Some(file_path) = record.path.clone() else {
                let _ = fs::remove_file(&path);
                continue;
            };
            let record = Recovery {
                local: ends_with_newline(record.local),
                ..record
            };
            if file_path.exists() {
                if already_kept_beside(&file_path, record.local.as_bytes()) {
                    let _ = fs::remove_file(&path);
                    continue;
                }
                let target = conflicted_copy_path(&file_path);
                match write_document(&target, record.local.as_bytes(), None) {
                    Ok(()) => {
                        conflicted += 1;
                        let _ = fs::remove_file(&path);
                    }
                    Err(error) => {
                        eprintln!("Markraft: {error}");
                        held += 1;
                    }
                }
                continue;
            }
            // Path missing: recreate the file from the recovery local text once.
            let Ok(source) = SourceDocument::parse(doc::schema(), &record.local) else {
                eprintln!(
                    "Markraft: {} holds text no document can be made of",
                    path.display()
                );
                let _ = fs::remove_file(&path);
                continue;
            };
            let mut note = Note {
                id: record.id.clone(),
                document: source.document().clone(),
                created_at: crate::storage::timestamp(),
                updated_at: crate::storage::timestamp(),
                deleted_at: None,
                pinned: false,
                path: Some(file_path.clone()),
                read_only: None,
                conflicted: false,
            };
            if let Some(existing) = library.note(&record.id) {
                note.created_at = existing.created_at;
                note.pinned = existing.pinned;
            }
            let bytes = record.local.clone().into_bytes();
            if let Some(parent) = file_path.parent()
                && let Err(error) = fs::create_dir_all(parent)
            {
                eprintln!("Markraft: {}", describe(parent, &error));
                held += 1;
                continue;
            }
            if let Err(error) = write_document(&file_path, &bytes, None) {
                eprintln!("Markraft: {error}");
                held += 1;
                continue;
            }
            note.path = Some(file_path.clone());
            self.manifest
                .paths
                .insert(file_path.clone(), identity(&note));
            self.files.insert(
                note.id.clone(),
                Saved {
                    path: file_path,
                    source: if record.source.is_empty() {
                        bytes.clone()
                    } else {
                        record.source
                    },
                    bytes,
                    note: note.clone(),
                },
            );
            library.adopt(note);
            let _ = fs::remove_file(&path);
        }
        if conflicted > 0 {
            self.notices.raise(
                "A note changed on disk; your edits were kept as a conflicted copy.".to_owned(),
            );
        }
        if held > 0 {
            self.notices.raise(
                "Some unsaved changes could not be written yet; Markraft will try again next time."
                    .to_owned(),
            );
        }
    }
    fn clear_recovery(&self, id: &str) {
        let current = self.state.join("recovery").join(format!("{id}.json"));
        let _ = fs::remove_file(current);
    }
    /// Write the library out. New non-blank notes are filed under their title
    /// immediately. Mid-write disk conflicts keep a conflicted copy of local edits
    /// and keep the disk version. After a note has a file, renaming is explicit —
    /// not driven by later title edits.
    pub fn save(&mut self, library: &Library, preferences: &Preferences) -> Result<(), StoreError> {
        library.validate()?;
        preferences.validate()?;
        self.manifest.workspace = library.workspace.clone();
        self.disk_won.clear();
        self.trashed.clear();
        let mut errors: Vec<StoreError> = Vec::new();
        // The notes disk won over, by title: one conflict error at the end, so a
        // caller can tell it from a failure and still not take them as saved.
        let mut conflicts: Vec<String> = Vec::new();
        let live_ids: HashSet<_> = library.notes.iter().map(|n| n.id.clone()).collect();
        // Notes the library no longer holds were deleted: trash their files.
        let to_trash: Vec<_> = self
            .files
            .keys()
            .filter(|id| !live_ids.contains(*id))
            .cloned()
            .collect();
        for id in to_trash {
            if let Some(saved) = self.files.get(&id).cloned()
                && let Err(error) = self.trash_note(&saved)
            {
                errors.push(error);
            }
        }
        for note in &library.notes {
            let saved = self.files.get(&note.id).cloned();
            if let Some(saved) = &saved {
                self.manifest
                    .paths
                    .insert(saved.path.clone(), identity(note));
                if let Some(current) = self.files.get_mut(&note.id) {
                    current.note.pinned = note.pinned;
                }
            }
            if self.pending.contains(&note.id) {
                let locally_changed = self
                    .previous
                    .get(&note.id)
                    .or_else(|| self.files.get(&note.id))
                    .is_none_or(|base| base.note.document != note.document);
                if locally_changed {
                    // Disk already won via refresh; keep local edits as a conflicted copy.
                    if let Err(error) = self.recover(note) {
                        errors.push(error);
                    } else {
                        self.disk_won.insert(note.id.clone());
                        conflicts.push(note.title());
                    }
                }
                continue;
            }
            if saved
                .as_ref()
                .is_some_and(|s| s.note.document == note.document)
            {
                continue;
            }
            if note.read_only.is_some() {
                errors.push(format!("{} is read-only", note.title()).into());
                continue;
            }
            if saved.is_none() && doc::is_blank(&note.document) {
                self.clear_recovery(&note.id);
                continue;
            }
            let result = self.save_note(note, saved.as_ref(), library);
            if let Err(error) = result {
                let external = saved.as_ref().is_some_and(|s| {
                    read_optional(&s.path).is_ok_and(|disk| disk.as_ref() != Some(&s.bytes))
                });
                if external {
                    // Disk wins mid-save: keep local as conflicted copy, adopt disk bytes.
                    let recovery = self.recover(note);
                    if let Some(saved) = &saved
                        && let Ok(Some(mut disk)) = self.read_path(&saved.path, Some(saved))
                    {
                        disk.note.id = note.id.clone();
                        self.files.insert(note.id.clone(), disk);
                    }
                    self.disk_won.insert(note.id.clone());
                    self.pending.insert(note.id.clone());
                    match recovery {
                        Ok(()) => conflicts.push(note.title()),
                        Err(e) => {
                            errors.push(format!("{error}; conflicted copy failed: {e}").into())
                        }
                    }
                } else {
                    errors.push(error);
                }
            }
        }
        self.manifest.active_id.clone_from(&library.active_id);
        self.persist_manifest()?;
        let settings = Settings {
            preferences: preferences.clone(),
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
        if !conflicts.is_empty() {
            errors.push(StoreError::Conflict(conflicts));
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(StoreError::several(errors))
        }
    }
    fn save_note(
        &mut self,
        note: &Note,
        saved: Option<&Saved>,
        library: &Library,
    ) -> Result<(), StoreError> {
        if let Some(saved) = saved {
            let current = read_optional(&saved.path).map_err(|e| describe(&saved.path, &e))?;
            if current.as_ref() != Some(&saved.bytes) {
                return Err(format!(
                    "“{}” changed on disk, so it was not overwritten.",
                    note.title()
                )
                .into());
            }
        }
        let bytes = render(saved, note, &self.house)?.into_bytes();
        // A tree that writes what the file already holds — a picture spelled
        // out under the caret, which saves as the same characters as the
        // picture — is not written again: the file keeps its bytes and its
        // modification time, and only the record follows the tree.
        if let Some(saved) = saved
            && saved.bytes == bytes
        {
            let mut stored = note.clone();
            stored.path = Some(saved.path.clone());
            stored.conflicted = false;
            stored.deleted_at = None;
            self.files.insert(
                note.id.clone(),
                Saved {
                    note: stored,
                    ..saved.clone()
                },
            );
            return Ok(());
        }
        let path = match saved {
            Some(s) => s.path.clone(),
            None if note.path.is_some() => {
                let path = note.path.as_ref().unwrap();
                if !path.is_absolute() {
                    return Err("Choose an absolute file location.".into());
                }
                absolute_file(path)?
            }
            None => {
                let relative = &library.workspace.new_note_directory;
                if !safe_relative(relative) {
                    return Err("The new-note location must stay inside the notes folder.".into());
                }
                let folder = self.directory.join(relative);
                reject_symlink_components(&self.directory, &folder)?;
                fs::create_dir_all(&folder).map_err(|e| describe(&folder, &e))?;
                let name = file_name(note, library.workspace.new_note_name);
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
        write_document(&path, &bytes, saved.map(|s| s.bytes.as_slice()))?;
        let mut stored = note.clone();
        stored.path = Some(path.clone());
        stored.conflicted = false;
        stored.deleted_at = None;
        self.manifest.paths.insert(path.clone(), identity(&stored));
        if !path.starts_with(&self.directory) {
            self.loose.insert(path.clone());
        }
        self.clear_recovery(&note.id);
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
    /// Move the file to the system trash and drop it from the store. No in-app
    /// restore tombstone is kept.
    fn trash_note(&mut self, saved: &Saved) -> Result<(), StoreError> {
        if let Some(landed) = move_to_trash(&saved.path)? {
            self.trashed.push(landed);
        }
        self.files.remove(&saved.note.id);
        self.manifest.paths.remove(&saved.path);
        self.loose.remove(&saved.path);
        self.pending.remove(&saved.note.id);
        self.previous.remove(&saved.note.id);
        self.clear_recovery(&saved.note.id);
        Ok(())
    }
    /// Spell new Markdown in `house`'s style from now on.
    pub fn set_house(&mut self, house: markraft_commonmark::HouseStyleHandle) {
        self.house = house;
    }
    pub fn update_settings(
        &mut self,
        update: impl FnOnce(&mut Settings),
    ) -> Result<(), StoreError> {
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
        gone: false,
    }
}
fn render(
    saved: Option<&Saved>,
    note: &Note,
    house: &markraft_commonmark::HouseStyleHandle,
) -> Result<String, StoreError> {
    match saved {
        Some(saved) => SourceDocument::parse(
            doc::schema(),
            std::str::from_utf8(&saved.source).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?
        .render(doc::schema(), &note.document)
        .map_err(|e| {
            StoreError::from(format!(
                "This edit cannot preserve the original Markdown safely: {e}"
            ))
        }),
        None => Ok(format!("{}\n", doc::to_markdown_in(&note.document, house))),
    }
}
fn collect_markdown(folder: &Path, paths: &mut Vec<PathBuf>) -> Result<(), StoreError> {
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
fn absolute_file(path: &Path) -> Result<PathBuf, StoreError> {
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
fn reject_symlink_components(root: &Path, path: &Path) -> Result<(), StoreError> {
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
fn unsafe_file(path: &Path) -> Result<Option<String>, StoreError> {
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
fn file_name(note: &Note, naming: crate::storage::NoteNaming) -> String {
    if naming == crate::storage::NoteNaming::DateTime {
        return date_time_name(
            note.created_at
                .saturating_add_signed(crate::platform::local_utc_offset() * 1000),
        );
    }
    let mut name = safe_stem(&note.title());
    while name.len() > 180 {
        name.pop();
    }
    if name.is_empty() {
        "Untitled".into()
    } else {
        name
    }
}
/// A local timestamp as a file name, `2026-09-23 14.05`: sortable, and free of the
/// `:` a name cannot hold.
fn date_time_name(local_milliseconds: u64) -> String {
    let (year, month, day, hour, minute, ..) = civil(local_milliseconds);
    format!("{year:04}-{month:02}-{day:02} {hour:02}.{minute:02}")
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
pub(crate) fn safe_stem(title: &str) -> String {
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
fn write_document(path: &Path, bytes: &[u8], expected: Option<&[u8]>) -> Result<(), StoreError> {
    if expected.is_some()
        && let Some(reason) = unsafe_file(path)?
    {
        return Err(reason.into());
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
/// The stem a user typed for a file, or why it cannot be one. A generated name is
/// quietly made safe; a typed one is refused instead, because silently filing the note
/// under something other than what was typed is its own surprise. The file's own
/// extension may be typed along with the name and is not part of it.
pub(crate) fn typed_stem(name: &str, extension: &str) -> Result<String, StoreError> {
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
        return Err("Enter a name.".into());
    }
    if safe_stem(stem) != stem {
        return Err("A name cannot contain / : \\ [ ] # ^ | or begin or end with a period.".into());
    }
    // The limit is the file system's, counted in bytes with the extension on.
    if stem.len() + suffix.len() > 255 {
        return Err("That name is too long.".into());
    }
    Ok(stem.to_owned())
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

/// `text` with the newline a text file ends in.
fn ends_with_newline(mut text: String) -> String {
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

/// What every sibling copy of `original` is called before the date: the part that
/// finds the ones already standing there, whatever day they were written on.
fn conflicted_copy_prefix(original: &Path) -> String {
    let stem = original
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Untitled".into());
    format!("{stem} (conflicted copy")
}

/// The conflicted copies already beside `original`.
fn conflicted_copies(original: &Path) -> Vec<PathBuf> {
    let parent = original.parent().unwrap_or(Path::new("."));
    let prefix = conflicted_copy_prefix(original);
    let Ok(entries) = fs::read_dir(parent) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(&prefix))
        })
        .collect()
}

/// Whether this exact text is already kept beside the file. One conflict can be seen
/// by a queued save and by the flush behind it, and a second identical copy says
/// nothing the first does not — it only leaves another file to clean up.
fn already_kept_beside(original: &Path, bytes: &[u8]) -> bool {
    conflicted_copies(original)
        .into_iter()
        .any(|path| fs::read(&path).is_ok_and(|existing| existing == bytes))
}

/// Dropbox-style sibling for local edits when disk wins:
/// `{stem} (conflicted copy YYYY-MM-DD).md`, with ` 2`, ` 3`, … on collision.
fn conflicted_copy_path(original: &Path) -> PathBuf {
    let parent = original.parent().unwrap_or(Path::new("."));
    let extension = original
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_else(|| "md".into());
    let (year, month, day, ..) = civil(crate::storage::timestamp());
    let base = format!(
        "{} {year:04}-{month:02}-{day:02})",
        conflicted_copy_prefix(original)
    );
    (1..)
        .map(|n| {
            parent.join(if n == 1 {
                format!("{base}.{extension}")
            } else {
                format!("{base} {n}.{extension}")
            })
        })
        .find(|p| !p.exists())
        .unwrap()
}

/// [`Store::open`] over the settings file as it stands, the way a launch reads it.
#[cfg(test)]
pub(crate) fn open_reading_settings(
    directory: PathBuf,
    settings_path: PathBuf,
) -> Result<(Store, Library), StoreError> {
    let settings = Settings::read(&settings_path).unwrap_or_default();
    Store::open(directory, settings_path, settings)
}

#[cfg(test)]
mod tests {
    use super::*;
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
            store.notices().take(),
            ["“Gone.md” was removed outside Markraft, so the note is gone too."]
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
        assert!(store.manifest.paths.is_empty());
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
                .state
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
                .state
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
        let recovery = store.state.join("recovery");
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
    fn a_new_note_takes_the_permissions_of_the_folder_it_lands_in() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        fixture(root.path(), "existing.md", b"Existing\n");
        let folder = root.path().join("notes");
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o755)).unwrap();
        let (mut store, mut library) = open(root.path());
        let id = library.new_note(doc::from_markdown("Fresh note"));
        store.save(&library, &Preferences::default()).unwrap();
        let path = store.files.get(&id).unwrap().path.clone();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644,
            "a new note sits in the folder like its neighbours"
        );
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o700)).unwrap();
        let private = library.new_note(doc::from_markdown("Private note"));
        store.save(&library, &Preferences::default()).unwrap();
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
        assert!(store.files.values().any(|s| s.bytes == b"Two"));
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
        let name = |source: &str| {
            let mut library = Library::default();
            let id = library.new_note(doc::from_markdown(source));
            file_name(library.note(&id).unwrap(), Default::default())
        };
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
        let path = store.files.get(&id).unwrap().path.clone();
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
        let path = store.files.get(&id).unwrap().path.clone();
        assert_eq!(path, fs::canonicalize(&target).unwrap());
        assert_eq!(fs::read_to_string(&path).unwrap(), "Draft text\n");
    }
    #[test]
    fn a_discarded_draft_does_not_come_back() {
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
            assert!(library.note(&id).is_none(), "a {discard}ed draft came back");
            assert!(!target.exists(), "the filed draft was left on disk");
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
