//! Local note-library persistence owned by the application, never by the editor.
use crate::doc;
use markraft_core::Node;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    env, fs, io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
use unicode_segmentation::UnicodeSegmentation;
use uuid::Uuid;

/// Folder name under `Documents` used when Settings has no notes folder yet.
pub const DEFAULT_NOTES_FOLDER_NAME: &str = "Markraft";

/// `~/Documents/Markraft` for the current user, when `HOME` is available.
pub fn default_notes_folder() -> Option<PathBuf> {
    Some(default_notes_folder_in(Path::new(&env::var_os("HOME")?)))
}

/// Notes folder used for a fresh install under `home`.
pub fn default_notes_folder_in(home: &Path) -> PathBuf {
    home.join("Documents").join(DEFAULT_NOTES_FOLDER_NAME)
}

/// Create `directory` if needed and return its canonical path.
pub fn ensure_notes_folder(directory: &Path) -> Result<PathBuf, String> {
    fs::create_dir_all(directory).map_err(|error| crate::vault::describe(directory, &error))?;
    fs::canonicalize(directory).map_err(|error| crate::vault::describe(directory, &error))
}

/// Pick the notes folder for this launch: `--dir`, then Settings, then the default.
///
/// `--dir` and the default are created when missing. A path already stored in
/// Settings is left alone so a missing vault surfaces as an error instead of
/// silently falling back to Documents/Markraft.
pub fn resolve_notes_folder(
    override_dir: Option<PathBuf>,
    settings_folder: Option<PathBuf>,
    home: Option<&Path>,
) -> Result<PathBuf, String> {
    if let Some(path) = override_dir {
        return ensure_notes_folder(&path);
    }
    if let Some(path) = settings_folder {
        return Ok(path);
    }
    let home = home.ok_or_else(|| "HOME is unavailable; use --dir PATH".to_owned())?;
    ensure_notes_folder(&default_notes_folder_in(home))
}

/// Whether `settings` already records `folder` (same path, allowing non-canonical forms).
pub fn notes_folder_matches(settings: Option<&Path>, folder: &Path) -> bool {
    let Some(settings) = settings else {
        return false;
    };
    if settings == folder {
        return true;
    }
    match (fs::canonicalize(settings), fs::canonicalize(folder)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

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
    /// Whether notes fetch the remote images they show. Settings files written
    /// before it existed deserialize to the default, on.
    pub remote_images: bool,
    /// The note's body text size in points; headings and spacing scale with it.
    pub text_size: f32,
    /// Hide the note when another app becomes active, for quick capture.
    pub hide_on_deactivate: bool,
    /// Keep the note above other apps' windows.
    pub always_on_top: bool,
    /// The global shortcut that opens a new note. Empty turns it off.
    pub new_note_hotkey: String,
    /// Whether the emoji menu and `:name:` write the emoji character rather than its
    /// shortcode. Off by default, as Typora writes shortcodes.
    pub emoji_characters: bool,
}

impl Preferences {
    pub const DEFAULT_TEXT_SIZE: f32 = 14.;
    pub const TEXT_SIZES: std::ops::RangeInclusive<f32> = 11.0..=24.0;
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            dark_mode: None,
            auto_height: true,
            hotkey: "Alt+N".into(),
            window_bounds: None,
            vim_mode: false,
            remote_images: true,
            text_size: Self::DEFAULT_TEXT_SIZE,
            hide_on_deactivate: false,
            always_on_top: true,
            new_note_hotkey: String::new(),
            emoji_characters: false,
        }
    }
}

/// A document and its local UI state. Paths never depend on the displayed title.
#[derive(Clone, Debug, PartialEq)]
pub struct Note {
    pub id: String,
    pub document: Node,
    pub created_at: u64,
    pub updated_at: u64,
    pub deleted_at: Option<u64>,
    pub pinned: bool,
    /// Absolute location, independent of the displayed title.
    pub path: Option<PathBuf>,
    pub read_only: Option<String>,
    pub conflicted: bool,
}

impl Note {
    /// Whether the note holds nothing at all. A file Markraft could not read comes
    /// back like this: it has a name and a reason, but never became a document.
    pub fn document_is_empty(&self) -> bool {
        crate::doc::is_blank(&self.document)
    }
    pub fn title(&self) -> String {
        doc::title_line(&self.document)
            .as_deref()
            .unwrap_or("Untitled")
            .graphemes(true)
            .take(64)
            .collect()
    }

    /// Where the file is, as a search matches it: the path under the notes folder
    /// when it is inside one, and otherwise the file's own name.
    pub fn location(&self, root: Option<&Path>) -> Option<String> {
        let path = self.path.as_ref()?;
        match root.and_then(|root| path.strip_prefix(root).ok()) {
            Some(relative) => Some(relative.display().to_string()),
            None => Some(path.file_name()?.to_string_lossy().into_owned()),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Library {
    pub version: u32,
    pub active_id: String,
    pub notes: Vec<Note>,
    pub preferences: Preferences,
    pub workspace: WorkspaceSettings,
}

impl Default for Library {
    fn default() -> Self {
        let mut library = Self {
            version: LIBRARY_VERSION,
            active_id: String::new(),
            notes: Vec::new(),
            preferences: Preferences::default(),
            workspace: WorkspaceSettings::default(),
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
            path: None,
            read_only: None,
            conflicted: false,
        });
        self.active_id.clone_from(&id);
        id
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

    /// The active note must exist; otherwise open the most recent one, or a new one
    /// when none is left.
    fn ensure_active(&mut self) {
        if self.note(&self.active_id).is_some() {
            return;
        }
        match self.search("", None).first().map(|note| note.id.clone()) {
            Some(id) => self.active_id = id,
            None => {
                self.new_note(doc::empty());
            }
        }
    }

    pub fn select(&mut self, id: &str) -> bool {
        if self.note(id).is_some() {
            self.active_id = id.to_owned();
            true
        } else {
            false
        }
    }

    /// Remove the note from the library. The file is trashed by the store on save.
    pub fn delete(&mut self, id: &str) -> bool {
        if !self.notes.iter().any(|note| note.id == id) {
            return false;
        }
        self.notes.retain(|note| note.id != id);
        self.ensure_active();
        true
    }

    pub fn set_document(&mut self, id: &str, document: Node) -> bool {
        let Some(note) = self.notes.iter_mut().find(|note| note.id == id) else {
            return false;
        };
        if note.read_only.is_some() || note.document == document {
            return false;
        }
        note.document = document;
        note.updated_at = timestamp();
        true
    }

    /// Notes matching `query`, by what they say or by where their file is. Names are
    /// independent of titles now, so a search has to reach them; `root` is the notes
    /// folder, which is what makes a match read like the path the Browse row shows.
    pub fn search(&self, query: &str, root: Option<&Path>) -> Vec<&Note> {
        let query = query.trim().to_lowercase();
        let mut notes: Vec<_> = self
            .notes
            .iter()
            .filter(|note| {
                query.is_empty()
                    || doc::plain_text(&note.document)
                        .to_lowercase()
                        .contains(&query)
                    || note
                        .location(root)
                        .is_some_and(|location| location.to_lowercase().contains(&query))
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
        if self.note(&self.active_id).is_none() {
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

/// The default is assets beside the note, created only on insertion.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum AttachmentPolicy {
    #[default]
    Default,
    WorkspaceFolder(PathBuf),
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkspaceSettings {
    pub new_note_directory: PathBuf,
    pub attachments: AttachmentPolicy,
}

/// Per-machine state kept outside the notes folder.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The folder holding the notes. Set on a normal launch to the chosen path
    /// or to `~/Documents/Markraft` when none was stored yet.
    pub notes_folder: Option<PathBuf>,
    pub open_files: Vec<PathBuf>,
    pub active_id: String,
    pub preferences: Preferences,
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
    fn notes_can_be_created_searched_and_deleted_without_losing_unicode() {
        let mut library = Library::default();
        let initial = library.active_id.clone();
        let document = doc::from_markdown("# 中文 👩🏽‍💻\n\n- [x] **Idea** é");
        let id = library.new_note(document.clone());
        assert_eq!(library.active_note().title(), "中文 👩🏽‍💻");
        assert_eq!(library.search("IDEA", None)[0].document, document);
        assert!(library.delete(&id));
        assert!(!library.select(&id));
        assert_eq!(library.active_id, initial);
        assert!(library.search("👩🏽‍💻", None).is_empty());
        assert!(library.delete(&initial));
        assert_eq!(library.search("", None).len(), 1);
        assert_eq!(library.active_note().document, doc::empty());
    }

    #[test]
    fn notes_are_found_by_where_their_file_is_as_well_as_by_what_they_say() {
        let root = PathBuf::from("/Users/someone/Notes");
        let mut library = Library::default();
        let filed = library.new_note(doc::from_markdown("# Standup\n\nagenda"));
        library
            .notes
            .iter_mut()
            .find(|n| n.id == filed)
            .unwrap()
            .path = Some(root.join("Work/Clients/quarterly-review.md"));
        let loose = library.new_note(doc::from_markdown("# Elsewhere"));
        library
            .notes
            .iter_mut()
            .find(|n| n.id == loose)
            .unwrap()
            .path = Some(PathBuf::from("/tmp/scratch-pad.md"));

        fn live(library: &Library, root: &Path, query: &str) -> Vec<String> {
            library
                .search(query, Some(root))
                .iter()
                .map(|note| note.id.clone())
                .collect()
        }
        // By file name, by a directory on the way to it, and case-insensitively.
        assert_eq!(
            live(&library, &root, "quarterly"),
            std::slice::from_ref(&filed)
        );
        assert_eq!(
            live(&library, &root, "CLIENTS"),
            std::slice::from_ref(&filed)
        );
        assert_eq!(
            live(&library, &root, "work/clients"),
            std::slice::from_ref(&filed)
        );
        // Outside the folder only the file's own name is matched, not its folders.
        assert_eq!(
            live(&library, &root, "scratch-pad"),
            std::slice::from_ref(&loose)
        );
        assert!(live(&library, &root, "tmp").is_empty());
        assert_eq!(
            live(&library, &root, "agenda"),
            std::slice::from_ref(&filed)
        );
        assert!(library.delete(&filed));
        assert!(live(&library, &root, "quarterly").is_empty());
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
    fn default_notes_folder_lives_under_documents() {
        let home = Path::new("/Users/someone");
        assert_eq!(
            default_notes_folder_in(home),
            PathBuf::from("/Users/someone/Documents/Markraft")
        );
    }

    #[test]
    fn resolve_prefers_override_then_settings_then_default() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let override_dir = root.path().join("override");
        let settings_dir = root.path().join("settings-vault");
        fs::create_dir_all(&settings_dir).unwrap();

        let resolved = resolve_notes_folder(
            Some(override_dir.clone()),
            Some(settings_dir.clone()),
            Some(&home),
        )
        .unwrap();
        assert_eq!(resolved, fs::canonicalize(&override_dir).unwrap());
        assert!(override_dir.is_dir());

        let resolved = resolve_notes_folder(None, Some(settings_dir.clone()), Some(&home)).unwrap();
        assert_eq!(resolved, settings_dir);

        let resolved = resolve_notes_folder(None, None, Some(&home)).unwrap();
        let expected = default_notes_folder_in(&home);
        assert_eq!(resolved, fs::canonicalize(&expected).unwrap());
        assert!(expected.is_dir());
    }

    #[test]
    fn resolve_does_not_create_a_missing_settings_folder() {
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("gone");
        let resolved = resolve_notes_folder(None, Some(missing.clone()), None).unwrap();
        assert_eq!(resolved, missing);
        assert!(!missing.exists());
    }

    #[test]
    fn resolve_requires_home_when_falling_back_to_the_default() {
        let error = resolve_notes_folder(None, None, None).unwrap_err();
        assert!(error.contains("HOME"), "{error}");
    }

    #[test]
    fn notes_folder_matches_canonical_and_literal_paths() {
        let root = tempfile::tempdir().unwrap();
        let folder = root.path().join("notes");
        fs::create_dir_all(&folder).unwrap();
        let canonical = fs::canonicalize(&folder).unwrap();
        assert!(notes_folder_matches(Some(&folder), &canonical));
        assert!(notes_folder_matches(Some(&canonical), &folder));
        assert!(!notes_folder_matches(None, &folder));
        assert!(!notes_folder_matches(
            Some(&root.path().join("other")),
            &folder
        ));
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
