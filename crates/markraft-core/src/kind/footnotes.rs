//! Going between a footnote's references and its definition.
//!
//! A reference is a mark carrying the label of the definition it names, and a
//! definition a block carrying the same label, so both ways are a walk over
//! the document for the other with that label.

use crate::kind::FOOTNOTE_LABEL_ATTR;
use crate::{Attrs, Mark, MarkTypeId, Node, Schema};
use std::ops::Range;

use crate::kind::DocTypes;

fn label(attrs: &Attrs) -> Option<&str> {
    attrs
        .get(FOOTNOTE_LABEL_ATTR)
        .and_then(|value| value.as_str())
}

/// The footnote reference at `pos`, as its label.
pub fn reference_at(doc: &Node, types: &DocTypes, pos: usize) -> Option<String> {
    let ty = types.footnote_reference?;
    let (_, mark) = marked_range_at(doc, ty, pos)?;
    label(&mark.attrs).map(str::to_owned)
}

/// Where the text of the definition labelled `wanted` starts: the start of its
/// first textblock.
pub fn definition(schema: &Schema, doc: &Node, types: &DocTypes, wanted: &str) -> Option<usize> {
    let ty = types.footnote_definition?;
    let mut found = None;
    doc.descendants(&mut |node, pos, _, _| {
        if found.is_some() {
            return false;
        }
        if node.type_id() == ty && label(node.attrs()) == Some(wanted) {
            let mut first = None;
            node.descendants(&mut |inner, offset, _, _| {
                if first.is_none() && inner.is_textblock(schema) {
                    first = Some(pos + 1 + offset + 1);
                }
                first.is_none()
            });
            found = Some(first.unwrap_or(pos + 1));
            return false;
        }
        true
    });
    found
}

/// Where the first reference to the footnote labelled `wanted` starts.
pub fn first_reference(doc: &Node, types: &DocTypes, wanted: &str) -> Option<usize> {
    let ty = types.footnote_reference?;
    let mut found = None;
    doc.descendants(&mut |node, pos, _, _| {
        if found.is_some() {
            return false;
        }
        if node
            .marks()
            .get(ty)
            .is_some_and(|mark| label(&mark.attrs) == Some(wanted))
        {
            found = Some(pos);
            return false;
        }
        true
    });
    found
}

/// The label of the footnote definition `pos` is in, innermost first.
pub fn definition_label_at(doc: &Node, types: &DocTypes, pos: usize) -> Option<String> {
    let ty = types.footnote_definition?;
    let resolved = doc.resolve(pos).ok()?;
    (1..=resolved.depth())
        .rev()
        .map(|depth| resolved.node(depth))
        .find(|node| node.type_id() == ty)
        .and_then(|node| label(node.attrs()).map(str::to_owned))
}

/// The mark covering `pos`, as a document-position range and mark value.
///
/// The range is the whole run of inline content carrying the same mark,
/// useful for both link editing and footnote lookup.
pub fn marked_range_at(doc: &Node, ty: MarkTypeId, pos: usize) -> Option<(Range<usize>, Mark)> {
    let resolved = doc.resolve(pos).ok()?;
    for depth in (1..=resolved.depth()).rev() {
        if let Some(mark) = resolved.node(depth).marks().get(ty) {
            return Some((resolved.before(depth)..resolved.after(depth), mark.clone()));
        }
    }
    let parent = resolved.parent();
    let start = resolved.pos() - resolved.parent_offset();
    let mut found: Option<(Range<usize>, Mark)> = None;
    let mut offset = 0usize;
    for child in parent.children() {
        let range = start + offset..start + offset + child.node_size();
        offset += child.node_size();
        let Some(mark) = child.marks().get(ty) else {
            if found.is_some() && range.start > pos {
                break;
            }
            found = None;
            continue;
        };
        match &mut found {
            Some((span, existing)) if existing == mark && span.end == range.start => {
                span.end = range.end;
            }
            _ => {
                if found.as_ref().is_some_and(|(span, _)| span.end > pos) {
                    break;
                }
                found = Some((range, mark.clone()));
            }
        }
    }
    found.filter(|(span, _)| span.start <= pos && pos <= span.end)
}
