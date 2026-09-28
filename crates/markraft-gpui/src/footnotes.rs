//! Footnote navigation queries shared with non-UI consumers.

pub(crate) use markraft_core::kind::footnotes::{
    definition, definition_label_at, first_reference, reference_at,
};

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use markraft_commonmark::{commonmark_doc_type_names, commonmark_schema, from_markdown};
    use markraft_core::kind::DocTypes;

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
