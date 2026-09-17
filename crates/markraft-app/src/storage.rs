//! Local note-library persistence owned by the application, never by the editor.
use markraft_core::Document;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs, io,
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
    /// Front matter lines Markraft does not own, kept verbatim.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub front_matter: Vec<String>,
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
            front_matter: Vec::new(),
        });
        self.active_id.clone_from(&id);
        id
    }

    /// Add a note without opening it.
    pub fn keep_copy(&mut self, document: Document) {
        let active = self.active_id.clone();
        self.new_note(document);
        self.active_id = active;
    }

    /// Take a note as another program left it on disk, replacing any note with its id.
    pub fn adopt(&mut self, note: Note) {
        match self
            .notes
            .iter_mut()
            .find(|existing| existing.id == note.id)
        {
            Some(existing) => *existing = note,
            None => self.notes.push(note),
        }
        self.ensure_active();
    }

    pub fn remove(&mut self, id: &str) {
        self.notes.retain(|note| note.id != id);
        self.ensure_active();
    }

    /// The active note must exist outside the trash; otherwise open the most recent
    /// one, or a new one when none is left.
    fn ensure_active(&mut self) {
        if self
            .note(&self.active_id)
            .is_some_and(|note| note.deleted_at.is_none())
        {
            return;
        }
        match self.search("", false).first().map(|note| note.id.clone()) {
            Some(id) => self.active_id = id,
            None => {
                self.new_note(Document::default());
            }
        }
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

    pub(crate) fn validate(&self) -> Result<(), String> {
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

/// Per-machine state kept outside the notes folder.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The folder holding the notes; unset until the user has chosen one.
    pub notes_folder: Option<PathBuf>,
    pub active_id: String,
    pub preferences: Preferences,
    /// The single-file library of earlier versions has been imported.
    pub legacy_imported: bool,
}

impl Settings {
    /// A missing file is a first launch. A damaged one is set aside, not overwritten.
    pub fn read(path: &Path) -> Result<Self, String> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(error.to_string()),
        };
        serde_json::from_slice(&bytes).or_else(|_| {
            let mut damaged = path.as_os_str().to_os_string();
            damaged.push(format!(".damaged-{}", Uuid::new_v4()));
            fs::rename(path, damaged).map_err(|error| error.to_string())?;
            Ok(Self::default())
        })
    }

    pub fn write(&self, path: &Path) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(self).map_err(|error| error.to_string())?;
        crate::vault::atomic_write(path, &bytes)
    }
}

/// Read the single-file library written by earlier versions.
pub fn read_legacy_library(path: &Path) -> Result<Library, String> {
    let bytes = fs::read(path).map_err(|error| error.to_string())?;
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    let mut library: Library = serde_json::from_value(value).map_err(|error| error.to_string())?;
    library.validate()?;
    for note in &mut library.notes {
        note.document.normalize();
    }
    Ok(library)
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
}
