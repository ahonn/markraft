//! Making a run of nodes fit a content rule.
//!
//! Neither comrak's tree nor a browser's DOM has to agree with the schema: a
//! list item may hold blocks the content rule forbids, a `<div>` may hold
//! anything at all, and a consumer's rule set may be looser still. Every
//! importer in this crate funnels its children through [`fit`], which walks the
//! type's content automaton and, for a child that does not match, tries in
//! order to
//!
//! 1. put the children the rule requires before it,
//! 2. wrap it in whatever chain of containers the schema says would fit,
//! 3. and only then drop it.
//!
//! Filling comes first because it adds nothing a reader sees: a task item's
//! code block gets the empty paragraph its box is written in, where a
//! wrapper that also fits there, a quote, would make the code a quotation.
//!
//! The result always passes [`Node::check`](markraft_core::Node::check), which
//! is what lets the parsers promise a valid document.

use markraft_core::commands::structure::content_valid;
use markraft_core::{Attrs, Fragment, MarkSet, Node, NodeError, NodeTypeId, Schema};

/// Build a node of `ty` holding `children`, repairing the content.
pub(crate) fn fit(
    schema: &Schema,
    ty: NodeTypeId,
    attrs: Attrs,
    children: Vec<Node>,
) -> Result<Node, NodeError> {
    let mut matched = schema.content_match(ty);
    let mut out: Vec<Node> = Vec::with_capacity(children.len());
    // The wrapper chain the previous child needed, so a run of children that
    // all want the same one shares it — two list items become one list, not two
    // lists a reader would join anyway.
    let mut wrapped_with: Option<Vec<NodeTypeId>> = None;
    for child in children {
        if let Some(next) = matched.match_type(child.type_id()) {
            matched = next;
            out.push(child);
            wrapped_with = None;
            continue;
        }
        if let Some(before) = schema.fill_before(matched, &[child.type_id()], false) {
            let mut fitted = true;
            for filler in &before {
                match create(schema, *filler) {
                    Some(node) => match matched.match_type(node.type_id()) {
                        Some(next) => {
                            matched = next;
                            out.push(node);
                        }
                        None => fitted = false,
                    },
                    None => fitted = false,
                }
            }
            if fitted && let Some(next) = matched.match_type(child.type_id()) {
                matched = next;
                out.push(child);
                wrapped_with = None;
                continue;
            }
        }
        if let Some(chain) = schema.find_wrapping(matched, child.type_id()) {
            if !chain.is_empty()
                && wrapped_with.as_deref() == Some(chain.as_slice())
                && let Some(last) = out.last()
                && last.type_id() == chain[0]
                && let Some(joined) = append_into(schema, last, &chain[1..], child.clone())
            {
                let index = out.len() - 1;
                out[index] = joined;
                continue;
            }
            if let Some(wrapped) = wrap(schema, &chain, child.clone())
                && let Some(next) = matched.match_type(wrapped.type_id())
            {
                matched = next;
                out.push(wrapped);
                wrapped_with = Some(chain);
                continue;
            }
        }
        wrapped_with = None;
        // Nothing the schema offers accepts this child. Dropping it is the last
        // resort; the presets in this crate never reach here.
    }
    if !matched.valid_end()
        && let Some(after) = schema.fill_before(matched, &[], true)
    {
        for filler in after {
            if let Some(node) = create(schema, filler) {
                out.push(node);
            }
        }
    }
    schema.create(ty, attrs, MarkSet::empty(), Fragment::from_nodes(out))
}

/// Build a document holding `blocks`, repairing the content.
pub(crate) fn fit_document(schema: &Schema, blocks: Vec<Node>) -> Result<Node, NodeError> {
    fit(schema, schema.top_type(), Attrs::empty(), blocks)
}

fn create(schema: &Schema, ty: NodeTypeId) -> Option<Node> {
    schema.create_and_fill(ty, Attrs::empty(), MarkSet::empty(), Fragment::empty())
}

/// Add `child` at the end of `node`, descending the rest of a wrapper chain.
fn append_into(schema: &Schema, node: &Node, rest: &[NodeTypeId], child: Node) -> Option<Node> {
    let mut children: Vec<Node> = node.children().cloned().collect();
    if rest.is_empty() {
        children.push(child);
    } else {
        let last = children.last()?;
        if last.type_id() != rest[0] {
            return None;
        }
        let joined = append_into(schema, last, &rest[1..], child)?;
        let index = children.len() - 1;
        children[index] = joined;
    }
    content_valid(schema, node.type_id(), &children)
        .then(|| node.copy(Fragment::from_nodes(children)))
}

fn wrap(schema: &Schema, chain: &[NodeTypeId], child: Node) -> Option<Node> {
    let mut node = child;
    for ty in chain.iter().rev() {
        node = schema.create_and_fill(
            *ty,
            Attrs::empty(),
            MarkSet::empty(),
            Fragment::from_node(node),
        )?;
    }
    Some(node)
}
