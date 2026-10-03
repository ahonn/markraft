//! The extension point for selection kinds this crate does not provide.

use std::any::Any;

use serde_json::Value;

use crate::change::ChangeDesc;
use crate::error::NodeError;
use crate::node::Node;
use crate::schema::Schema;
use crate::slice::Slice;

use super::{Selection, SelectionRange};

/// A selection kind provided by an extension.
///
/// Implementors are values: `clone_box` and `eq_kind` give
/// [`Selection`] its `Clone` and `PartialEq`, and `as_any` lets a consumer
/// recover the concrete type.
///
/// The document is passed to every method rather than stored, so a kind can be
/// held across edits and mapped like any other selection.
pub trait SelectionKind: std::fmt::Debug + Send + Sync {
    /// A short, stable name used as the JSON discriminator.
    fn tag(&self) -> &str;

    /// Clone into a fresh box.
    fn clone_box(&self) -> Box<dyn SelectionKind>;

    /// Downcasting hook.
    fn as_any(&self) -> &dyn Any;

    /// Whether this selection equals `other`.
    ///
    /// Implementations normally downcast `other` and compare fields; a
    /// different concrete kind must compare unequal.
    fn eq_kind(&self, other: &dyn SelectionKind) -> bool;

    /// The fixed side of the selection.
    fn anchor(&self, doc: &Node) -> usize;

    /// The moving side of the selection.
    fn head(&self, doc: &Node) -> usize;

    /// Map the selection into the document `changes` produces.
    ///
    /// `doc` is the document after the change, so the implementation can fall
    /// back to a built-in selection when its own invariants no longer hold.
    fn map(&self, doc: &Node, changes: &ChangeDesc) -> Selection;

    /// Serialise the selection. The `tag` is added by the caller.
    fn to_json(&self, schema: &Schema) -> Value;

    /// The stretches this selection covers.
    fn ranges(&self, doc: &Node) -> Vec<SelectionRange> {
        vec![SelectionRange::new(self.anchor(doc), self.head(doc))]
    }

    /// The range replaced when content is typed over this selection.
    fn replacement_range(&self, doc: &Node) -> SelectionRange {
        let (anchor, head) = (self.anchor(doc), self.head(doc));
        SelectionRange::new(anchor, head)
    }

    /// The selected content.
    fn content(&self, doc: &Node) -> Slice {
        let range = self.replacement_range(doc);
        doc.slice(range.from, range.to)
            .unwrap_or_else(|_| Slice::empty())
    }

    /// Clipboard content with access to enclosing semantic scopes.
    fn content_with_schema(&self, doc: &Node, _schema: &Schema) -> Slice {
        self.content(doc)
    }

    /// Add a change to `spec` replacing this selection with `slice`.
    fn replace(
        &self,
        spec: crate::state::TransactionSpec,
        doc: &Node,
        slice: Slice,
    ) -> crate::state::TransactionSpec {
        let range = self.replacement_range(doc);
        spec.changes([crate::change::Change::replace(range.from, range.to, slice)])
    }

    /// Replace with access to the receiving schema. Existing kinds retain
    /// their replacement contract unless they need schema-aware fitting.
    fn replace_with_schema(
        &self,
        spec: crate::state::TransactionSpec,
        doc: &Node,
        _schema: &Schema,
        slice: Slice,
    ) -> crate::state::TransactionSpec {
        self.replace(spec, doc, slice)
    }

    /// Validate the selection against `doc`.
    fn check(&self, doc: &Node, schema: &Schema) -> Result<(), NodeError> {
        let _ = schema;
        let size = doc.content_size();
        for pos in [self.anchor(doc), self.head(doc)] {
            if pos > size {
                return Err(NodeError::PosOutOfRange { pos, size });
            }
        }
        Ok(())
    }
}
