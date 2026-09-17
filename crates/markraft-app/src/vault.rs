//! Notes on disk: one Markdown file per note in a folder the user owns.
//!
//! ```text
//! notes/
//!   2026-09-17 Shopping list.md     front matter + Markdown
//!   .trash/                         deleted notes, same format
//!   .markraft/                      writer lock and the previous version of each note
//! ```
//!
//! Preferences and the active note are per machine and live in a separate settings file.
use crate::storage::{Library, Note, Settings};
use markraft_core::Document;
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};
use unicode_segmentation::UnicodeSegmentation;
use uuid::Uuid;

const TRASH: &str = ".trash";
const INTERNAL: &str = ".markraft";

/// What was last read from or written to one note's file.
struct Saved {
    /// Relative to the notes folder.
    path: PathBuf,
    bytes: Vec<u8>,
    note: Note,
}

/// A held lock coordinates application instances. All writes should run on one serial
/// worker. Edits by other programs are detected per note before it is overwritten.
pub struct Store {
    directory: PathBuf,
    settings_path: PathBuf,
    _lock: File,
    files: HashMap<String, Saved>,
    settings: Settings,
    saved_library: Library,
}

impl Store {
    pub fn open(directory: PathBuf, settings_path: PathBuf) -> Result<(Self, Library), String> {
        fs::create_dir_all(directory.join(INTERNAL)).map_err(|error| error.to_string())?;
        let directory = fs::canonicalize(directory).map_err(|error| error.to_string())?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(directory.join(INTERNAL).join("lock"))
            .map_err(|error| error.to_string())?;
        lock.try_lock().map_err(|error| {
            format!("Another Markraft instance may already be using this folder: {error}")
        })?;
        let settings = Settings::read(&settings_path)?;
        let mut store = Self {
            directory,
            settings_path,
            _lock: lock,
            files: HashMap::new(),
            settings,
            saved_library: Library::default(),
        };
        let library = store.scan()?;
        store.saved_library = library.clone();
        Ok((store, library))
    }

    #[cfg(test)]
    fn settings(&self) -> &Settings {
        &self.settings
    }

    #[cfg(test)]
    fn is_saved(&self, library: &Library) -> bool {
        &self.saved_library == library
    }

    /// Explicitly adopt what is on disk while retaining the writer lock.
    pub fn reload(&mut self) -> Result<Library, String> {
        let library = self.scan()?;
        self.saved_library = library.clone();
        Ok(library)
    }

    /// Read every note. A file is never rejected: text without front matter is a note
    /// whose metadata comes from the file system.
    fn scan(&mut self) -> Result<Library, String> {
        let mut files = HashMap::new();
        let mut notes = Vec::new();
        for (folder, deleted) in [(PathBuf::new(), false), (PathBuf::from(TRASH), true)] {
            let entries = match fs::read_dir(self.directory.join(&folder)) {
                Ok(entries) => entries,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.to_string()),
            };
            let mut paths: Vec<_> = entries
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .filter(|path| {
                    path.extension().is_some_and(|extension| extension == "md") && path.is_file()
                })
                .collect();
            paths.sort();
            for path in paths {
                let bytes = fs::read(&path).map_err(|error| error.to_string())?;
                let modified = fs::metadata(&path)
                    .and_then(|metadata| metadata.modified())
                    .ok()
                    .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                    .map_or(0, |duration| duration.as_millis() as u64);
                let mut note = decode(&String::from_utf8_lossy(&bytes), modified);
                // A copied file carries its original's identity.
                if note.id.is_empty() || files.contains_key(&note.id) {
                    note.id = Uuid::new_v4().to_string();
                }
                if deleted {
                    note.deleted_at.get_or_insert(modified);
                } else {
                    note.deleted_at = None;
                }
                let relative = folder.join(path.file_name().expect("listed files have names"));
                files.insert(
                    note.id.clone(),
                    Saved {
                        path: relative,
                        bytes,
                        note: note.clone(),
                    },
                );
                notes.push(note);
            }
        }
        self.files = files;
        let mut library = Library {
            notes,
            preferences: self.settings.preferences.clone(),
            ..Library::default()
        };
        // `Library::default` starts with one empty note; keep it only in an empty folder.
        let placeholder = library.active_id.clone();
        if library.notes.iter().any(|note| note.deleted_at.is_none()) {
            library.active_id = library
                .note(&self.settings.active_id)
                .filter(|note| note.deleted_at.is_none())
                .or_else(|| library.search("", false).into_iter().next())
                .map(|note| note.id.clone())
                .unwrap_or(placeholder);
        } else {
            let blank = Library::default();
            library.active_id.clone_from(&blank.active_id);
            library.notes.extend(blank.notes);
        }
        library.validate()?;
        Ok(library)
    }

    /// Write the notes that changed. A note changed by another program is left alone
    /// and reported; the others are still saved.
    pub fn save(&mut self, library: &Library) -> Result<(), String> {
        library.validate()?;
        let mut conflicts = Vec::new();
        let mut taken: HashSet<PathBuf> = self
            .files
            .values()
            .map(|saved| saved.path.clone())
            .collect();
        for note in &library.notes {
            let saved = self.files.get(&note.id);
            if saved.is_some_and(|saved| &saved.note == note) {
                continue;
            }
            // A blank note that was never written stays in memory only.
            if saved.is_none() && note.document.plain_text().trim().is_empty() {
                continue;
            }
            if let Some(saved) = saved
                && read_optional(&self.directory.join(&saved.path))
                    .map_err(|error| error.to_string())?
                    .as_ref()
                    != Some(&saved.bytes)
            {
                conflicts.push(note.title());
                continue;
            }
            // Keep a file's name, even one Markraft would not have chosen, until the
            // note's title changes or it moves in or out of the trash.
            let path = match saved {
                Some(saved)
                    if file_name(&saved.note) == file_name(note)
                        && saved.note.deleted_at.is_some() == note.deleted_at.is_some() =>
                {
                    saved.path.clone()
                }
                _ => {
                    if let Some(saved) = saved {
                        taken.remove(&saved.path);
                    }
                    self.free_path(note, &taken)
                }
            };
            taken.insert(path.clone());
            let bytes = encode(note).into_bytes();
            let target = self.directory.join(&path);
            fs::create_dir_all(target.parent().expect("note paths have a parent"))
                .map_err(|error| error.to_string())?;
            if let Some(saved) = saved {
                atomic_write(
                    &self
                        .directory
                        .join(INTERNAL)
                        .join("backups")
                        .join(format!("{}.md", note.id)),
                    &saved.bytes,
                )?;
            }
            atomic_write(&target, &bytes)?;
            if let Some(saved) = saved
                && saved.path != path
            {
                fs::remove_file(self.directory.join(&saved.path))
                    .map_err(|error| error.to_string())?;
            }
            self.files.insert(
                note.id.clone(),
                Saved {
                    path,
                    bytes,
                    note: note.clone(),
                },
            );
        }
        let settings = Settings {
            active_id: library.active_id.clone(),
            preferences: library.preferences.clone(),
            ..self.settings.clone()
        };
        if settings != self.settings || !self.settings_path.exists() {
            settings.write(&self.settings_path)?;
            self.settings = settings;
        }
        if conflicts.is_empty() {
            self.saved_library = library.clone();
            Ok(())
        } else {
            Err(format!(
                "Changed on disk by another program and not overwritten: {}. \
                 Reload to use the version on disk.",
                conflicts.join(", ")
            ))
        }
    }

    /// Update settings that are not part of the library, such as the notes folder.
    pub fn update_settings(&mut self, update: impl FnOnce(&mut Settings)) -> Result<(), String> {
        let mut settings = self.settings.clone();
        update(&mut settings);
        settings.write(&self.settings_path)?;
        self.settings = settings;
        Ok(())
    }

    /// Bring the single-file library of earlier versions into an empty notes folder,
    /// once. The old file is only read.
    pub fn import_legacy(&mut self, library: Library, legacy: &Path) -> Library {
        let blank = library
            .notes
            .iter()
            .all(|note| note.document.plain_text().trim().is_empty());
        if self.settings.legacy_imported || !blank || !legacy.exists() {
            return library;
        }
        let imported = crate::storage::read_legacy_library(legacy).and_then(|imported| {
            self.update_settings(|settings| settings.legacy_imported = true)?;
            self.save(&imported)?;
            Ok(imported)
        });
        match imported {
            Ok(imported) => imported,
            Err(error) => {
                eprintln!(
                    "Earlier notes were not imported: {error}. Original: {}",
                    legacy.display()
                );
                library
            }
        }
    }

    fn free_path(&self, note: &Note, taken: &HashSet<PathBuf>) -> PathBuf {
        let folder = if note.deleted_at.is_some() {
            PathBuf::from(TRASH)
        } else {
            PathBuf::new()
        };
        let name = file_name(note);
        (1..)
            .map(|attempt| {
                folder.join(if attempt == 1 {
                    format!("{name}.md")
                } else {
                    format!("{name} {attempt}.md")
                })
            })
            .find(|path| !taken.contains(path) && !self.directory.join(path).exists())
            .expect("an unused name exists")
    }
}

/// `2026-09-17 Title`, from the creation date (UTC) and the note's first line.
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
    let title: String = title
        .trim_matches(|c: char| c == '.' || c.is_whitespace())
        .graphemes(true)
        .take(60)
        .collect();
    let (year, month, day, ..) = civil(note.created_at);
    format!(
        "{year:04}-{month:02}-{day:02} {}",
        if title.is_empty() { "Untitled" } else { &title }
    )
}

fn encode(note: &Note) -> String {
    let mut text = format!(
        "---\nid: {}\ncreated: {}\nupdated: {}\n",
        note.id,
        iso(note.created_at),
        iso(note.updated_at)
    );
    if note.pinned {
        text.push_str("pinned: true\n");
    }
    if let Some(deleted) = note.deleted_at {
        text.push_str(&format!("deleted: {}\n", iso(deleted)));
    }
    for line in &note.front_matter {
        text.push_str(line);
        text.push('\n');
    }
    text.push_str("---\n");
    text.push_str(&note.document.to_markdown());
    text.push('\n');
    text
}

/// Front matter is optional and read leniently; lines Markraft does not own, such as
/// tags added in another editor, are kept verbatim.
fn decode(text: &str, modified: u64) -> Note {
    let text = text.replace("\r\n", "\n");
    let mut note = Note {
        id: String::new(),
        document: Document::default(),
        created_at: modified,
        updated_at: modified,
        deleted_at: None,
        pinned: false,
        front_matter: Vec::new(),
    };
    let (header, body) = text
        .strip_prefix("---\n")
        .and_then(|rest| {
            rest.split_once("\n---\n")
                .or_else(|| rest.strip_suffix("\n---").map(|header| (header, "")))
                .or_else(|| rest.strip_prefix("---\n").map(|body| ("", body)))
        })
        .unwrap_or(("", &text));
    for line in header.lines() {
        let value = |key: &str| {
            line.strip_prefix(key)
                .and_then(|rest| rest.strip_prefix(':'))
                .map(|value| value.trim().trim_matches(['"', '\'']))
        };
        if let Some(id) = value("id") {
            note.id = id.to_owned();
        } else if let Some(time) = value("created") {
            note.created_at = parse_iso(time).unwrap_or(modified);
        } else if let Some(time) = value("updated") {
            note.updated_at = parse_iso(time).unwrap_or(modified);
        } else if let Some(time) = value("deleted") {
            note.deleted_at = parse_iso(time);
        } else if let Some(pinned) = value("pinned") {
            note.pinned = pinned == "true";
        } else {
            note.front_matter.push(line.to_owned());
        }
    }
    note.document = Document::from_markdown(body.strip_suffix('\n').unwrap_or(body));
    note
}

/// Milliseconds since the epoch as (year, month, day, hour, minute, second, millisecond).
fn civil(milliseconds: u64) -> (i64, u32, u32, u32, u32, u32, u32) {
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

fn iso(milliseconds: u64) -> String {
    let (year, month, day, hour, minute, second, millisecond) = civil(milliseconds);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millisecond:03}Z")
}

/// `2026-09-17T10:02:03.123Z`; the fraction and the time are optional.
fn parse_iso(text: &str) -> Option<u64> {
    let text = text.trim_end_matches('Z');
    let (date, time) = text.split_once(['T', ' ']).unwrap_or((text, "00:00:00"));
    let mut date = date.split('-').map(str::parse::<i64>);
    let (year, month, day) = (date.next()?.ok()?, date.next()?.ok()?, date.next()?.ok()?);
    let (time, fraction) = time.split_once('.').unwrap_or((time, "0"));
    let mut time = time.split(':').map(str::parse::<u64>);
    let (hour, minute) = (time.next()?.ok()?, time.next()?.ok()?);
    let second = time.next().and_then(Result::ok).unwrap_or(0);
    let millisecond = format!("{fraction:0<3}")[..3].parse::<u64>().ok()?;
    // Civil date to days, after Howard Hinnant's `days_from_civil`.
    let year = year - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let shifted_month = (month + 9) % 12;
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = u64::try_from(era * 146_097 + day_of_era - 719_468).ok()?;
    Some((days * 86_400 + hour * 3600 + minute * 60 + second) * 1000 + millisecond)
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
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn open(root: &Path) -> (Store, Library) {
        Store::open(root.join("notes"), root.join("settings.json")).unwrap()
    }

    fn listing(root: &Path, folder: &str) -> Vec<String> {
        let mut names: Vec<_> = fs::read_dir(root.join("notes").join(folder))
            .map(|entries| {
                entries
                    .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                    .filter(|name| name.ends_with(".md"))
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    #[test]
    fn notes_are_markdown_files_named_by_date_and_title() {
        let root = tempfile::tempdir().unwrap();
        let (mut store, mut library) = open(root.path());
        store.save(&library).unwrap();
        assert!(
            listing(root.path(), "").is_empty(),
            "blank notes are not written"
        );

        let id = library.active_id.clone();
        library.set_document(
            &id,
            Document::from_markdown("# Plan: a/b\n- [x] **done** 中文"),
        );
        library.notes[0].created_at = parse_iso("2026-09-17T10:02:03.123Z").unwrap();
        library.notes[0].pinned = true;
        store.save(&library).unwrap();
        assert_eq!(listing(root.path(), ""), ["2026-09-17 Plan- a-b.md"]);
        let text = fs::read_to_string(root.path().join("notes/2026-09-17 Plan- a-b.md")).unwrap();
        assert!(text.starts_with(&format!(
            "---\nid: {id}\ncreated: 2026-09-17T10:02:03.123Z\nupdated: "
        )));
        assert!(text.ends_with("pinned: true\n---\n# Plan: a/b\n- [x] **done** 中文\n"));
        assert!(store.is_saved(&library));

        drop(store);
        let (store, reopened) = open(root.path());
        assert_eq!(reopened, library);
        assert_eq!(store.settings().active_id, id);
    }

    #[test]
    fn retitling_renames_the_file_and_the_trash_is_a_folder() {
        let root = tempfile::tempdir().unwrap();
        let (mut store, mut library) = open(root.path());
        let id = library.active_id.clone();
        library.notes[0].created_at = 0;
        library.set_document(&id, Document::from_markdown("First"));
        store.save(&library).unwrap();
        library.set_document(&id, Document::from_markdown("Second"));
        let other = library.new_note(Document::from_markdown("Second"));
        library.notes[1].created_at = 0;
        store.save(&library).unwrap();
        assert_eq!(
            listing(root.path(), ""),
            ["1970-01-01 Second 2.md", "1970-01-01 Second.md"]
        );
        assert!(
            root.path()
                .join("notes/.markraft/backups")
                .join(format!("{id}.md"))
                .exists()
        );

        library.delete(&other);
        store.save(&library).unwrap();
        assert_eq!(listing(root.path(), ""), ["1970-01-01 Second.md"]);
        assert_eq!(listing(root.path(), TRASH), ["1970-01-01 Second.md"]);
        drop(store);
        let (mut store, mut reopened) = open(root.path());
        assert_eq!(reopened.search("", true)[0].id, other);
        reopened.restore(&other);
        store.save(&reopened).unwrap();
        assert!(listing(root.path(), TRASH).is_empty());
        assert_eq!(listing(root.path(), "").len(), 2);
    }

    #[test]
    fn foreign_files_are_adopted_untouched_until_edited() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("notes")).unwrap();
        let path = root.path().join("notes/todo.md");
        let original = "---\ntags: [work]\n---\n| a | b |\n\nplain *text*\n";
        fs::write(&path, original).unwrap();
        let (mut store, mut library) = open(root.path());
        assert_eq!(library.notes.len(), 1);
        assert_eq!(library.active_note().front_matter, ["tags: [work]"]);
        store.save(&library).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), original);

        // An edit that keeps the title keeps the name the user gave the file.
        let id = library.active_id.clone();
        let mut document = library.active_note().document.clone();
        document.blocks.push(Default::default());
        library.set_document(&id, document);
        store.save(&library).unwrap();
        assert_eq!(listing(root.path(), ""), ["todo.md"]);
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.starts_with(&format!("---\nid: {id}\n")));
        assert!(text.contains("\ntags: [work]\n---\n"));
    }

    #[test]
    fn a_second_writer_is_refused_and_damaged_settings_are_set_aside() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("settings.json"), b"{ not json").unwrap();
        let (store, library) = open(root.path());
        assert_eq!(library.preferences, Default::default());
        assert!(Store::open(root.path().join("notes"), root.path().join("other.json")).is_err());
        drop(store);
        assert!(fs::read_dir(root.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("settings.json.damaged-")
        }));
    }

    #[test]
    fn timestamps_round_trip_through_front_matter() {
        for milliseconds in [0, 951_782_400_000, 1_789_639_323_123, 4_102_444_799_999] {
            assert_eq!(parse_iso(&iso(milliseconds)), Some(milliseconds));
        }
        assert_eq!(iso(951_782_400_000), "2000-02-29T00:00:00.000Z");
        assert_eq!(parse_iso("2026-09-17"), parse_iso("2026-09-17T00:00:00Z"));
    }

    #[test]
    fn earlier_single_file_libraries_are_imported_once_and_left_in_place() {
        let root = tempfile::tempdir().unwrap();
        let legacy = root.path().join("notes.json");
        let mut old = Library::default();
        let id = old.active_id.clone();
        old.set_document(&id, Document::from_markdown("# Kept\n~~old~~"));
        old.preferences.hotkey = "Alt+M".into();
        let bytes = serde_json::to_vec(&old).unwrap();
        std::fs::write(&legacy, &bytes).unwrap();

        let open = || Store::open(root.path().join("notes"), root.path().join("settings.json"));
        let (mut store, library) = open().unwrap();
        let library = store.import_legacy(library, &legacy);
        assert_eq!(library, old);
        assert_eq!(std::fs::read(&legacy).unwrap(), bytes);
        drop(store);

        // Emptying the folder afterwards does not bring the old notes back.
        let (mut store, reopened) = open().unwrap();
        assert_eq!(reopened, old);
        for entry in std::fs::read_dir(root.path().join("notes")).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|extension| extension == "md") {
                std::fs::remove_file(path).unwrap();
            }
        }
        let emptied = store.reload().unwrap();
        assert_eq!(store.import_legacy(emptied.clone(), &legacy), emptied);
    }
}
