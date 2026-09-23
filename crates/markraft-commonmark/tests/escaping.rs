//! Escaping: what keeps text reading as itself.
//!
//! A textblock's text is its source, so escapes are characters of it: the
//! guard's backslashes keep a line from opening a block, and a backslash or an
//! entity the author wrote stays where it was, concealed. Content that has no
//! spelling of its own — pasted HTML — is escaped when it is spelled, and only
//! where a reader would see syntax.

mod common;

use common::Codec;
use markraft_commonmark::schema as md;
use markraft_commonmark::serialize::spell_document;
use markraft_core::Node;

/// `parse` then `serialize`, which is the normalisation the codec promises.
fn round(source: &str) -> String {
    Codec::new().normalize(source)
}

fn shape(source: &str) -> String {
    let codec = Codec::new();
    codec.describe(&codec.parse(source))
}

fn paragraph(codec: &Codec, text: &str) -> Node {
    let schema = &codec.schema;
    schema
        .doc([schema
            .node(md::PARAGRAPH, [schema.text(text)])
            .expect("a paragraph")])
        .expect("a document")
}

#[test]
fn spelled_text_is_escaped_only_where_a_reader_would_see_syntax() {
    for text in [
        "R&D costs 5.0 (approx.)",
        "a-b",
        "C#",
        "a < b",
        "x|y and {z}",
        "5.0 and 1.5",
        "snake_case_name",
        "a == b and a = b",
        "costs $ 5",
        "a ===b=== c",
    ] {
        let codec = Codec::new();
        let doc = spell_document(&codec.serializer, &paragraph(&codec, text));
        assert_eq!(codec.write(&doc), text, "{text:?} needs no escaping");
        assert_eq!(codec.parse(text), doc);
    }
}

#[test]
fn a_block_marker_is_guarded_only_at_the_start_of_a_line() {
    let cases = [
        ("# heading", "\\# heading"),
        ("> quote", "\\> quote"),
        ("- item", "\\- item"),
        ("+ item", "\\+ item"),
        ("1. item", "1\\. item"),
        ("1) item", "1\\) item"),
        ("---", "\\---"),
        ("a # b", "a # b"),
    ];
    let codec = Codec::new();
    for (text, expected) in cases {
        // Text that reached the tree without its guard is written with it…
        assert_eq!(codec.write(&paragraph(&codec, text)), expected, "{text:?}");
        // …and the backslash is a character of the text it reads back as.
        assert_eq!(round(expected), expected, "{text:?} does not come back");
    }
    assert_eq!(
        shape("\\# heading"),
        r##"doc(paragraph("\"{syntax}, "# heading"))"##
    );
}

#[test]
fn a_character_reference_is_text_that_displays_what_it_names() {
    assert_eq!(
        shape("a &amp; b"),
        r#"doc(paragraph("a ", "&amp;"{syntax}, " b"))"#
    );
    assert_eq!(round("a &amp; b"), "a &amp; b");
    assert_eq!(round("a \\&amp; b"), "a \\&amp; b");
    assert_eq!(
        shape("a \\&amp; b"),
        r#"doc(paragraph("a ", "\"{syntax}, "&amp; b"))"#
    );
    assert_eq!(round("R&D"), "R&D");
}

#[test]
fn indentation_spelled_as_a_reference_stays_a_reference() {
    assert_eq!(round("&#32;   four"), "&#32;   four");
    assert_eq!(
        shape("&#32;   four"),
        r#"doc(paragraph("&#32;"{syntax}, "   four"))"#
    );
    // Indentation itself is no part of the text: a reader strips it.
    assert_eq!(shape("  two"), r#"doc(paragraph("two"))"#);
}

#[test]
fn spelled_text_shaped_like_a_tag_is_protected() {
    // A tag in the source is an atom, so text that only looks like one has to
    // be spelled so it comes back as the text it is.
    let codec = Codec::new();
    for text in [
        "a<br>b",
        "<img src=\"x.png\" alt=\"img\">",
        "<a href=\"/u\">anchor</a>",
    ] {
        let doc = spell_document(&codec.serializer, &paragraph(&codec, text));
        let written = codec.write(&doc);
        assert!(written.contains("\\<"), "{written:?} keeps a bare tag");
        assert_eq!(codec.parse(&written), doc, "{text:?} does not come back");
        assert!(!codec.describe(&doc).contains("raw_inline"), "{text:?}");
    }
}

#[test]
fn a_line_ending_spelled_as_a_reference_stays_one() {
    assert_eq!(
        shape("a&#10;b"),
        r#"doc(paragraph("a", "&#10;"{syntax}, "b"))"#
    );
    assert_eq!(round("a&#10;b"), "a&#10;b");
}

#[test]
fn spelled_text_keeps_highlight_superscript_and_math_delimiters_literal() {
    let cases = [
        ("2^10", "2\\^10"),
        ("x==y==z", "x\\==y\\==z"),
        ("==", "\\=\\="),
        ("costs $5 and $10", "costs \\$5 and \\$10"),
        ("$x$", "\\$x\\$"),
        ("$$ a", "\\$$ a"),
    ];
    let codec = Codec::new();
    for (text, expected) in cases {
        let doc = spell_document(&codec.serializer, &paragraph(&codec, text));
        let written = codec.write(&doc);
        assert_eq!(written, expected, "{text:?}");
        assert_eq!(codec.parse(&written), doc, "{text:?} does not come back");
        assert!(!codec.describe(&doc).contains("highlight"), "{text:?}");
        assert!(!codec.describe(&doc).contains("math"), "{text:?}");
    }
}
