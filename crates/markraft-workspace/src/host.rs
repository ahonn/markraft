//! Public construction and host request contracts.
use crate::storage::EditorPreferences;
use std::path::PathBuf;

/// What every workspace needs, whatever stores its notes. The storage itself is
/// an argument of the constructor: Markdown folders, a backend, or a session.
#[derive(Clone, Default)]
#[non_exhaustive]
pub struct WorkspaceOptions {
    pub cache_directory: Option<PathBuf>,
    pub preferences: EditorPreferences,
}
#[derive(Clone)]
#[non_exhaustive]
pub enum WorkspaceEvent {
    OpenSettings,
    /// The host persists editor preferences changed through workspace controls.
    OptionsChanged(EditorPreferences),
    HideRequested,
    QuitRequested,
    ReportIssue,
    RevealLogs,
    LocaleChanged(crate::locale::I18n),
}
/// What a workspace save committed: the revision, each note's outcome and the
/// Markdown paths when the notes are files. The headless library's receipt is
/// `markraft_notes::SaveReceipt`, which names notes by ID instead.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct WorkspaceSaveReceipt {
    pub revision: u64,
    pub notes: Vec<markraft_notes::NoteSaveOutcome>,
    pub markdown_paths: Vec<(String, PathBuf)>,
    pub conflict_notes: Vec<String>,
}
/// A save that was queued when its future was made. The system quit hook has no
/// window to call back into, so this one request answers through a future.
pub type PendingSave = std::pin::Pin<
    Box<dyn Future<Output = Result<WorkspaceSaveReceipt, WorkspaceError>> + Send + 'static>,
>;
/// Host-observable operation failures, independent of translated UI messages.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
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
