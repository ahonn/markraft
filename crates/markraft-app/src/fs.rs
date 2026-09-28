//! The filesystem primitives the store and the settings file share: atomic and
//! no-replace writes, the Trash, metadata copying, and the sentence an
//! `io::Error` becomes for the person reading it.
//!
//! Nothing here knows what a note or a setting is; both `vault` and `storage`
//! are built on it, and neither on the other. [`StoreError`] is what every
//! failure below the application is reported as.

use crate::locale::Message;
use std::{
    fmt,
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
};

/// Why the store, the settings file or the instance lock could not do what
/// was asked. The application renders [`Self::message`] in its current language;
/// [`fmt::Display`] keeps diagnostics and logs in English. A caller that needs
/// to distinguish a lock from a full disk, or several failures from one, reads
/// the variant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreError {
    /// The operating system refused something at `path`. `detail` is its own
    /// report, kept for the log and for the last-resort sentence.
    Io {
        path: PathBuf,
        kind: io::ErrorKind,
        detail: String,
    },
    /// Another Markraft holds what this one needs: the folder's lock, or the
    /// running instance that should have answered.
    Locked(Message),
    /// A state file the store keeps could not be read as what it should hold.
    Json { path: PathBuf, detail: Message },
    /// A refusal the store made itself: a name it cannot file under, a path
    /// outside the folder, a library or preferences it will not write.
    Invalid(Message),
    /// The save worker is gone or did not answer in time.
    Worker(Message),
    /// Disk won: these notes, by title, kept the disk version and their local
    /// edits were written beside the file as conflicted copies. Not a failure
    /// of the store, but not a save of what the caller handed it either, so a
    /// caller must not take the note as saved.
    Conflict(Vec<Message>),
    /// Several failures from one save, reported together.
    Several(Vec<StoreError>),
}

impl StoreError {
    /// One error from a save's worth of them: the error itself when there was
    /// one, all of them otherwise.
    pub fn several(mut errors: Vec<StoreError>) -> StoreError {
        if errors.len() == 1 {
            errors.remove(0)
        } else {
            StoreError::Several(errors)
        }
    }
}

impl StoreError {
    pub fn message(&self) -> Message {
        match self {
            Self::Io { path, kind, detail } => message(path, *kind, detail),
            Self::Locked(text) | Self::Invalid(text) | Self::Worker(text) => text.clone(),
            Self::Json { detail, .. } => {
                Message::new("error.saved-state").arg("detail", detail.clone())
            }
            Self::Conflict(titles) => Message::join(
                titles
                    .iter()
                    .map(|title| Message::new("error.conflict-note").arg("title", title.clone()))
                    .collect(),
                "\n",
            ),
            Self::Several(errors) => {
                Message::join(errors.iter().map(Self::message).collect(), "\n")
            }
        }
    }
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message().fmt(f)
    }
}

impl From<Message> for StoreError {
    fn from(message: Message) -> Self {
        Self::Invalid(message)
    }
}

impl From<&StoreError> for Message {
    fn from(error: &StoreError) -> Self {
        error.message()
    }
}

impl From<StoreError> for Message {
    fn from(error: StoreError) -> Self {
        error.message()
    }
}

impl std::error::Error for StoreError {}

/// External diagnostic text. Application-owned sentences use [`Message`] keys.
impl From<String> for StoreError {
    fn from(text: String) -> StoreError {
        StoreError::Invalid(text.into())
    }
}

impl From<&str> for StoreError {
    fn from(text: &str) -> StoreError {
        StoreError::Invalid(text.into())
    }
}

pub(crate) fn same_regular_file(left: &Path, right: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (fs::symlink_metadata(left), fs::symlink_metadata(right)) {
        (Ok(a), Ok(b)) => {
            a.file_type().is_file()
                && b.file_type().is_file()
                && a.nlink() == 1
                && b.nlink() == 1
                && a.dev() == b.dev()
                && a.ino() == b.ino()
        }
        _ => false,
    }
}
/// Rename `from` to `to` without ever replacing a file already there. Checking first
/// and renaming after would leave a moment for another program to put a file at `to`,
/// and that file would be lost, so the refusal is the file system's own.
pub(crate) fn move_without_replacing(from: &Path, to: &Path) -> Result<(), StoreError> {
    let occupied = || {
        Message::new("error.rename-exists")
            .arg(
                "name",
                to.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
            )
            .into()
    };
    // A name that differs only in case is the same file on a file system that does
    // not keep case, and renaming a file over itself replaces nothing.
    if same_regular_file(from, to) {
        return fs::rename(from, to).map_err(|e| describe(from, &e));
    }
    #[cfg(target_os = "macos")]
    {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        let path = |p: &Path| CString::new(p.as_os_str().as_bytes()).map_err(|e| e.to_string());
        let (source, target) = (path(from)?, path(to)?);
        // SAFETY: both arguments are NUL-terminated strings that outlive the call.
        if unsafe { libc::renamex_np(source.as_ptr(), target.as_ptr(), libc::RENAME_EXCL) } == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EEXIST) => return Err(occupied()),
            // A volume that cannot promise exclusivity falls through to the check below.
            Some(libc::ENOTSUP) => {}
            _ => return Err(describe(from, &error)),
        }
    }
    if fs::symlink_metadata(to).is_ok() {
        return Err(occupied());
    }
    fs::rename(from, to).map_err(|e| describe(from, &e))
}
/// Moves a file into the system Trash. Returns the Trash location when the platform
/// reports one; callers do not restore from it.
#[cfg(target_os = "macos")]
pub(crate) fn move_to_trash(path: &Path) -> Result<Option<PathBuf>, StoreError> {
    use objc2::rc::Retained;
    use objc2_foundation::{NSFileManager, NSURL};
    let url = NSURL::fileURLWithPath(&objc2_foundation::NSString::from_str(
        &path.to_string_lossy(),
    ));
    let manager = NSFileManager::defaultManager();
    let mut result: Option<Retained<NSURL>> = None;
    manager
        .trashItemAtURL_resultingItemURL_error(&url, Some(&mut result))
        .map_err(|e| e.to_string())?;
    // Where it landed, for the window to reveal. The platform does not always say,
    // and a file that went without an address went all the same.
    Ok(result
        .and_then(|url| url.path())
        .map(|path| PathBuf::from(path.to_string())))
}
#[cfg(not(target_os = "macos"))]
pub(crate) fn move_to_trash(_path: &Path) -> Result<Option<PathBuf>, StoreError> {
    Err(Message::new("error.trash-unavailable").into())
}
/// The permissions a new file in `folder` should have: the folder's own, without the
/// execute bits a Markdown file has no use for.
#[cfg(unix)]
pub(crate) fn inherit_folder_mode(folder: &Path, file: &Path) -> Result<(), StoreError> {
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::metadata(folder)
        .map_err(|e| describe(folder, &e))?
        .permissions()
        .mode()
        & 0o666;
    fs::set_permissions(file, fs::Permissions::from_mode(mode)).map_err(|e| describe(file, &e))
}
#[cfg(not(unix))]
pub(crate) fn inherit_folder_mode(_folder: &Path, _file: &Path) -> Result<(), StoreError> {
    Ok(())
}
#[cfg(target_os = "macos")]
pub(crate) fn copy_metadata(from: &Path, to: &Path) -> Result<(), StoreError> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    unsafe extern "C" {
        fn copyfile(
            from: *const std::ffi::c_char,
            to: *const std::ffi::c_char,
            state: *mut std::ffi::c_void,
            flags: u32,
        ) -> i32;
    }
    let source = CString::new(from.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    let target = CString::new(to.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    // COPYFILE_ACL | COPYFILE_STAT | COPYFILE_XATTR, without copying file data.
    if unsafe { copyfile(source.as_ptr(), target.as_ptr(), std::ptr::null_mut(), 7) } != 0 {
        return Err(describe(from, &io::Error::last_os_error()));
    }
    Ok(())
}
#[cfg(not(target_os = "macos"))]
pub(crate) fn copy_metadata(from: &Path, to: &Path) -> Result<(), StoreError> {
    fs::set_permissions(
        to,
        fs::metadata(from)
            .map_err(|e| describe(from, &e))?
            .permissions(),
    )
    .map_err(|e| describe(to, &e))
}
/// The one place where a file-system failure becomes something a person can act
/// on. An `io::Error` reads as the operating system's own report of a system
/// call — the kind of sentence that belongs in a log, not in a window — so the
/// raw text is printed for a bug report and reaches the interface only inside
/// the parentheses of the last resort, through [`StoreError::Io`]'s `Display`.
pub(crate) fn describe(path: &Path, error: &io::Error) -> StoreError {
    log::warn!("{}: {error} ({:?})", path.display(), error.kind());
    StoreError::Io {
        path: path.to_owned(),
        kind: error.kind(),
        detail: error.to_string(),
    }
}

/// The sentence a person reads for an [`StoreError::Io`], separated so it can
/// be tested without the log.
fn message(path: &Path, kind: io::ErrorKind, detail: &str) -> Message {
    let message = match kind {
        io::ErrorKind::NotFound => Message::new("error.file-missing"),
        io::ErrorKind::PermissionDenied => Message::new("error.file-permission"),
        io::ErrorKind::AlreadyExists => Message::new("error.file-exists"),
        io::ErrorKind::InvalidFilename => Message::new("error.file-invalid-name"),
        io::ErrorKind::StorageFull => Message::new("error.disk-full"),
        io::ErrorKind::ReadOnlyFilesystem => Message::new("error.disk-read-only"),
        io::ErrorKind::TimedOut => Message::new("error.file-timeout"),
        _ => Message::new("error.file-unavailable").arg("detail", detail.to_owned()),
    };
    message.arg("name", file_label(path))
}

/// A file or folder as the user knows it. The whole path belongs in the log; in
/// a message it would bury the sentence.
pub(crate) fn file_label(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

pub(crate) fn read_optional(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    let parent = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(parent).map_err(|error| describe(parent, &error))?;
    faults::check(path, faults::Stage::Create).map_err(|error| describe(parent, &error))?;
    let mut output =
        tempfile::NamedTempFile::new_in(parent).map_err(|error| describe(parent, &error))?;
    faults::check(path, faults::Stage::Write)
        .and_then(|_| output.write_all(bytes))
        .and_then(|_| output.as_file().sync_all())
        .map_err(|error| describe(path, &error))?;
    faults::check(path, faults::Stage::Persist).map_err(|error| describe(path, &error))?;
    output
        .persist(path)
        .map_err(|error| describe(path, &error.error))?;
    // Sync the directory entry as well as the file contents when the platform supports it.
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| describe(parent, &error))?;
    Ok(())
}

/// Where a write can be made to fail in tests: the disk filling up or a rename
/// being refused cannot be staged on a real disk on demand. Outside tests every
/// check passes.
pub(crate) mod faults {
    /// A step of a write through a temporary file.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum Stage {
        /// Creating the temporary file beside the target.
        Create,
        /// Writing and syncing its bytes.
        Write,
        /// Renaming it over the target.
        Persist,
    }

    #[cfg(not(test))]
    #[inline(always)]
    pub(crate) fn check(_path: &std::path::Path, _stage: Stage) -> std::io::Result<()> {
        Ok(())
    }

    #[cfg(test)]
    pub(crate) use injected::{check, inject};

    /// Faults are kept by folder rather than by thread: the vault writes from a
    /// thread of its own, and tests over other folders run beside each other.
    #[cfg(test)]
    #[cfg_attr(coverage_nightly, coverage(off))]
    mod injected {
        use super::Stage;
        use std::{
            io,
            path::{Path, PathBuf},
            sync::Mutex,
        };

        static FAULTS: Mutex<Vec<(PathBuf, Stage, io::ErrorKind)>> = Mutex::new(Vec::new());

        /// Make every write under `folder` fail at `stage` with `kind` until the
        /// returned value is dropped.
        #[must_use]
        pub(crate) fn inject(folder: &Path, stage: Stage, kind: io::ErrorKind) -> Fault {
            let folder = folder.canonicalize().expect("an existing folder");
            faults().push((folder.clone(), stage, kind));
            Fault(folder)
        }

        pub(crate) struct Fault(PathBuf);

        impl Drop for Fault {
            fn drop(&mut self) {
                faults().retain(|(folder, ..)| *folder != self.0);
            }
        }

        pub(crate) fn check(path: &Path, stage: Stage) -> io::Result<()> {
            let Some(parent) = path.parent().and_then(|parent| parent.canonicalize().ok()) else {
                return Ok(());
            };
            match faults()
                .iter()
                .find(|(folder, at, _)| *at == stage && parent.starts_with(folder))
            {
                Some((.., kind)) => Err(io::Error::from(*kind)),
                None => Ok(()),
            }
        }

        fn faults() -> std::sync::MutexGuard<'static, Vec<(PathBuf, Stage, io::ErrorKind)>> {
            FAULTS
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn a_failure_is_worded_by_its_kind_and_names_only_the_file() {
        let path = Path::new("/somewhere/deep/Notes/todo.md");
        let gone = describe(path, &io::Error::from(io::ErrorKind::NotFound)).to_string();
        assert!(gone.starts_with("“todo.md” is no longer there"), "{gone}");
        assert!(!gone.contains("/somewhere"), "{gone}");
        let other = describe(path, &io::Error::other("boom")).to_string();
        assert_eq!(other, "Markraft could not use “todo.md” (boom).");
        let several = StoreError::several(vec!["one".into(), "two".into()]);
        assert_eq!(several.to_string(), "one\ntwo");
        assert_eq!(StoreError::several(vec!["one".into()]), "one".into());
    }

    #[test]
    fn store_errors_keep_nested_messages_localizable_and_diagnostics_literal() {
        let language = crate::locale::LanguagePreference::Locale("zh-Hant".into());
        let i18n = crate::locale::I18n::for_preference(&language);
        let error = StoreError::several(vec![
            describe(
                Path::new("/notes/草稿%{name}.md"),
                &io::Error::other("EIO: %{detail}"),
            ),
            Message::new("error.recovery-cleanup")
                .arg("detail", Message::new("error.worker-stopped"))
                .into(),
        ]);
        assert_eq!(
            error.message().render(&i18n),
            "Markraft 無法使用「草稿%{name}.md」（EIO: %{detail}）。\n無法清除舊的復原草稿：儲存工作已停止"
        );
        assert!(
            error
                .to_string()
                .contains("Old recovery drafts could not be cleared: The save worker stopped")
        );
    }

    #[test]
    fn an_atomic_write_creates_the_folder_and_replaces_the_file_whole() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("nested").join("a.txt");
        atomic_write(&path, b"one").unwrap();
        atomic_write(&path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        assert_eq!(read_optional(&path).unwrap().as_deref(), Some(&b"two"[..]));
        assert_eq!(
            read_optional(&path.with_extension("missing")).unwrap(),
            None
        );
    }
}
