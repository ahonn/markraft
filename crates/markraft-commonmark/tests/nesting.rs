//! Nesting deeper than the tree is built to.
//!
//! Every pass over a document recurses once per level, so the readers stop
//! building containers at a fixed depth and keep what lies below as source
//! (Markdown) or as text (HTML). These run on a test thread's default stack,
//! which is the size of the threads the app parses and saves on.

mod common;

use common::Codec;
use markraft_commonmark::html::HtmlParser;
use markraft_core::Node;

/// How deep the document's containers go.
fn depth(node: &Node) -> usize {
    1 + node.children().map(depth).max().unwrap_or(0)
}

/// `source` read, written, and read and written again: the first save may
/// respell the part kept as source, and from then on the file holds still.
fn saves_to_a_fixed_point(source: &str) -> String {
    let codec = Codec::new();
    let doc = codec.parse(source);
    assert!(depth(&doc) < 150, "the tree is {} deep", depth(&doc));
    let written = codec.write(&doc);
    assert_eq!(
        codec.normalize(&written),
        written,
        "a second save respelled the file"
    );
    written
}

#[test]
fn a_quote_ten_thousand_deep_keeps_its_markers_and_text() {
    let written = saves_to_a_fixed_point(&format!("{} x\n", ">".repeat(10_000)));
    assert_eq!(written.matches('>').count(), 10_000);
    assert!(
        written.ends_with(" x"),
        "{:?}",
        &written[written.len() - 20..]
    );
}

// A list item cannot stand as source among its list's items: a raw block there
// would be wrapped in an item of its own, and each save would add a marker.
#[test]
fn quotes_and_lists_alternating_past_the_depth_hold_their_markers() {
    for (unit, count) in [("> - ", 3_000), ("> 1. ", 100), ("> - [ ] ", 100)] {
        let source = format!("{}x\n", unit.repeat(count));
        let written = saves_to_a_fixed_point(&source);
        let marker = unit.trim_start_matches("> ").trim_end();
        assert_eq!(written.matches(marker).count(), count, "{unit:?}");
    }
}

#[test]
fn a_list_nested_past_the_depth_keeps_every_item() {
    let source: String = (0..150)
        .map(|i| format!("{}- item {i}\n", "  ".repeat(i)))
        .collect();
    let written = saves_to_a_fixed_point(&source);
    for i in 0..150 {
        assert!(
            written.contains(&format!("- item {i}\n")) || written.ends_with(&format!("- item {i}"))
        );
    }
}

#[test]
fn a_table_deep_in_a_quote_keeps_its_cells() {
    let quote = ">".repeat(200);
    let source = format!("{quote} | a | b |\n{quote} |---|---|\n{quote} | 1 | 2 |\n");
    let written = saves_to_a_fixed_point(&source);
    assert!(
        written.ends_with("| 1 | 2 |"),
        "{:?}",
        &written[written.len() - 30..]
    );
}

#[test]
fn html_nested_ten_thousand_deep_keeps_its_text() {
    let codec = Codec::new();
    let parser = HtmlParser::commonmark(codec.schema.clone(), &codec.house);
    for tag in ["blockquote", "div", "span", "b"] {
        let html = format!(
            "{}deep text{}",
            format!("<{tag}>").repeat(10_000),
            format!("</{tag}>").repeat(10_000)
        );
        let doc = parser
            .parse(&html)
            .unwrap_or_else(|error| panic!("<{tag}>: {error}"));
        assert!(
            depth(&doc) < 150,
            "<{tag}>: the tree is {} deep",
            depth(&doc)
        );
        assert!(codec.write(&doc).contains("deep text"), "<{tag}>");
    }
}
