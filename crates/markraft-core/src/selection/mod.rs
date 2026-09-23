//! Selections: what is selected, and where typed content goes.
//!
//! A selection is a value like everything else in this crate: it is mapped
//! through changes rather than mutated, and it is validated against the
//! document it belongs to.
//!
//! # Built-in kinds
//!
//! * [`Selection::Text`] — a range inside inline content. The *stored marks*
//!   that content typed at a cursor should get live on the state
//!   ([`EditorState::stored_marks`](crate::EditorState::stored_marks)), not
//!   here.
//! * [`Selection::Node`] — one selectable node, addressed by the position
//!   directly before it.
//! * [`Selection::All`] — the whole document.
//! * [`Selection::Custom`] — anything a consumer needs (a table cell range, a
//!   gap cursor), behind [`SelectionKind`].
//!
//! # Positions
//!
//! `anchor` is the fixed side and `head` the moving side; `from`/`to` are the
//! same pair ordered. Because `Node` and `All` derive their range from the
//! document, every accessor takes the document the selection belongs to.

mod custom;
mod json;
mod near;

pub use custom::SelectionKind;

use crate::change::ChangeDesc;
use crate::error::NodeError;
use crate::fragment::Fragment;
use crate::node::Node;
use crate::schema::Schema;
use crate::slice::Slice;

/// One contiguous stretch of a selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectionRange {
    /// Start of the range.
    pub from: usize,
    /// End of the range.
    pub to: usize,
}

impl SelectionRange {
    /// A range, ordered.
    pub fn new(a: usize, b: usize) -> SelectionRange {
        SelectionRange {
            from: a.min(b),
            to: a.max(b),
        }
    }

    /// Whether the range covers nothing.
    pub fn is_empty(&self) -> bool {
        self.from == self.to
    }
}

/// What is currently selected.
///
/// Cloning is cheap for the built-in kinds; a custom kind pays whatever its
/// own `clone_box` costs.
#[derive(Debug)]
pub enum Selection {
    /// A range in inline content.
    Text {
        /// The fixed side of the selection.
        anchor: usize,
        /// The moving side of the selection.
        head: usize,
    },
    /// One node, addressed by the position directly before it.
    Node {
        /// The position before the selected node.
        pos: usize,
    },
    /// The entire document.
    All,
    /// A selection kind provided by an extension.
    Custom(Box<dyn SelectionKind>),
}

impl Clone for Selection {
    fn clone(&self) -> Selection {
        match self {
            Selection::Text { anchor, head } => Selection::Text {
                anchor: *anchor,
                head: *head,
            },
            Selection::Node { pos } => Selection::Node { pos: *pos },
            Selection::All => Selection::All,
            Selection::Custom(kind) => Selection::Custom(kind.clone_box()),
        }
    }
}

impl PartialEq for Selection {
    fn eq(&self, other: &Selection) -> bool {
        match (self, other) {
            (
                Selection::Text {
                    anchor: a1,
                    head: h1,
                },
                Selection::Text {
                    anchor: a2,
                    head: h2,
                },
            ) => a1 == a2 && h1 == h2,
            (Selection::Node { pos: a }, Selection::Node { pos: b }) => a == b,
            (Selection::All, Selection::All) => true,
            (Selection::Custom(a), Selection::Custom(b)) => a.eq_kind(b.as_ref()),
            _ => false,
        }
    }
}

impl Eq for Selection {}

impl Default for Selection {
    fn default() -> Selection {
        Selection::cursor(0)
    }
}

impl Selection {
    /// An empty text selection at `pos`.
    pub fn cursor(pos: usize) -> Selection {
        Selection::Text {
            anchor: pos,
            head: pos,
        }
    }

    /// A text selection from `anchor` to `head`.
    pub fn text(anchor: usize, head: usize) -> Selection {
        Selection::Text { anchor, head }
    }

    /// A node selection on the node starting at `pos`.
    pub fn node(pos: usize) -> Selection {
        Selection::Node { pos }
    }

    /// A selection of a custom kind.
    pub fn custom(kind: Box<dyn SelectionKind>) -> Selection {
        Selection::Custom(kind)
    }

    /// The fixed side of the selection.
    pub fn anchor(&self, doc: &Node) -> usize {
        match self {
            Selection::Text { anchor, .. } => *anchor,
            Selection::Node { pos } => *pos,
            Selection::All => 0,
            Selection::Custom(kind) => kind.anchor(doc),
        }
    }

    /// The moving side of the selection.
    pub fn head(&self, doc: &Node) -> usize {
        match self {
            Selection::Text { head, .. } => *head,
            Selection::Node { pos } => *pos + node_size_at(doc, *pos),
            Selection::All => doc.content_size(),
            Selection::Custom(kind) => kind.head(doc),
        }
    }

    /// The lower end of the selection.
    pub fn from(&self, doc: &Node) -> usize {
        self.anchor(doc).min(self.head(doc))
    }

    /// The upper end of the selection.
    pub fn to(&self, doc: &Node) -> usize {
        self.anchor(doc).max(self.head(doc))
    }

    /// Whether the selection covers nothing.
    pub fn is_empty(&self, doc: &Node) -> bool {
        self.ranges(doc).iter().all(SelectionRange::is_empty)
    }

    /// Whether this is an empty *text* selection.
    pub fn is_cursor(&self) -> bool {
        matches!(self, Selection::Text { anchor, head, .. } if anchor == head)
    }

    /// The stretches this selection covers.
    ///
    /// The built-in kinds cover exactly one stretch; a custom kind may cover
    /// several (a table cell selection, for instance).
    pub fn ranges(&self, doc: &Node) -> Vec<SelectionRange> {
        match self {
            Selection::Custom(kind) => kind.ranges(doc),
            _ => vec![SelectionRange::new(self.anchor(doc), self.head(doc))],
        }
    }

    /// The range replaced when content is typed or pasted over this selection.
    pub fn replacement_range(&self, doc: &Node) -> SelectionRange {
        match self {
            Selection::Custom(kind) => kind.replacement_range(doc),
            _ => SelectionRange::new(self.from(doc), self.to(doc)),
        }
    }

    /// The selected content.
    pub fn content(&self, doc: &Node) -> Slice {
        match self {
            Selection::Node { pos } => match doc.node_at(*pos) {
                Some(node) => Slice::from_fragment(Fragment::from_node(node)),
                None => Slice::empty(),
            },
            Selection::Custom(kind) => kind.content(doc),
            _ => doc
                .slice(self.from(doc), self.to(doc))
                .unwrap_or_else(|_| Slice::empty()),
        }
    }

    /// Selected clipboard content, retaining enclosing inline semantic scopes.
    pub fn content_with_schema(&self, doc: &Node, schema: &Schema) -> Slice {
        match self {
            Selection::Node { .. } | Selection::Custom(_) => self.content(doc),
            _ => doc
                .slice_with_schema(schema, self.from(doc), self.to(doc))
                .unwrap_or_else(|_| Slice::empty()),
        }
    }

    /// Map this selection into the document `changes` produces.
    ///
    /// `doc` is the document *after* the change. A text selection whose head
    /// no longer lands in inline content, and a node selection whose node is
    /// gone, both fall back to [`Selection::near`].
    ///
    /// Unlike Wordgard, the mapping takes the resulting document as well as the
    /// change description: the built-in kinds need it to decide whether the
    /// mapped position is still valid.
    pub fn map(&self, schema: &Schema, doc: &Node, changes: &ChangeDesc) -> Selection {
        match self {
            Selection::Text { anchor, head } => {
                let size = doc.content_size();
                let mapped_head = changes
                    .map_pos(*head, 1, Default::default())
                    .unwrap_or(size);
                if !near::in_inline_content(schema, doc, mapped_head) {
                    return Selection::near(schema, doc, mapped_head, 1);
                }
                let mapped_anchor = changes
                    .map_pos(*anchor, 1, Default::default())
                    .unwrap_or(size);
                let anchor = if near::in_inline_content(schema, doc, mapped_anchor) {
                    mapped_anchor
                } else {
                    mapped_head
                };
                Selection::Text {
                    anchor,
                    head: mapped_head,
                }
            }
            Selection::Node { pos } => {
                let tracked = changes.map_pos(*pos, 1, crate::change::TrackMode::After);
                match tracked {
                    Some(mapped) if Selection::is_selectable(schema, doc, mapped) => {
                        Selection::Node { pos: mapped }
                    }
                    _ => {
                        let fallback = changes
                            .map_pos(*pos, 1, Default::default())
                            .unwrap_or_else(|| doc.content_size());
                        Selection::near(schema, doc, fallback, 1)
                    }
                }
            }
            Selection::All => Selection::All,
            Selection::Custom(kind) => kind.map(doc, changes),
        }
    }

    /// Whether a selectable node starts at `pos`.
    pub fn is_selectable(schema: &Schema, doc: &Node, pos: usize) -> bool {
        doc.node_at(pos).is_some_and(|node| {
            doc.resolve(pos).is_ok_and(|resolved| {
                resolved.node_after().is_some_and(|after| after == node)
                    && schema.node_type(node.type_id()).is_selectable()
            })
        })
    }

    /// Validate this selection against `doc`.
    ///
    /// Checks that positions are inside the document and that a node selection
    /// really points at a node. It deliberately does *not* require a text
    /// selection to sit in inline content: a position between blocks is a valid
    /// document position, and [`Selection::map`] is what normalises selections
    /// that should stay in text.
    pub fn check(&self, doc: &Node, schema: &Schema) -> Result<(), NodeError> {
        let size = doc.content_size();
        let bad = |pos: usize| NodeError::PosOutOfRange { pos, size };
        match self {
            Selection::Text { anchor, head, .. } => {
                for pos in [*anchor, *head] {
                    if pos > size {
                        return Err(bad(pos));
                    }
                    doc.resolve(pos)?;
                }
                Ok(())
            }
            Selection::Node { pos } => {
                if *pos > size {
                    return Err(bad(*pos));
                }
                let resolved = doc.resolve(*pos)?;
                if resolved.node_after().is_none() {
                    return Err(NodeError::InvalidContent {
                        node: "selection".into(),
                        message: format!("no node starts at {pos}"),
                    });
                }
                Ok(())
            }
            Selection::All => Ok(()),
            Selection::Custom(kind) => kind.check(doc, schema),
        }
    }
}

/// The size of the node starting at `pos`, or zero when there is none.
pub(crate) fn node_size_at(doc: &Node, pos: usize) -> usize {
    doc.resolve(pos)
        .ok()
        .and_then(|resolved| resolved.node_after())
        .map(|node| node.node_size())
        .unwrap_or(0)
}
