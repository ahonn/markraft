//! Reading-text compatibility imports for the editor surface.

pub(crate) use markraft_core::kind::reading::{ShownPiece, ShownText, line_pieces};

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use markraft_commonmark::{commonmark_doc_type_names, commonmark_schema, from_markdown};
    use markraft_core::kind::{DocTypes, conceal::Reveal};
    use markraft_core::projection::RunContent;
    use markraft_core::projection::projection_of;
    use markraft_core::{EditorState, EditorStateConfig};
    use std::ops::Range;

    fn shown(source: &str) -> (ShownText, markraft_core::projection::Projection) {
        let schema = commonmark_schema();
        let doc = from_markdown(&schema, source).expect("valid Markdown");
        let state = EditorState::create(
            EditorStateConfig::new(schema.clone())
                .doc(doc)
                .extensions(markraft_core::projection::projection()),
        )
        .expect("a valid state");
        let projection = projection_of(&state);
        let types = DocTypes::from_schema_names(&schema, &commonmark_doc_type_names());
        let owned = projection.as_ref().clone();
        (
            ShownText::build(&projection, &types, &Reveal::nothing()),
            owned,
        )
    }

    fn line_offset(projection: &markraft_core::projection::Projection, offset: usize) -> usize {
        projection.lines()[0].offset_to_pos(offset).unwrap()
    }

    #[test]
    fn concealed_markup_is_absent_and_an_entity_is_its_character() {
        let (text, projection) = shown("x **ab** &amp; y");
        assert_eq!(text.text(), "x ab & y");
        let line = &projection.lines()[0];
        // The same source offsets the accessibility tree reports for this line.
        assert_eq!(
            text.matches("a")[0],
            line.offset_to_pos(4).unwrap()..line.offset_to_pos(5).unwrap()
        );
        assert_eq!(
            text.matches("&")[0],
            line.offset_to_pos(9).unwrap()..line.offset_to_pos(14).unwrap(),
            "the ampersand selects the whole entity"
        );
        assert_eq!(
            text.matches("ab"),
            vec![line_offset(&projection, 4)..line_offset(&projection, 6)]
        );
        assert_eq!(text.matches("**"), Vec::<Range<usize>>::new());
    }

    #[test]
    fn a_wiki_link_is_found_by_its_label_and_selects_the_atom() {
        let (text, projection) = shown("see [[Notes]] now");
        assert_eq!(text.text(), "see Notes now");
        let hit = text.matches("Notes").remove(0);
        let line = &projection.lines()[0];
        let atom = line
            .runs()
            .iter()
            .find(|run| matches!(run.content, RunContent::Atom(_)))
            .unwrap();
        assert_eq!(hit, line.abs(atom.start)..line.abs(atom.end));
    }

    #[test]
    fn a_code_block_and_a_table_cell_are_their_source() {
        let (text, _) = shown("```\nlet needle = 1\n```\n\n| a |\n| --- |\n| needle |\n");
        assert!(text.text().contains("let needle = 1"), "{}", text.text());
        assert_eq!(text.matches("needle").len(), 2);
    }

    #[test]
    fn matches_ignore_case_and_an_empty_query_matches_nothing() {
        let (text, _) = shown("Say Bold please");
        assert_eq!(text.matches("bold").len(), 1);
        assert!(text.matches("").is_empty());
        assert!(text.matches("   ").is_empty());
    }

    #[test]
    fn multibyte_matches_map_to_complete_source_characters() {
        for (word, query) in [
            ("你好", "你好"),
            ("CAFÉ", "café"),
            ("🙂", "🙂"),
            ("👩‍💻", "👩‍💻"),
        ] {
            let (text, projection) = shown(&format!("前 {word} / {word}"));
            let first = 2;
            let len = word.chars().count();
            let second = first + len + 3;
            let range =
                |start| line_offset(&projection, start)..line_offset(&projection, start + len);
            assert_eq!(
                text.matches(query),
                vec![range(first), range(second)],
                "{query}"
            );
        }
    }

    #[test]
    fn a_fold_that_grows_still_points_at_one_character() {
        let (text, _) = shown("İstanbul");
        let hits = text.matches("ist");
        assert_eq!(hits.len(), 1);
        assert!(hits[0].start < hits[0].end);
    }

    #[test]
    fn the_text_inside_a_selection_is_what_the_reader_sees() {
        let (text, projection) = shown("x **ab** y");
        let line = &projection.lines()[0];
        let word = line.offset_to_pos(4).unwrap()..line.offset_to_pos(6).unwrap();
        assert_eq!(text.text_inside(word).as_deref(), Some("ab"));
        assert_eq!(text.text_inside(0..0), None);
    }

    #[test]
    fn replacement_mapping_distinguishes_literals_whole_atoms_and_partial_labels() {
        use markraft_core::kind::reading::MatchMapping;
        let (text, _) = shown("plain [[Notes|Notebook]] &amp; tail");
        let mapping = |query| text.mapped_matches(query)[0].mapping;
        assert_eq!(mapping("plain"), MatchMapping::Exact);
        assert_eq!(mapping("Notebook"), MatchMapping::WholeSourceSpan);
        assert_eq!(mapping("Note"), MatchMapping::Composite);
        assert_eq!(mapping("&"), MatchMapping::WholeSourceSpan);
        assert_eq!(mapping("& tail"), MatchMapping::Composite);
    }

    #[test]
    fn replacement_mapping_rejects_hidden_syntax_and_structural_boundaries() {
        use markraft_core::kind::reading::MatchMapping;
        let (text, _) = shown("a **bold** z\n\nnext");
        assert_eq!(text.mapped_matches("bold")[0].mapping, MatchMapping::Exact);
        assert_eq!(
            text.mapped_matches("a bold")[0].mapping,
            MatchMapping::Composite
        );
        assert_eq!(
            text.mapped_matches("z\nnext")[0].mapping,
            MatchMapping::Composite
        );
    }

    #[test]
    fn replacement_mapping_keeps_unicode_coordinates_and_grapheme_boundaries() {
        use markraft_core::kind::reading::{MatchMapping, ShownRange};
        let (text, projection) = shown("中🙂 e\u{301} 👩‍💻");
        let hit = &text.mapped_matches("中🙂")[0];
        assert_eq!(hit.shown, ShownRange(0..2));
        assert_eq!(
            hit.document.0,
            line_offset(&projection, 0)..line_offset(&projection, 2)
        );
        assert_eq!(hit.mapping, MatchMapping::Exact);
        assert_eq!(text.mapped_matches("e")[0].mapping, MatchMapping::Composite);
        assert_eq!(
            text.mapped_matches("👩")[0].mapping,
            MatchMapping::Composite
        );
        assert_eq!(text.mapped_matches("👩‍💻")[0].mapping, MatchMapping::Exact);
    }

    #[test]
    fn reading_policy_keeps_math_source_and_descriptive_image_labels() {
        use markraft_core::kind::reading::MatchMapping;
        let (text, _) = shown("math $x^2$ ![Diagram](chart.png) ![[image.png|Cover]] :smile:");
        assert!(text.text().contains("x^2"));
        assert!(text.text().contains("Diagram"));
        assert!(text.text().contains("Cover"));
        assert!(text.text().contains("😄"));
        assert_eq!(
            text.mapped_matches("Diagram")[0].mapping,
            MatchMapping::WholeSourceSpan
        );
        assert_eq!(
            text.mapped_matches("Dia")[0].mapping,
            MatchMapping::Composite
        );
    }

    #[test]
    fn table_cell_matches_do_not_make_cross_cell_replacement_exact() {
        use markraft_core::kind::reading::MatchMapping;
        let (text, _) = shown("| left | right |\n| --- | --- |\n| one | two |\n");
        assert_eq!(text.mapped_matches("left")[0].mapping, MatchMapping::Exact);
        assert_eq!(text.mapped_matches("right")[0].mapping, MatchMapping::Exact);
        let left = text.text().find("left").unwrap();
        let right = text.text().find("right").unwrap() + "right".len();
        let across_cells = &text.text()[left..right];
        assert_eq!(
            text.mapped_matches(across_cells)[0].mapping,
            MatchMapping::Composite
        );
    }
}
