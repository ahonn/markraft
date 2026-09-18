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
use crate::doc;
use crate::storage::{Library, Note, Notices, Settings};
use std::{
    borrow::Cow,
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
/// The longest file name Markraft writes, in bytes. File systems stop at 255
/// bytes per component, and a note's title can be far longer in UTF-8 than it
/// looks; the rest of the budget leaves room for a name's date prefix, its
/// extension and the suffix that separates notes sharing a title.
const NAME_BUDGET: usize = 180;
/// How many conflicting note titles a message lists before counting the rest.
const LISTED_TITLES: usize = 3;

/// What was last read from or written to one note's file.
struct Saved {
    /// Relative to the notes folder.
    path: PathBuf,
    bytes: Vec<u8>,
    note: Note,
}

/// A change another program made in the notes folder.
#[derive(Clone, Debug, PartialEq)]
pub enum External {
    /// A file appeared or changed. `previous` is what Markraft last knew of the note.
    Updated {
        previous: Option<Note>,
        note: Note,
    },
    Removed(Note),
}

/// A held lock coordinates application instances. All writes should run on one serial
/// worker. Edits by other programs are detected per note before it is overwritten.
pub struct Store {
    directory: PathBuf,
    settings_path: PathBuf,
    _lock: File,
    files: HashMap<String, Saved>,
    /// Notes changed on disk that the application has not taken over yet. Its snapshots
    /// may still hold the older text, so they are not written for these notes.
    pending: HashSet<String>,
    settings: Settings,
    saved_library: Library,
    notices: Notices,
}

impl Store {
    pub fn open(directory: PathBuf, settings_path: PathBuf) -> Result<(Self, Library), String> {
        fs::create_dir_all(directory.join(INTERNAL))
            .map_err(|error| describe(&directory, &error))?;
        let directory =
            fs::canonicalize(&directory).map_err(|error| describe(&directory, &error))?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(directory.join(INTERNAL).join("lock"))
            .map_err(|error| describe(&directory, &error))?;
        lock.try_lock().map_err(|error| {
            eprintln!("Markraft: {} is locked: {error}", directory.display());
            format!(
                "Another copy of Markraft is already using “{}”. Quit that one, \
                 or choose a different notes folder.",
                file_label(&directory)
            )
        })?;
        let settings = Settings::read(&settings_path)?;
        let notices = Notices::default();
        if let Some(notice) = settings.recovery_notice() {
            notices.raise(notice);
        }
        let mut store = Self {
            directory,
            settings_path,
            _lock: lock,
            files: HashMap::new(),
            pending: HashSet::new(),
            settings,
            saved_library: Library::default(),
            notices,
        };
        let library = store.scan()?;
        store.saved_library = library.clone();
        Ok((store, library))
    }

    /// What the user should be told about how this folder was opened and read.
    /// The store is handed to the save worker, so the application keeps this
    /// handle and drains it once it has a window to show them in.
    pub fn notices(&self) -> Notices {
        self.notices.clone()
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

    /// Read every note file. A file is never rejected: text without front matter is a
    /// note whose metadata comes from the file system. Such a file has no stored id, so
    /// it keeps the one it was given while it stays at the same path.
    fn read_folder(&self) -> Result<HashMap<String, Saved>, String> {
        let known: HashMap<&Path, &Saved> = self
            .files
            .values()
            .map(|saved| (saved.path.as_path(), saved))
            .collect();
        let mut files = HashMap::new();
        for (folder, deleted) in [(PathBuf::new(), false), (PathBuf::from(TRASH), true)] {
            let read = self.directory.join(&folder);
            let entries = match fs::read_dir(&read) {
                Ok(entries) => entries,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(describe(&read, &error)),
            };
            let mut paths: Vec<_> = entries
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .filter(|path| {
                    path.extension().is_some_and(|extension| extension == "md") && path.is_file()
                })
                .collect();
            paths.sort();
            for path in paths {
                let relative = folder.join(path.file_name().expect("listed files have names"));
                // Another program may remove a file between listing and reading it.
                let Some(bytes) = read_optional(&path).map_err(|error| describe(&path, &error))?
                else {
                    continue;
                };
                let previous = known.get(relative.as_path());
                if let Some(previous) = previous
                    && previous.bytes == bytes
                    && !files.contains_key(&previous.note.id)
                {
                    files.insert(
                        previous.note.id.clone(),
                        Saved {
                            path: relative,
                            bytes,
                            note: previous.note.clone(),
                        },
                    );
                    continue;
                }
                let modified = fs::metadata(&path)
                    .and_then(|metadata| metadata.modified())
                    .ok()
                    .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                    .map_or(0, |duration| duration.as_millis() as u64);
                // A file that is not valid text is still read, with what could not
                // be decoded replaced; the note remembers that so its first save
                // does not quietly make the replacement permanent.
                let text = String::from_utf8_lossy(&bytes);
                let lossy = matches!(text, Cow::Owned(_));
                let mut note = decode(&text, modified);
                note.lossy = lossy;
                if lossy {
                    self.notices.raise(format!(
                        "“{}” is not saved as plain text, so the parts Markraft could not \
                         read show as “�”. Keep a copy before editing it: saving the note \
                         writes the replacements to the file.",
                        file_label(&path)
                    ));
                }
                if note.id.is_empty()
                    && let Some(previous) = previous
                {
                    note.id.clone_from(&previous.note.id);
                }
                // A copied file carries its original's identity.
                if note.id.is_empty() || files.contains_key(&note.id) {
                    note.id = Uuid::new_v4().to_string();
                }
                if deleted {
                    note.deleted_at.get_or_insert(modified);
                } else {
                    note.deleted_at = None;
                }
                files.insert(
                    note.id.clone(),
                    Saved {
                        path: relative,
                        bytes,
                        note,
                    },
                );
            }
        }
        Ok(files)
    }

    fn scan(&mut self) -> Result<Library, String> {
        let files = self.read_folder()?;
        let mut notes: Vec<_> = files.values().map(|saved| saved.note.clone()).collect();
        notes.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
        self.pending.clear();
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

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Notice what other programs changed since the folder was last read or written.
    /// Markraft's own writes are recognised by their content and are not reported.
    pub fn refresh(&mut self) -> Result<Vec<External>, String> {
        let files = self.read_folder()?;
        let mut changes = Vec::new();
        for (id, saved) in &files {
            let previous = self.files.get(id).map(|previous| &previous.note);
            if previous != Some(&saved.note) {
                changes.push(External::Updated {
                    previous: previous.cloned(),
                    note: saved.note.clone(),
                });
            }
        }
        for (id, previous) in &self.files {
            if !files.contains_key(id) {
                changes.push(External::Removed(previous.note.clone()));
            }
        }
        for change in &changes {
            let (External::Updated { note, .. } | External::Removed(note)) = change;
            self.pending.insert(note.id.clone());
            self.saved_library.notes.retain(|saved| saved.id != note.id);
            if let External::Updated { note, .. } = change {
                self.saved_library.notes.push(note.clone());
            }
        }
        self.files = files;
        Ok(changes)
    }

    /// Delete these notes' files for good, and forget them, so that the next refresh
    /// reads a folder the bookkeeping already agrees with rather than adopting them
    /// back. The copy each one left in `.markraft/backups` stays: it is what remains
    /// after the trash that held it is gone.
    ///
    /// A note whose file is already missing is still forgotten. One whose file cannot
    /// be removed stops the purge with the reason, and the notes named before it are
    /// already gone; the caller drops from its library exactly what this reports.
    pub fn purge(&mut self, ids: &[String]) -> (Vec<String>, Result<(), String>) {
        let mut purged = Vec::new();
        for id in ids {
            if let Some(saved) = self.files.remove(id) {
                let path = self.directory.join(&saved.path);
                match fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => {
                        // Put it back, so the folder and the bookkeeping still agree.
                        self.files.insert(id.clone(), saved);
                        return (purged, Err(describe(&path, &error)));
                    }
                }
            }
            self.pending.remove(id);
            self.saved_library.notes.retain(|note| &note.id != id);
            purged.push(id.clone());
        }
        (purged, Ok(()))
    }

    /// The application has taken over these external changes; its snapshots are
    /// authoritative for the notes again.
    pub fn acknowledge(&mut self, ids: &[String]) {
        for id in ids {
            self.pending.remove(id);
        }
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
            if saved.is_some_and(|saved| &saved.note == note) || self.pending.contains(&note.id) {
                continue;
            }
            // A blank note that was never written stays in memory only.
            if saved.is_none() && doc::is_blank(&note.document) {
                continue;
            }
            let written = saved.map(|saved| self.directory.join(&saved.path));
            if let (Some(saved), Some(written)) = (saved, written.as_deref())
                && read_optional(written)
                    .map_err(|error| describe(written, &error))?
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
            let parent = target.parent().expect("note paths have a parent");
            fs::create_dir_all(parent).map_err(|error| describe(parent, &error))?;
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
                let previous = self.directory.join(&saved.path);
                fs::remove_file(&previous).map_err(|error| describe(&previous, &error))?;
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
                listed(&conflicts)
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
            .all(|note| doc::is_blank(&note.document));
        if self.settings.legacy_imported || !blank || !legacy.exists() {
            return library;
        }
        let imported = crate::legacy::read_library(legacy).and_then(|imported| {
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
    let (year, month, day, ..) = civil(note.created_at);
    let prefix = format!("{year:04}-{month:02}-{day:02} ");
    // What the title may spend: the rest of the budget goes to the prefix, the
    // extension and the ` 999` that separates notes sharing this title.
    let budget = NAME_BUDGET.saturating_sub(prefix.len() + ".md".len() + " 999".len());
    let title = clamp(
        title.trim_matches(|c: char| c == '.' || c.is_whitespace()),
        60,
        budget,
    );
    let title = title.trim_end_matches(|c: char| c == '.' || c.is_whitespace());
    format!(
        "{prefix}{}",
        if title.is_empty() { "Untitled" } else { title }
    )
}

/// At most `clusters` grapheme clusters and `bytes` bytes, cut on a cluster
/// boundary so a name never splits a character or an emoji sequence.
fn clamp(text: &str, clusters: usize, bytes: usize) -> String {
    let mut clamped = String::new();
    for cluster in text.graphemes(true).take(clusters) {
        if clamped.len() + cluster.len() > bytes {
            break;
        }
        clamped.push_str(cluster);
    }
    clamped
}

/// Titles for a message, counting the ones it does not name: a folder edited
/// elsewhere can conflict in every note at once.
fn listed(titles: &[String]) -> String {
    let named = titles
        .iter()
        .take(LISTED_TITLES)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    match titles.len().saturating_sub(LISTED_TITLES) {
        0 => named,
        rest => format!("{named} and {rest} more"),
    }
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
    text.push_str(&doc::to_markdown(&note.document));
    text.push('\n');
    text
}

/// Front matter is optional and read leniently; lines Markraft does not own, such as
/// tags added in another editor, are kept verbatim.
fn decode(text: &str, modified: u64) -> Note {
    let text = text.replace("\r\n", "\n");
    let mut note = Note {
        id: String::new(),
        document: doc::empty(),
        created_at: modified,
        updated_at: modified,
        deleted_at: None,
        pinned: false,
        front_matter: Vec::new(),
        lossy: false,
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
    note.document = doc::from_markdown(body.strip_suffix('\n').unwrap_or(body));
    note
}

/// Every note as one self-describing JSON file, for the rescue path that runs when
/// the notes folder itself cannot be written.
///
/// A note's body travels as Markdown rather than as a tree, so the file stays
/// readable and every note in it can be dropped back into a folder as a `.md`.
pub fn backup(library: &Library) -> Result<Vec<u8>, String> {
    let notes: Vec<serde_json::Value> = library
        .notes
        .iter()
        .map(|note| {
            serde_json::json!({
                "id": note.id,
                "title": note.title(),
                "created": iso(note.created_at),
                "updated": iso(note.updated_at),
                "deleted": note.deleted_at.map(iso),
                "pinned": note.pinned,
                "front_matter": note.front_matter,
                "markdown": doc::to_markdown(&note.document),
            })
        })
        .collect();
    serde_json::to_vec_pretty(&serde_json::json!({
        "format": "markraft-backup-v1",
        "active_id": library.active_id,
        "notes": notes,
    }))
    .map_err(|error| {
        eprintln!("Markraft: the backup could not be encoded: {error}");
        "Markraft could not prepare the backup file.".to_owned()
    })
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
            doc::from_markdown("# Plan: a/b\n\n- [x] **done** 中文"),
        );
        library.notes[0].created_at = parse_iso("2026-09-17T10:02:03.123Z").unwrap();
        library.notes[0].pinned = true;
        store.save(&library).unwrap();
        assert_eq!(listing(root.path(), ""), ["2026-09-17 Plan- a-b.md"]);
        let text = fs::read_to_string(root.path().join("notes/2026-09-17 Plan- a-b.md")).unwrap();
        assert!(text.starts_with(&format!(
            "---\nid: {id}\ncreated: 2026-09-17T10:02:03.123Z\nupdated: "
        )));
        assert!(
            text.ends_with("pinned: true\n---\n# Plan: a/b\n\n- [x] **done** 中文\n"),
            "{text}"
        );
        assert!(store.is_saved(&library));

        drop(store);
        let (store, reopened) = open(root.path());
        assert_eq!(reopened, library);
        assert_eq!(store.settings().active_id, id);
    }

    #[test]
    fn non_text_notes_are_saved_and_survive_reopening() {
        for source in ["![](photo.png)", "***", "```\n```", "- [ ] "] {
            let root = tempfile::tempdir().unwrap();
            let (mut store, mut library) = open(root.path());
            let id = library.active_id.clone();
            let document = doc::from_markdown(source);
            library.set_document(&id, document.clone());
            store.save(&library).unwrap();
            assert_eq!(listing(root.path(), "").len(), 1, "{source}");
            drop(store);

            let (_, reopened) = open(root.path());
            assert_eq!(reopened.active_note().id, id, "{source}");
            assert_eq!(reopened.active_note().document, document, "{source}");
        }
    }

    #[test]
    fn unformatted_whitespace_only_notes_remain_unwritten() {
        let root = tempfile::tempdir().unwrap();
        let (mut store, mut library) = open(root.path());
        let paragraph = doc::schema()
            .node("paragraph", [doc::schema().text(" \t ")])
            .unwrap();
        let document = doc::schema().doc([paragraph]).unwrap();
        let id = library.active_id.clone();
        library.set_document(&id, document);
        store.save(&library).unwrap();
        assert!(listing(root.path(), "").is_empty());
    }

    #[test]
    fn retitling_renames_the_file_and_the_trash_is_a_folder() {
        let root = tempfile::tempdir().unwrap();
        let (mut store, mut library) = open(root.path());
        let id = library.active_id.clone();
        library.notes[0].created_at = 0;
        library.set_document(&id, doc::from_markdown("First"));
        store.save(&library).unwrap();
        library.set_document(&id, doc::from_markdown("Second"));
        let other = library.new_note(doc::from_markdown("Second"));
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
    fn purging_removes_a_deleted_note_for_good_without_adopting_it_back() {
        let root = tempfile::tempdir().unwrap();
        let (mut store, mut library) = open(root.path());
        let id = library.active_id.clone();
        library.notes[0].created_at = 0;
        library.set_document(&id, doc::from_markdown("Throw away"));
        store.save(&library).unwrap();
        library.delete(&id);
        store.save(&library).unwrap();
        assert_eq!(listing(root.path(), TRASH), ["1970-01-01 Throw away.md"]);

        let (purged, result) = store.purge(std::slice::from_ref(&id));
        result.unwrap();
        assert_eq!(purged, std::slice::from_ref(&id));
        assert!(listing(root.path(), TRASH).is_empty());
        // The last version written stays in the backups folder.
        assert!(
            root.path()
                .join("notes/.markraft/backups")
                .join(format!("{id}.md"))
                .exists()
        );
        // The folder and the bookkeeping agree, so nothing is reported as an external
        // change and the note is not read back in.
        library.remove(&id);
        assert!(store.refresh().unwrap().is_empty());
        store.save(&library).unwrap();
        assert!(listing(root.path(), TRASH).is_empty());
        drop(store);
        let (_, reopened) = open(root.path());
        assert!(reopened.search("", true).is_empty());
        assert!(reopened.search("Throw away", false).is_empty());
    }

    #[test]
    fn purging_a_note_that_was_never_written_is_not_a_failure() {
        let root = tempfile::tempdir().unwrap();
        let (mut store, _) = open(root.path());
        let (purged, result) = store.purge(&["never-saved".to_owned()]);
        assert_eq!(purged, ["never-saved"]);
        assert_eq!(result, Ok(()));
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
        let document = library.active_note().document.clone();
        let extended = format!("{}\n\nmore", doc::to_markdown(&document));
        library.set_document(&id, doc::from_markdown(&extended));
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
    fn long_cjk_and_emoji_titles_are_cut_to_a_name_the_file_system_accepts() {
        let root = tempfile::tempdir().unwrap();
        let (mut store, mut library) = open(root.path());
        // 60 clusters of this are far past the 255-byte limit on a path component.
        let title = "中文標題👩🏽‍💻".repeat(30);
        let id = library.active_id.clone();
        library.set_document(&id, doc::from_markdown(&format!("# {title}\n\nbody")));
        library.notes[0].created_at = 0;
        // A second note with the same title takes the collision suffix, which must
        // fit in the budget as well.
        library.keep_copy(doc::from_markdown(&format!("# {title}")));
        library.notes[1].created_at = 0;
        store.save(&library).unwrap();

        let names = listing(root.path(), "");
        assert_eq!(names.len(), 2, "{names:?}");
        for name in &names {
            assert!(name.len() <= NAME_BUDGET, "{} bytes: {name}", name.len());
            assert!(name.starts_with("1970-01-01 中文標題👩🏽‍💻"), "{name}");
            let written = name
                .strip_suffix(".md")
                .and_then(|stem| stem.strip_prefix("1970-01-01 "))
                .expect("a dated Markdown name")
                .trim_end_matches(" 2");
            // Cutting inside a cluster would leave a lone skin tone or zero-width
            // joiner behind, which every cluster matching the title's rules out.
            assert!(
                written
                    .graphemes(true)
                    .zip(title.graphemes(true))
                    .all(|(written, title)| written == title),
                "{written}"
            );
        }
        drop(store);
        let (_, reopened) = open(root.path());
        assert_eq!(reopened.search("", false).len(), 2);
    }

    #[test]
    fn a_note_that_opens_with_html_is_named_after_its_text() {
        let root = tempfile::tempdir().unwrap();
        let (mut store, mut library) = open(root.path());
        let id = library.active_id.clone();
        library.set_document(
            &id,
            doc::from_markdown("<div class=\"card\">\n\nShopping: list/things\n"),
        );
        library.notes[0].created_at = 0;
        store.save(&library).unwrap();
        assert_eq!(
            listing(root.path(), ""),
            ["1970-01-01 Shopping- list-things.md"]
        );
    }

    #[test]
    fn a_file_that_is_not_text_is_read_with_something_to_tell_the_user() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("notes")).unwrap();
        fs::write(root.path().join("notes/broken.md"), b"caf\xe9 au lait").unwrap();
        let (store, library) = open(root.path());
        assert_eq!(library.notes.len(), 1);
        assert!(library.notes[0].lossy);
        assert!(doc::plain_text(&library.notes[0].document).contains('\u{fffd}'));
        let notices = store.notices().take();
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(notices[0].contains("broken.md"), "{}", notices[0]);
        assert!(store.notices().take().is_empty());
    }

    #[test]
    fn a_conflict_names_a_few_notes_and_counts_the_rest() {
        let titles: Vec<String> = (1..=5).map(|number| format!("Note {number}")).collect();
        assert_eq!(listed(&titles[..1]), "Note 1");
        assert_eq!(listed(&titles[..3]), "Note 1, Note 2, Note 3");
        assert_eq!(listed(&titles), "Note 1, Note 2, Note 3 and 2 more");
    }

    #[test]
    fn file_system_failures_are_explained_without_the_operating_system_wording() {
        let path = Path::new("/Users/someone/Notes/Shopping list.md");
        for (kind, expected) in [
            (io::ErrorKind::NotFound, "no longer there"),
            (io::ErrorKind::PermissionDenied, "not allowed"),
            (io::ErrorKind::AlreadyExists, "already exists"),
            (
                io::ErrorKind::InvalidFilename,
                "not a name this disk accepts",
            ),
            (io::ErrorKind::StorageFull, "no room left"),
            (io::ErrorKind::ReadOnlyFilesystem, "cannot be written to"),
            (io::ErrorKind::TimedOut, "did not respond in time"),
        ] {
            let text = message(
                path,
                &io::Error::new(kind, "File name too long (os error 63)"),
            );
            assert!(text.contains("Shopping list.md"), "{text}");
            assert!(text.contains(expected), "{text}");
            assert!(!text.contains("os error"), "{text}");
        }
        // Anything else still says which file, and keeps the detail in parentheses.
        let text = message(path, &io::Error::other("the disk fell over"));
        assert!(
            text.contains("“Shopping list.md” (the disk fell over)"),
            "{text}"
        );
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
        // The JSON an earlier version wrote, which no type in the workspace produces
        // any more; `crate::legacy` is what reads it.
        let bytes =
            br#"{"version":2,"active_id":"kept","notes":[{"id":"kept","document":{"blocks":[
            {"kind":{"Heading":1},"spans":[{"text":"Kept"}]},
            {"kind":"Paragraph","spans":[{"text":"old","marks":{"strikethrough":true}}]}
        ]},"created_at":1,"updated_at":2}],"preferences":{"hotkey":"Alt+M"}}"#
                .to_vec();
        std::fs::write(&legacy, &bytes).unwrap();

        let open = || Store::open(root.path().join("notes"), root.path().join("settings.json"));
        let (mut store, library) = open().unwrap();
        let library = store.import_legacy(library, &legacy);
        let old = library.clone();
        assert_eq!(library.preferences.hotkey, "Alt+M");
        assert_eq!(
            doc::to_markdown(&library.notes[0].document),
            "# Kept\n\n~~old~~"
        );
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
