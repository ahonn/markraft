//! Persistent user-selected file access, separate from portable note metadata.
//!
//! Keep each restored security scope alive for the application state lifetime:
//! save workers, image loaders, and file watchers may still use it after a panel
//! closes or a different note becomes active.

use crate::{locale::Message, storage::Settings};
use std::path::Path;

#[cfg(any(feature = "mac-app-store", test))]
mod bookmarks;
#[cfg(feature = "mac-app-store")]
mod native;

#[derive(Default)]
pub struct FileAccess {
    #[cfg(feature = "mac-app-store")]
    store: Option<bookmarks::Store<native::Native>>,
}

impl FileAccess {
    /// Only the primary process owns the bookmark file. Direct-download builds
    /// and headless application tests use the disabled default instance.
    pub fn load(settings_path: &Path) -> (Self, Vec<Message>) {
        #[cfg(feature = "mac-app-store")]
        {
            let path = settings_path.with_extension("bookmarks.json");
            let (store, errors) = bookmarks::Store::load(path, native::Native);
            (
                Self { store: Some(store) },
                errors.into_iter().map(Message::from).collect(),
            )
        }
        #[cfg(not(feature = "mac-app-store"))]
        {
            let _ = settings_path;
            (Self::default(), Vec::new())
        }
    }

    /// Restore permissions before the store, watchers, or session restoration
    /// first access external paths. A moved directory also relocates its notes.
    pub fn restore(&mut self, settings: &mut Settings) -> Vec<Message> {
        #[cfg(feature = "mac-app-store")]
        if let Some(store) = &mut self.store {
            let mut paths = settings.open_files.clone();
            paths.extend(settings.notes_folder.iter().cloned());
            let errors = store.restore(&mut paths);
            if settings.notes_folder.is_some() {
                settings.notes_folder = paths.pop();
            }
            settings.open_files = paths;
            return errors.into_iter().map(Message::from).collect();
        }
        let _ = settings;
        Vec::new()
    }

    /// Atomic saves create a sibling temporary file, so a file-only grant is
    /// insufficient. This checks the already-active directory scopes only.
    #[cfg(feature = "mac-app-store")]
    pub fn has_write_access(&self, parent: &Path) -> bool {
        #[cfg(feature = "mac-app-store")]
        if let Some(store) = &self.store {
            return inside_container(parent) || store.has_write_access(parent);
        }
        let _ = parent;
        true
    }

    /// Call while the process still holds the implicit grant from an open panel,
    /// drag-and-drop, or Finder. This API never obtains access without that grant.
    pub fn remember(&mut self, path: &Path) -> Result<(), Message> {
        #[cfg(feature = "mac-app-store")]
        if let Some(store) = &mut self.store {
            if inside_container(path) {
                return Ok(());
            }
            return store.remember(path).map_err(|detail| {
                Message::new("error.file-unavailable")
                    .arg(
                        "name",
                        path.file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string(),
                    )
                    .arg("detail", detail)
            });
        }
        let _ = path;
        Ok(())
    }
}

#[cfg(feature = "mac-app-store")]
fn inside_container(path: &Path) -> bool {
    let home = objc2_foundation::NSHomeDirectory();
    // Container paths may include symlinks to external user folders.
    // Compare actual targets so those selections still receive bookmarks.
    match (
        path.canonicalize(),
        Path::new(&home.to_string()).canonicalize(),
    ) {
        (Ok(path), Ok(home)) => path.starts_with(home),
        _ => false,
    }
}
