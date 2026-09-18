//! Mark toggling and the predicates it is built from.

use crate::attr::Attrs;
use crate::change::Change;
use crate::mark::Mark;
use crate::node::Node;
use crate::schema::{MarkTypeId, Schema};
use crate::selection::{Selection, SelectionRange};
use crate::state::TransactionSpec;

use super::{Command, changes_spec, command};

/// Whether a mark of type `ty` may be applied anywhere in `ranges`.
///
/// A mark is a property of inline content, and which marks are allowed is
/// declared by the node that *holds* that content, so this asks the parents.
///
/// Unlike ProseMirror's `markApplies`, an empty range is answered from the node
/// the position sits in rather than from a walk that visits nothing — which is
/// what makes toggling a mark at a cursor work at document position 0.
pub fn mark_applies(
    schema: &Schema,
    doc: &Node,
    ranges: &[SelectionRange],
    ty: MarkTypeId,
) -> bool {
    for range in ranges {
        if range.is_empty() {
            let Ok(resolved) = doc.resolve(range.from) else {
                continue;
            };
            let parent = schema.node_type(resolved.parent().type_id());
            if parent.has_inline_content() && parent.allows_mark_in_content(ty) {
                return true;
            }
            continue;
        }
        let mut can = false;
        doc.nodes_between(range.from, range.to, &mut |node, _, _, _| {
            if can {
                return false;
            }
            let node_ty = schema.node_type(node.type_id());
            can = node_ty.has_inline_content() && node_ty.allows_mark_in_content(ty);
            true
        });
        if can {
            return true;
        }
    }
    false
}

/// Whether any node in `from..to` carries a mark of type `ty`.
pub fn range_has_mark(doc: &Node, from: usize, to: usize, ty: MarkTypeId) -> bool {
    if to <= from {
        return false;
    }
    let mut found = false;
    doc.nodes_between(from, to, &mut |node, _, _, _| {
        if node.marks().contains_type(ty) {
            found = true;
        }
        !found
    });
    found
}

/// Add or remove a mark over the selection.
///
/// With a non-empty selection the mark is removed when any of the selected
/// content already carries it and added otherwise. With a cursor nothing is
/// changed in the document: the mark is toggled in the selection's *stored
/// marks*, which [`insert_text`](super::insert_text) then applies to whatever
/// is typed next.
pub fn toggle_mark(mark_type: MarkTypeId, attrs: Attrs) -> Command {
    command(move |state| {
        let doc = state.doc();
        let schema = state.schema();
        schema.try_mark_type(mark_type)?;
        let selection = state.selection();
        let ranges = selection.ranges(doc);
        if !mark_applies(schema, doc, &ranges, mark_type) {
            return None;
        }
        let mark = Mark::with_attrs(mark_type, schema.build_mark_attrs(mark_type, &attrs).ok()?);

        if selection.is_cursor() {
            let pos = selection.head(doc);
            let resolved = doc.resolve(pos).ok()?;
            let current = selection
                .stored_marks()
                .cloned()
                .unwrap_or_else(|| resolved.marks(schema));
            let present = current.contains_type(mark_type);
            let next = if present {
                current.remove_type(mark_type)
            } else {
                current.add(schema, mark)
            };
            return Some(
                TransactionSpec::new()
                    .selection(Selection::cursor_with_marks(pos, next))
                    .user_event(if present { "mark.remove" } else { "mark.add" }),
            );
        }

        let add = !ranges
            .iter()
            .any(|range| range_has_mark(doc, range.from, range.to, mark_type));
        let changes: Vec<Change> = ranges
            .iter()
            .filter(|range| !range.is_empty())
            .map(|range| {
                if add {
                    Change::add_mark(range.from, range.to, mark.clone())
                } else {
                    Change::remove_mark_type(range.from, range.to, mark_type)
                }
            })
            .collect();
        changes_spec(state, changes, if add { "mark.add" } else { "mark.remove" })
    })
}
