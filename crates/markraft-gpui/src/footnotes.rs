//! Going between a footnote's references and its definition.
//!
//! A reference is a mark carrying the label of the definition it names, and a
//! definition a block carrying the same label, so both ways are a walk over
//! the document for the other with that label.

use markraft_core::kind::FOOTNOTE_LABEL_ATTR;
use markraft_core::{Attrs, Node, Schema};

use markraft_core::kind::DocTypes;

fn label(attrs: &Attrs) -> Option<&str> {
    attrs
        .get(FOOTNOTE_LABEL_ATTR)
        .and_then(|value| value.as_str())
}

/// The footnote reference at `pos`, as its label.
pub(crate) fn reference_at(doc: &Node, types: &DocTypes, pos: usize) -> Option<String> {
    let ty = types.footnote_reference?;
    let (_, mark) = crate::links::link_at(doc, ty, pos)?;
    label(&mark.attrs).map(str::to_owned)
}

/// Where the text of the definition labelled `wanted` starts: the start of its
/// first textblock.
pub(crate) fn definition(
    schema: &Schema,
    doc: &Node,
    types: &DocTypes,
    wanted: &str,
) -> Option<usize> {
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
pub(crate) fn first_reference(doc: &Node, types: &DocTypes, wanted: &str) -> Option<usize> {
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
pub(crate) fn definition_label_at(doc: &Node, types: &DocTypes, pos: usize) -> Option<String> {
    let ty = types.footnote_definition?;
    let resolved = doc.resolve(pos).ok()?;
    (1..=resolved.depth())
        .rev()
        .map(|depth| resolved.node(depth))
        .find(|node| node.type_id() == ty)
        .and_then(|node| label(node.attrs()).map(str::to_owned))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use markraft_commonmark::{commonmark_doc_type_names, commonmark_schema, from_markdown};

    #[test]
    fn a_reference_and_its_definition_find_each_other() {
        let schema = commonmark_schema();
        let types = DocTypes::from_schema_names(&schema, &commonmark_doc_type_names());
        let doc = from_markdown(&schema, "a[^n] b\n\n[^n]: note\n").expect("a document");
        // `a` is at 1, `[^` at 2..4, the label at 4.
        assert_eq!(reference_at(&doc, &types, 4).as_deref(), Some("n"));
        assert_eq!(reference_at(&doc, &types, 1), None);
        let target = definition(&schema, &doc, &types, "n").expect("the definition");
        let resolved = doc.resolve(target).expect("a position");
        let text: String = resolved
            .parent()
            .children()
            .filter_map(|leaf| leaf.text())
            .collect();
        assert_eq!(text, "note");
        assert_eq!(resolved.parent_offset(), 0);
        assert_eq!(
            definition_label_at(&doc, &types, target).as_deref(),
            Some("n")
        );
        assert_eq!(first_reference(&doc, &types, "n"), Some(2));
        assert_eq!(definition(&schema, &doc, &types, "missing"), None);
    }
}
