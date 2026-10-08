use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

pub(super) trait Backend {
    type Scope;
    fn capture(&self, path: &Path) -> Result<Vec<u8>, String>;
    fn resolve(&self, bytes: &[u8]) -> Result<Resolved<Self::Scope>, String>;
}

pub(super) struct Resolved<S> {
    pub path: PathBuf,
    pub refreshed: Option<Vec<u8>>,
    pub scope: S,
}

#[derive(Clone, Serialize, Deserialize)]
struct Bookmark {
    path: PathBuf,
    directory: bool,
    // Settings retain the selected spelling, which may traverse a symlink.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    aliases: Vec<PathBuf>,
    data: String,
}

impl Bookmark {
    fn contains(&self, path: &Path) -> bool {
        self.suffix(path).is_some()
    }

    fn suffix<'a>(&self, path: &'a Path) -> Option<&'a Path> {
        std::iter::once(&self.path)
            .chain(&self.aliases)
            .find_map(|root| {
                if path == root || (self.directory && path.starts_with(root)) {
                    path.strip_prefix(root).ok()
                } else {
                    None
                }
            })
    }
}

pub(super) struct Store<B: Backend> {
    path: PathBuf,
    entries: Vec<Bookmark>,
    active: Vec<(Bookmark, B::Scope)>,
    backend: B,
    // An unreadable file must never be silently replaced with an empty catalog.
    read_error: Option<String>,
}

impl<B: Backend> Store<B> {
    pub fn load(path: PathBuf, backend: B) -> (Self, Vec<String>) {
        let mut errors = Vec::new();
        let mut read_error = None;
        let entries = match fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(entries) => entries,
                Err(error) => {
                    // Preserve invalid data before allowing a fresh user selection
                    // to create a new catalog, as settings recovery does.
                    let damaged = path.with_extension(format!("damaged-{}", uuid::Uuid::new_v4()));
                    if let Err(error) = fs::rename(&path, &damaged) {
                        read_error = Some(error.to_string());
                    }
                    errors.push(format!("Saved file access could not be read ({error}). Select your notes folder again."));
                    Vec::new()
                }
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(error) => {
                read_error = Some(error.to_string());
                errors.push(format!("Saved file access could not be read: {error}"));
                Vec::new()
            }
        };
        (
            Self {
                path,
                entries,
                active: Vec::new(),
                backend,
                read_error,
            },
            errors,
        )
    }

    fn save(&self, entries: &[Bookmark]) -> Result<(), String> {
        if let Some(error) = &self.read_error {
            return Err(format!("Saved file access is unavailable: {error}"));
        }
        let bytes = serde_json::to_vec_pretty(entries).map_err(|e| e.to_string())?;
        crate::fs::atomic_write(&self.path, &bytes).map_err(|e| e.to_string())
    }

    pub fn has_write_access(&self, parent: &Path) -> bool {
        let Ok(parent) = parent.canonicalize() else {
            return false;
        };
        self.active.iter().any(|(bookmark, _)| {
            bookmark.directory
                && bookmark
                    .path
                    .canonicalize()
                    .is_ok_and(|root| parent.starts_with(root))
        })
    }

    pub fn remember(&mut self, path: &Path) -> Result<(), String> {
        let selected = path.to_path_buf();
        let path = fs::canonicalize(path).map_err(|e| e.to_string())?;
        if self
            .active
            .iter()
            .any(|(bookmark, _)| bookmark.contains(&path) && bookmark.contains(&selected))
        {
            return Ok(());
        }
        let bytes = self.backend.capture(&path)?;
        let resolved = self.backend.resolve(&bytes)?;
        let mut aliases = self
            .entries
            .iter()
            .filter(|entry| entry.path == resolved.path)
            .flat_map(|entry| entry.aliases.iter().cloned())
            .collect::<Vec<_>>();
        if selected != resolved.path && !aliases.contains(&selected) {
            aliases.push(selected);
        }
        let bookmark = Bookmark {
            directory: path.is_dir(),
            path: resolved.path,
            aliases,
            data: STANDARD.encode(resolved.refreshed.unwrap_or(bytes)),
        };
        let mut entries = self.entries.clone();
        // Keep descendant grants: their aliases can still be the only spelling
        // present in a later session after the parent folder has been closed.
        entries.retain(|entry| entry.path != bookmark.path);
        entries.push(bookmark.clone());
        self.save(&entries)?;
        self.entries = entries;
        self.active.push((bookmark, resolved.scope));
        Ok(())
    }

    pub fn restore(&mut self, paths: &mut [PathBuf]) -> Vec<String> {
        let mut errors = Vec::new();
        let mut changed = false;
        // Restore only grants needed by this session, most specific first.
        self.entries
            .sort_by_key(|entry| std::cmp::Reverse(entry.path.components().count()));
        for entry in &mut self.entries {
            if !paths.iter().any(|path| {
                entry.contains(path)
                    && !self.active.iter().any(|(active, _)| {
                        active.contains(path) && (!entry.directory || active.directory)
                    })
            }) {
                continue;
            }
            let result = STANDARD
                .decode(&entry.data)
                .map_err(|e| e.to_string())
                .and_then(|bytes| self.backend.resolve(&bytes));
            match result {
                Ok(resolved) => {
                    for path in paths.iter_mut().filter(|path| entry.contains(path)) {
                        let suffix = entry.suffix(path).expect("matched bookmark");
                        *path = if suffix.as_os_str().is_empty() {
                            resolved.path.clone()
                        } else {
                            resolved.path.join(suffix)
                        };
                    }
                    if entry.path != resolved.path {
                        entry.path = resolved.path;
                        changed = true;
                    }
                    if let Some(bytes) = resolved.refreshed {
                        entry.data = STANDARD.encode(bytes);
                        changed = true;
                    }
                    self.active.push((entry.clone(), resolved.scope));
                }
                Err(error) => {
                    log::warn!(
                        "could not restore access to {}: {error}",
                        entry.path.display()
                    );
                    errors.push(format!(
                        "Access to “{}” has expired. Select the file or folder again.",
                        entry.path.display()
                    ));
                }
            }
        }
        if changed && let Err(error) = self.save(&self.entries) {
            errors.push(format!("Updated file access could not be saved: {error}"));
        }
        errors
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, rc::Rc};

    struct Fake {
        moved: Option<PathBuf>,
        fail: bool,
        live: Rc<Cell<usize>>,
    }
    struct Scope(Rc<Cell<usize>>);
    impl Drop for Scope {
        fn drop(&mut self) {
            self.0.set(self.0.get() - 1);
        }
    }
    impl Backend for Fake {
        type Scope = Scope;
        fn capture(&self, path: &Path) -> Result<Vec<u8>, String> {
            Ok(path.to_string_lossy().as_bytes().to_vec())
        }
        fn resolve(&self, bytes: &[u8]) -> Result<Resolved<Scope>, String> {
            if self.fail {
                return Err("revoked".into());
            }
            let path = self
                .moved
                .clone()
                .unwrap_or_else(|| PathBuf::from(String::from_utf8(bytes.to_vec()).unwrap()));
            self.live.set(self.live.get() + 1);
            Ok(Resolved {
                refreshed: self
                    .moved
                    .as_ref()
                    .map(|_| path.to_string_lossy().as_bytes().to_vec()),
                path,
                scope: Scope(self.live.clone()),
            })
        }
    }
    fn fake() -> Fake {
        Fake {
            moved: None,
            fail: false,
            live: Rc::new(Cell::new(0)),
        }
    }

    #[test]
    fn a_selected_folder_restores_descendants_and_holds_its_scope() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("notes");
        fs::create_dir(&folder).unwrap();
        let file = dir.path().join("access.json");
        let (mut store, _) = Store::load(file.clone(), fake());
        store.remember(&folder).unwrap();
        store.remember(&folder).unwrap();
        assert_eq!(store.entries.len(), 1);
        drop(store);
        let backend = fake();
        let live = backend.live.clone();
        let (mut store, errors) = Store::load(file, backend);
        assert!(errors.is_empty());
        let mut paths = [folder.join("one.md"), folder.join("nested/two.md")];
        assert!(store.restore(&mut paths).is_empty());
        assert_eq!(live.get(), 1);
        drop(store);
        assert_eq!(live.get(), 0);
    }

    #[test]
    fn stale_bookmarks_refresh_and_relocate_the_session() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("old");
        fs::create_dir(&old).unwrap();
        let file = dir.path().join("access.json");
        let (mut store, _) = Store::load(file.clone(), fake());
        store.remember(&old).unwrap();
        drop(store);
        let new = dir.path().canonicalize().unwrap().join("new");
        let mut backend = fake();
        backend.moved = Some(new.clone());
        let (mut store, _) = Store::load(file.clone(), backend);
        let mut paths = [old.clone(), old.join("nested/note.md")];
        assert!(store.restore(&mut paths).is_empty());
        assert_eq!(paths, [new.clone(), new.join("nested/note.md")]);
        let (reloaded, _) = Store::load(file, fake());
        assert_eq!(reloaded.entries[0].path, new);
        assert_eq!(
            STANDARD.decode(&reloaded.entries[0].data).unwrap(),
            new.to_string_lossy().as_bytes()
        );
    }

    #[test]
    fn revoked_access_preserves_paths_and_does_not_discard_the_bookmark() {
        let dir = tempfile::tempdir().unwrap();
        let note = dir.path().join("note.md");
        fs::write(&note, "note").unwrap();
        let file = dir.path().join("access.json");
        let (mut store, _) = Store::load(file.clone(), fake());
        store.remember(&note).unwrap();
        drop(store);
        let before = fs::read(&file).unwrap();
        let mut backend = fake();
        backend.fail = true;
        let (mut store, _) = Store::load(file.clone(), backend);
        let mut paths = [note.clone()];
        assert_eq!(store.restore(&mut paths).len(), 1);
        assert_eq!(paths, [note]);
        assert!(store.active.is_empty());
        assert_eq!(fs::read(file).unwrap(), before);
    }

    #[test]
    fn invalid_catalog_is_preserved_before_reauthorization() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("access.json");
        fs::write(&file, "invalid json").unwrap();
        let (mut store, errors) = Store::load(file.clone(), fake());
        assert_eq!(errors.len(), 1);
        let damaged = fs::read_dir(dir.path())
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert_ne!(damaged, file);
        assert_eq!(fs::read_to_string(damaged).unwrap(), "invalid json");
        store.remember(dir.path()).unwrap();
        assert!(file.is_file());
    }
    #[test]
    fn selected_symlink_spellings_restore_and_can_be_reselected() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("notes");
        fs::create_dir(&folder).unwrap();
        let alias = dir.path().join("alias");
        std::os::unix::fs::symlink(&folder, &alias).unwrap();
        let file = dir.path().join("access.json");
        let (mut store, _) = Store::load(file.clone(), fake());
        store.remember(&folder).unwrap();
        store.remember(&alias).unwrap();
        drop(store);
        let backend = fake();
        let live = backend.live.clone();
        let (mut store, _) = Store::load(file, backend);
        let mut paths = [alias.join("missing.md")];
        assert!(store.restore(&mut paths).is_empty());
        assert_eq!(paths[0], folder.canonicalize().unwrap().join("missing.md"));
        assert_eq!(live.get(), 1);
    }

    #[test]
    fn failed_persistence_keeps_the_catalog_and_releases_the_new_scope() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("notes");
        fs::create_dir(&folder).unwrap();
        let file = dir.path().join("access.json");
        let backend = fake();
        let live = backend.live.clone();
        let (mut store, _) = Store::load(file.clone(), backend);
        store.remember(&folder).unwrap();
        let before = fs::read(&file).unwrap();
        let other = dir.path().join("other");
        fs::create_dir(&other).unwrap();
        // A regular file cannot be the parent of an atomic-write target.
        store.path = file.join("unwritable.json");
        assert!(store.remember(&other).is_err());
        assert_eq!(store.entries.len(), 1);
        assert_eq!(store.active.len(), 1);
        assert_eq!(live.get(), 1);
        assert_eq!(fs::read(&file).unwrap(), before);
        drop(store);
        assert_eq!(live.get(), 0);
    }

    #[test]
    fn failed_refresh_persistence_keeps_the_restored_scope_alive() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("notes");
        fs::create_dir(&folder).unwrap();
        let file = dir.path().join("access.json");
        let (mut store, _) = Store::load(file.clone(), fake());
        store.remember(&folder).unwrap();
        drop(store);
        let before = fs::read(&file).unwrap();
        let moved = dir.path().join("moved");
        let mut backend = fake();
        backend.moved = Some(moved.clone());
        let live = backend.live.clone();
        let (mut store, _) = Store::load(file.clone(), backend);
        store.path = file.join("unwritable.json");
        let mut paths = [folder];
        assert_eq!(store.restore(&mut paths).len(), 1);
        assert_eq!(paths, [moved]);
        assert_eq!(live.get(), 1);
        assert_eq!(fs::read(&file).unwrap(), before);
        drop(store);
        assert_eq!(live.get(), 0);
    }
    #[test]
    fn a_later_parent_grant_preserves_an_independently_selected_file_alias() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("notes");
        fs::create_dir(&folder).unwrap();
        let note = folder.join("note.md");
        fs::write(&note, "note").unwrap();
        let alias = dir.path().join("alias.md");
        std::os::unix::fs::symlink(&note, &alias).unwrap();
        let file = dir.path().join("access.json");
        let (mut store, _) = Store::load(file.clone(), fake());
        store.remember(&alias).unwrap();
        store.remember(&folder).unwrap();
        drop(store);
        let backend = fake();
        let live = backend.live.clone();
        let (mut store, _) = Store::load(file, backend);
        let mut paths = [alias];
        assert!(store.restore(&mut paths).is_empty());
        assert_eq!(
            paths[0].as_os_str(),
            note.canonicalize().unwrap().as_os_str()
        );
        assert_eq!(fs::read_to_string(&paths[0]).unwrap(), "note");
        assert_eq!(live.get(), 2);
        assert!(store.has_write_access(&folder));
    }
    #[test]
    fn atomic_write_access_requires_an_active_parent_directory_grant() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("notes");
        fs::create_dir_all(folder.join("nested")).unwrap();
        let note = folder.join("note.md");
        fs::write(&note, "note").unwrap();
        let file = dir.path().join("access.json");
        let (mut store, _) = Store::load(file.clone(), fake());
        store.remember(&note).unwrap();
        assert!(!store.has_write_access(&folder));
        store.remember(&folder).unwrap();
        assert!(store.has_write_access(&folder));
        assert!(store.has_write_access(&folder.join("nested")));
        assert!(!store.has_write_access(dir.path()));
        drop(store);
        let (mut store, _) = Store::load(file, fake());
        assert!(!store.has_write_access(&folder));
        assert!(store.restore(&mut [folder.clone()]).is_empty());
        assert!(store.has_write_access(&folder));
        drop(store);
        let (mut store, _) = Store::load(dir.path().join("access.json"), fake());
        assert!(store.restore(&mut [note]).is_empty());
        assert!(store.has_write_access(&folder));
    }

    #[test]
    fn directory_write_access_follows_targets_without_authorizing_external_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("notes");
        let outside = dir.path().join("outside");
        fs::create_dir(&folder).unwrap();
        fs::create_dir(&outside).unwrap();
        let alias = dir.path().join("alias");
        std::os::unix::fs::symlink(&folder, &alias).unwrap();
        let external_link = folder.join("external");
        std::os::unix::fs::symlink(&outside, &external_link).unwrap();
        let (mut store, _) = Store::load(dir.path().join("access.json"), fake());
        store.remember(&folder).unwrap();
        assert!(store.has_write_access(&alias));
        assert!(!store.has_write_access(&external_link));
    }
}
