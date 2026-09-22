//! Regressions for semantic nesting, opaque primitives and their editing view.
mod common;

use common::{Codec, html};
use markraft_commonmark::{HtmlParser, HtmlSerializer, commonmark_extensions, schema as md};
use markraft_core::commands::{Direction, delete_by_grapheme, insert_text, run_command};
use markraft_core::projection::Projection;
use markraft_core::{EditorState, EditorStateConfig, Selection};

#[test]
fn html_mark_transitions_keep_the_ordered_common_prefix() {
    let codec = Codec::new();
    let written = HtmlSerializer::commonmark(&codec.schema).serialize(&codec.parse("*a **b** c*"));
    assert_eq!(written, "<p><em>a <strong>b</strong> c</em></p>");
    // Also exercise the flat set transition: membership is not a prefix.
    let em = codec
        .schema
        .mark(md::EM, markraft_core::Attrs::empty())
        .unwrap();
    let strong = codec
        .schema
        .mark(md::STRONG, markraft_core::Attrs::empty())
        .unwrap();
    let text = |text, marks| {
        codec.schema.text_marked(
            text,
            markraft_core::MarkSet::from_marks(&codec.schema, marks),
        )
    };
    let flat = codec
        .schema
        .doc([codec
            .schema
            .node(
                md::PARAGRAPH,
                [
                    text("a ", vec![em.clone()]),
                    text("b", vec![strong, em.clone()]),
                    text(" c", vec![em]),
                ],
            )
            .unwrap()])
        .unwrap();
    assert_eq!(
        HtmlSerializer::commonmark(&codec.schema).serialize(&flat),
        "<p><em>a </em><strong><em>b</em></strong><em> c</em></p>"
    );
}

#[test]
fn a_cell_resolves_links_and_images_before_definitions_disappear() {
    let codec = Codec::new();
    for source in [
        "| a |\n| - |\n| [foo][ref] |\n\n[ref]: https://example.com \"title\"",
        "| a |\n| - |\n| ![alt][ref] |\n\n[ref]: photo.png",
        "> | a |\n> | - |\n> | [foo][ref] |\n\n[ref]: https://example.com",
    ] {
        let written = codec.normalize(source);
        assert_eq!(html(&written), html(source), "{written:?}");
        assert_eq!(codec.normalize(&written), written);
    }
}

#[test]
fn nested_html_marks_close_only_their_own_scope() {
    let codec = Codec::new();
    // Underline has no Markdown spelling — nesting collapses to plain text.
    assert_eq!(codec.normalize("<u>a <u>b</u> c</u>"), "a b c");
    for tag in ["em", "strong", "del"] {
        let source = format!("<{tag}>a <{tag}>b</{tag}> c</{tag}>");
        let written = codec.normalize(&source);
        assert!(
            !written.contains('<'),
            "{tag} wrote HTML instead of delimiters: {written:?}"
        );
        assert_eq!(codec.normalize(&written), written);
    }
}

#[test]
fn html_clipboard_keeps_nested_spans_empty_links_and_raw_primitives() {
    let codec = Codec::new();
    let writer = HtmlSerializer::commonmark(&codec.schema);
    let reader = HtmlParser::commonmark(codec.schema.clone());
    for source in [
        "**foo **bar****",
        "*a **b** c*",
        "[](url \"title\")",
        "<span class=\"red\">hello</span>",
        "hello\nworld",
    ] {
        let doc = codec.parse(source);
        let rich = writer.serialize(&doc);
        let pasted = reader.parse(&rich).expect("clipboard HTML");
        assert_eq!(pasted, doc, "{source:?} -> {rich:?}");
        assert_eq!(html(&codec.write(&pasted)), html(source));
    }
}

#[test]
fn editing_a_nested_span_changes_its_content_and_retains_its_nesting() {
    let codec = Codec::new();
    let doc = codec.parse("**foo **bar****");
    let projection = Projection::of(&doc, &codec.schema);
    let pos = projection.line_offset_to_pos(0, 5).unwrap();
    let state = EditorState::create(
        EditorStateConfig::new(codec.schema.clone())
            .doc(doc)
            .selection(Selection::cursor(pos))
            .extensions(commonmark_extensions(&codec.schema)),
    )
    .unwrap();
    let inserted = run_command(&state, &insert_text("X"))
        .unwrap()
        .unwrap()
        .state()
        .clone();
    let written = codec.write(inserted.doc());
    assert_eq!(html(&written), html("**foo **bXar****"), "{written}");
    let deleted = run_command(&inserted, &delete_by_grapheme(Direction::Backward))
        .unwrap()
        .unwrap()
        .state()
        .clone();
    assert_eq!(html(&codec.write(deleted.doc())), html("**foo **bar****"));
}

#[test]
fn projected_spans_have_one_caret_stop_per_visible_grapheme() {
    let codec = Codec::new();
    let doc = codec.parse("**a **😀e\u{301}**** z");
    let projection = Projection::of(&doc, &codec.schema);
    assert_eq!(projection.plain_text(), "a 😀e\u{301} z");
    let line = projection.line(0).unwrap();
    assert_eq!(line.len(), 7);
    let mut positions = Vec::new();
    let mut pos = line.offset_to_pos(0).unwrap();
    loop {
        assert!(projection.is_caret_position(pos));
        let (_, offset) = projection.pos_to_line_offset(pos).unwrap();
        assert_eq!(projection.line_offset_to_pos(0, offset), Some(pos));
        assert_eq!(
            projection.utf16_to_pos(projection.pos_to_utf16(pos).unwrap()),
            Some(pos)
        );
        positions.push(pos);
        let next = projection.next_grapheme_in_line(pos).unwrap();
        if next == pos {
            break;
        }
        pos = next;
    }
    assert_eq!(positions.len(), 7); // Six clusters, with one final boundary.
    for &pos in positions.iter().skip(1) {
        let previous = projection.prev_grapheme_in_line(pos).unwrap();
        assert_eq!(projection.next_grapheme_in_line(previous), Some(pos));
    }
    let strong = codec.schema.mark_id(md::STRONG).unwrap();
    assert!(
        line.runs
            .iter()
            .take(2)
            .all(|run| run.marks.contains_type(strong))
    );
    assert!(line.runs.last().unwrap().marks.is_empty());
    assert_eq!(
        projection.text_between(line.from, line.to),
        Some("a 😀e\u{301} z")
    );
}

#[test]
fn html_judge_does_not_hide_attribute_comment_or_preformatted_whitespace_loss() {
    use common::normalize_html;
    for (original, corrupted) in [
        ("<a title=\"a  b\">x</a>", "<a title=\"a b\">x</a>"),
        ("<!-- a  b -->", "<!-- a b -->"),
        ("<pre>a  b\n</pre>", "<pre>a b\n</pre>"),
        ("<p>a\u{a0}b</p>", "<p>a b</p>"),
    ] {
        assert_ne!(normalize_html(original), normalize_html(corrupted));
    }
    assert_eq!(normalize_html("<p>a\nb</p>"), normalize_html("<p>a b</p>"));
}

#[test]
fn copying_inside_a_span_keeps_its_ancestor_marks_in_every_rich_flavour() {
    let codec = Codec::new();
    let doc = codec.parse("**foo **bar****");
    let projection = Projection::of(&doc, &codec.schema);
    let from = projection.line_offset_to_pos(0, 4).unwrap();
    let to = projection.line_offset_to_pos(0, 5).unwrap();
    let slice = Selection::text(from, to).content_with_schema(&doc, &codec.schema);
    assert_eq!(
        markraft_commonmark::slice_to_plain_text(&codec.schema, &slice),
        "b"
    );
    let markdown = codec.serializer.serialize_fragment(&slice);
    assert_eq!(html(&markdown), html("<strong>**b**</strong>"));
    let rich = HtmlSerializer::commonmark(&codec.schema).serialize_fragment(&slice);
    assert_eq!(rich, "<p><strong><strong>b</strong></strong></p>");
}

#[test]
fn toggling_a_mark_inside_a_nested_span_affects_only_selected_visible_text() {
    let codec = Codec::new();
    let doc = codec.parse("**foo **bar****");
    let projection = Projection::of(&doc, &codec.schema);
    let from = projection.line_offset_to_pos(0, 4).unwrap();
    let to = projection.line_offset_to_pos(0, 5).unwrap();
    let state = EditorState::create(
        EditorStateConfig::new(codec.schema.clone())
            .doc(doc)
            .selection(Selection::text(from, to)),
    )
    .unwrap();
    let strong = codec.schema.mark_id(md::STRONG).unwrap();
    let changed = run_command(
        &state,
        &markraft_core::commands::toggle_mark(strong, markraft_core::Attrs::empty()),
    )
    .unwrap()
    .unwrap()
    .state()
    .clone();
    let projection = Projection::of(changed.doc(), &codec.schema);
    assert_eq!(projection.plain_text(), "foo bar");
    for run in &projection.line(0).unwrap().runs {
        assert_eq!(
            run.marks.contains_type(strong),
            run.char_from != 4,
            "{run:?}"
        );
    }
    // Nested strong has no perfect CommonMark spelling; never fall back to HTML.
    let written = codec.write(changed.doc());
    assert!(
        !written.contains('<'),
        "expected Markdown delimiters, got {written:?}"
    );
}
