//! Local note-library persistence owned by the application, never by the editor.
use markraft_core::Document;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use unicode_segmentation::UnicodeSegmentation;
use uuid::Uuid;

const LIBRARY_VERSION: u32 = 2;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub dark_mode: Option<bool>,
    pub auto_height: bool,
    pub hotkey: String,
    pub window_bounds: Option<[f32; 4]>,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            dark_mode: None,
            auto_height: true,
            hotkey: "Alt+N".into(),
            window_bounds: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Note {
    pub id: String,
    pub document: Document,
    pub created_at: u64,
    pub updated_at: u64,
    pub deleted_at: Option<u64>,
    #[serde(default)]
    pub pinned: bool,
}

impl Note {
    pub fn title(&self) -> String {
        self.document
            .plain_text()
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("Untitled")
            .graphemes(true)
            .take(64)
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Library {
    pub version: u32,
    pub active_id: String,
    pub notes: Vec<Note>,
    #[serde(default)]
    pub preferences: Preferences,
}

impl Default for Library {
    fn default() -> Self {
        let mut library = Self {
            version: LIBRARY_VERSION,
            active_id: String::new(),
            notes: Vec::new(),
            preferences: Preferences::default(),
        };
        library.new_note(Document::default());
        library
    }
}

impl Library {
    pub fn active_note(&self) -> &Note {
        self.note(&self.active_id)
            .expect("the library always has an active note")
    }

    pub fn note(&self, id: &str) -> Option<&Note> {
        self.notes.iter().find(|note| note.id == id)
    }

    pub fn new_note(&mut self, mut document: Document) -> String {
        document.normalize();
        let id = Uuid::new_v4().to_string();
        let now = timestamp();
        self.notes.push(Note {
            id: id.clone(),
            document,
            created_at: now,
            updated_at: now,
            deleted_at: None,
            pinned: false,
        });
        self.active_id.clone_from(&id);
        id
    }

    pub fn select(&mut self, id: &str) -> bool {
        if self.note(id).is_some_and(|note| note.deleted_at.is_none()) {
            self.active_id = id.to_owned();
            true
        } else {
            false
        }
    }

    pub fn delete(&mut self, id: &str) -> bool {
        let Some(note) = self.notes.iter_mut().find(|note| note.id == id) else {
            return false;
        };
        if note.deleted_at.is_some() {
            return false;
        }
        note.deleted_at = Some(timestamp());
        if self.active_id == id {
            if let Some(next) = self.search("", false).first() {
                self.active_id = next.id.clone();
            } else {
                self.new_note(Document::default());
            }
        }
        true
    }

    pub fn restore(&mut self, id: &str) -> bool {
        let Some(note) = self.notes.iter_mut().find(|note| note.id == id) else {
            return false;
        };
        if note.deleted_at.take().is_none() {
            return false;
        }
        note.updated_at = timestamp();
        self.active_id = id.to_owned();
        true
    }

    pub fn set_document(&mut self, id: &str, mut document: Document) -> bool {
        document.normalize();
        let Some(note) = self.notes.iter_mut().find(|note| note.id == id) else {
            return false;
        };
        if note.deleted_at.is_some() || note.document == document {
            return false;
        }
        note.document = document;
        note.updated_at = timestamp();
        true
    }

    pub fn search(&self, query: &str, deleted: bool) -> Vec<&Note> {
        let query = query.trim().to_lowercase();
        let mut notes: Vec<_> = self
            .notes
            .iter()
            .filter(|note| {
                note.deleted_at.is_some() == deleted
                    && (query.is_empty()
                        || note.document.plain_text().to_lowercase().contains(&query))
            })
            .collect();
        notes.sort_by(|a, b| {
            b.pinned
                .cmp(&a.pinned)
                .then(b.updated_at.cmp(&a.updated_at))
                .then(a.id.cmp(&b.id))
        });
        notes
    }

    fn validate(&self) -> Result<(), String> {
        if self.version != LIBRARY_VERSION {
            return Err(format!("Unsupported library version: {}", self.version));
        }
        let mut ids = HashSet::new();
        if self
            .notes
            .iter()
            .any(|note| note.id.is_empty() || !ids.insert(&note.id))
        {
            return Err("The note library contains missing or duplicate note IDs.".into());
        }
        if self
            .note(&self.active_id)
            .is_none_or(|note| note.deleted_at.is_some())
        {
            return Err("The note library does not contain an active note.".into());
        }
        if self.preferences.window_bounds.is_some_and(|bounds| {
            bounds.iter().any(|value| !value.is_finite()) || bounds[2] <= 0.0 || bounds[3] <= 0.0
        }) {
            return Err("The saved window bounds are invalid.".into());
        }
        Ok(())
    }
}

fn timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

/// A held sidecar lock coordinates application instances across atomic file replacements.
/// All writes should run on one serial worker. External, non-cooperating edits are detected
/// before saving but cannot be made transactional with their writers.
pub struct Store {
    path: PathBuf,
    _lock: File,
    saved_bytes: Option<Vec<u8>>,
    saved_library: Library,
    legacy: bool,
}

impl Store {
    pub fn open(path: PathBuf) -> Result<(Self, Library), String> {
        let (path, lock) = lock_path(path)?;
        let saved_bytes = read_optional(&path).map_err(|error| error.to_string())?;
        let (library, legacy) = match &saved_bytes {
            Some(bytes) => decode(bytes)?,
            None => (Library::default(), false),
        };
        Ok((
            Self {
                path,
                _lock: lock,
                saved_bytes,
                saved_library: library.clone(),
                legacy,
            },
            library,
        ))
    }

    pub fn is_saved(&self, library: &Library) -> bool {
        !self.legacy && &self.saved_library == library
    }

    /// Explicitly adopt the current disk snapshot while retaining the writer lock.
    /// A failed read or validation leaves the previous conflict-detection baseline intact.
    pub fn reload(&mut self) -> Result<Library, String> {
        let bytes = fs::read(&self.path).map_err(|error| error.to_string())?;
        let (library, legacy) = decode(&bytes)?;
        self.saved_bytes = Some(bytes);
        self.saved_library = library.clone();
        self.legacy = legacy;
        Ok(library)
    }

    pub fn save(&mut self, library: &Library) -> Result<(), String> {
        library.validate()?;
        let bytes = serde_json::to_vec_pretty(library).map_err(|error| error.to_string())?;
        if read_optional(&self.path).map_err(|error| error.to_string())? != self.saved_bytes {
            return Err(
                "The notes changed on disk. Export your current note before reopening the library."
                    .into(),
            );
        }
        if self.saved_bytes.is_some() && self.is_saved(library) {
            return Ok(());
        }
        // A backup is always a previously decoded valid snapshot, never unknown disk contents.
        if let Some(previous) = &self.saved_bytes {
            if self.legacy {
                preserve_once(&sidecar(&self.path, ".legacy"), previous)?;
            }
            atomic_write(&sidecar(&self.path, ".backup"), previous)?;
        }
        atomic_write(&self.path, &bytes)?;
        self.saved_bytes = Some(bytes);
        self.saved_library = library.clone();
        self.legacy = false;
        Ok(())
    }

    pub fn read_backup(path: &Path) -> Result<Library, String> {
        // Use the same canonical target as open, including a caller-supplied symlink.
        let path = canonical_target(path.to_path_buf())?;
        let bytes = fs::read(sidecar(&path, ".backup")).map_err(|error| error.to_string())?;
        decode(&bytes).map(|(library, _)| library)
    }

    /// Explicit recovery preserves the current file under a unique name before replacement.
    /// Merely opening a damaged library never performs recovery or overwrites it.
    pub fn recover_backup(path: PathBuf) -> Result<(Self, Library), String> {
        let (path, lock) = lock_path(path)?;
        let bytes = fs::read(sidecar(&path, ".backup")).map_err(|error| error.to_string())?;
        let (library, legacy) = decode(&bytes)?;
        if let Some(original) = read_optional(&path).map_err(|error| error.to_string())? {
            preserve_once(
                &sidecar(&path, &format!(".recovered-{}", Uuid::new_v4())),
                &original,
            )?;
        }
        atomic_write(&path, &bytes)?;
        Ok((
            Self {
                path,
                _lock: lock,
                saved_bytes: Some(bytes),
                saved_library: library.clone(),
                legacy,
            },
            library,
        ))
    }
}

fn decode(bytes: &[u8]) -> Result<(Library, bool), String> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    if value.get("document").is_some() && value.get("notes").is_none() {
        let document =
            Document::from_json(std::str::from_utf8(bytes).map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?;
        let mut library = Library::default();
        let id = library.active_id.clone();
        library.set_document(&id, document);
        return Ok((library, true));
    }
    let mut library: Library = serde_json::from_value(value).map_err(|error| error.to_string())?;
    library.validate()?;
    for note in &mut library.notes {
        note.document.normalize();
    }
    Ok((library, false))
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

fn canonical_target(path: PathBuf) -> Result<PathBuf, String> {
    if path.exists() {
        return fs::canonicalize(path).map_err(|error| error.to_string());
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let parent = fs::canonicalize(parent).map_err(|error| error.to_string())?;
    let name = path
        .file_name()
        .ok_or("The library path must name a file.")?;
    Ok(parent.join(name))
}

fn lock_path(path: PathBuf) -> Result<(PathBuf, File), String> {
    let path = canonical_target(path)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(sidecar(&path, ".lock"))
        .map_err(|error| error.to_string())?;
    lock.try_lock().map_err(|error| {
        format!("Another Markraft instance may already be using this library: {error}")
    })?;
    Ok((path, lock))
}

fn read_optional(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path.parent().unwrap_or(Path::new("."));
    let mut output = tempfile::NamedTempFile::new_in(parent).map_err(|error| error.to_string())?;
    output
        .write_all(bytes)
        .and_then(|_| output.as_file().sync_all())
        .map_err(|error| error.to_string())?;
    output.persist(path).map_err(|error| error.to_string())?;
    // Sync the directory entry as well as the file contents when the platform supports it.
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn preserve_once(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path.parent().unwrap_or(Path::new("."));
    let mut output = tempfile::NamedTempFile::new_in(parent).map_err(|error| error.to_string())?;
    output
        .write_all(bytes)
        .and_then(|_| output.as_file().sync_all())
        .map_err(|error| error.to_string())?;
    match output.persist_noclobber(path) {
        Ok(_) => File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| error.to_string()),
        Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_can_be_created_searched_deleted_and_restored_without_losing_unicode() {
        let mut library = Library::default();
        let initial = library.active_id.clone();
        let document = Document::from_markdown("# 中文 👩🏽‍💻\n- [x] **Idea** é");
        let id = library.new_note(document.clone());
        assert_eq!(library.active_note().title(), "中文 👩🏽‍💻");
        assert_eq!(library.search("IDEA", false)[0].document, document);
        assert!(library.delete(&id));
        assert!(!library.select(&id));
        assert_eq!(library.active_id, initial);
        assert_eq!(library.search("👩🏽‍💻", true)[0].id, id);
        assert!(library.restore(&id));
        assert_eq!(library.active_note().document, document);
        assert!(library.delete(&initial));
        assert!(library.delete(&id));
        assert_eq!(library.search("", false).len(), 1);
        assert_eq!(library.search("", true).len(), 2);
        assert_eq!(library.active_note().document, Document::default());
    }

    #[test]
    fn missing_library_is_not_written_until_save_and_lock_is_released_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/note.json");
        let (mut store, mut library) = Store::open(path.clone()).unwrap();
        assert!(!path.exists());
        assert!(store.is_saved(&library));
        assert!(Store::open(path.clone()).is_err());
        library.new_note(Document::from_markdown("中文 👨‍👩‍👧‍👦"));
        assert!(!store.is_saved(&library));
        store.save(&library).unwrap();
        assert!(store.is_saved(&library));
        drop(store);
        assert_eq!(Store::open(path).unwrap().1, library);
    }

    #[test]
    fn legacy_document_migrates_only_on_save_and_original_bytes_are_retained() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("note.json");
        let document = Document::from_markdown("# **你好**\n- [x] 👨‍👩‍👧‍👦");
        let original = document.to_json().unwrap().into_bytes();
        fs::write(&path, &original).unwrap();
        let (mut store, library) = Store::open(path.clone()).unwrap();
        assert_eq!(library.active_note().document, document);
        assert!(!store.is_saved(&library));
        assert_eq!(fs::read(&path).unwrap(), original);
        store.save(&library).unwrap();
        assert_eq!(fs::read(sidecar(&path, ".legacy")).unwrap(), original);
        assert_eq!(fs::read(sidecar(&path, ".backup")).unwrap(), original);
        assert_eq!(
            Store::read_backup(&path).unwrap().active_note().document,
            document
        );
        drop(store);
        assert_eq!(Store::open(path).unwrap().1, library);
    }

    #[test]
    fn backups_hold_previous_valid_snapshot_and_explicit_recovery_preserves_damage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("note.json");
        let (mut store, mut library) = Store::open(path.clone()).unwrap();
        library.new_note(Document::from_markdown("First saved text"));
        store.save(&library).unwrap();
        let previous = library.clone();
        let id = library.active_id.clone();
        library.set_document(&id, Document::from_markdown("New text"));
        store.save(&library).unwrap();
        assert_eq!(Store::read_backup(&path).unwrap(), previous);
        store.save(&library).unwrap();
        assert_eq!(Store::read_backup(&path).unwrap(), previous);
        drop(store);
        fs::write(&path, b"damaged json").unwrap();
        assert!(Store::open(path.clone()).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"damaged json");
        let (store, recovered) = Store::recover_backup(path.clone()).unwrap();
        assert_eq!(recovered, previous);
        assert!(Store::open(path.clone()).is_err());
        let preserved = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .find(|entry| entry.file_name().to_string_lossy().contains(".recovered-"))
            .unwrap();
        assert_eq!(fs::read(preserved.path()).unwrap(), b"damaged json");
        drop(store);
        assert_eq!(Store::open(path).unwrap().1, previous);
    }

    #[test]
    fn external_modification_is_never_replaced_or_added_to_backups() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("note.json");
        let (mut store, mut library) = Store::open(path.clone()).unwrap();
        store.save(&library).unwrap();
        fs::write(&path, b"External change").unwrap();
        library.new_note(Document::from_markdown("Local work"));
        assert!(store.save(&library).is_err());
        assert_eq!(fs::read(path.clone()).unwrap(), b"External change");
        assert!(!sidecar(&path, ".backup").exists());
    }

    #[test]
    fn explicit_reload_adopts_external_changes_and_allows_further_saves() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("notes.json");
        let (mut store, local) = Store::open(path.clone()).unwrap();
        store.save(&local).unwrap();
        let mut external = local.clone();
        external.new_note(Document::from_markdown("External 中文 👩🏽‍💻"));
        fs::write(&path, serde_json::to_vec(&external).unwrap()).unwrap();
        assert!(store.save(&local).is_err());
        let mut reloaded = store.reload().unwrap();
        assert_eq!(reloaded, external);
        assert!(store.is_saved(&reloaded));
        assert!(Store::open(path.clone()).is_err());
        let id = reloaded.active_id.clone();
        reloaded.set_document(&id, Document::from_markdown("Edited after reload"));
        store.save(&reloaded).unwrap();
        assert_eq!(Store::read_backup(&path).unwrap(), external);
        assert_eq!(decode(&fs::read(path).unwrap()).unwrap().0, reloaded);
    }

    #[test]
    fn invalid_reload_keeps_the_original_writer_baseline_and_preserves_disk_contents() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("notes.json");
        let (mut store, mut local) = Store::open(path.clone()).unwrap();
        store.save(&local).unwrap();
        let original = fs::read(&path).unwrap();
        fs::write(&path, b"broken external file").unwrap();
        assert!(store.reload().is_err());
        assert!(store.is_saved(&local));
        local.new_note(Document::from_markdown("Retained local work"));
        assert!(store.save(&local).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"broken external file");
        assert!(!sidecar(&path, ".backup").exists());
        // Restoring the exact original bytes still matches the untouched writer baseline.
        fs::write(&path, original).unwrap();
        store.save(&local).unwrap();
        assert_eq!(decode(&fs::read(path).unwrap()).unwrap().0, local);
    }

    #[test]
    fn unknown_versions_and_invalid_active_ids_are_rejected_without_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("note.json");
        for bytes in [
            r#"{"version":999,"document":{"blocks":[]}}"#.to_owned(),
            serde_json::to_string(&Library {
                version: 999,
                ..Library::default()
            })
            .unwrap(),
            serde_json::to_string(&Library {
                active_id: "missing".into(),
                ..Library::default()
            })
            .unwrap(),
        ] {
            fs::write(&path, &bytes).unwrap();
            assert!(Store::open(path.clone()).is_err());
            assert_eq!(fs::read_to_string(&path).unwrap(), bytes);
        }
    }
}
