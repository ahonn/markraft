//! Public construction and host request contracts.
use crate::storage::Preferences;
use std::path::PathBuf;

#[derive(Clone)]
pub struct WorkspaceOptions {
    pub notes_directory: PathBuf,
    pub state_directory: PathBuf,
    pub cache_directory: Option<PathBuf>,
    pub preferences: Preferences,
}
impl WorkspaceOptions {
    pub fn new(notes_directory: PathBuf, state_directory: PathBuf) -> Self {
        Self {
            notes_directory,
            state_directory,
            cache_directory: None,
            preferences: Preferences::default(),
        }
    }
}
#[derive(Clone)]
pub enum WorkspaceEvent {
    OpenSettings,
    /// The host persists editor preferences changed through workspace controls.
    OptionsChanged(Preferences),
    HideRequested,
    QuitRequested,
    ReportIssue,
    RevealLogs,
    LocaleChanged(crate::locale::I18n),
}
/// A save receipt distinguishes the committed revision and actual Markdown paths.
#[derive(Debug, Clone)]
pub struct SaveReceipt {
    pub revision: u64,
    pub markdown_paths: Vec<(String, PathBuf)>,
    pub conflict_notes: Vec<String>,
}
/// Host-observable operation failures, independent of translated UI messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceError {
    Storage(crate::fs::StoreError),
    /// An accepted operation or another close must finish before retrying.
    Busy,
    Closed,
    /// The host replaced the notes directory before this request completed.
    Superseded,
}
impl From<crate::fs::StoreError> for WorkspaceError {
    fn from(error: crate::fs::StoreError) -> Self {
        Self::Storage(error)
    }
}
impl std::fmt::Display for WorkspaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Storage(error) => error.fmt(f),
            Self::Busy => {
                f.write_str("The workspace is busy; retry after the current operation completes")
            }
            Self::Closed => f.write_str("The workspace is closed"),
            Self::Superseded => f.write_str("The workspace changed before the operation completed"),
        }
    }
}
impl std::error::Error for WorkspaceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Storage(error) => Some(error),
            _ => None,
        }
    }
}
