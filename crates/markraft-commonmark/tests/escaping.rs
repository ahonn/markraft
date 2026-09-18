//! Escaping: what the serialiser protects, and what it leaves alone.

mod common;

use common::Codec;
use markraft_commonmark::schema as md;

/// `parse` then `serialize`, which is the normalisation the codec promises.
fn round(source: &str) -> String {
    Codec::new().normalize(source)
}

fn shape(source: &str) -> String {
    let codec = Codec::new();
    codec.describe(&codec.parse(source))
}

#[test]
fn text_is_escaped_only_where_a_reader_would_see_syntax() {
    for text in [
        "R&D costs 5.0 (approx.)",
        "a-b",
        "C#",
        "a < b",
        "x|y and {z}",
        "5.0 and 1.5",
        "snake_case_name",
    ] {
        let codec = Codec::new();
        let schema = &codec.schema;
        let doc = schema
            .doc([schema
                .node(md::PARAGRAPH, [schema.text(text)])
                .expect("a paragraph")])
            .expect("a document");
        assert_eq!(codec.write(&doc), text, "{text:?} needs no escaping");
        assert_eq!(codec.parse(text), doc);
    }
}

#[test]
fn a_block_marker_is_escaped_only_at_the_start_of_a_line() {
    let cases = [
        ("# heading", "\\# heading"),
        ("> quote", "\\> quote"),
        ("- item", "\\- item"),
        ("+ item", "\\+ item"),
        ("1. item", "1\\. item"),
        ("1) item", "1\\) item"),
        ("---", "\\---"),
    ];
    let codec = Codec::new();
    let schema = &codec.schema;
    for (text, expected) in cases {
        let doc = schema
            .doc([schema
                .node(md::PARAGRAPH, [schema.text(text)])
                .expect("a paragraph")])
            .expect("a document");
        assert_eq!(codec.write(&doc), expected, "{text:?}");
        assert_eq!(codec.parse(expected), doc, "{text:?} does not come back");
    }
}

#[test]
fn a_character_reference_is_escaped_but_a_bare_ampersand_is_not() {
    assert_eq!(shape("a &amp; b"), r#"doc(paragraph("a & b"))"#);
    assert_eq!(round("a \\&amp; b"), "a \\&amp; b");
    assert_eq!(shape("a \\&amp; b"), r#"doc(paragraph("a &amp; b"))"#);
    assert_eq!(round("R&D"), "R&D");
}

#[test]
fn four_columns_of_indentation_travel_as_a_character_reference() {
    assert_eq!(round("&#32;   four"), "&#32;   four");
    assert_eq!(shape("&#32;   four"), r#"doc(paragraph("    four"))"#);
    // Less indentation changes no structure, so it is left alone.
    assert_eq!(shape("  two"), r#"doc(paragraph("two"))"#);
}

#[test]
fn a_line_ending_inside_inline_content_travels_as_a_reference() {
    assert_eq!(shape("a&#10;b"), "doc(paragraph(\"a\nb\"))");
    assert_eq!(round("a&#10;b"), "a&#10;b");
}
