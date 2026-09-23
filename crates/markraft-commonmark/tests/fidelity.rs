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
        "<p><em>a <strong>b</strong> c</em></p>"
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
fn nested_html_tags_are_written_as_they_were_read() {
    let codec = Codec::new();
    for tag in ["u", "em", "strong", "del"] {
        let source = format!("<{tag}>a <{tag}>b</{tag}> c</{tag}>");
        assert_eq!(codec.normalize(&source), source);
        assert_eq!(html(&codec.normalize(&source)), html(&source));
    }
    // Nested `<u>` pairs are nested underline spans.
    let underline = codec.schema.mark_id(md::UNDERLINE).unwrap();
    let doc = codec.parse("<u>a <u>b</u> c</u>");
    let paragraph = doc.child(0);
    assert!(
        paragraph
            .children()
            .filter(|child| child.text().is_some_and(|text| !text.starts_with('<')))
            .all(|child| child.marks().contains_type(underline))
    );
}

#[test]
fn html_clipboard_keeps_nested_spans_empty_links_and_raw_primitives() {
    let codec = Codec::new();
    let writer = HtmlSerializer::commonmark(&codec.schema);
    let reader = HtmlParser::commonmark(codec.schema.clone());
    for source in [
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
fn editing_nested_emphasis_keeps_delimiters() {
    let codec = Codec::new();
    let doc = codec.parse("*a **b** c*");
    let projection = Projection::of(&doc, &codec.schema);
    let plain = projection.plain_text();
    let b_at = plain.find('b').expect("b");
    let pos = projection.line_offset_to_pos(0, b_at + 1).unwrap();
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
    assert_eq!(html(&written), html("*a **bX** c*"), "{written}");
    let deleted = run_command(&inserted, &delete_by_grapheme(Direction::Backward))
        .unwrap()
        .unwrap()
        .state()
        .clone();
    assert_eq!(html(&codec.write(deleted.doc())), html("*a **b** c*"));
}

#[test]
fn projected_delimiters_keep_their_caret_stops() {
    let codec = Codec::new();
    let doc = codec.parse("**a 😀e\u{301}** z");
    let projection = Projection::of(&doc, &codec.schema);
    assert_eq!(projection.plain_text(), "**a 😀e\u{301}** z");
    assert_eq!(
        markraft_commonmark::to_plain_text(&codec.schema, &doc),
        "a 😀e\u{301} z"
    );
    let line = projection.line(0).unwrap();
    let mut positions = Vec::new();
    let mut pos = line.offset_to_pos(0).unwrap();
    loop {
        assert!(projection.is_caret_position(pos));
        positions.push(pos);
        let next = projection.next_grapheme_in_line(pos).unwrap();
        if next == pos {
            break;
        }
        pos = next;
    }
    assert!(positions.len() > 7, "delimiters add caret stops");
    let strong = codec.schema.mark_id(md::STRONG).unwrap();
    assert!(line.runs.iter().any(|run| {
        run.marks.contains_type(strong)
            && matches!(run.content, markraft_core::projection::RunContent::Text(_))
    }));
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

/// A copy taken inside a span is its source, which is `b` alone: the Markdown
/// flavour is the text verbatim. The rich flavour carries the styles over it.
#[test]
fn copying_inside_a_span_keeps_style_in_the_rich_flavour() {
    let codec = Codec::new();
    let doc = codec.parse("*a **b** c*");
    let projection = Projection::of(&doc, &codec.schema);
    let plain = projection.plain_text();
    let b_at = plain.find('b').expect("b");
    let from = projection.line_offset_to_pos(0, b_at).unwrap();
    let to = projection.line_offset_to_pos(0, b_at + 1).unwrap();
    let slice = Selection::text(from, to).content_with_schema(&doc, &codec.schema);
    assert_eq!(
        markraft_commonmark::slice_to_plain_text(&codec.schema, &slice),
        "b"
    );
    let markdown = codec.serializer.serialize_fragment(&slice);
    assert_eq!(markdown, "b");
    let rich = HtmlSerializer::commonmark(&codec.schema).serialize_fragment(&slice);
    assert!(
        rich.contains("<strong>") && rich.contains("<em>") && rich.contains('b'),
        "{rich}"
    );
}

#[test]
fn toggling_a_style_off_deletes_its_delimiters() {
    let codec = Codec::new();
    let doc = codec.parse("**hello**");
    let projection = Projection::of(&doc, &codec.schema);
    let plain = projection.plain_text();
    let start = plain.find("hello").expect("hello");
    let from = projection.line_offset_to_pos(0, start).unwrap();
    let to = projection.line_offset_to_pos(0, start + 5).unwrap();
    let state = EditorState::create(
        EditorStateConfig::new(codec.schema.clone())
            .doc(doc)
            .selection(Selection::text(from, to))
            .extensions(commonmark_extensions(&codec.schema)),
    )
    .unwrap();
    let strong = codec.schema.mark_id(md::STRONG).unwrap();
    let changed = run_command(
        &state,
        &markraft_commonmark::toggle_style_mark(strong, markraft_core::Attrs::empty()),
    )
    .unwrap()
    .unwrap()
    .state()
    .clone();
    assert_eq!(
        markraft_commonmark::to_plain_text(&codec.schema, changed.doc()),
        "hello"
    );
    let written = codec.write(changed.doc());
    assert!(
        !written.contains('<'),
        "expected Markdown delimiters, got {written:?}"
    );
    assert_eq!(html(&written), html("hello"));
}
