//! Structural predicates and token-level edits shared by the commands.
//!
//! Everything here is a pure function of a document and a schema. The
//! predicates mirror ProseMirror's `canReplace`/`canSplit`/`liftTarget`
//! family, and the edit builders return [`Change`]s in the coordinates of the
//! document they were computed from — a set of them can therefore be handed to
//! [`ChangeSet::create`](crate::ChangeSet::create) unchanged.
//!
//! # Structural edits are token edits
//!
//! * splitting inserts a close and an open token,
//! * joining deletes the close and open token between two nodes,
//! * wrapping inserts open tokens before a range and close tokens after it,
//! * lifting deletes the wrapper's tokens on the sides where the range reaches
//!   the wrapper's edge, and replaces them with a close/open pair where it does
//!   not — which is what splits the wrapper around the lifted content.

use crate::attr::Attrs;
use crate::change::Change;
use crate::node::{Markup, Node};
use crate::pos::NodeRange;
use crate::schema::{ContentMatch, NodeTypeId, Schema};
use crate::slice::{Slice, Token};

/// Markup for a node type, completing `attrs` with the type's defaults.
///
/// Falls back to the declared defaults when `attrs` is not valid for the type,
/// so callers that build a wrapper from a name never have to handle an error
/// they cannot act on.
pub fn markup_of(schema: &Schema, ty: NodeTypeId, attrs: &Attrs) -> Markup {
    let attrs = schema
        .build_node_attrs(ty, attrs)
        .unwrap_or_else(|_| schema.node_type(ty).default_attrs().clone());
    Markup::with_attrs(ty, attrs)
}

/// The content automaton state of `node` after its first `index` children.
///
/// Returns `None` when the existing children already break the content rule.
pub fn content_match_at<'a>(
    schema: &'a Schema,
    node: &Node,
    index: usize,
) -> Option<ContentMatch<'a>> {
    let mut m = schema.content_match(node.type_id());
    for child in &node.content().as_slice()[..index.min(node.child_count())] {
        m = m.match_type(child.type_id())?;
    }
    Some(m)
}

/// Whether `nodes` are valid content for a node of type `ty`.
pub fn content_valid(schema: &Schema, ty: NodeTypeId, nodes: &[Node]) -> bool {
    let mut m = schema.content_match(ty);
    for node in nodes {
        match m.match_type(node.type_id()) {
            Some(next) => m = next,
            None => return false,
        }
    }
    m.valid_end()
}

/// Whether `parent` still satisfies its content rule when children
/// `from..to` are replaced by nodes of the given types.
pub fn can_replace_with(
    schema: &Schema,
    parent: &Node,
    from: usize,
    to: usize,
    types: &[NodeTypeId],
) -> bool {
    let Some(mut m) = content_match_at(schema, parent, from) else {
        return false;
    };
    for ty in types {
        match m.match_type(*ty) {
            Some(next) => m = next,
            None => return false,
        }
    }
    for child in &parent.content().as_slice()[to.min(parent.child_count())..] {
        match m.match_type(child.type_id()) {
            Some(next) => m = next,
            None => return false,
        }
    }
    m.valid_end()
}

/// Whether children `from..to` of `parent` may be removed.
pub fn can_replace(schema: &Schema, parent: &Node, from: usize, to: usize) -> bool {
    can_replace_with(schema, parent, from, to, &[])
}

/// Whether `after`'s children may be appended to `before`'s.
///
/// This is the test a join has to pass: joining moves `after`'s content into
/// `before`, so `before`'s content rule has to accept the result.
pub fn can_append(schema: &Schema, before: &Node, after: &Node) -> bool {
    if before.is_leaf() {
        return false;
    }
    let Some(mut m) = content_match_at(schema, before, before.child_count()) else {
        return false;
    };
    for child in after.children() {
        match m.match_type(child.type_id()) {
            Some(next) => m = next,
            None => return false,
        }
    }
    m.valid_end()
}

/// Whether the range `from..to` of `node`'s children can be cut out of it
/// without leaving either side invalid.
pub fn can_cut(schema: &Schema, node: &Node, from: usize, to: usize) -> bool {
    (from == 0 || can_replace(schema, node, from, node.child_count()))
        && (to == node.child_count() || can_replace(schema, node, 0, to))
}

/// The type a new sibling at `index` of `parent` would get, when the schema
/// offers one that can be created without attributes.
pub fn default_block_type(schema: &Schema, parent: &Node, index: usize) -> Option<NodeTypeId> {
    let m = content_match_at(schema, parent, index)?;
    schema.default_type(m)
}

/// Whether the node at `pos` — and, with `depth > 1`, its ancestors — can be
/// split there.
///
/// `types_after` optionally overrides the type the node on each level *after*
/// the split gets, innermost last, mirroring ProseMirror's `canSplit`.
pub fn can_split(
    schema: &Schema,
    doc: &Node,
    pos: usize,
    depth: usize,
    types_after: &[Option<NodeTypeId>],
) -> bool {
    let Ok(resolved) = doc.resolve(pos) else {
        return false;
    };
    let Some(base) = resolved.depth().checked_sub(depth) else {
        return false;
    };
    let parent = resolved.parent();
    let parent_ty = schema.node_type(parent.type_id());
    if parent_ty.is_isolating() {
        return false;
    }
    let index = resolved.index(resolved.depth());
    // The half before the split has to remain valid...
    if !can_replace(schema, parent, index, parent.child_count()) {
        return false;
    }
    // ...and so does the half after it, under whatever type it ends up with.
    let inner_ty = types_after.last().copied().flatten();
    let rest: Vec<Node> = split_rest(parent, &resolved);
    if !content_valid(schema, inner_ty.unwrap_or(parent.type_id()), &rest) {
        return false;
    }

    let mut d = resolved.depth().wrapping_sub(1);
    let mut i = depth as isize - 2;
    while d > base && d < resolved.depth() {
        let node = resolved.node(d);
        if schema.node_type(node.type_id()).is_isolating() {
            return false;
        }
        let index = resolved.index(d);
        let mut rest: Vec<Node> = node.content().as_slice()[index..].to_vec();
        if let Some(Some(child_ty)) = usize::try_from(i + 1).ok().and_then(|k| types_after.get(k))
            && let Some(first) = rest.first().cloned()
            && let Ok(replacement) = schema.create(
                *child_ty,
                schema.node_type(*child_ty).default_attrs().clone(),
                first.marks().clone(),
                first.content().clone(),
            )
        {
            rest[0] = replacement;
        }
        let after_ty = usize::try_from(i)
            .ok()
            .and_then(|k| types_after.get(k).copied().flatten())
            .unwrap_or(node.type_id());
        if !can_replace(schema, node, index + 1, node.child_count())
            || !content_valid(schema, after_ty, &rest)
        {
            return false;
        }
        d = d.wrapping_sub(1);
        i -= 1;
    }

    let base_node = resolved.node(base);
    let index = resolved.index_after(base);
    let base_ty = types_after
        .first()
        .copied()
        .flatten()
        .unwrap_or_else(|| resolved.node(base + 1).type_id());
    can_replace_with(schema, base_node, index, index, &[base_ty])
}

/// The children of `parent` that end up after a split at `resolved`.
fn split_rest(parent: &Node, resolved: &crate::pos::ResolvedPos) -> Vec<Node> {
    let index = resolved.index(resolved.depth());
    let offset = resolved.text_offset();
    let mut out: Vec<Node> = Vec::new();
    if offset > 0 {
        let child = parent.child(index);
        out.push(child.cut_text(offset, child.text_len()));
        out.extend(parent.content().as_slice()[index + 1..].iter().cloned());
    } else {
        out.extend(parent.content().as_slice()[index..].iter().cloned());
    }
    out
}

/// The outermost depth `range` can be lifted to, or `None` when it cannot be
/// lifted at all.
pub fn lift_target(schema: &Schema, range: &NodeRange) -> Option<usize> {
    lift_target_within(schema, range, range.end_index())
}

/// [`lift_target`], with the index the range ends at inside its own parent
/// overridden.
///
/// The list commands reshape a list before lifting out of it, which changes
/// where the range ends without changing anything further out.
pub fn lift_target_within(
    schema: &Schema,
    range: &NodeRange,
    own_end_index: usize,
) -> Option<usize> {
    let content = range.content();
    let types: Vec<NodeTypeId> = content.iter().map(Node::type_id).collect();
    let from = range.resolved_from();
    let to = range.resolved_to();
    let mut depth = range.depth();
    loop {
        let node = from.node(depth);
        let index = from.index(depth);
        let end_index = if depth == range.depth() {
            own_end_index
        } else {
            to.index_after(depth)
        };
        if depth < range.depth() && can_replace_with(schema, node, index, end_index, &types) {
            return Some(depth);
        }
        if depth == 0
            || schema.node_type(node.type_id()).is_isolating()
            || !can_cut(schema, node, index, end_index)
        {
            return None;
        }
        depth -= 1;
    }
}

/// The changes that lift `range` out to `target`.
///
/// On a side where the range reaches the wrapper's edge the wrapper's own token
/// is deleted; on a side where it does not, a close (respectively open) token
/// is put in its place, which splits the wrapper around the lifted content.
pub fn lift_changes(range: &NodeRange, target: usize) -> Vec<Change> {
    let depth = range.depth();
    let from = range.resolved_from();
    let to = range.resolved_to();
    let gap_start = range.start();
    let gap_end = range.end();

    let mut start = gap_start;
    let mut closes: Vec<Token> = Vec::new();
    let mut splitting = false;
    for d in (target + 1..=depth).rev() {
        if splitting || from.index(d) > 0 {
            splitting = true;
            closes.push(Token::Close(from.node(d).markup().clone()));
        } else {
            start -= 1;
        }
    }

    let mut end = gap_end;
    let mut opens: Vec<Token> = Vec::new();
    let mut splitting = false;
    for d in (target + 1..=depth).rev() {
        let after = if d == depth { gap_end } else { to.after(d + 1) };
        if splitting || after < to.end(d) {
            splitting = true;
            opens.push(Token::Open(to.node(d).markup().clone()));
        } else {
            end += 1;
        }
    }
    opens.reverse();

    vec![
        Change::replace(start, gap_start, Slice::from_tokens(&closes)),
        Change::replace(gap_end, end, Slice::from_tokens(&opens)),
    ]
}

/// The chain of container types `range` has to be wrapped in so that a node of
/// type `ty` holds it, outermost first, or `None` when no such chain exists.
pub fn find_wrapping(
    schema: &Schema,
    range: &NodeRange,
    ty: NodeTypeId,
) -> Option<Vec<NodeTypeId>> {
    let around = find_wrapping_outside(schema, range, ty)?;
    let inside = find_wrapping_inside(schema, range, ty)?;
    let mut out = around;
    out.push(ty);
    out.extend(inside);
    Some(out)
}

fn find_wrapping_outside(
    schema: &Schema,
    range: &NodeRange,
    ty: NodeTypeId,
) -> Option<Vec<NodeTypeId>> {
    let parent = range.parent();
    let m = content_match_at(schema, parent, range.start_index())?;
    let around = schema.find_wrapping(m, ty)?;
    let outer = around.first().copied().unwrap_or(ty);
    if can_replace_with(
        schema,
        parent,
        range.start_index(),
        range.end_index(),
        &[outer],
    ) {
        Some(around)
    } else {
        None
    }
}

fn find_wrapping_inside(
    schema: &Schema,
    range: &NodeRange,
    ty: NodeTypeId,
) -> Option<Vec<NodeTypeId>> {
    let parent = range.parent();
    let inner = parent.maybe_child(range.start_index())?;
    let inside = schema.find_wrapping(schema.content_match(ty), inner.type_id())?;
    let last = inside.last().copied().unwrap_or(ty);
    let mut m = schema.content_match(last);
    for i in range.start_index()..range.end_index() {
        m = m.match_type(parent.child(i).type_id())?;
    }
    if m.valid_end() { Some(inside) } else { None }
}

/// The changes that wrap `range` in `wrappers`, outermost first.
pub fn wrap_changes(schema: &Schema, range: &NodeRange, wrappers: &[NodeTypeId]) -> Vec<Change> {
    wrap_changes_with(
        range,
        &wrappers
            .iter()
            .map(|ty| markup_of(schema, *ty, &Attrs::empty()))
            .collect::<Vec<_>>(),
    )
}

/// The changes that wrap `range` in containers with the given markup,
/// outermost first.
pub fn wrap_changes_with(range: &NodeRange, wrappers: &[Markup]) -> Vec<Change> {
    let opens: Vec<Token> = wrappers.iter().cloned().map(Token::Open).collect();
    let closes: Vec<Token> = wrappers.iter().rev().cloned().map(Token::Close).collect();
    vec![
        Change::insert(range.start(), Slice::from_tokens(&opens)),
        Change::insert(range.end(), Slice::from_tokens(&closes)),
    ]
}

/// Whether the nodes on either side of `pos` can be joined into one.
pub fn can_join(schema: &Schema, doc: &Node, pos: usize) -> bool {
    let Ok(resolved) = doc.resolve(pos) else {
        return false;
    };
    let (Some(before), Some(after)) = (resolved.node_before(), resolved.node_after()) else {
        return false;
    };
    if resolved.text_offset() != 0 {
        return false;
    }
    let index = resolved.index(resolved.depth());
    !before.is_leaf()
        && can_append(schema, &before, &after)
        && can_replace(schema, resolved.parent(), index, index + 1)
}

/// The nearest position at or outside `pos`, in direction `dir`, at which two
/// containers can be joined.
///
/// Mirrors ProseMirror's `joinPoint`: textblocks are deliberately skipped, so
/// this finds structural joins only.
pub fn join_point(schema: &Schema, doc: &Node, pos: usize, dir: i32) -> Option<usize> {
    let resolved = doc.resolve(pos).ok()?;
    let mut pos = pos;
    for d in (0..=resolved.depth()).rev() {
        let mut index = resolved.index(d);
        let (before, after) = if d == resolved.depth() {
            (resolved.node_before(), resolved.node_after())
        } else if dir > 0 {
            let before = Some(resolved.node(d + 1).clone());
            index += 1;
            (before, resolved.node(d).maybe_child(index).cloned())
        } else {
            (
                index
                    .checked_sub(1)
                    .and_then(|i| resolved.node(d).maybe_child(i))
                    .cloned(),
                Some(resolved.node(d + 1).clone()),
            )
        };
        if let (Some(before), Some(after)) = (&before, &after)
            && !before.is_textblock(schema)
            && !before.is_leaf()
            && can_append(schema, before, after)
            && can_replace(schema, resolved.node(d), index, index + 1)
        {
            return Some(pos);
        }
        if d == 0 {
            return None;
        }
        pos = if dir < 0 {
            resolved.before(d)
        } else {
            resolved.after(d)
        };
    }
    None
}

/// The changes that join the nodes on either side of `pos`.
pub fn join_changes(pos: usize, depth: usize) -> Vec<Change> {
    vec![Change::delete(pos - depth, pos + depth)]
}
