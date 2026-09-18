//! Local note-library persistence owned by the application, never by the editor.
use crate::doc;
use markraft_core::Node;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
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
    /// Modal editing in the note editors. Settings files written before it existed
    /// deserialize to `false`.
    pub vim_mode: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            dark_mode: None,
            auto_height: true,
            hotkey: "Alt+N".into(),
            window_bounds: None,
            vim_mode: false,
        }
    }
}

/// One note. The library is only ever built from the notes folder or from the
/// legacy import, so it carries no serde of its own: a document is a tree, and
/// what is written to disk is the Markdown [`crate::vault`] encodes.
#[derive(Clone, Debug, PartialEq)]
pub struct Note {
    pub id: String,
    pub document: Node,
    pub created_at: u64,
    pub updated_at: u64,
    pub deleted_at: Option<u64>,
    pub pinned: bool,
    /// Front matter lines Markraft does not own, kept verbatim.
    pub front_matter: Vec<String>,
    /// The file this note was read from was not valid text, so what could not be
    /// decoded now reads as `U+FFFD`. Writing the note makes that replacement
    /// permanent, which is worth warning about before the first edit is saved.
    /// It describes the file as it was read: a save does not clear it, a reload does.
    pub lossy: bool,
}

impl Note {
    pub fn title(&self) -> String {
        doc::title_line(&self.document)
            .as_deref()
            .unwrap_or("Untitled")
            .graphemes(true)
            .take(64)
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Library {
    pub version: u32,
    pub active_id: String,
    pub notes: Vec<Note>,
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
        library.new_note(doc::empty());
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

    pub fn new_note(&mut self, document: Node) -> String {
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
            lossy: false,
        });
        self.active_id.clone_from(&id);
        id
    }

    /// Add a note without opening it.
    pub fn keep_copy(&mut self, document: Node) {
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
                self.new_note(doc::empty());
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
                self.new_note(doc::empty());
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

    pub fn set_document(&mut self, id: &str, document: Node) -> bool {
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
                        || doc::plain_text(&note.document)
                            .to_lowercase()
                            .contains(&query))
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

    pub fn validate(&self) -> Result<(), String> {
        if self.version != LIBRARY_VERSION {
            return Err(
                "These notes were written by a different version of Markraft. \
                 Update Markraft, then open them again."
                    .into(),
            );
        }
        let mut ids = HashSet::new();
        if self
            .notes
            .iter()
            .any(|note| note.id.is_empty() || !ids.insert(&note.id))
        {
            return Err(
                "Markraft cannot tell two of your notes apart, so it stopped before \
                 saving. Reload the folder to use the notes on disk."
                    .into(),
            );
        }
        if self
            .note(&self.active_id)
            .is_none_or(|note| note.deleted_at.is_some())
        {
            return Err(
                "Markraft lost track of which note is open, so it stopped before saving. \
                 Open a note from the list, then try again."
                    .into(),
            );
        }
        if self.preferences.window_bounds.is_some_and(|bounds| {
            bounds.iter().any(|value| !value.is_finite()) || bounds[2] <= 0.0 || bounds[3] <= 0.0
        }) {
            return Err(
                "The window size Markraft remembered cannot be used, so it stopped before \
                 saving. Resize the window, then try again."
                    .into(),
            );
        }
        Ok(())
    }
}

pub(crate) fn timestamp() -> u64 {
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
    /// Where the unreadable settings file was kept when these settings had to
    /// fall back to the defaults. It belongs to this launch, not to the file, so
    /// it is never written back.
    #[serde(skip)]
    pub recovered_from: Option<PathBuf>,
}

impl Settings {
    /// A missing file is a first launch. A damaged one is set aside, not overwritten.
    pub fn read(path: &Path) -> Result<Self, String> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(crate::vault::describe(path, &error)),
        };
        serde_json::from_slice(&bytes).or_else(|error| {
            eprintln!(
                "Markraft: {} is not readable settings: {error}",
                path.display()
            );
            let mut damaged = path.as_os_str().to_os_string();
            damaged.push(format!(".damaged-{}", Uuid::new_v4()));
            let damaged = PathBuf::from(damaged);
            fs::rename(path, &damaged).map_err(|error| crate::vault::describe(path, &error))?;
            Ok(Self {
                recovered_from: Some(damaged),
                ..Self::default()
            })
        })
    }

    /// What to tell the user when their folder choice and hotkey were lost with
    /// the settings file, so the reset does not go unexplained.
    pub fn recovery_notice(&self) -> Option<String> {
        self.recovered_from.as_deref().map(|damaged| {
            format!(
                "Markraft could not read your settings, so your notes folder and shortcut \
                 were reset. The unreadable file was kept as “{}”.",
                crate::vault::file_label(damaged)
            )
        })
    }

    pub fn write(&self, path: &Path) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(self).map_err(|error| {
            eprintln!("Markraft: settings could not be encoded: {error}");
            "Markraft could not prepare your settings for saving.".to_owned()
        })?;
        crate::vault::atomic_write(path, &bytes)
    }
}

/// Things the user should be told once, noticed where there is no interface to
/// show them in: on the way to the first window, or on the save worker's thread.
/// Whoever can show them drains them.
#[derive(Clone, Debug, Default)]
pub struct Notices(Arc<Mutex<Vec<String>>>);

impl Notices {
    /// Repeats are dropped: the same file is read again on every refresh.
    pub fn raise(&self, text: String) {
        if let Ok(mut pending) = self.0.lock()
            && !pending.contains(&text)
        {
            pending.push(text);
        }
    }

    /// Everything raised since the last call.
    pub fn take(&self) -> Vec<String> {
        self.0
            .lock()
            .map(|mut pending| std::mem::take(&mut *pending))
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_can_be_created_searched_deleted_and_restored_without_losing_unicode() {
        let mut library = Library::default();
        let initial = library.active_id.clone();
        let document = doc::from_markdown("# 中文 👩🏽‍💻\n\n- [x] **Idea** é");
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
        assert_eq!(library.active_note().document, doc::empty());
    }

    #[test]
    fn a_title_skips_leading_markup_and_falls_back_to_untitled() {
        let mut library = Library::default();
        let html = library.new_note(doc::from_markdown("<div class=\"card\">\n\nReal title\n"));
        assert_eq!(library.note(&html).unwrap().title(), "Real title");
        let markup = library.new_note(doc::from_markdown("<hr/>"));
        assert_eq!(library.note(&markup).unwrap().title(), "Untitled");
    }

    #[test]
    fn damaged_settings_are_set_aside_with_something_to_tell_the_user() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        fs::write(&path, b"{ not json").unwrap();

        let settings = Settings::read(&path).unwrap();
        assert_eq!(settings.notes_folder, None);
        assert_eq!(settings.preferences, Preferences::default());
        let kept = settings.recovered_from.clone().expect("the file was kept");
        assert!(kept.exists());
        let notice = settings.recovery_notice().expect("a notice");
        let name = kept.file_name().unwrap().to_string_lossy().into_owned();
        assert!(notice.contains(&name), "{notice}");

        // The recovery belongs to this launch: it is not written back, and the
        // settings that replace the damaged file load without a notice.
        settings.write(&path).unwrap();
        let reread = Settings::read(&path).unwrap();
        assert_eq!(reread.recovered_from, None);
        assert_eq!(reread.recovery_notice(), None);
    }

    #[test]
    fn notices_are_taken_once_and_never_repeat_themselves() {
        let notices = Notices::default();
        notices.raise("A note could not be read.".into());
        notices.raise("A note could not be read.".into());
        notices.raise("The settings were reset.".into());
        assert_eq!(
            notices.take(),
            ["A note could not be read.", "The settings were reset."]
        );
        assert!(notices.take().is_empty());
    }
}
