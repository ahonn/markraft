//! Finding a valid selection near a position.
//!
//! The scan is linear in the document's token count. That is deliberate: the
//! rules for "where may a cursor go" depend on the schema at every depth, and a
//! straightforward scan is far easier to keep correct than an index. Callers
//! that need this in a hot loop should cache the result.

use crate::node::Node;
use crate::schema::Schema;

use super::Selection;

/// Whether `pos` sits in a node that holds inline content.
pub(crate) fn in_inline_content(schema: &Schema, doc: &Node, pos: usize) -> bool {
    doc.resolve(pos).is_ok_and(|resolved| {
        schema
            .node_type(resolved.parent().type_id())
            .has_inline_content()
    })
}

impl Selection {
    /// The first position at or after (`dir > 0`) or at or before (`dir < 0`)
    /// `pos` that a selection may sit at.
    ///
    /// With `text_only`, only positions inside inline content qualify;
    /// otherwise a position directly before a selectable node qualifies too.
    /// Returns `None` when the document offers no such position in that
    /// direction.
    pub fn find_from(
        schema: &Schema,
        doc: &Node,
        pos: usize,
        dir: i32,
        text_only: bool,
    ) -> Option<Selection> {
        let size = doc.content_size();
        let pos = pos.min(size);
        let mut at = pos;
        loop {
            if in_inline_content(schema, doc, at) {
                return Some(Selection::cursor(at));
            }
            if !text_only && Selection::is_selectable(schema, doc, at) {
                return Some(Selection::Node { pos: at });
            }
            if dir < 0 {
                at = at.checked_sub(1)?;
            } else {
                at += 1;
                if at > size {
                    return None;
                }
            }
        }
    }

    /// A valid selection at or near `pos`.
    ///
    /// `bias` says which direction to search first; the other direction is
    /// tried when the first finds nothing. Falls back to [`Selection::All`] for
    /// a document that has no place to put a cursor at all.
    pub fn near(schema: &Schema, doc: &Node, pos: usize, bias: i32) -> Selection {
        let dir = if bias < 0 { -1 } else { 1 };
        Selection::find_from(schema, doc, pos, dir, false)
            .or_else(|| Selection::find_from(schema, doc, pos, -dir, false))
            .unwrap_or(Selection::All)
    }

    /// A selection at the start of the document.
    pub fn at_start(schema: &Schema, doc: &Node) -> Selection {
        Selection::near(schema, doc, 0, 1)
    }

    /// A selection at the end of the document.
    pub fn at_end(schema: &Schema, doc: &Node) -> Selection {
        Selection::near(schema, doc, doc.content_size(), -1)
    }
}
