//! The document kind's shared formula boundaries, used by the GPUI surface.

pub(crate) use markraft_core::kind::math::formula_spans;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use markraft_commonmark::{MarkdownParser, commonmark_doc_type_names, commonmark_schema};
    use markraft_core::kind::math::FormulaSpan;
    use markraft_core::kind::{DocTypes, conceal};
    use markraft_core::projection::Projection;

    fn projected(markdown: &str) -> (Projection, DocTypes) {
        let schema = commonmark_schema();
        let doc = MarkdownParser::commonmark(schema.clone())
            .parse(markdown)
            .unwrap();
        let types = DocTypes::from_schema_names(&schema, &commonmark_doc_type_names());
        (Projection::of(&doc, &schema), types)
    }

    fn formulas(markdown: &str) -> Vec<FormulaSpan> {
        let (projection, types) = projected(markdown);
        projection
            .lines()
            .iter()
            .enumerate()
            .flat_map(|(index, line)| {
                formula_spans(line, projection.line_text(index).unwrap(), &types)
            })
            .collect()
    }

    #[test]
    fn formula_boundaries_come_from_kind_marks_not_delimiter_spelling() {
        let schema = commonmark_schema();
        let doc = MarkdownParser::commonmark(schema.clone())
            .parse("$x$")
            .unwrap();
        let paragraph = doc.child(0);
        let custom = paragraph.copy(markraft_core::Fragment::from_nodes(vec![
            paragraph.child(0).with_text("⟦"),
            paragraph.child(1).clone(),
            paragraph.child(2).with_text("⟧"),
        ]));
        let doc = doc.copy(markraft_core::Fragment::from_node(custom));
        let types = DocTypes::from_schema_names(&schema, &commonmark_doc_type_names());
        let projection = Projection::of(&doc, &schema);
        let spans = formula_spans(&projection.lines()[0], "⟦x⟧", &types);
        assert_eq!(
            spans,
            [FormulaSpan {
                source: 0..3,
                content: 1..2,
                tex: "x".into(),
                display: false,
            }]
        );
    }

    #[test]
    fn inline_offsets_are_characters_and_tex_is_verbatim() {
        assert_eq!(
            formulas("中文 $\\frac{α}{2}$ tail"),
            [FormulaSpan {
                source: 3..16,
                content: 4..15,
                tex: "\\frac{α}{2}".into(),
                display: false,
            }]
        );
    }

    #[test]
    fn display_math_retains_internal_line_breaks() {
        assert_eq!(
            formulas("$$\nE=mc^2\n$$"),
            [FormulaSpan {
                source: 0..12,
                content: 2..10,
                tex: "\nE=mc^2\n".into(),
                display: true,
            }]
        );
    }

    #[test]
    fn empty_display_body_remains_an_editable_formula() {
        let found = formulas("$$\n\n$$");
        assert_eq!(
            found,
            [FormulaSpan {
                source: 0..6,
                content: 2..4,
                tex: "\n\n".into(),
                display: true,
            }]
        );
        assert!(found[0].is_standalone("$$\n\n$$"));
    }

    #[test]
    fn editable_body_excludes_every_delimiter_character() {
        for source in ["中文 $x$ tail", "$`x`$", "$$x$$", "$$\n\n$$"] {
            let (projection, types) = projected(source);
            let line = &projection.lines()[0];
            let span = formula_spans(line, projection.line_text(0).unwrap(), &types).remove(0);
            let from = line.offset_to_pos(span.content.start).unwrap();
            let to = line.offset_to_pos(span.content.end).unwrap();
            assert!(span.selection_within(line, from..from), "{source}");
            assert!(span.selection_within(line, to..to), "{source}");
            assert!(span.selection_within(line, from..to), "{source}");
            assert!(!span.selection_within(line, from - 1..from), "{source}");
            assert!(!span.selection_within(line, to..to + 1), "{source}");
            assert!(!span.selection_within(line, to..from), "{source}");
        }
    }

    #[test]
    fn only_a_display_formula_without_surrounding_prose_is_standalone() {
        assert!(formulas("$$x$$")[0].is_standalone("$$x$$"));
        assert!(!formulas("$x$")[0].is_standalone("$x$"));
        assert!(!formulas("before $$x$$ after")[0].is_standalone("before $$x$$ after"));
        assert!(!formulas("$$x$$ and $$y$$")[0].is_standalone("$$x$$ and $$y$$"));
    }

    #[test]
    fn adjacent_formulas_keep_distinct_conceal_spans() {
        let found = formulas("$a$$b$");
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].source, 0..3);
        assert_eq!(found[0].tex, "a");
        assert_eq!(found[1].source, 3..6);
        assert_eq!(found[1].tex, "b");
    }

    #[test]
    fn code_math_uses_its_own_two_character_fences() {
        let found = formulas("$`x^2`$");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].tex, "x^2");
        assert!(!found[0].display);
    }

    #[test]
    fn incomplete_escaped_currency_and_code_are_not_formulas() {
        for source in [
            "$unfinished",
            "$$unfinished",
            r"\$x\$",
            "costs $5 and $10",
            "$ a $",
            "$5$6",
            "`$x$`",
            "```tex\n$x$\n```",
        ] {
            assert!(formulas(source).is_empty(), "{source:?}");
        }
    }

    #[test]
    fn formulas_inside_table_cells_keep_local_offsets() {
        let found = formulas("| Formula |\n| --- |\n| $x^2$ |\n");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].source, 0..5);
        assert_eq!(found[0].tex, "x^2");
    }

    #[test]
    fn caret_selection_and_composition_use_existing_reveal_contract() {
        let (projection, types) = projected("before $x$ after");
        let line = &projection.lines()[0];
        let span = formula_spans(line, projection.line_text(0).unwrap(), &types).remove(0);
        let from = line.offset_to_pos(span.source.start).unwrap();
        let to = line.offset_to_pos(span.source.end).unwrap();
        assert!(!span.revealed(line, &conceal::Reveal::nothing()));
        assert!(span.revealed(line, &conceal::Reveal::at(from..from, None)));
        assert!(span.revealed(line, &conceal::Reveal::at(to..to, None)));
        assert!(!span.revealed(line, &conceal::Reveal::at(0..from, None)));
        assert!(span.revealed(line, &conceal::Reveal::at(from + 1..to, None)));
        assert!(span.revealed(line, &conceal::Reveal::at(0..0, Some(from + 1..to))));
    }
}
