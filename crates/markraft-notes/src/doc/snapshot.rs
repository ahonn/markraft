//! Frozen committed input for export and other background document readers.
//!
//! This does not introduce source editing: byte-only edits and cross-mode undo
//! need an explicit history contract before a source editor can be added.

use super::*;
use crate::{fs::StoreError, storage::Note};
use markraft_commonmark::{HouseStyle, SourceSnapshot};
use std::path::PathBuf;

/// A coherent document and its exact Markdown at capture time.
#[derive(Clone, Debug)]
pub struct DocumentSnapshot {
    pub note_id: String,
    pub document: Node,
    /// The existing library generation at capture, scoped to that library's
    /// lifetime. This is not a globally unique document version.
    pub library_generation: u64,
    pub markdown: String,
    pub base_path: Option<PathBuf>,
    pub auto_number_equations: bool,
    /// The file's bytes as read, for writing a changed copy of the document
    /// back with everything it did not change as it was.
    pub source: Option<SourceSnapshot>,
}

/// Capturing clones immutable state; rendering belongs on the persistence worker.
pub(crate) struct PendingSnapshot {
    note: Note,
    generation: u64,
    auto_number_equations: bool,
    source: Option<SourceSnapshot>,
    house: HouseStyle,
}

impl PendingSnapshot {
    pub(crate) fn new(
        note: Note,
        generation: u64,
        auto_number_equations: bool,
        source: Option<SourceSnapshot>,
        house: HouseStyle,
    ) -> Self {
        Self {
            note,
            generation,
            auto_number_equations,
            source,
            house,
        }
    }

    pub(crate) fn render(self) -> Result<DocumentSnapshot, StoreError> {
        let markdown = match &self.source {
            Some(source) => source
                .render(schema(), &self.note.document)
                .map_err(|error| StoreError::from(error.to_string()))?,
            None => format!(
                "{}\n",
                to_markdown_in(&self.note.document, &HouseStyleHandle::new(self.house))
            ),
        };
        let snapshot = DocumentSnapshot {
            note_id: self.note.id,
            document: self.note.document,
            library_generation: self.generation,
            markdown,
            base_path: self
                .note
                .path
                .and_then(|path| path.parent().map(ToOwned::to_owned)),
            auto_number_equations: self.auto_number_equations,
            source: self.source,
        };
        log::debug!(
            "document_snapshot note={} generation={} positions={} bytes={} file_backed={} auto_number={}",
            snapshot.note_id,
            snapshot.library_generation,
            snapshot.document.content_size(),
            snapshot.markdown.len(),
            snapshot.base_path.is_some(),
            snapshot.auto_number_equations,
        );
        Ok(snapshot)
    }
}
