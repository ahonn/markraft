//! Mark toggling and the predicates it is built from.

use crate::attr::Attrs;
use crate::change::Change;
use crate::change::{ChangeSet, TrackMode};
use crate::mark::Mark;
use crate::node::Node;
use crate::schema::{MarkTypeId, Schema};
use crate::selection::{Selection, SelectionRange};
use crate::slice::{Slice, Token};
use crate::state::{EditorState, TransactionSpec};

use super::{Command, command};

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
        change_mark_spec(state, mark_type, add.then_some(mark))
    })
}

/// Set a mark over the selection, replacing any existing mark of that type.
/// Inline container scopes are split at selection boundaries so an inherited
/// mark can be changed without touching the content outside the selection.
pub fn set_mark(mark_type: MarkTypeId, attrs: Attrs) -> Command {
    command(move |state| {
        let mark = Mark::with_attrs(
            mark_type,
            state.schema().build_mark_attrs(mark_type, &attrs).ok()?,
        );
        change_mark_spec(state, mark_type, Some(mark))
    })
}

/// Remove a mark, including marks inherited from inline containers.
pub fn remove_mark(mark_type: MarkTypeId) -> Command {
    command(move |state| change_mark_spec(state, mark_type, None))
}

fn change_mark_spec(
    state: &EditorState,
    ty: MarkTypeId,
    mark: Option<Mark>,
) -> Option<TransactionSpec> {
    let schema = state.schema();
    let doc = state.doc();
    if state.selection().is_cursor() {
        let resolved = doc.resolve(state.selection().head(doc)).ok()?;
        let marks = state
            .selection()
            .stored_marks()
            .cloned()
            .unwrap_or_else(|| resolved.marks(schema));
        let marks = match mark {
            Some(mark) => marks.add(schema, mark),
            None => marks.remove_type(ty),
        };
        return Some(
            TransactionSpec::new()
                .selection(Selection::cursor_with_marks(resolved.pos(), marks))
                .user_event("mark"),
        );
    }
    let from = state.selection().from(doc);
    let to = state.selection().to(doc);
    if from == to {
        return None;
    }
    let mut boundaries = [from, to];
    let mut changes = Vec::new();
    let mut counts = [0, 0];
    for (index, boundary) in boundaries.iter_mut().enumerate() {
        let resolved = doc.resolve(*boundary).ok()?;
        // At a scope's edge, include its boundary instead of manufacturing an
        // empty sibling (which would itself be meaningful for an empty link).
        for d in (1..=resolved.depth()).rev() {
            if !schema.node_type(resolved.node(d).type_id()).is_inline() {
                break;
            }
            if index == 0 && *boundary == resolved.start(d) {
                *boundary = resolved.before(d);
            } else if index == 1 && *boundary == resolved.end(d) {
                *boundary = resolved.after(d);
            } else {
                break;
            }
        }
        let pos = *boundary;
        let resolved = doc.resolve(pos).ok()?;
        let scopes: Vec<_> = (1..=resolved.depth())
            .filter(|&d| schema.node_type(resolved.node(d).type_id()).is_inline())
            .map(|d| resolved.node(d).markup().clone())
            .collect();
        counts[index] = scopes.len();
        if scopes.is_empty() {
            continue;
        }
        let tokens: Vec<_> = scopes
            .iter()
            .rev()
            .cloned()
            .map(Token::Close)
            .chain(scopes.iter().cloned().map(Token::Open))
            .collect();
        changes.push(Change::insert(pos, Slice::from_tokens(&tokens)));
    }
    let split = ChangeSet::create(schema, doc, changes).ok()?;
    let separated = split.apply(doc).ok()?;
    let start = split.map_pos(boundaries[0], -1, TrackMode::Simple)? + counts[0];
    let end = split.map_pos(boundaries[1], -1, TrackMode::Simple)? + counts[1];
    let resolved = separated.resolve(start).ok()?;
    let mut parents: Vec<_> = (0..=resolved.depth())
        .map(|d| resolved.node(d).type_id())
        .collect();
    let tokens = separated
        .slice(start, end)
        .ok()?
        .tokens()
        .into_iter()
        .map(|token| match token {
            Token::Node(node) => Token::Node(rewrite_mark(
                schema,
                &node,
                *parents.last().expect("a document frame"),
                ty,
                mark.as_ref(),
            )),
            Token::Open(ref markup) => {
                parents.push(markup.ty);
                token
            }
            Token::Close(_) => {
                if parents.len() > 1 {
                    parents.pop();
                }
                token
            }
        })
        .collect::<Vec<_>>();
    let replacement = ChangeSet::create(
        schema,
        &separated,
        [Change::replace(start, end, Slice::from_tokens(&tokens))],
    )
    .ok()?;
    let changes = split.compose(&replacement).ok()?;
    let next = changes.apply(doc).ok()?;
    next.check(schema).ok()?;
    let selection = if state.selection().anchor(doc) <= state.selection().head(doc) {
        Selection::text(start, end)
    } else {
        Selection::text(end, start)
    };
    Some(
        TransactionSpec::new()
            .change_set(changes)
            .selection(selection)
            .user_event(if mark.is_some() {
                "mark.add"
            } else {
                "mark.remove"
            }),
    )
}

fn rewrite_mark(
    schema: &Schema,
    node: &Node,
    parent: crate::NodeTypeId,
    ty: MarkTypeId,
    mark: Option<&Mark>,
) -> Node {
    let inline = schema.node_type(node.type_id()).is_inline();
    if inline
        && node.is_container()
        && node.marks().contains_type(ty)
        && let Some(mark) = mark
    {
        // Updating the scope itself keeps one link/format wrapper around its
        // nested content instead of distributing it over unrelated leaves.
        return node.mark(node.marks().add(schema, mark.clone()));
    }
    let mut next = if inline {
        node.mark(node.marks().remove_type(ty))
    } else {
        node.clone()
    };
    if node.is_container() {
        next = next.copy(
            node.children()
                .map(|child| rewrite_mark(schema, child, node.type_id(), ty, mark))
                .collect(),
        );
    } else if inline
        && schema.node_type(parent).allows_mark_in_content(ty)
        && let Some(mark) = mark
    {
        next = next.mark(next.marks().add(schema, mark.clone()));
    }
    next
}
