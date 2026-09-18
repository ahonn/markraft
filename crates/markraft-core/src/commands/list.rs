//! List editing, matching `prosemirror-schema-list`.
//!
//! Nothing here knows what a list *is* beyond what the schema says: a list type
//! is any container whose content rule holds items, and an item type is any
//! container that may hold the blocks being wrapped. The commands are given
//! both by the caller.

use crate::attr::Attrs;
use crate::change::Change;
use crate::node::{Markup, Node};
use crate::pos::NodeRange;
use crate::schema::{NodeTypeId, Schema};
use crate::selection::Selection;
use crate::slice::{Slice, Token};
use crate::state::{EditorState, TransactionSpec};

use super::structure::{can_split, content_match_at, lift_target_within, markup_of};
use super::{Command, changes_spec, command, resolve_changes};

/// Wrap the blocks the selection covers in a list.
///
/// When the selection is already at the top of a list item, the blocks become a
/// *nested* list inside the item before it, which is what pressing the list
/// button inside a list does. The first item of a list has nothing to nest
/// into, so the command does not apply there.
pub fn wrap_in_list(list_type: NodeTypeId, attrs: Attrs) -> Command {
    command(move |state| {
        let doc = state.doc();
        let schema = state.schema();
        let selection = state.selection();
        let from = doc.resolve(selection.from(doc)).ok()?;
        let to = doc.resolve(selection.to(doc)).ok()?;
        let range = from.block_range(schema, &to, None)?;
        let depth = range.depth();
        let parent = range.parent();

        let nest = depth >= 2
            && range.start_index() == 0
            && compatible_content(schema, from.node(depth - 1).type_id(), list_type);
        if nest {
            if from.index(depth - 1) == 0 {
                return None;
            }
            let first = parent.maybe_child(range.start_index())?;
            let inside = schema.find_wrapping(schema.content_match(list_type), first.type_id())?;
            let mut markups = vec![markup_of(schema, list_type, &attrs)];
            markups.extend(
                inside
                    .iter()
                    .map(|ty| markup_of(schema, *ty, &Attrs::empty())),
            );
            let extended = range.end_index() < parent.child_count();
            let end_index = if extended {
                parent.child_count()
            } else {
                range.end_index()
            };
            let end = if extended {
                from.end(depth)
            } else {
                range.end()
            };
            let mut changes = vec![
                Change::replace(
                    range.start().checked_sub(2)?,
                    range.start(),
                    Slice::from_tokens(&opens(&markups)),
                ),
                Change::insert(end, Slice::from_tokens(&closes(&markups))),
            ];
            changes.extend(item_splits(&range, end_index, &markups[1..]));
            return changes_spec(state, changes, "wrap");
        }

        let wrapping = super::structure::find_wrapping(schema, &range, list_type)?;
        let markups: Vec<Markup> = wrapping
            .iter()
            .map(|ty| {
                if *ty == list_type {
                    markup_of(schema, *ty, &attrs)
                } else {
                    markup_of(schema, *ty, &Attrs::empty())
                }
            })
            .collect();
        let found = wrapping.iter().position(|ty| *ty == list_type)? + 1;
        let mut changes = vec![
            Change::insert(range.start(), Slice::from_tokens(&opens(&markups))),
            Change::insert(range.end(), Slice::from_tokens(&closes(&markups))),
        ];
        changes.extend(item_splits(&range, range.end_index(), &markups[found..]));
        changes_spec(state, changes, "wrap")
    })
}

/// A close/open pair for every wrapper below the list, at each boundary
/// between two blocks in the range, so each block gets its own item.
fn item_splits(range: &NodeRange, end_index: usize, inner: &[Markup]) -> Vec<Change> {
    if inner.is_empty() {
        return Vec::new();
    }
    let mut tokens = closes(inner);
    tokens.extend(opens(inner));
    let slice = Slice::from_tokens(&tokens);
    let parent = range.parent();
    let mut out = Vec::new();
    let mut pos = range.start();
    for index in range.start_index()..end_index.min(parent.child_count()) {
        if index > range.start_index() {
            out.push(Change::insert(pos, slice.clone()));
        }
        pos += parent.child(index).node_size();
    }
    out
}

fn opens(markups: &[Markup]) -> Vec<Token> {
    markups.iter().cloned().map(Token::Open).collect()
}

fn closes(markups: &[Markup]) -> Vec<Token> {
    markups.iter().rev().cloned().map(Token::Close).collect()
}

/// Whether two container types can hold any of the same children — an
/// approximation of ProseMirror's `compatibleContent`.
fn compatible_content(schema: &Schema, a: NodeTypeId, b: NodeTypeId) -> bool {
    if a == b {
        return true;
    }
    let left = schema.content_expr(a).mentioned_types();
    let right = schema.content_expr(b).mentioned_types();
    left.iter().any(|ty| right.contains(ty))
}

/// Split the list item around the cursor in two.
///
/// Does not apply in an empty item, so a chain can fall through to
/// [`lift_list_item`] or [`lift_empty_block`](super::lift_empty_block) and turn
/// Enter in an empty item into "leave the list". The new item keeps the
/// original item's attributes, as ProseMirror's `splitListItem` does.
pub fn split_list_item(item_type: NodeTypeId) -> Command {
    command(move |state| {
        let doc = state.doc();
        let schema = state.schema();
        let selection = state.selection();
        if matches!(selection, Selection::Node { .. }) {
            return None;
        }
        let range = selection.replacement_range(doc);
        let from = doc.resolve(range.from).ok()?;
        let to = doc.resolve(range.to).ok()?;
        let depth = (1..=from.depth())
            .rev()
            .find(|&d| from.node(d).is_textblock(schema))?;
        if depth < 2 || !from.same_parent(&to) {
            return None;
        }
        let item = from.node(depth - 1);
        if item.type_id() != item_type {
            return None;
        }
        if from.node(depth).content_size() == 0 && item.child_count() == from.index_after(depth - 1)
        {
            return None;
        }
        let at_end = to.pos() + to.depth() - depth == from.end(depth);
        let next_ty = if at_end {
            content_match_at(schema, item, 0).and_then(|m| schema.default_type(m))
        } else {
            None
        };
        let block_markup = match next_ty {
            Some(ty) if ty != from.node(depth).type_id() => markup_of(schema, ty, &Attrs::empty()),
            _ => from.node(depth).markup().clone(),
        };
        let item_markup = item.markup().clone();
        let split_depth = from.depth() - depth + 2;
        let mut types_after = vec![Some(item_markup.ty), Some(block_markup.ty)];
        types_after.extend((depth + 1..=from.depth()).map(|d| Some(from.node(d).type_id())));
        if !can_split(schema, doc, range.from, split_depth, &types_after) {
            return None;
        }
        let mut tokens: Vec<_> = (depth..=from.depth())
            .rev()
            .map(|d| Token::Close(from.node(d).markup().clone()))
            .collect();
        tokens.push(Token::Close(item_markup.clone()));
        tokens.push(Token::Open(item_markup));
        tokens.push(Token::Open(block_markup));
        tokens
            .extend((depth + 1..=from.depth()).map(|d| Token::Open(from.node(d).markup().clone())));
        let (set, new_doc) = resolve_changes(
            state,
            vec![Change::replace(
                range.from,
                range.to,
                Slice::from_tokens(&tokens),
            )],
        )?;
        let caret = Selection::cursor(range.from + 2 * split_depth);
        caret.check(&new_doc, schema).ok()?;
        Some(
            TransactionSpec::new()
                .change_set(set)
                .selection(caret)
                .user_event("split")
                .scroll_into_view(),
        )
    })
}

/// Lift the list items the selection covers one level out.
///
/// Inside a nested list the items become items of the list above; in a
/// top-level list they leave the list altogether.
pub fn lift_list_item(item_type: NodeTypeId) -> Command {
    command(move |state| {
        let doc = state.doc();
        let schema = state.schema();
        let selection = state.selection();
        let from = doc.resolve(selection.from(doc)).ok()?;
        let to = doc.resolve(selection.to(doc)).ok()?;
        let holds_items =
            |node: &Node| node.first_child().is_some_and(|c| c.type_id() == item_type);
        let range = from.block_range(schema, &to, Some(&holds_items))?;
        let depth = range.depth();
        if depth > 0 && from.node(depth - 1).type_id() == item_type {
            lift_to_outer_list(state, &range)
        } else {
            lift_out_of_list(state, &range)
        }
    })
}

/// Lift items of a nested list into the list that holds their parent item.
fn lift_to_outer_list(state: &EditorState, range: &NodeRange) -> Option<TransactionSpec> {
    let schema = state.schema();
    let depth = range.depth();
    let from = range.resolved_from();
    let to = range.resolved_to();
    let end = range.end();
    let end_of_list = to.end(depth);
    let parent = range.parent();

    let target = lift_target_within(schema, range, parent.child_count())?;
    let mut changes: Vec<Change> = Vec::new();

    // Items after the lifted ones become a nested list inside the last one.
    let mut tail: Vec<Token> = Vec::new();
    if end < end_of_list {
        let last = parent.maybe_child(range.end_index().checked_sub(1)?)?;
        changes.push(Change::replace(
            end - 1,
            end,
            Slice::from_tokens(&[Token::Open(parent.markup().clone())]),
        ));
        tail.push(Token::Close(parent.markup().clone()));
        tail.push(Token::Close(last.markup().clone()));
    }

    let mut start = range.start();
    let mut head: Vec<Token> = Vec::new();
    let mut splitting = false;
    for d in (target + 1..=depth).rev() {
        if splitting || from.index(d) > 0 {
            splitting = true;
            head.push(Token::Close(from.node(d).markup().clone()));
        } else {
            start -= 1;
        }
    }

    let mut right_end = end_of_list;
    let mut opens: Vec<Token> = Vec::new();
    let mut splitting = false;
    for d in (target + 1..=depth).rev() {
        let at_end = d == depth || to.after(d + 1) == to.end(d);
        if splitting || !at_end {
            splitting = true;
            opens.push(Token::Open(to.node(d).markup().clone()));
        } else {
            right_end += 1;
        }
    }
    opens.reverse();
    tail.extend(opens);

    changes.push(Change::replace(
        start,
        range.start(),
        Slice::from_tokens(&head),
    ));
    changes.push(Change::replace(
        end_of_list,
        right_end,
        Slice::from_tokens(&tail),
    ));
    changes_spec(state, changes, "unwrap")
}

/// Lift items out of a top-level list.
fn lift_out_of_list(state: &EditorState, range: &NodeRange) -> Option<TransactionSpec> {
    let list = range.parent();
    let start = range.start();
    let end = range.end();
    let at_start = range.start_index() == 0;
    let at_end = range.end_index() == list.child_count();
    let list_markup = list.markup().clone();

    let mut changes: Vec<Change> = Vec::new();
    // Merge the selected items into one, so a single node is lifted.
    let mut pos = start;
    for index in range.start_index()..range.end_index() {
        if index > range.start_index() {
            changes.push(Change::delete(pos - 1, pos + 1));
        }
        pos += list.child(index).node_size();
    }

    // Where the range reaches the list's edge the list's own token is consumed;
    // where it does not, the list is closed before and reopened after.
    changes.push(Change::replace(
        start - usize::from(at_start),
        start + 1,
        if at_start {
            Slice::empty()
        } else {
            Slice::from_tokens(&[Token::Close(list_markup.clone())])
        },
    ));
    changes.push(Change::replace(
        end - 1,
        end + usize::from(at_end),
        if at_end {
            Slice::empty()
        } else {
            Slice::from_tokens(&[Token::Open(list_markup)])
        },
    ));
    changes_spec(state, changes, "unwrap")
}

/// Sink the list items the selection covers into a nested list inside the item
/// before them.
pub fn sink_list_item(item_type: NodeTypeId) -> Command {
    command(move |state| {
        let doc = state.doc();
        let schema = state.schema();
        let selection = state.selection();
        let from = doc.resolve(selection.from(doc)).ok()?;
        let to = doc.resolve(selection.to(doc)).ok()?;
        let holds_items =
            |node: &Node| node.first_child().is_some_and(|c| c.type_id() == item_type);
        let range = from.block_range(schema, &to, Some(&holds_items))?;
        let start_index = range.start_index();
        if start_index == 0 {
            return None;
        }
        let parent = range.parent();
        let before = parent.child(start_index - 1);
        if before.type_id() != item_type {
            return None;
        }
        let nested = before
            .last_child()
            .filter(|last| last.type_id() == parent.type_id());
        let start = range.start();
        let end = range.end();
        let changes = match nested {
            Some(nested_list) => {
                let last = nested_list.last_child()?;
                vec![
                    Change::replace(
                        start.checked_sub(3)?,
                        start,
                        Slice::from_tokens(&[Token::Close(last.markup().clone())]),
                    ),
                    Change::insert(
                        end,
                        Slice::from_tokens(&[
                            Token::Close(nested_list.markup().clone()),
                            Token::Close(before.markup().clone()),
                        ]),
                    ),
                ]
            }
            None => vec![
                Change::replace(
                    start.checked_sub(1)?,
                    start,
                    Slice::from_tokens(&[Token::Open(parent.markup().clone())]),
                ),
                Change::insert(
                    end,
                    Slice::from_tokens(&[
                        Token::Close(parent.markup().clone()),
                        Token::Close(before.markup().clone()),
                    ]),
                ),
            ],
        };
        changes_spec(state, changes, "wrap")
    })
}
