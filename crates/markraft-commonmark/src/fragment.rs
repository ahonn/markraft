//! Slices: the unit a clipboard carries.
//!
//! A [`Slice`] is a cut of a document, and its *open depths* say how many
//! containers the cut passed through on each side — which is what tells a paste
//! whether the content merges with its surroundings or lands beside them.
//! Markdown has no way to write an open depth down, so the two directions are
//! not symmetric:
//!
//! * **Copying** closes the slice: its partial blocks are wrapped in whatever
//!   the schema needs to make a document of them, so a range spanning two list
//!   items reaches the clipboard as `- one\n- two` rather than as two orphaned
//!   items.
//! * **Pasting** opens the parsed document only where it has an incomplete
//!   block to merge ([`open_fragment`]): a fragment that is one textblock is
//!   open on both sides, so its inline content joins the caret's block, and
//!   anything else is closed, so its blocks land beside what they were pasted
//!   into rather than dissolving into it.

use markraft_core::{Attrs, Fragment, Node, Schema, Slice};

use crate::fit::{fit, fit_document};

/// Open a parsed fragment for pasting.
///
/// A fragment that is a single textblock is open on both sides: it has no
/// block of its own to contribute, so its inline content merges into the one
/// the caret sits in. Everything else is closed, because its blocks *are* the
/// content — a heading pasted in the middle of a paragraph splits it and stays
/// a heading rather than dissolving into the text.
///
/// A code block is never opened: its content is literal, and merging it into
/// ordinary text would change what it is.
pub fn open_fragment(schema: &Schema, content: Fragment) -> Slice {
    let lone_textblock = content.child_count() == 1
        && content.first_child().is_some_and(|block| {
            let ty = schema.node_type(block.type_id());
            ty.is_textblock() && !ty.is_code() && !ty.is_isolating()
        });
    if lone_textblock {
        Slice::new(content, 1, 1)
    } else {
        Slice::new(content, 0, 0)
    }
}

/// The blocks of a slice's content, grouping any inline content into the
/// schema's default textblock so a cut taken inside a paragraph becomes one.
pub(crate) fn as_blocks(schema: &Schema, content: &Fragment) -> Vec<Node> {
    let textblock = schema.default_type(schema.content_match(schema.top_type()));
    let mut out: Vec<Node> = Vec::new();
    let mut inline: Vec<Node> = Vec::new();
    let flush = |inline: &mut Vec<Node>, out: &mut Vec<Node>| {
        if inline.is_empty() {
            return;
        }
        let taken = std::mem::take(inline);
        if let Some(block) = textblock.and_then(|ty| fit(schema, ty, Attrs::empty(), taken).ok()) {
            out.push(block);
        }
    };
    for child in content.iter() {
        if schema.node_type(child.type_id()).is_inline() {
            inline.push(child.clone());
        } else {
            flush(&mut inline, &mut out);
            out.push(child.clone());
        }
    }
    flush(&mut inline, &mut out);
    out
}

/// Close a slice into a document, so it can be written with the ordinary rules.
pub(crate) fn close(schema: &Schema, slice: &Slice) -> Option<Node> {
    fit_document(schema, as_blocks(schema, slice.content())).ok()
}

/// Put `whitespace` back on the end of the document's last textblock.
///
/// A reader strips the trailing whitespace of a paragraph, but a *fragment*
/// needs it: pasting `hello ` in front of `tail` has to leave the two words
/// apart. Code blocks are left alone — their whitespace is content and was
/// never stripped.
pub(crate) fn append_trailing(schema: &Schema, node: &Node, whitespace: &str) -> Node {
    if whitespace.is_empty() {
        return node.clone();
    }
    let ty = schema.node_type(node.type_id());
    if ty.is_textblock() && !ty.is_code() {
        let mut children: Vec<Node> = node.children().cloned().collect();
        match children.last_mut() {
            Some(last) if last.is_text() && last.marks().is_empty() => {
                let joined = format!("{}{whitespace}", last.text().unwrap_or_default());
                *last = last.with_text(&joined);
            }
            _ => children.push(schema.text(whitespace)),
        }
        return node.copy(Fragment::from_nodes(children));
    }
    if !node.is_container() || node.child_count() == 0 {
        return node.clone();
    }
    let mut children: Vec<Node> = node.children().cloned().collect();
    let index = children.len() - 1;
    children[index] = append_trailing(schema, &children[index], whitespace);
    node.copy(Fragment::from_nodes(children))
}
