//! Splitting, lifting, wrapping and re-typing blocks.

use crate::attr::Attrs;
use crate::change::Change;
use crate::fragment::Fragment;
use crate::mark::MarkSet;
use crate::node::{Markup, Node};
use crate::schema::{MarkTypeId, NodeTypeId, Schema};
use crate::selection::Selection;
use crate::slice::{Slice, Token};
use crate::state::{EditorState, TransactionSpec};

use super::general::default_type_after;
use super::structure::{
    can_replace_with, can_split, content_match_at, lift_changes, lift_target, markup_of,
    wrap_changes_with,
};
use super::{Command, changes_spec, command, cursor, resolve_rounds, selection_block_range};
use crate::protocol::event;

/// Split the block around the cursor in two.
///
/// At the end of a block the new block gets the schema's default type for that
/// position, so pressing Enter at the end of a heading starts a paragraph. In
/// the middle of a block both halves keep the original type, and at the very
/// start the *first*, now empty, half becomes the default type instead.
pub fn split_block() -> Command {
    command(|state| split_block_impl(state, false))
}

/// [`split_block`], carrying the marks at the cursor into the new block.
pub fn split_block_keep_marks() -> Command {
    command(|state| split_block_impl(state, true))
}

fn split_block_impl(state: &EditorState, keep_marks: bool) -> Option<TransactionSpec> {
    if !state.selection().is_empty(state.doc()) {
        // A cross-container replacement must first fit the deletion. Splitting
        // the resulting textblock then keeps both sides structurally balanced.
        let deletion = super::general::delete_selection()(state)?;
        let deleted = state.update([deletion]).ok()?;
        let from = state.selection().from(state.doc());
        let mapped = deleted
            .changes()
            .map_pos(from, -1, crate::change::TrackMode::Simple)?;
        let selection = Selection::find_from(state.schema(), deleted.new_doc(), mapped, 1, true)
            .or_else(|| {
                Selection::find_from(state.schema(), deleted.new_doc(), mapped, -1, true)
            })?;
        let deleted = state
            .update([TransactionSpec::new()
                .change_set(deleted.changes().clone())
                .selection(selection)])
            .ok()?;
        let split = split_block_impl(deleted.state(), keep_marks)?;
        let split = deleted.state().update([split]).ok()?;
        return Some(
            TransactionSpec::new()
                .change_set(deleted.changes().compose(split.changes()).ok()?)
                .selection(split.state().selection().clone())
                .user_event(event::SPLIT)
                .scroll_into_view(),
        );
    }
    let doc = state.doc();
    let schema = state.schema();
    let range = state.selection().replacement_range(doc);
    let (from, to) = (range.from, range.to);
    let resolved_from = doc.resolve(from).ok()?;
    let resolved_to = doc.resolve(to).ok()?;
    let depth = (1..=resolved_from.depth())
        .rev()
        .find(|&depth| resolved_from.node(depth).is_textblock(schema))?;
    let parent = resolved_from.node(depth).clone();
    if !schema.node_type(parent.type_id()).is_block() {
        return None;
    }
    let at_end = (depth..=resolved_to.depth())
        .all(|d| resolved_to.pos() + resolved_to.depth() - d == resolved_to.end(d));
    let at_start = (depth..=resolved_from.depth())
        .all(|d| resolved_from.pos() - (resolved_from.depth() - d) == resolved_from.start(d));
    let block_start = doc.resolve(resolved_from.start(depth)).ok()?;
    let default = default_type_after(schema, &block_start);

    let mut changes = Vec::new();
    let (close_markup, open_markup) = if at_end {
        let ty = default.unwrap_or(parent.type_id());
        let open = if ty == parent.type_id() {
            parent.markup().clone()
        } else {
            markup_of(schema, ty, &Attrs::empty())
        };
        (parent.markup().clone(), open)
    } else if at_start
        && let Some(ty) = default.filter(|ty| *ty != parent.type_id())
        && can_replace_with(
            schema,
            resolved_from.node(depth - 1),
            resolved_from.index(depth - 1),
            resolved_from.index(depth - 1) + 1,
            &[ty],
        )
    {
        // The half before the cursor is empty; give it the default type.
        let empty = markup_of(schema, ty, &Attrs::empty());
        let before = resolved_from.before(depth);
        changes.push(Change::replace(
            before,
            before + 1,
            Slice::from_tokens(&[Token::Open(empty.clone())]),
        ));
        (empty, parent.markup().clone())
    } else {
        (parent.markup().clone(), parent.markup().clone())
    };

    let split_depth = resolved_from.depth() - depth + 1;
    let mut types_after = vec![Some(open_markup.ty)];
    types_after
        .extend((depth + 1..=resolved_from.depth()).map(|d| Some(resolved_from.node(d).type_id())));
    if !can_split(schema, doc, from, split_depth, &types_after) {
        return None;
    }
    let mut tokens: Vec<_> = (depth + 1..=resolved_from.depth())
        .rev()
        .map(|d| Token::Close(resolved_from.node(d).markup().clone()))
        .collect();
    tokens.push(Token::Close(close_markup));
    tokens.push(Token::Open(open_markup.clone()));
    tokens.extend(
        (depth + 1..=resolved_from.depth())
            .map(|d| Token::Open(resolved_from.node(d).markup().clone())),
    );
    changes.push(Change::replace(from, to, Slice::from_tokens(&tokens)));

    let (set, new_doc) = super::resolve_changes(state, changes)?;
    let caret = from + 2 * split_depth;
    let selection = Selection::cursor(caret);
    selection.check(&new_doc, schema).ok()?;
    let mut spec = TransactionSpec::new()
        .change_set(set)
        .selection(selection)
        .user_event(event::SPLIT)
        .scroll_into_view();
    if keep_marks {
        let marks = marks_at(state, &resolved_from);
        let parent_ty = schema.node_type(open_markup.ty);
        spec = spec.stored_marks(Some(
            marks.filter(|mark| parent_ty.allows_mark_in_content(mark.ty)),
        ));
    }
    Some(spec)
}

fn marks_at(state: &EditorState, resolved: &crate::pos::ResolvedPos) -> MarkSet {
    state
        .stored_marks()
        .cloned()
        .unwrap_or_else(|| resolved.marks(state.schema()))
}

/// Lift an empty textblock out of its parent, or split the parent around it.
pub fn lift_empty_block() -> Command {
    command(|state| {
        let doc = state.doc();
        let schema = state.schema();
        let resolved = cursor(state)?;
        if resolved.parent().content_size() != 0 {
            return None;
        }
        let depth = resolved.depth();
        if depth > 1 && resolved.after(depth) != resolved.end(depth - 1) {
            let before = resolved.before(depth);
            if can_split(schema, doc, before, 1, &[]) {
                let markup = resolved.node(depth - 1).markup().clone();
                let tokens = [Token::Close(markup.clone()), Token::Open(markup)];
                if let Some(spec) = changes_spec(
                    state,
                    vec![Change::insert(before, Slice::from_tokens(&tokens))],
                    "split",
                ) {
                    return Some(spec);
                }
            }
        }
        let range = resolved.block_range(schema, &resolved, None)?;
        let target = lift_target(schema, &range)?;
        changes_spec(state, lift_changes(&range, target), "unwrap")
    })
}

/// Insert a newline inside a code block.
pub fn new_line_in_code() -> Command {
    command(|state| {
        let doc = state.doc();
        let schema = state.schema();
        let selection = state.selection();
        let from = doc.resolve(selection.from(doc)).ok()?;
        let to = doc.resolve(selection.to(doc)).ok()?;
        if !schema.node_type(from.parent().type_id()).is_code() || !from.same_parent(&to) {
            return None;
        }
        super::text::insert_text_spec(state, "\n")
    })
}

/// Leave a code block by creating a default block after it.
pub fn exit_code() -> Command {
    command(|state| {
        let doc = state.doc();
        let schema = state.schema();
        let selection = state.selection();
        let from = doc.resolve(selection.from(doc)).ok()?;
        let to = doc.resolve(selection.to(doc)).ok()?;
        if !schema.node_type(from.parent().type_id()).is_code()
            || !from.same_parent(&to)
            || from.depth() == 0
        {
            return None;
        }
        let depth = from.depth();
        let above = from.node(depth - 1);
        let after_index = from.index_after(depth - 1);
        let ty =
            content_match_at(schema, above, after_index).and_then(|m| schema.default_type(m))?;
        if !can_replace_with(schema, above, after_index, after_index, &[ty]) {
            return None;
        }
        let node = schema.create_and_fill(
            ty,
            schema.node_type(ty).default_attrs().clone(),
            MarkSet::empty(),
            Fragment::empty(),
        )?;
        let pos = from.after(depth);
        let (set, new_doc) = super::resolve_changes(
            state,
            vec![Change::insert(
                pos,
                Slice::from_fragment(Fragment::from_node(node)),
            )],
        )?;
        Some(
            TransactionSpec::new()
                .change_set(set)
                .selection(Selection::near(schema, &new_doc, pos, 1))
                .user_event(event::INSERT)
                .scroll_into_view(),
        )
    })
}

/// Create a paragraph next to a selected block that cannot hold inline content.
pub fn create_paragraph_near() -> Command {
    command(|state| {
        let doc = state.doc();
        let schema = state.schema();
        let selection = state.selection();
        if matches!(selection, Selection::All) {
            return None;
        }
        let from = doc.resolve(selection.from(doc)).ok()?;
        let to = doc.resolve(selection.to(doc)).ok()?;
        if schema
            .node_type(from.parent().type_id())
            .has_inline_content()
            || schema.node_type(to.parent().type_id()).has_inline_content()
        {
            return None;
        }
        let depth = to.depth();
        let index = to.index_after(depth);
        let ty =
            content_match_at(schema, to.node(depth), index).and_then(|m| schema.default_type(m))?;
        if !schema.node_type(ty).is_textblock() {
            return None;
        }
        let node = schema.create_and_fill(
            ty,
            schema.node_type(ty).default_attrs().clone(),
            MarkSet::empty(),
            Fragment::empty(),
        )?;
        let side = if from.parent_offset() == 0 && to.index(to.depth()) < to.parent().child_count()
        {
            from.pos()
        } else {
            to.pos()
        };
        let (set, new_doc) = super::resolve_changes(
            state,
            vec![Change::insert(
                side,
                Slice::from_fragment(Fragment::from_node(node)),
            )],
        )?;
        let selection = Selection::cursor(side + 1);
        selection.check(&new_doc, schema).ok()?;
        Some(
            TransactionSpec::new()
                .change_set(set)
                .selection(selection)
                .user_event(event::INSERT)
                .scroll_into_view(),
        )
    })
}

/// Lift the blocks the selection covers out of their parent.
pub fn lift() -> Command {
    command(|state| {
        let range = selection_block_range(state, None)?;
        let target = lift_target(state.schema(), &range)?;
        changes_spec(state, lift_changes(&range, target), "unwrap")
    })
}

/// Wrap the blocks the selection covers in a node of the given type.
pub fn wrap_in(node_type: NodeTypeId, attrs: Attrs) -> Command {
    command(move |state| {
        let schema = state.schema();
        let range = selection_block_range(state, None)?;
        let wrapping = super::structure::find_wrapping(schema, &range, node_type)?;
        let markups: Vec<Markup> = wrapping
            .iter()
            .map(|ty| {
                if *ty == node_type {
                    markup_of(schema, *ty, &attrs)
                } else {
                    markup_of(schema, *ty, &Attrs::empty())
                }
            })
            .collect();
        changes_spec(state, wrap_changes_with(&range, &markups), "wrap")
    })
}

/// Set the type and attributes of every textblock the selection touches.
///
/// Marks the new type does not allow on its content are removed, which is what
/// turning a paragraph with emphasis into a code block has to do.
pub fn set_block_type(node_type: NodeTypeId, attrs: Attrs) -> Command {
    command(move |state| {
        let doc = state.doc();
        let schema = state.schema();
        if !schema.node_type(node_type).is_textblock() {
            return None;
        }
        let markup = markup_of(schema, node_type, &attrs);
        let mut structural = Vec::new();
        let mut mark_rounds: Vec<Vec<Change>> = Vec::new();
        for range in state.selection().ranges(doc) {
            doc.nodes_between(range.from, range.to, &mut |node, pos, parent, index| {
                if !node.is_textblock(schema) {
                    return true;
                }
                let Some(parent) = parent else {
                    return false;
                };
                if node.has_markup(node_type, &markup.attrs, node.marks()) {
                    return false;
                }
                if !can_replace_with(schema, parent, index, index + 1, &[node_type]) {
                    return false;
                }
                let size = node.node_size();
                let new_markup = Markup {
                    ty: node_type,
                    attrs: markup.attrs.clone(),
                    marks: node.marks().clone(),
                };
                if schema.node_type(node_type).is_code()
                    && node.children().any(|child| !child.is_text())
                {
                    // Code has plain text content. Inline scopes and atoms must
                    // become their visible text rather than invalid children.
                    let text = crate::projection::slice_to_plain_text(
                        schema,
                        &Slice::from_fragment(node.content().clone()),
                    );
                    let content = if text.is_empty() {
                        Fragment::empty()
                    } else {
                        Fragment::from_node(schema.text(&text))
                    };
                    structural.push(Change::replace(
                        pos,
                        pos + size,
                        Slice::from_fragment(Fragment::from_node(Node::container(
                            new_markup, content,
                        ))),
                    ));
                    return false;
                }
                structural.push(Change::replace(
                    pos,
                    pos + 1,
                    Slice::from_tokens(&[Token::Open(new_markup.clone())]),
                ));
                structural.push(Change::replace(
                    pos + size - 1,
                    pos + size,
                    Slice::from_tokens(&[Token::Close(new_markup)]),
                ));
                for (round, ty) in forbidden_marks(schema, node, node_type)
                    .into_iter()
                    .enumerate()
                {
                    if mark_rounds.len() <= round {
                        mark_rounds.push(Vec::new());
                    }
                    mark_rounds[round].push(Change::remove_mark_type(pos + 1, pos + size - 1, ty));
                }
                false
            });
        }
        if structural.is_empty() {
            return None;
        }
        // Marks are stripped first, while the block still allows them: a
        // removal recorded against a type that forbids the mark is dropped.
        let mut rounds = mark_rounds;
        rounds.push(structural);
        let (set, next_doc) = resolve_rounds(state, rounds)?;
        let mut spec = TransactionSpec::new()
            .change_set(set)
            .user_event(event::SETTYPE)
            .scroll_into_view();
        if let Selection::Text { anchor, head } = state.selection() {
            let before = crate::projection::projection_of(state);
            let after = before.update(schema, doc, &next_doc);
            if let (Some((a_line, a_offset)), Some((h_line, h_offset))) = (
                before.pos_to_line_offset(*anchor),
                before.pos_to_line_offset(*head),
            ) && let (Some(anchor), Some(head)) = (
                after.line_offset_to_pos(a_line, a_offset),
                after.line_offset_to_pos(h_line, h_offset),
            ) {
                spec = spec
                    .selection(Selection::text(anchor, head))
                    .stored_marks(state.stored_marks().cloned());
            }
        }
        Some(spec)
    })
}

/// The mark types on `node`'s content that `ty` would not allow.
fn forbidden_marks(schema: &Schema, node: &Node, ty: NodeTypeId) -> Vec<MarkTypeId> {
    let new_ty = schema.node_type(ty);
    let mut out = Vec::new();
    for child in node.children() {
        for mark in child.marks().iter() {
            if !new_ty.allows_mark_in_content(mark.ty) && !out.contains(&mark.ty) {
                out.push(mark.ty);
            }
        }
    }
    out
}
