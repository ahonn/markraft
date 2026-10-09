//! Host-owned persistence contracts. Implementations run on Markraft's storage
//! worker and must finish local writes before returning a successful commit.
use crate::{NoteId, storage::WorkspaceSettings};
use std::sync::Arc;

/// Opaque, durable compare-and-swap token. Unlike an editor generation, this
/// token remains meaningful when the host closes and reopens its database.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct StorageRevision(pub String);

#[derive(Clone, Debug)]
pub struct BackendNote {
    pub id: NoteId,
    /// Exact Markdown, including front matter and unmodified whitespace.
    pub markdown: String,
    pub title: Option<String>,
    pub logical_key: Option<String>,
    pub revision: StorageRevision,
    pub created_at: u64,
    pub updated_at: u64,
    pub pinned: bool,
}

#[derive(Clone, Debug, Default)]
pub struct BackendSnapshot {
    pub notes: Vec<BackendNote>,
    pub active_id: Option<NoteId>,
    pub workspace: WorkspaceSettings,
}

#[derive(Clone, Debug)]
pub enum BackendMutation {
    Put {
        id: NoteId,
        /// None means create-only, never unconditional overwrite. A deleted
        /// note counts as absent, so a create replaces its tombstone.
        expected: Option<StorageRevision>,
        markdown: String,
        title: Option<String>,
        logical_key: Option<String>,
        created_at: u64,
        updated_at: u64,
        pinned: bool,
    },
    Delete {
        id: NoteId,
        expected: StorageRevision,
        deleted_at: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum BackendError {
    Conflict {
        id: NoteId,
        /// The live record's revision. None when the note is absent or deleted.
        actual: Option<StorageRevision>,
    },
    Unsupported(&'static str),
    Invalid(String),
    Unavailable(String),
}
impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Conflict { id, .. } => write!(f, "Note {id} changed in storage"),
            Self::Unsupported(operation) => write!(f, "Storage does not support {operation}"),
            Self::Invalid(message) | Self::Unavailable(message) => f.write_str(message),
        }
    }
}
impl std::error::Error for BackendError {}

#[derive(Clone, Copy, Debug, Default)]
pub struct BackendCapabilities {
    pub file_operations: bool,
    pub assets: bool,
}

/// The SHA-256 of `bytes` in lowercase hexadecimal. Stored notes and their hosts
/// keep these strings, so the spelling never changes.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write;
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// The scheme of an image source that names an asset in the backend.
pub const ASSET_SCHEME: &str = "markraft-asset:";

/// The SHA-256 of an asset's bytes, in lowercase hexadecimal. Equal content has
/// one ID on every device, so a repeated write stores nothing new and an ID can
/// never come to name different content.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct AssetId(pub String);
impl AssetId {
    pub fn for_content(bytes: &[u8]) -> Self {
        Self(sha256_hex(bytes))
    }
    /// The image source that refers to this asset.
    pub fn source(&self) -> String {
        format!("{ASSET_SCHEME}{}", self.0)
    }
    /// The asset that an image source refers to, if it names one.
    pub fn from_source(source: &str) -> Option<Self> {
        source
            .strip_prefix(ASSET_SCHEME)
            .map(|id| Self(id.to_owned()))
    }
}
#[derive(Clone, Debug)]
pub struct Asset {
    pub id: AssetId,
    pub media_type: String,
    pub bytes: Vec<u8>,
}
impl Asset {
    /// An asset whose ID matches its content.
    pub fn new(media_type: impl Into<String>, bytes: Vec<u8>) -> Self {
        Self {
            id: AssetId::for_content(&bytes),
            media_type: media_type.into(),
            bytes,
        }
    }
}

/// The assets that a note's Markdown refers to. The component never deletes an
/// asset, because undo, history, and another device can still refer to it. A host
/// that wants to reclaim space collects these from every note it keeps.
pub fn asset_references(markdown: &str) -> Result<Vec<AssetId>, BackendError> {
    let source = markraft_commonmark::SourceDocument::parse(crate::doc::schema(), markdown)
        .map_err(|error| BackendError::Invalid(error.to_string()))?;
    let mut found = Vec::new();
    source.document().descendants(&mut |node, _, _, _| {
        if let Some(id) = node
            .attrs()
            .get("src")
            .and_then(|value| value.as_str())
            .and_then(AssetId::from_source)
            && !found.contains(&id)
        {
            found.push(id);
        }
        true
    });
    Ok(found)
}

/// Tells the storage worker that the host changed records outside this session,
/// for example after it imported remote changes. The call carries no content:
/// the worker reads the named notes again through the backend.
#[derive(Clone)]
pub struct ChangeNotifier(Arc<dyn Fn(Option<Vec<NoteId>>) + Send + Sync>);
impl ChangeNotifier {
    pub(crate) fn new(notify: impl Fn(Option<Vec<NoteId>>) + Send + Sync + 'static) -> Self {
        Self(Arc::new(notify))
    }
    /// Read these notes again. An ID that the backend no longer returns is treated as deleted.
    pub fn changed(&self, ids: Vec<NoteId>) {
        (self.0)(Some(ids))
    }
    /// Read every note again. Use it when the host cannot name the changed notes.
    pub fn changed_all(&self) {
        (self.0)(None)
    }
}

/// A local persistence boundary, independent of GPUI and network runtimes.
///
/// Each commit is atomic for one note, including its revision and any pending
/// sync marker. Markraft reports individual outcomes for multi-note saves.
/// Cloud acknowledgements and account-specific state belong to the host.
/// Implementations must reject stale expected revisions and retain the current
/// record on failure. `load` is also the full-resync fallback: callers can refresh
/// after the host imports remote changes without trusting a lossy event queue.
///
/// A backend must store each note under the ID it receives and must not assign
/// its own. A note with a logical key has an ID derived from that key
/// ([`NoteId::for_logical_key`]), so the ID alone keeps the key unique.
pub trait NotesBackend: Send + 'static {
    fn set_change_notifier(&mut self, _notify: ChangeNotifier) {}
    fn load(&mut self) -> Result<BackendSnapshot, BackendError>;
    /// The live notes among `ids`. Absent and deleted notes are left out.
    /// Override the default when reading every note is expensive.
    fn read(&mut self, ids: &[NoteId]) -> Result<Vec<BackendNote>, BackendError> {
        let mut notes = self.load()?.notes;
        notes.retain(|note| ids.contains(&note.id));
        Ok(notes)
    }
    fn commit(&mut self, mutation: BackendMutation) -> Result<StorageRevision, BackendError>;
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities::default()
    }
    fn save_workspace(
        &mut self,
        _active_id: Option<&NoteId>,
        _workspace: &WorkspaceSettings,
    ) -> Result<(), BackendError> {
        Ok(())
    }
    fn read_asset(&mut self, _id: &AssetId) -> Result<Asset, BackendError> {
        Err(BackendError::Unsupported("attachments"))
    }
    fn write_asset(&mut self, _asset: Asset) -> Result<(), BackendError> {
        Err(BackendError::Unsupported("attachments"))
    }
}
