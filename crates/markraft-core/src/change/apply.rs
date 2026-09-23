//! Applying and inverting change sets.
//!
//! Application is a token splice. The smallest subtree that contains every
//! change is found, its content is turned into a token run (whole untouched
//! children stay single tokens, so their subtrees are shared), the run is
//! spliced, and the result is parsed back into a fragment. Everything outside
//! that subtree is reused by reference.

use crate::error::ChangeError;
use crate::fragment::Fragment;
use crate::mark::MarkSet;
use crate::node::{Markup, Node, apply_marks};
use crate::schema::{MarkTypeId, NodeTypeId, Schema};
use crate::slice::{Token, tokens_cut, tokens_size};

use super::{ChangeSet, MarkChange, SectionBuilder, SectionOp};

/// The tokens of a node's content, one token per direct child.
pub(crate) fn content_tokens(node: &Node) -> Vec<Token> {
    node.children().map(|c| Token::Node(c.clone())).collect()
}

/// The token run of `doc` between two document positions.
///
/// The run may be unbalanced when the range crosses node boundaries: it then
/// carries unmatched `Close` tokens at the start and unmatched `Open` tokens at
/// the end.
pub(crate) fn doc_token_run(doc: &Node, from: usize, to: usize) -> Result<Vec<Token>, ChangeError> {
    if from >= to {
        return Ok(Vec::new());
    }
    let resolved = doc.resolve(from)?;
    let shared = resolved.shared_depth(to);
    let base = resolved.start(shared);
    let container = resolved.node(shared);
    Ok(tokens_cut(
        &content_tokens(container),
        from - base,
        to - base,
    ))
}

/// Parse a balanced token run into a fragment.
pub(crate) fn fragment_from_tokens(tokens: &[Token]) -> Result<Fragment, ChangeError> {
    let mut stack: Vec<(Option<Markup>, Vec<Node>)> = vec![(None, Vec::new())];
    for token in tokens {
        match token {
            Token::Open(markup) => stack.push((Some(markup.clone()), Vec::new())),
            Token::Close(_) => {
                if stack.len() == 1 {
                    return Err(ChangeError::Unbalanced(
                        "a close token has no matching open token".into(),
                    ));
                }
                let (markup, content) = stack.pop().expect("stack is not empty");
                let node = Node::container(
                    markup.expect("only the root frame has no markup"),
                    Fragment::from_nodes(content),
                );
                stack.last_mut().expect("root frame remains").1.push(node);
            }
            Token::Node(node) => stack
                .last_mut()
                .expect("root frame remains")
                .1
                .push(node.clone()),
        }
    }
    if stack.len() != 1 {
        return Err(ChangeError::Unbalanced(format!(
            "{} container(s) left open",
            stack.len() - 1
        )));
    }
    let (_, content) = stack.pop().expect("stack is not empty");
    Ok(Fragment::from_nodes(content))
}

/// Build the mark-set transformation a list of modifications describes.
pub(crate) fn mark_fn<'a>(
    schema: &'a Schema,
    mods: &'a [MarkChange],
) -> impl Fn(&MarkSet) -> MarkSet + 'a {
    move |set: &MarkSet| {
        let mut out = set.clone();
        for change in mods {
            out = match change {
                MarkChange::Add(mark) => out.add(schema, mark.clone()),
                MarkChange::Remove(mark) => out.remove(mark),
                MarkChange::RemoveType(ty) => out.remove_type(*ty),
            };
        }
        out
    }
}

/// Apply mark modifications to every node in a token run.
///
/// Open and close tokens are left alone: they only appear when their container
/// is partially covered, in which case the modification belongs to the content
/// inside the range rather than to the container.
pub(crate) fn mark_tokens(schema: &Schema, tokens: &[Token], mods: &[MarkChange]) -> Vec<Token> {
    let f = mark_fn(schema, mods);
    tokens
        .iter()
        .map(|token| match token {
            Token::Node(node) => Token::Node(apply_marks(schema, node, &f)),
            other => other.clone(),
        })
        .collect()
}

impl ChangeSet {
    /// The range of the starting document that this set touches.
    pub(crate) fn changed_span(&self) -> Option<(usize, usize)> {
        let mut lo = usize::MAX;
        let mut hi = 0usize;
        let mut pos = 0usize;
        for section in &self.sections {
            let end = pos + section.len;
            if section.op != SectionOp::Keep {
                lo = lo.min(pos);
                hi = hi.max(end);
            }
            pos = end;
        }
        if lo == usize::MAX {
            None
        } else {
            Some((lo, hi))
        }
    }

    /// The depth of the subtree that has to be rebuilt.
    ///
    /// The whole spliced token run has to balance inside that subtree, so the
    /// region must be at least as deep as the deepest container any inserted
    /// run closes without having opened it. A split, for instance, closes its
    /// parent, so it is rebuilt one level further out.
    fn region_depth(
        &self,
        doc: &Node,
        at_lo: &crate::pos::ResolvedPos,
        hi: usize,
    ) -> Result<usize, ChangeError> {
        let mut shared = at_lo.shared_depth(hi);
        let mut pos = 0usize;
        for section in &self.sections {
            if let SectionOp::Replace(tokens) = &section.op {
                let dip = crate::slice::min_prefix_delta(tokens);
                if dip < 0 {
                    let depth = doc.resolve(pos)?.depth() as isize;
                    shared = shared.min((depth + dip).max(0) as usize);
                }
            }
            pos += section.len;
        }
        Ok(shared)
    }

    /// Apply this set to `doc`, producing a new document.
    ///
    /// The result shares every subtree the change did not touch with `doc`.
    /// Fails when `doc` has a different size than the set was created for, or
    /// when the resulting token run does not describe a well-formed tree.
    pub fn apply(&self, doc: &Node) -> Result<Node, ChangeError> {
        if doc.content_size() != self.len_before {
            return Err(ChangeError::LengthMismatch {
                expected: self.len_before,
                actual: doc.content_size(),
            });
        }
        let Some((lo, hi)) = self.changed_span() else {
            return Ok(doc.clone());
        };
        let resolved = doc.resolve(lo)?;
        let mut shared = self.region_depth(doc, &resolved, hi)?;
        loop {
            match self.rebuild(&resolved, shared) {
                // Earlier changes can lower the depth a later run starts from,
                // in which case the estimated region is still too deep. Widen it
                // rather than reporting a change as unbalanced that is not.
                Err(ChangeError::Unbalanced(_)) if shared > 0 => shared -= 1,
                other => return other,
            }
        }
    }

    /// Rebuild the subtree at `shared` and splice the result back into `doc`.
    fn rebuild(
        &self,
        resolved: &crate::pos::ResolvedPos,
        shared: usize,
    ) -> Result<Node, ChangeError> {
        let base = resolved.start(shared);
        let container = resolved.node(shared).clone();
        let region_end = base + container.content_size();
        let container_tokens = content_tokens(&container);

        let mut out: Vec<Token> = Vec::new();
        let mut pos_a = 0usize;
        for section in &self.sections {
            let end_a = pos_a + section.len;
            match &section.op {
                SectionOp::Keep | SectionOp::Mark(_) => {
                    let start = pos_a.max(base);
                    let end = end_a.min(region_end);
                    if start < end {
                        let run = tokens_cut(&container_tokens, start - base, end - base);
                        match &section.op {
                            SectionOp::Mark(mods) => {
                                out.extend(mark_tokens(&self.schema, &run, mods))
                            }
                            _ => out.extend(run),
                        }
                    }
                }
                SectionOp::Replace(tokens) => out.extend(tokens.iter().cloned()),
            }
            pos_a = end_a;
        }

        let content = fragment_from_tokens(&out)?;
        let mut node = container.copy(content);
        for depth in (0..shared).rev() {
            let parent = resolved.node(depth);
            let index = resolved.index(depth);
            node = parent.copy(parent.content().replace_child(index, node));
        }
        Ok(node)
    }

    /// The inverse of this set, as a change over the document `apply` produced.
    ///
    /// `doc` must be the document this set starts from, so that the inverse can
    /// record the content and marks the change removed. For any change `a`,
    /// `a.invert(doc).apply(a.apply(doc)) == doc`.
    pub fn invert(&self, doc: &Node) -> Result<ChangeSet, ChangeError> {
        if doc.content_size() != self.len_before {
            return Err(ChangeError::LengthMismatch {
                expected: self.len_before,
                actual: doc.content_size(),
            });
        }
        let mut builder = SectionBuilder::new();
        let mut pos_a = 0usize;
        for section in &self.sections {
            let end_a = pos_a + section.len;
            match &section.op {
                SectionOp::Keep => builder.keep(section.len),
                SectionOp::Replace(tokens) => {
                    let old = doc_token_run(doc, pos_a, end_a)?;
                    builder.replace(tokens_size(tokens), old);
                }
                SectionOp::Mark(mods) => {
                    let run = doc_token_run(doc, pos_a, end_a)?;
                    let f = mark_fn(&self.schema, mods);
                    let mut runs = Vec::new();
                    invert_mark_runs(&self.schema, &run, &f, &mut runs);
                    for (len, inverse) in runs {
                        builder.mark(len, inverse);
                    }
                }
            }
            pos_a = end_a;
        }
        Ok(builder.finish(&self.schema, self.len_after))
    }
}

fn invert_mark_runs(
    schema: &Schema,
    tokens: &[Token],
    f: &dyn Fn(&MarkSet) -> MarkSet,
    out: &mut Vec<(usize, Vec<MarkChange>)>,
) {
    for token in tokens {
        match token {
            Token::Open(_) | Token::Close(_) => out.push((1, Vec::new())),
            Token::Node(node) => invert_mark_node(schema, node, f, out),
        }
    }
}

fn invert_mark_node(
    schema: &Schema,
    node: &Node,
    f: &dyn Fn(&MarkSet) -> MarkSet,
    out: &mut Vec<(usize, Vec<MarkChange>)>,
) {
    // Mirrors `apply_marks` exactly: inline nodes take the modification, block
    // nodes are descended into. Anything else would make the inverse cover a
    // different set of nodes than the change it undoes.
    if schema.node_type(node.type_id()).is_inline() {
        let next = f(node.marks());
        let ops = if next == *node.marks() {
            Vec::new()
        } else {
            inverse_ops(node.marks(), &next)
        };
        out.push((node.node_size(), ops));
    } else if node.is_container() && node.content_size() > 0 {
        out.push((1, Vec::new()));
        for child in node.children() {
            invert_mark_node(schema, child, f, out);
        }
        out.push((1, Vec::new()));
    } else {
        out.push((node.node_size(), Vec::new()));
    }
}

/// The modifications that turn `new` back into `old`.
fn inverse_ops(old: &MarkSet, new: &MarkSet) -> Vec<MarkChange> {
    let mut ops = Vec::new();
    for mark in new.iter() {
        if !old.contains(mark) {
            ops.push(MarkChange::Remove(mark.clone()));
        }
    }
    for mark in old.iter() {
        if !new.contains(mark) {
            ops.push(MarkChange::Add(mark.clone()));
        }
    }
    ops
}

/// Split a mark change over `from..to` into runs whose modifications the
/// content's own parent allows.
///
/// Marks belong to inline content and are governed by the node that holds it,
/// so a change that spans a paragraph and a code block may only take effect in
/// the paragraph. Resolving this once, against the document the change is
/// created for, is what lets `apply`, `invert` and `compose` handle mark
/// sections without re-deriving the parent of every node.
pub(crate) fn split_mark_change(
    schema: &Schema,
    doc: &Node,
    from: usize,
    to: usize,
    mods: &[MarkChange],
) -> Result<Vec<(usize, Vec<MarkChange>)>, ChangeError> {
    let mut out = Vec::new();
    if from >= to {
        return Ok(out);
    }
    let resolved = doc.resolve(from)?;
    let shared = resolved.shared_depth(to);
    let base = resolved.start(shared);
    let container = resolved.node(shared);
    let run = tokens_cut(&content_tokens(container), from - base, to - base);
    // The run starts inside the ancestors of `from`, so it can close containers
    // it never opened. Seeding the stack with the whole chain keeps the parent
    // of every token known.
    let mut parents: Vec<NodeTypeId> = (shared..=resolved.depth())
        .map(|depth| resolved.node(depth).type_id())
        .collect();
    for token in &run {
        match token {
            Token::Open(markup) => {
                parents.push(markup.ty);
                out.push((1, Vec::new()));
            }
            Token::Close(_) => {
                if parents.len() > 1 {
                    parents.pop();
                }
                out.push((1, Vec::new()));
            }
            Token::Node(node) => {
                let parent = *parents.last().expect("the outermost frame remains");
                split_mark_node(schema, parent, node, mods, &mut out);
            }
        }
    }
    Ok(out)
}

fn split_mark_node(
    schema: &Schema,
    parent: NodeTypeId,
    node: &Node,
    mods: &[MarkChange],
    out: &mut Vec<(usize, Vec<MarkChange>)>,
) {
    if schema.node_type(node.type_id()).is_inline() {
        let parent_ty = schema.node_type(parent);
        let allowed: Vec<MarkChange> = mods
            .iter()
            .filter(|change| parent_ty.allows_mark_in_content(mark_change_type(change)))
            .cloned()
            .collect();
        out.push((node.node_size(), allowed));
    } else if node.is_container() && node.content_size() > 0 {
        out.push((1, Vec::new()));
        let inner = node.type_id();
        for child in node.children() {
            split_mark_node(schema, inner, child, mods, out);
        }
        out.push((1, Vec::new()));
    } else {
        out.push((node.node_size(), Vec::new()));
    }
}

/// Resolve "make `from..to` carry exactly `marks`" into per-run modifications.
///
/// Each inline node gets the removals and additions that take its own marks to
/// `marks` restricted to what its parent allows; removals come first so the
/// additions never meet a mark they exclude. Runs are reported in the same
/// shape as [`split_mark_change`].
pub(crate) fn split_set_marks(
    schema: &Schema,
    doc: &Node,
    from: usize,
    to: usize,
    marks: &MarkSet,
) -> Result<Vec<(usize, Vec<MarkChange>)>, ChangeError> {
    let mut out = Vec::new();
    if from >= to {
        return Ok(out);
    }
    let resolved = doc.resolve(from)?;
    let shared = resolved.shared_depth(to);
    let base = resolved.start(shared);
    let container = resolved.node(shared);
    let run = tokens_cut(&content_tokens(container), from - base, to - base);
    let mut parents: Vec<NodeTypeId> = (shared..=resolved.depth())
        .map(|depth| resolved.node(depth).type_id())
        .collect();
    for token in &run {
        match token {
            Token::Open(markup) => {
                parents.push(markup.ty);
                out.push((1, Vec::new()));
            }
            Token::Close(_) => {
                if parents.len() > 1 {
                    parents.pop();
                }
                out.push((1, Vec::new()));
            }
            Token::Node(node) => {
                let parent = *parents.last().expect("the outermost frame remains");
                set_marks_node(schema, parent, node, marks, &mut out);
            }
        }
    }
    Ok(out)
}

fn set_marks_node(
    schema: &Schema,
    parent: NodeTypeId,
    node: &Node,
    marks: &MarkSet,
    out: &mut Vec<(usize, Vec<MarkChange>)>,
) {
    if schema.node_type(node.type_id()).is_inline() {
        let parent_ty = schema.node_type(parent);
        let target = marks.filter(|m| parent_ty.allows_mark_in_content(m.ty));
        let mut ops = Vec::new();
        for mark in node.marks().iter() {
            if !target.contains(mark) {
                ops.push(MarkChange::Remove(mark.clone()));
            }
        }
        for mark in target.iter() {
            if !node.marks().contains(mark) {
                ops.push(MarkChange::Add(mark.clone()));
            }
        }
        out.push((node.node_size(), ops));
    } else if node.is_container() && node.content_size() > 0 {
        out.push((1, Vec::new()));
        let inner = node.type_id();
        for child in node.children() {
            set_marks_node(schema, inner, child, marks, out);
        }
        out.push((1, Vec::new()));
    } else {
        out.push((node.node_size(), Vec::new()));
    }
}

/// The mark type a modification acts on.
pub(crate) fn mark_change_type(change: &MarkChange) -> MarkTypeId {
    match change {
        MarkChange::Add(mark) | MarkChange::Remove(mark) => mark.ty,
        MarkChange::RemoveType(ty) => *ty,
    }
}
