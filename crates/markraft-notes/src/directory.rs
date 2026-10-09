//! A folder of Markdown files as a notes backend. Application metadata never enters
//! the folder: identities, pins and the folder's settings are kept in a manifest
//! under the application's own state directory.
//!
//! A file holds only its text, so a note here has no host title and no logical
//! key, and its modification time is the file's. One handle owns a folder at a
//! time. The store reaches what only a file has, such as its path, a move to
//! another name and the Trash, through this type's own methods.
use crate::{
    BackendCapabilities, BackendError, BackendMutation, BackendNote, BackendSnapshot, NoteId,
    NotesBackend, StorageRevision, doc,
    fs::{
        StoreError, atomic_write, copy_metadata, describe,
        faults::{self, Stage},
        inherit_folder_mode, move_to_trash, move_without_replacing, read_optional,
        same_regular_file,
    },
    locale::Message,
    storage::{NoteNaming, Notices, Preferences, Settings, WorkspaceSettings},
    vault::{civil, safe_relative, safe_stem, typed_stem},
};
use markraft_commonmark::SourceDocument;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions, TryLockError},
    io::{self, Write},
    path::{Component, Path, PathBuf},
    time::UNIX_EPOCH,
};
use uuid::Uuid;

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
/// The manifest layout this build writes. A folder whose manifest says more
/// was last opened by a newer Markraft, and is refused rather than misread.
const MANIFEST_VERSION: u32 = 1;
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
/// The manifest in `bytes`, and whether it could not be read and was replaced.
///
/// It holds what Markraft remembers about the folder — identities, pins, the
/// folder's settings — never a note's text, so an unreadable one is set aside
/// beside itself and the folder opens fresh rather than not at all. One a newer
/// Markraft wrote is refused instead: starting fresh would write over it.
fn read_manifest(path: &Path, bytes: &[u8]) -> Result<(Manifest, bool), StoreError> {
    let newer = || StoreError::Invalid(Message::new("error.newer-folder"));
    match serde_json::from_slice::<Manifest>(bytes) {
        Ok(manifest) if manifest.version > MANIFEST_VERSION => Err(newer()),
        Ok(manifest) => Ok((manifest, false)),
        Err(error) => {
            let version = serde_json::from_slice::<serde_json::Value>(bytes)
                .ok()
                .and_then(|value| value.get("version")?.as_u64());
            if version.is_some_and(|version| version > u64::from(MANIFEST_VERSION)) {
                return Err(newer());
            }
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_secs());
            let aside = path.with_file_name(format!("manifest.unreadable-{stamp}.json"));
            log::warn!("{} could not be read: {error}", path.display());
            if let Err(error) = fs::rename(path, &aside) {
                log::warn!("{} could not be set aside: {error}", path.display());
            }
            Ok((Manifest::default(), true))
        }
    }
}

/// One note's file as the directory last read it.
#[derive(Clone)]
struct Entry {
    path: PathBuf,
    /// Why the file cannot be written: its encoding, or what the file system says.
    read_only: Option<Message>,
}

pub(crate) struct MarkdownDirectory {
    directory: PathBuf,
    state: PathBuf,
    settings_path: PathBuf,
    persist_settings: bool,
    _lock: File,
    manifest: Manifest,
    manifest_bytes: Vec<u8>,
    settings: Settings,
    notices: Notices,
    loose: HashSet<PathBuf>,
    entries: HashMap<String, Entry>,
    /// Where a note that has no file yet was asked to be written.
    placements: HashMap<String, PathBuf>,
    /// Where deletions landed in the Trash since the store last asked.
    trashed: Vec<PathBuf>,
}

/// What storage calls a file that holds `bytes`: equal content is one revision,
/// in this session and the next.
fn revision(bytes: &[u8]) -> StorageRevision {
    StorageRevision(crate::backend::sha256_hex(bytes))
}

/// Read an attempt to lock the state at `path`. Only a lock that another handle
/// holds reports the folder as in use. Any other failure is the lock file's own.
fn claim(attempt: Result<(), TryLockError>, path: &Path) -> Result<(), StoreError> {
    match attempt {
        Ok(()) => Ok(()),
        Err(TryLockError::WouldBlock) => {
            Err(StoreError::Locked(Message::new("error.folder-locked")))
        }
        Err(TryLockError::Error(error)) => Err(describe(path, &error)),
    }
}

fn conflict(id: &NoteId, actual: Option<StorageRevision>) -> StoreError {
    StoreError::Backend(BackendError::Conflict {
        id: id.clone(),
        actual,
    })
}

impl MarkdownDirectory {
    /// Take ownership of the notes in `directory`, keeping the folder's state under
    /// the application's settings folder. `settings` is the settings file as the
    /// caller read it — the directory keeps the copy it writes `settings.json` from.
    pub(crate) fn open(
        directory: PathBuf,
        settings_path: PathBuf,
        settings: Settings,
    ) -> Result<Self, StoreError> {
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
            return Err(Message::new("error.settings-outside-folder").into());
        }
        fs::create_dir_all(&state).map_err(|e| describe(&state, &e))?;
        // The lock is on this handle's state, never on the notes folder: a
        // descriptor held there would keep the folder's volume from being ejected.
        let lock_path = state.join("lock");
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|e| describe(&lock_path, &e))?;
        claim(lock.try_lock(), &lock_path)?;
        let manifest_path = state.join("manifest.json");
        let (manifest, reset) =
            match read_optional(&manifest_path).map_err(|e| describe(&manifest_path, &e))? {
                Some(bytes) => read_manifest(&manifest_path, &bytes)?,
                None => (Manifest::default(), false),
            };
        let loose: HashSet<PathBuf> = manifest.loose.iter().cloned().collect();
        let mut opened = Self {
            directory,
            state,
            settings_path,
            persist_settings: true,
            _lock: lock,
            manifest,
            manifest_bytes: Vec::new(),
            settings,
            notices: Notices::default(),
            loose,
            entries: HashMap::new(),
            placements: HashMap::new(),
            trashed: Vec::new(),
        };
        if reset {
            opened
                .notices
                .raise(Message::new("error.folder-settings-reset"));
        }
        opened.restore_recovery();
        Ok(opened)
    }
    /// Keep the host's settings file out of it: a headless library has none.
    pub(crate) fn without_settings(mut self) -> Self {
        self.persist_settings = false;
        self
    }

    pub(crate) fn notices(&self) -> Notices {
        self.notices.clone()
    }
    pub(crate) fn directory(&self) -> &Path {
        &self.directory
    }
    #[cfg(test)]
    pub(crate) fn state(&self) -> &Path {
        &self.state
    }
    #[cfg(test)]
    pub(crate) fn remembers_no_paths(&self) -> bool {
        self.manifest.paths.is_empty()
    }
    pub(crate) fn extra_watch_directories(&self) -> Vec<PathBuf> {
        self.loose
            .iter()
            .filter(|p| !p.starts_with(&self.directory))
            .filter_map(|p| p.parent().map(Path::to_owned))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect()
    }
    /// Where the note's file is.
    pub(crate) fn path(&self, id: &str) -> Option<PathBuf> {
        self.entries.get(id).map(|entry| entry.path.clone())
    }
    /// Why the note's file cannot be written, as of the last read.
    pub(crate) fn read_only(&self, id: &str) -> Option<Message> {
        self.entries
            .get(id)
            .and_then(|entry| entry.read_only.clone())
    }
    pub(crate) fn paths(&self) -> Vec<(String, PathBuf)> {
        self.entries
            .iter()
            .map(|(id, entry)| (id.clone(), entry.path.clone()))
            .collect()
    }
    /// Where deletions landed in the Trash since the last call. Empty when the
    /// platform did not say, which is not a failure: the file is still gone.
    pub(crate) fn take_trashed(&mut self) -> Vec<PathBuf> {
        std::mem::take(&mut self.trashed)
    }

    fn persist_manifest(&mut self) -> Result<(), StoreError> {
        self.manifest.version = MANIFEST_VERSION;
        self.manifest.loose = self.loose.iter().cloned().collect();
        self.manifest.loose.sort();
        let path = self.state.join("manifest.json");
        let bytes = serde_json::to_vec_pretty(&self.manifest).map_err(|e| StoreError::Json {
            path: path.clone(),
            detail: e.to_string().into(),
        })?;
        if bytes == self.manifest_bytes {
            return Ok(());
        }
        atomic_write(&path, &bytes)?;
        self.manifest_bytes = bytes;
        Ok(())
    }
    // File changes must reach the editor even when auxiliary metadata cannot be
    // persisted. Keep the new baseline and report that independent failure.
    fn persist_refresh(&mut self) {
        if let Err(error) = self.persist_manifest() {
            log::warn!("saving refreshed folder metadata failed: {error}");
            self.notices
                .raise(Message::new("error.folder-settings-save").arg("detail", error));
        }
    }
    fn open_files(&self) -> Vec<PathBuf> {
        let mut paths: Vec<_> = self.loose.iter().cloned().collect();
        paths.sort();
        paths
    }

    /// The note in the file at `path`, and what the file system says of the file.
    fn read_file(
        &self,
        id: &str,
        path: &Path,
    ) -> Result<Option<(BackendNote, Entry, Identity)>, StoreError> {
        let Some(bytes) = read_optional(path).map_err(|e| describe(path, &e))? else {
            return Ok(None);
        };
        let metadata = fs::symlink_metadata(path).map_err(|e| describe(path, &e))?;
        let modified = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_millis() as u64);
        let known = self.manifest.paths.get(path);
        let created = known.map_or(modified, |identity| identity.created);
        let pinned = known.is_some_and(|identity| identity.pinned);
        let revision = revision(&bytes);
        let mut read_only = unsafe_metadata(&metadata);
        // A file in another encoding has no Markdown to hand over. It is listed
        // with nothing in it and a reason, and is never written.
        let markdown = String::from_utf8(bytes).unwrap_or_else(|_| {
            read_only = Some(Message::new("error.not-utf8"));
            String::new()
        });
        let note = BackendNote {
            id: NoteId::new(id),
            markdown,
            title: None,
            logical_key: None,
            revision,
            created_at: created,
            updated_at: modified,
            pinned,
        };
        let identity = Identity {
            id: id.to_owned(),
            pinned,
            created,
            gone: false,
        };
        Ok(Some((
            note,
            Entry {
                path: path.to_owned(),
                read_only,
            },
            identity,
        )))
    }
    /// Every Markdown file in the folder, and every file opened from outside it.
    pub(crate) fn snapshot(&mut self) -> Result<BackendSnapshot, StoreError> {
        let mut paths = self.loose.iter().cloned().collect::<Vec<_>>();
        collect_markdown(&self.directory, &mut paths)?;
        paths.sort();
        paths.dedup();
        let known: HashMap<&Path, &str> = self
            .entries
            .iter()
            .map(|(id, entry)| (entry.path.as_path(), id.as_str()))
            .collect();
        let mut notes = Vec::new();
        let mut entries = HashMap::new();
        let mut identities = Vec::new();
        for path in paths {
            let id = known
                .get(path.as_path())
                .map(|id| (*id).to_owned())
                .or_else(|| self.manifest.paths.get(&path).map(|i| i.id.clone()))
                // Two files never share an identity: the second is a note of its own.
                .filter(|id| !entries.contains_key(id))
                .unwrap_or_else(|| Uuid::new_v4().to_string());
            if let Some((note, entry, identity)) = self.read_file(&id, &path)? {
                notes.push(note);
                entries.insert(id, entry);
                identities.push((path, identity));
            }
        }
        self.entries = entries;
        self.manifest.paths.extend(identities);
        self.persist_refresh();
        Ok(BackendSnapshot {
            notes,
            active_id: (!self.manifest.active_id.is_empty())
                .then(|| NoteId::new(self.manifest.active_id.clone())),
            workspace: self.manifest.workspace.clone(),
        })
    }
    /// The notes among `ids` whose files are still there.
    pub(crate) fn read_notes(&mut self, ids: &[NoteId]) -> Result<Vec<BackendNote>, StoreError> {
        let mut notes = Vec::new();
        for id in ids {
            let Some(path) = self.path(id.as_str()) else {
                continue;
            };
            match self.read_file(id.as_str(), &path)? {
                Some((note, entry, identity)) => {
                    notes.push(note);
                    self.entries.insert(id.to_string(), entry);
                    self.manifest.paths.insert(path, identity);
                }
                None => {
                    self.entries.remove(id.as_str());
                }
            }
        }
        self.persist_refresh();
        Ok(notes)
    }
    /// The notes whose files are at `paths`, as a watcher reports them. A file
    /// that was not there before becomes a note here. A path this folder does not
    /// read, such as one in `.git`, names none.
    pub(crate) fn ids_for_paths(&mut self, paths: &[PathBuf]) -> Result<Vec<NoteId>, StoreError> {
        let mut ids = Vec::new();
        let mut seen = HashSet::new();
        for path in paths {
            let path = absolute_file(path)?;
            if !seen.insert(path.clone()) {
                continue;
            }
            let relative = path.strip_prefix(&self.directory).ok();
            if !self.loose.contains(&path)
                && relative.is_none_or(|relative| relative.components().any(ignored))
            {
                continue;
            }
            let identity = self.manifest.paths.get(&path).map(|i| i.id.clone());
            let tracked = identity
                .as_ref()
                .is_some_and(|id| self.entries.contains_key(id));
            if !tracked
                && !self.loose.contains(&path)
                && fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink())
            {
                continue;
            }
            let id = identity.unwrap_or_else(|| Uuid::new_v4().to_string());
            if !tracked {
                self.entries.insert(
                    id.clone(),
                    Entry {
                        path,
                        read_only: None,
                    },
                );
            }
            ids.push(NoteId::new(id));
        }
        Ok(ids)
    }
    /// These notes' files left while the folder was watched. Their identities stay
    /// for a file that comes straight back.
    pub(crate) fn mark_gone(&mut self, paths: &[PathBuf]) {
        for path in paths {
            if let Some(identity) = self.manifest.paths.get_mut(path) {
                identity.gone = true;
            }
        }
        self.persist_refresh();
    }
    /// A file that left while Markraft was not watching has no note to vanish from
    /// the window, so the list would simply be shorter with nothing said. Name what
    /// went, once, and stop expecting it.
    pub(crate) fn forget_missing(&mut self) {
        let known: HashSet<_> = self.entries.values().map(|entry| &entry.path).collect();
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
            [path] => self
                .notices
                .raise(Message::new("error.file-deleted").arg("name", crate::fs::file_label(path))),
            _ => self
                .notices
                .raise(Message::new("error.files-deleted").arg("count", missing.len().to_string())),
        }
        self.persist_refresh();
    }

    /// Write or delete one note's file, when the file still holds the expected revision.
    pub(crate) fn commit_note(
        &mut self,
        mutation: BackendMutation,
    ) -> Result<StorageRevision, StoreError> {
        match mutation {
            BackendMutation::Put {
                id,
                expected,
                markdown,
                title,
                created_at,
                pinned,
                ..
            } => self.put(
                &id,
                expected.as_ref(),
                &markdown,
                title.as_deref(),
                created_at,
                pinned,
            ),
            BackendMutation::Delete {
                id,
                expected,
                deleted_at,
            } => self.delete(&id, &expected, deleted_at),
        }
    }
    fn put(
        &mut self,
        id: &NoteId,
        expected: Option<&StorageRevision>,
        markdown: &str,
        title: Option<&str>,
        created_at: u64,
        pinned: bool,
    ) -> Result<StorageRevision, StoreError> {
        let bytes = markdown.as_bytes();
        let identity = Identity {
            id: id.to_string(),
            pinned,
            created: created_at,
            gone: false,
        };
        let existing = match self.path(id.as_str()) {
            Some(path) => read_optional(&path)
                .map_err(|e| describe(&path, &e))?
                .map(|current| (path, current)),
            None => None,
        };
        let Some((path, current)) = existing else {
            if expected.is_some() {
                return Err(conflict(id, None));
            }
            return self.create(id, bytes, title, markdown, identity);
        };
        let actual = revision(&current);
        if expected != Some(&actual) {
            return Err(conflict(id, Some(actual)));
        }
        // Text that is what the file already holds is not written again: the file
        // keeps its bytes and its modification time, and only what the manifest
        // remembers of the note follows.
        if current != bytes {
            atomic_write(
                &self.state.join("backups").join(format!("{id}.md")),
                &current,
            )?;
            if path.starts_with(&self.directory) {
                let parent = path.parent().ok_or(Message::new("error.no-parent"))?;
                reject_symlink_components(&self.directory, parent)?;
            }
            let write_started = std::time::Instant::now();
            let written = write_document(&path, bytes, Some(&current));
            log::debug!(
                "save_write note={id} bytes={} elapsed_us={} success={}",
                bytes.len(),
                write_started.elapsed().as_micros(),
                written.is_ok()
            );
            match written {
                Ok(()) => {}
                // Another program wrote the file between the read above and the
                // replacement: the same refusal, found a moment later.
                Err(StoreError::Invalid(reason)) if reason.is_key("error.changed-during-save") => {
                    let now = read_optional(&path).map_err(|e| describe(&path, &e))?;
                    return Err(conflict(id, now.as_deref().map(revision)));
                }
                Err(error) => return Err(error),
            }
        }
        self.manifest.paths.insert(path, identity);
        self.clear_recovery(id.as_str());
        Ok(revision(bytes))
    }
    fn create(
        &mut self,
        id: &NoteId,
        bytes: &[u8],
        title: Option<&str>,
        markdown: &str,
        identity: Identity,
    ) -> Result<StorageRevision, StoreError> {
        let path = match self
            .placements
            .remove(id.as_str())
            .or_else(|| self.path(id.as_str()))
        {
            Some(path) => path,
            None => {
                let relative = &self.manifest.workspace.new_note_directory;
                if !safe_relative(relative) {
                    return Err(Message::new("error.new-note-outside").into());
                }
                let folder = self.directory.join(relative);
                reject_symlink_components(&self.directory, &folder)?;
                fs::create_dir_all(&folder).map_err(|e| describe(&folder, &e))?;
                let name = file_name(
                    title,
                    markdown,
                    identity.created,
                    self.manifest.workspace.new_note_name,
                );
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
        if path.starts_with(&self.directory) {
            let parent = path.parent().ok_or(Message::new("error.no-parent"))?;
            reject_symlink_components(&self.directory, parent)?;
        }
        let write_started = std::time::Instant::now();
        let written = write_document(&path, bytes, None);
        log::debug!(
            "save_write note={id} bytes={} elapsed_us={} success={}",
            bytes.len(),
            write_started.elapsed().as_micros(),
            written.is_ok()
        );
        written?;
        self.manifest.paths.insert(path.clone(), identity);
        if !path.starts_with(&self.directory) {
            self.loose.insert(path.clone());
        }
        self.clear_recovery(id.as_str());
        self.entries.insert(
            id.to_string(),
            Entry {
                path,
                read_only: None,
            },
        );
        Ok(revision(bytes))
    }
    /// Move the file to the system trash. No in-app restore tombstone is kept.
    fn delete(
        &mut self,
        id: &NoteId,
        expected: &StorageRevision,
        deleted_at: u64,
    ) -> Result<StorageRevision, StoreError> {
        let Some(path) = self.path(id.as_str()) else {
            return Err(conflict(id, None));
        };
        // A file another program changed since it was read holds text nobody
        // here has seen, and is not thrown away.
        let actual = read_optional(&path)
            .map_err(|e| describe(&path, &e))?
            .as_deref()
            .map(revision);
        if actual.as_ref() != Some(expected) {
            return Err(conflict(id, actual));
        }
        if let Some(landed) = move_to_trash(&path)? {
            self.trashed.push(landed);
        }
        self.entries.remove(id.as_str());
        self.manifest.paths.remove(&path);
        self.loose.remove(&path);
        self.clear_recovery(id.as_str());
        Ok(StorageRevision(format!("deleted:{id}:{deleted_at}")))
    }

    /// Remember what the manifest keeps of a note whose file is not written: its pin
    /// and when it was made.
    pub(crate) fn identify(&mut self, id: &str, pinned: bool, created: u64) {
        if let Some(path) = self.path(id) {
            self.manifest.paths.insert(
                path,
                Identity {
                    id: id.to_owned(),
                    pinned,
                    created,
                    gone: false,
                },
            );
        }
    }
    /// Write the note that has no file yet at `path`, not under a name made from its title.
    pub(crate) fn place(&mut self, id: &str, path: &Path) -> Result<(), StoreError> {
        if !path.is_absolute() {
            return Err(Message::new("error.absolute-path").into());
        }
        self.placements.insert(id.to_owned(), absolute_file(path)?);
        Ok(())
    }
    /// Give a note's file another name, in the folder it is already in.
    ///
    /// The file is moved rather than rewritten, so it keeps its bytes, permissions and
    /// extended attributes, and the note keeps its identity: the manifest follows the
    /// file to its new path, which is what stops the watcher from reading the move as
    /// one note leaving and a stranger arriving.
    pub(crate) fn rename(
        &mut self,
        id: &str,
        name: &str,
        expected: &StorageRevision,
        title: Message,
    ) -> Result<PathBuf, StoreError> {
        let from = self.path(id).ok_or(Message::new("error.rename-unsaved"))?;
        if let Some(reason) = unsafe_file(&from)? {
            return Err(reason.into());
        }
        let extension = from
            .extension()
            .map(|e| e.to_string_lossy().into_owned())
            .unwrap_or_else(|| "md".into());
        let stem = typed_stem(name, &extension)?;
        let target = from.with_file_name(format!("{stem}.{extension}"));
        if target == from {
            return Ok(target);
        }
        let disk = read_optional(&from).map_err(|e| describe(&from, &e))?;
        if disk.as_deref().map(revision).as_ref() != Some(expected) {
            return Err(Message::new("error.rename-changed")
                .arg("title", title)
                .into());
        }
        move_without_replacing(&from, &target)?;
        let parent = target.parent().ok_or(Message::new("error.no-parent"))?;
        File::open(parent)
            .and_then(|f| f.sync_all())
            .map_err(|e| describe(parent, &e))?;
        let identity = self.manifest.paths.remove(&from);
        if self.loose.remove(&from) {
            self.loose.insert(target.clone());
            self.settings.open_files = self.open_files();
            if self.persist_settings {
                self.settings.write(&self.settings_path)?;
            }
        }
        if let Some(entry) = self.entries.get_mut(id) {
            entry.path.clone_from(&target);
        }
        self.manifest.paths.insert(
            target.clone(),
            Identity {
                id: id.to_owned(),
                gone: false,
                ..identity.unwrap_or_default()
            },
        );
        self.persist_manifest()?;
        Ok(target)
    }
    /// Take a file from anywhere on the computer as a note, and remember it.
    pub(crate) fn add_loose(&mut self, path: PathBuf) -> Result<NoteId, StoreError> {
        let path = absolute_file(&path)?;
        if let Some((id, _)) = self
            .entries
            .iter()
            .find(|(_, entry)| entry.path == path || same_regular_file(&entry.path, &path))
        {
            return Ok(NoteId::new(id.clone()));
        }
        let id = self
            .manifest
            .paths
            .get(&path)
            .map(|identity| identity.id.clone())
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let (_, entry, identity) = self
            .read_file(&id, &path)?
            .ok_or(Message::new("error.file-gone"))?;
        self.loose.insert(path.clone());
        self.settings.open_files = self.open_files();
        if self.persist_settings {
            self.settings.write(&self.settings_path)?;
        }
        self.manifest.paths.insert(path, identity);
        self.entries.insert(id.clone(), entry);
        self.persist_manifest()?;
        Ok(NoteId::new(id))
    }
    /// The text of the file at `relative` in the notes folder, or `None` when there is
    /// no such readable text file there.
    pub(crate) fn read_text(&self, relative: &Path) -> Option<String> {
        if !safe_relative(relative) {
            return None;
        }
        let bytes = read_optional(&self.directory.join(relative)).ok()??;
        String::from_utf8(bytes).ok()
    }
    /// Make the file at `relative` with `contents`, or take the one already there.
    ///
    /// The file is written without replacing anything, so a note another program made
    /// at that path first — or makes while this runs — is the one named, as it is.
    pub(crate) fn create_at(
        &mut self,
        relative: &Path,
        contents: &str,
    ) -> Result<NoteId, StoreError> {
        if !safe_relative(relative) || relative.as_os_str().is_empty() {
            return Err(Message::new("error.new-note-outside").into());
        }
        let path = self.directory.join(relative);
        if let Some((id, _)) = self.entries.iter().find(|(_, entry)| entry.path == path) {
            return Ok(NoteId::new(id.clone()));
        }
        let parent = path.parent().ok_or(Message::new("error.no-parent"))?;
        reject_symlink_components(&self.directory, parent)?;
        fs::create_dir_all(parent).map_err(|e| describe(parent, &e))?;
        if let Err(error) = write_document(&path, contents.as_bytes(), None)
            && !path.exists()
        {
            return Err(error);
        }
        let id = self
            .manifest
            .paths
            .get(&path)
            .map(|identity| identity.id.clone())
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        self.entries.insert(
            id.clone(),
            Entry {
                path,
                read_only: None,
            },
        );
        Ok(NoteId::new(id))
    }
    /// Keep `text` beside the file at `original` as a conflicted copy. The copy is a
    /// text file and ends in a newline, which also makes two sightings of one
    /// conflict compare equal: the second does not become a second file.
    pub(crate) fn keep_beside(&self, original: &Path, text: &str) -> Result<(), StoreError> {
        let parent = original.parent().ok_or(Message::new("error.no-parent"))?;
        if !parent.exists() {
            return Err(describe(
                parent,
                &io::Error::new(io::ErrorKind::NotFound, "not found"),
            ));
        }
        if !already_kept_beside(original, text.as_bytes()) {
            let target = conflicted_copy_path(original);
            write_document(&target, text.as_bytes(), None)?;
        }
        Ok(())
    }

    /// On open: migrate leftover recovery JSON from the versions that kept it.
    /// When the file is still there the local text is kept beside it as a conflicted
    /// copy; when the file is gone it is written back once, under the identity it had.
    ///
    /// Best effort throughout. A draft nobody can read is discarded, because there is
    /// no text left in it to keep; one that cannot be written yet is left where it is
    /// for the next launch. Neither is worth refusing to open the whole folder over,
    /// which is what a leftover file that always fails would otherwise do forever.
    fn restore_recovery(&mut self) {
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
                            detail: Message::new("error.unreadable-draft")
                                .arg("detail", e.to_string()),
                        })
                    }) {
                    Ok(record) => record,
                    Err(error) => {
                        log::warn!("{} was discarded: {error}", path.display());
                        let _ = fs::remove_file(&path);
                        continue;
                    }
                };
            let Some(file_path) = record.path.clone() else {
                let _ = fs::remove_file(&path);
                continue;
            };
            let local = ends_with_newline(record.local);
            if file_path.exists() {
                if already_kept_beside(&file_path, local.as_bytes()) {
                    let _ = fs::remove_file(&path);
                    continue;
                }
                let target = conflicted_copy_path(&file_path);
                match write_document(&target, local.as_bytes(), None) {
                    Ok(()) => {
                        conflicted += 1;
                        let _ = fs::remove_file(&path);
                    }
                    Err(error) => {
                        log::warn!("{error}");
                        held += 1;
                    }
                }
                continue;
            }
            // Path missing: recreate the file from the recovery local text once.
            if SourceDocument::parse(doc::schema(), &local).is_err() {
                log::warn!("{} holds text no document can be made of", path.display());
                let _ = fs::remove_file(&path);
                continue;
            }
            if let Some(parent) = file_path.parent()
                && let Err(error) = fs::create_dir_all(parent)
            {
                log::warn!("{}", describe(parent, &error));
                held += 1;
                continue;
            }
            if let Err(error) = write_document(&file_path, local.as_bytes(), None) {
                log::warn!("{error}");
                held += 1;
                continue;
            }
            let known = self.manifest.paths.get(&file_path);
            let identity = Identity {
                id: record.id.clone(),
                pinned: known.is_some_and(|identity| identity.pinned),
                created: known.map_or_else(crate::storage::timestamp, |identity| identity.created),
                gone: false,
            };
            self.manifest.paths.insert(file_path, identity);
            let _ = fs::remove_file(&path);
        }
        if conflicted > 0 {
            self.notices.raise(crate::storage::conflict_kept());
        }
        if held > 0 {
            self.notices.raise(Message::new("error.recovery-retry"));
        }
    }
    pub(crate) fn clear_recovery(&self, id: &str) {
        let current = self.state.join("recovery").join(format!("{id}.json"));
        let _ = fs::remove_file(current);
    }
    /// An explicit reload adopts the folder as it is, and with it drops the drafts
    /// an earlier version left for notes that reload has now replaced.
    pub(crate) fn drop_recovery_drafts(&self) {
        let folder = self.state.join("recovery");
        let cleanup = (|| -> Result<(), StoreError> {
            if folder.exists() {
                for entry in fs::read_dir(&folder).map_err(|e| describe(&folder, &e))? {
                    let path = entry.map_err(|e| e.to_string())?.path();
                    if path.extension().is_some_and(|e| e == "json") {
                        fs::remove_file(&path).map_err(|e| describe(&path, &e))?;
                    }
                }
            }
            Ok(())
        })();
        if let Err(error) = cleanup {
            log::warn!("clearing legacy recovery drafts failed: {error}");
            self.notices
                .raise(Message::new("error.recovery-cleanup").arg("detail", error));
        }
    }

    /// The folder's settings as the save in progress has them: a new note is
    /// filed where they say.
    pub(crate) fn set_workspace(&mut self, workspace: &WorkspaceSettings) {
        self.manifest.workspace = workspace.clone();
    }
    pub(crate) fn save_folder_state(
        &mut self,
        active_id: Option<&NoteId>,
        workspace: &WorkspaceSettings,
    ) -> Result<(), StoreError> {
        self.manifest.workspace = workspace.clone();
        if let Some(active) = active_id {
            self.manifest.active_id = active.to_string();
        }
        self.persist_manifest()
    }
    /// Write `settings.json` when the preferences or the files opened from
    /// outside the folder changed.
    pub(crate) fn save_settings(&mut self, preferences: &Preferences) -> Result<(), StoreError> {
        let settings = Settings {
            preferences: preferences.clone(),
            open_files: self.open_files(),
            ..self.settings.clone()
        };
        if self.persist_settings && (settings != self.settings || !self.settings_path.exists()) {
            settings.write(&self.settings_path)?;
            self.settings = settings;
        }
        Ok(())
    }
    pub(crate) fn update_settings(
        &mut self,
        update: impl FnOnce(&mut Settings),
    ) -> Result<(), StoreError> {
        let mut settings = self.settings.clone();
        update(&mut settings);
        if self.persist_settings {
            settings.write(&self.settings_path)?;
        }
        self.settings = settings;
        Ok(())
    }
}

/// A store failure as a host backend reports one. The store itself calls the
/// methods above and keeps the whole error.
fn backend_error(error: StoreError) -> BackendError {
    match error {
        StoreError::Backend(error) => error,
        other => BackendError::Unavailable(other.to_string()),
    }
}
impl NotesBackend for MarkdownDirectory {
    fn load(&mut self) -> Result<BackendSnapshot, BackendError> {
        self.snapshot().map_err(backend_error)
    }
    fn read(&mut self, ids: &[NoteId]) -> Result<Vec<BackendNote>, BackendError> {
        self.read_notes(ids).map_err(backend_error)
    }
    fn commit(&mut self, mutation: BackendMutation) -> Result<StorageRevision, BackendError> {
        self.commit_note(mutation).map_err(backend_error)
    }
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities { assets: false }
    }
    fn save_workspace(
        &mut self,
        active_id: Option<&NoteId>,
        workspace: &WorkspaceSettings,
    ) -> Result<(), BackendError> {
        self.save_folder_state(active_id, workspace)
            .map_err(backend_error)
    }
}

/// A folder whose files are not notes, whatever they are called.
fn ignored(component: Component<'_>) -> bool {
    matches!(component, Component::Normal(name) if matches!(
        name.to_str(),
        Some(".git" | ".obsidian" | ".markraft" | ".trash" | "node_modules")
    ))
}
pub(crate) fn collect_markdown(folder: &Path, paths: &mut Vec<PathBuf>) -> Result<(), StoreError> {
    for entry in fs::read_dir(folder).map_err(|e| describe(folder, &e))? {
        let entry = entry.map_err(|e| describe(folder, &e))?;
        let path = entry.path();
        let kind = entry.file_type().map_err(|e| describe(&path, &e))?;
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            if !ignored(Component::Normal(&entry.file_name())) {
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
    let parent = path.parent().ok_or(Message::new("error.no-parent"))?;
    let parent = fs::canonicalize(parent).map_err(|e| describe(parent, &e))?;
    Ok(parent.join(path.file_name().ok_or(Message::new("error.no-name"))?))
}
fn reject_symlink_components(root: &Path, path: &Path) -> Result<(), StoreError> {
    let mut current = root.to_owned();
    for part in path
        .strip_prefix(root)
        .map_err(|_| Message::new("error.path-outside"))?
        .components()
    {
        current.push(part);
        if fs::symlink_metadata(&current).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(Message::new("error.symlink-directory").into());
        }
    }
    Ok(())
}
fn unsafe_file(path: &Path) -> Result<Option<Message>, StoreError> {
    let metadata = fs::symlink_metadata(path).map_err(|e| describe(path, &e))?;
    Ok(unsafe_metadata(&metadata))
}
/// [`unsafe_file`] for a file whose own metadata, not its target's, is in hand.
fn unsafe_metadata(m: &fs::Metadata) -> Option<Message> {
    use std::os::unix::fs::MetadataExt;
    if m.file_type().is_symlink() {
        Some(Message::new("error.symlink-read-only"))
    } else if m.nlink() > 1 {
        Some(Message::new("error.hardlink-read-only"))
    } else if m.permissions().readonly() {
        Some(Message::new("error.file-read-only"))
    } else {
        None
    }
}
/// The name a new note's file takes: the note's title, or when it was made.
pub(crate) fn file_name(
    title: Option<&str>,
    markdown: &str,
    created_at: u64,
    naming: NoteNaming,
) -> String {
    if naming == NoteNaming::DateTime {
        return date_time_name(
            created_at.saturating_add_signed(crate::platform::local_utc_offset() * 1000),
        );
    }
    let document = SourceDocument::parse(doc::schema(), markdown)
        .map(|source| source.document().clone())
        .unwrap_or_else(|_| doc::empty());
    let mut name = safe_stem(&crate::storage::title_of(title, &document));
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
pub(crate) fn date_time_name(local_milliseconds: u64) -> String {
    let (year, month, day, hour, minute, ..) = civil(local_milliseconds);
    format!("{year:04}-{month:02}-{day:02} {hour:02}.{minute:02}")
}
pub(crate) fn write_document(
    path: &Path,
    bytes: &[u8],
    expected: Option<&[u8]>,
) -> Result<(), StoreError> {
    if expected.is_some()
        && let Some(reason) = unsafe_file(path)?
    {
        return Err(reason.into());
    }
    let parent = path.parent().ok_or(Message::new("error.no-parent"))?;
    faults::check(path, Stage::Create).map_err(|e| describe(parent, &e))?;
    let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(|e| describe(parent, &e))?;
    faults::check(path, Stage::Write)
        .and_then(|_| temp.write_all(bytes))
        .map_err(|e| describe(path, &e))?;
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
        return Err(Message::new("error.changed-during-save").into());
    }
    faults::check(path, Stage::Persist).map_err(|e| describe(path, &e))?;
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

/// `text` with the newline a text file ends in.
pub(crate) fn ends_with_newline(mut text: String) -> String {
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

/// The sibling that keeps local edits when disk wins:
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

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    /// CRLF, trailing spaces, non-ASCII text, and no final newline.
    const SOURCE: &str = "Heading\r\n=======\r\n\n中文 café  \n\ttab";

    fn open(root: &Path) -> MarkdownDirectory {
        let notes = root.join("notes");
        fs::create_dir_all(&notes).unwrap();
        MarkdownDirectory::open(notes, root.join("settings.json"), Settings::default())
            .unwrap()
            .without_settings()
    }
    fn put(id: &NoteId, expected: Option<&StorageRevision>, markdown: &str) -> BackendMutation {
        BackendMutation::Put {
            id: id.clone(),
            expected: expected.cloned(),
            markdown: markdown.into(),
            title: None,
            logical_key: None,
            created_at: 11,
            updated_at: 22,
            pinned: true,
        }
    }
    fn refused(result: Result<StorageRevision, BackendError>) -> Option<StorageRevision> {
        match result {
            Err(BackendError::Conflict { actual, .. }) => actual,
            other => panic!("expected a conflict, got {other:?}"),
        }
    }

    // Notes, manifests and hosts keep these strings from one session to the next.
    #[test]
    fn a_revision_and_an_asset_id_spell_the_sha256_in_lowercase_hex() {
        const EMPTY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert_eq!(revision(b"").0, EMPTY);
        assert_eq!(revision(b"abc").0, ABC);
        assert_eq!(crate::AssetId::for_content(b"abc").0, ABC);
    }

    // Only another holder reports a folder as in use. A lock that cannot be taken
    // for another reason says what that reason is.
    #[test]
    fn only_a_held_lock_reports_the_folder_as_in_use() {
        let path = Path::new("/state/lock");
        assert!(claim(Ok(()), path).is_ok());
        assert!(matches!(
            claim(Err(TryLockError::WouldBlock), path),
            Err(StoreError::Locked(_))
        ));
        let unsupported = TryLockError::Error(io::ErrorKind::Unsupported.into());
        let refused = claim(Err(unsupported), path).unwrap_err();
        assert!(!matches!(refused, StoreError::Locked(_)), "{refused:?}");
    }

    // An open descriptor on the notes folder would keep its volume from being
    // ejected for as long as the application runs.
    #[cfg(target_os = "macos")]
    #[test]
    fn an_open_folder_holds_no_descriptor_on_the_notes_folder() {
        let root = tempfile::tempdir().unwrap();
        let folder = open(root.path());
        let notes = fs::canonicalize(root.path().join("notes")).unwrap();
        let held = (0..1024).any(|descriptor| {
            let mut path = [0u8; libc::PATH_MAX as usize];
            // SAFETY: the buffer is PATH_MAX bytes, which is what F_GETPATH writes at most.
            let known = unsafe { libc::fcntl(descriptor, libc::F_GETPATH, path.as_mut_ptr()) };
            known != -1
                && std::ffi::CStr::from_bytes_until_nul(&path)
                    .is_ok_and(|path| Path::new(&*path.to_string_lossy()) == notes)
        });
        drop(folder);
        assert!(!held);
    }

    // The part of the backend contract that a file can keep: exact text, a revision
    // that survives a restart, and no write over a version the writer has not seen.
    // A file has no host title, no logical key, and one owner at a time.
    #[test]
    fn a_folder_is_a_notes_backend() {
        let root = tempfile::tempdir().unwrap();
        let id = NoteId::new("11111111-1111-4111-8111-111111111111");
        let mut folder = open(root.path());
        assert!(folder.load().unwrap().notes.is_empty());
        let created = folder.commit(put(&id, None, SOURCE)).unwrap();
        assert_eq!(
            refused(folder.commit(put(&id, None, "created twice"))),
            Some(created.clone())
        );
        let stored = folder.read(std::slice::from_ref(&id)).unwrap().remove(0);
        assert_eq!(stored.markdown, SOURCE);
        assert_eq!(stored.revision, created);
        assert_eq!((stored.created_at, stored.pinned), (11, true));
        let path = folder.path(id.as_str()).unwrap();
        assert_eq!(path.file_name().unwrap(), "Heading.md");
        assert_eq!(fs::read(&path).unwrap(), SOURCE.as_bytes());

        let edited = folder.commit(put(&id, Some(&created), "edited")).unwrap();
        assert_ne!(edited, created);
        assert_eq!(
            refused(folder.commit(put(&id, Some(&created), "stale"))),
            Some(edited.clone())
        );
        // Another program is a writer like any other.
        fs::write(&path, "outside").unwrap();
        let outside = refused(folder.commit(put(&id, Some(&edited), "mine"))).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "outside");

        drop(folder);
        let mut folder = open(root.path());
        let loaded = folder.load().unwrap().notes.remove(0);
        assert_eq!(loaded.id, id);
        assert_eq!(loaded.revision, outside);
        assert!(loaded.pinned);

        let delete = |expected: &StorageRevision| BackendMutation::Delete {
            id: id.clone(),
            expected: expected.clone(),
            deleted_at: 33,
        };
        refused(folder.commit(delete(&edited)));
        assert!(path.exists());
        folder.commit(delete(&outside)).unwrap();
        assert!(!path.exists());
        assert!(folder.read(std::slice::from_ref(&id)).unwrap().is_empty());
        assert!(folder.load().unwrap().notes.is_empty());
        assert_eq!(
            refused(folder.commit(put(&id, Some(&outside), "stale"))),
            None
        );
        folder.commit(put(&id, None, "created again")).unwrap();
        assert_eq!(
            folder.read(std::slice::from_ref(&id)).unwrap()[0].markdown,
            "created again"
        );
    }
}
