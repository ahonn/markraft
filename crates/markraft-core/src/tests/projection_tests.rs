//! The projection: the line model, the conversions and the boundary helpers.

use std::sync::Arc;

use crate::projection::*;
use crate::selection::Selection;
use crate::state::{Extension, TransactionSpec};

use super::support::*;

/// `doc(heading("Hi"), blockquote(paragraph("ab")), horizontal_rule,
///      paragraph("x", hard_break, "y"))`
fn sample() -> (crate::schema::Schema, crate::node::Node) {
    let schema = shared_schema();
    let document = doc(
        &schema,
        [
            n(&schema, "heading", [t(&schema, "Hi")]),
            n(
                &schema,
                "blockquote",
                [n(&schema, "paragraph", [t(&schema, "ab")])],
            ),
            n(&schema, "horizontal_rule", []),
            n(
                &schema,
                "paragraph",
                [
                    t(&schema, "x"),
                    n(&schema, "hard_break", []),
                    t(&schema, "y"),
                ],
            ),
        ],
    );
    (schema, document)
}

#[test]
fn lines_carry_their_range_ancestors_and_runs() {
    let (schema, document) = sample();
    let projection = Projection::of(&document, &schema);
    assert_eq!(projection.line_count(), 4);

    let heading = projection.line(0).expect("line");
    assert_eq!(heading.kind, LineKind::Textblock);
    assert_eq!((heading.from, heading.to), (1, 3));
    assert_eq!(heading.depth(), 1);
    assert_eq!(heading.node_type(), schema.node_id("heading"));

    let quoted = projection.line(1).expect("line");
    assert_eq!(quoted.depth(), 2);
    assert_eq!(
        quoted.ancestors[0].node_type,
        schema.node_id("blockquote").expect("known")
    );
    assert_eq!(quoted.ancestors[0].index, 1);

    let rule = projection.line(2).expect("line");
    assert_eq!(rule.kind, LineKind::LeafBlock);
    assert!(rule.is_empty());

    let last = projection.line(3).expect("line");
    assert_eq!(last.runs.len(), 3);
    assert!(matches!(last.runs[1].content, RunContent::Atom(_)));
    assert_eq!(last.rows.len(), 2);
    assert_eq!(last.rows[0].char_from, 0);
    assert_eq!(last.rows[0].char_to, 1);
    assert_eq!(last.rows[1].char_from, 2);
}

#[test]
fn plain_text_is_the_lines_joined_by_newlines() {
    let (schema, document) = sample();
    let projection = Projection::of(&document, &schema);
    let joined: Vec<&str> = (0..projection.line_count())
        .map(|index| projection.line_text(index).expect("line text"))
        .collect();
    assert_eq!(projection.plain_text(), joined.join("\n"));
    assert_eq!(projection.plain_text(), "Hi\nab\n\nx\ny");
}

#[test]
fn positions_round_trip_through_line_offsets() {
    let (schema, document) = sample();
    let projection = Projection::of(&document, &schema);
    for pos in 0..=document.content_size() {
        if let Some((line, offset)) = projection.pos_to_line_offset(pos) {
            assert_eq!(projection.line_offset_to_pos(line, offset), Some(pos));
            let byte = projection.pos_to_line_byte(pos).expect("byte offset");
            assert_eq!(projection.line_byte_to_pos(byte.0, byte.1), Some(pos));
        }
    }
}

#[test]
fn utf16_conversions_round_trip() {
    let schema = shared_schema();
    let document = doc(
        &schema,
        [
            n(&schema, "paragraph", [t(&schema, "a😀b")]),
            n(&schema, "paragraph", [t(&schema, "héllo")]),
        ],
    );
    let projection = Projection::of(&document, &schema);
    for pos in 0..=document.content_size() {
        if projection.pos_to_line_offset(pos).is_none() {
            continue;
        }
        let units = projection.pos_to_utf16(pos).expect("utf16 offset");
        assert_eq!(projection.utf16_to_pos(units), Some(pos));
    }
    // A range that spans two lines works too: both of its ends sit in a line.
    let (from, to) = projection
        .pos_range_to_utf16_range(1, 8)
        .expect("utf16 range");
    assert_eq!(projection.utf16_range_to_pos_range(from, to), Some((1, 8)));
}

#[test]
fn grapheme_boundaries_never_split_a_cluster() {
    let schema = shared_schema();
    let cases = [
        "e\u{0301}",                                   // combining acute
        "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}", // ZWJ family
        "\u{1F1EF}\u{1F1F5}",                          // regional indicators
        "\u{1100}\u{1161}\u{11A8}",                    // Hangul jamo
    ];
    for case in cases {
        let document = doc(&schema, [n(&schema, "paragraph", [t(&schema, case)])]);
        let projection = Projection::of(&document, &schema);
        let line = projection.line(0).expect("line");
        // One cluster, so the only boundaries are the two ends.
        assert!(projection.is_grapheme_boundary(line.from));
        assert!(projection.is_grapheme_boundary(line.to));
        for pos in line.from + 1..line.to {
            assert!(
                !projection.is_grapheme_boundary(pos),
                "{case:?} at {pos} should be inside a cluster"
            );
            assert_eq!(projection.floor_grapheme(pos), line.from);
            assert_eq!(projection.ceil_grapheme(pos), line.to);
        }
        assert_eq!(projection.next_grapheme_boundary(line.from), Some(line.to));
        assert_eq!(projection.prev_grapheme_boundary(line.to), Some(line.from));
    }
}

#[test]
fn grapheme_motion_crosses_lines_and_stops_at_the_ends() {
    let schema = shared_schema();
    let document = doc(
        &schema,
        [
            n(&schema, "paragraph", [t(&schema, "ab")]),
            n(&schema, "paragraph", [t(&schema, "cd")]),
        ],
    );
    let projection = Projection::of(&document, &schema);
    assert_eq!(projection.next_grapheme_boundary(3), Some(5));
    assert_eq!(projection.prev_grapheme_boundary(5), Some(3));
    assert_eq!(projection.next_grapheme_boundary(7), None);
    assert_eq!(projection.prev_grapheme_boundary(1), None);
}

#[test]
fn word_boundaries_skip_whitespace() {
    let schema = shared_schema();
    let document = doc(
        &schema,
        [n(&schema, "paragraph", [t(&schema, "one  two three")])],
    );
    let projection = Projection::of(&document, &schema);
    assert_eq!(projection.next_word_boundary(1), Some(4));
    assert_eq!(projection.next_word_boundary(4), Some(9));
    assert_eq!(projection.prev_word_boundary(9), Some(6));
    assert_eq!(projection.prev_word_boundary(6), Some(1));
}

#[test]
fn grapheme_motion_in_a_line_stops_at_its_own_ends() {
    let schema = shared_schema();
    let document = doc(
        &schema,
        [
            n(&schema, "paragraph", [t(&schema, "ae\u{0301}b")]),
            n(&schema, "paragraph", []),
        ],
    );
    let projection = Projection::of(&document, &schema);
    // "a", "é" as two scalars, "b": the line runs 1..5.
    assert_eq!(projection.next_grapheme_in_line(1), Some(2));
    assert_eq!(projection.next_grapheme_in_line(2), Some(4));
    assert_eq!(projection.prev_grapheme_in_line(4), Some(2));
    // A position inside the cluster still lands on a boundary.
    assert_eq!(projection.next_grapheme_in_line(3), Some(4));
    assert_eq!(projection.prev_grapheme_in_line(3), Some(2));
    // Both ends are fixed points rather than a crossing into the next line.
    assert_eq!(projection.next_grapheme_in_line(5), Some(5));
    assert_eq!(projection.prev_grapheme_in_line(1), Some(1));
    assert_eq!(projection.next_grapheme_boundary(5), Some(7));
    // An empty line is its own fixed point in both directions.
    assert_eq!(projection.next_grapheme_in_line(7), Some(7));
    assert_eq!(projection.prev_grapheme_in_line(7), Some(7));
    // A position between two blocks belongs to no line.
    assert_eq!(projection.next_grapheme_in_line(6), None);
    assert_eq!(projection.prev_grapheme_in_line(6), None);
}

#[test]
fn a_lines_graphemes_are_walkable_from_either_end() {
    let schema = shared_schema();
    let document = doc(
        &schema,
        [
            n(&schema, "paragraph", [t(&schema, "a\u{1F1EF}\u{1F1F5}b")]),
            n(&schema, "horizontal_rule", []),
        ],
    );
    let projection = Projection::of(&document, &schema);
    let forwards: Vec<_> = projection.graphemes(0).collect();
    assert_eq!(
        forwards,
        vec![(1, "a"), (2, "\u{1F1EF}\u{1F1F5}"), (4, "b")]
    );
    let mut backwards: Vec<_> = projection.graphemes(0).rev().collect();
    backwards.reverse();
    assert_eq!(backwards, forwards);
    assert_eq!(projection.grapheme_at(2), Some("\u{1F1EF}\u{1F1F5}"));
    // Inside the cluster, past the line's end, and on a leaf block: nothing.
    assert_eq!(projection.grapheme_at(3), None);
    assert_eq!(projection.grapheme_at(5), None);
    assert_eq!(projection.graphemes(1).count(), 0);
}

#[test]
fn text_and_words_of_a_range_stay_inside_one_line() {
    let schema = shared_schema();
    let document = doc(
        &schema,
        [
            n(&schema, "paragraph", [t(&schema, "one  two three")]),
            n(&schema, "paragraph", [t(&schema, " ")]),
        ],
    );
    let projection = Projection::of(&document, &schema);
    let line = projection.line(0).expect("a line");
    assert_eq!(projection.text_between(1, 4), Some("one"));
    assert_eq!(
        projection.text_between(line.from, line.to),
        Some("one  two three")
    );
    // Inverted, past the line's end, and across a block boundary: nothing.
    assert_eq!(projection.text_between(4, 1), None);
    assert_eq!(projection.text_between(1, 16), None);
    assert_eq!(projection.text_between(16, 17), None);
    assert_eq!(
        projection.word_ranges(line.from, line.to),
        vec![1..4, 6..9, 10..15]
    );
    // The range is segmented on its own, so a bound inside a word cuts it.
    assert_eq!(projection.word_ranges(line.from, 2), vec![1..2]);
    assert_eq!(projection.word_ranges(2, line.to), vec![2..4, 6..9, 10..15]);
    // A line of whitespace holds no word, and so does a rejected range.
    assert!(projection.word_ranges(17, 18).is_empty());
    assert!(projection.word_ranges(4, 1).is_empty());
}

#[test]
fn slice_text_matches_the_projection() {
    let (schema, document) = sample();
    let projection = Projection::of(&document, &schema);
    let whole = document
        .slice(0, document.content_size())
        .expect("whole document");
    assert_eq!(
        slice_to_plain_text(&schema, &whole),
        projection.plain_text()
    );
}

#[test]
fn the_projection_field_is_kept_while_the_document_stands_still() {
    let schema = shared_schema();
    let start = state(
        doc(&schema, [n(&schema, "paragraph", [t(&schema, "abc")])]),
        projection(),
    );
    let first = start.field(projection_field()).expect("configured").clone();
    let moved = start
        .update([TransactionSpec::new().selection(Selection::cursor(2))])
        .expect("resolves")
        .state()
        .clone();
    let second = moved.field(projection_field()).expect("configured");
    assert!(Arc::ptr_eq(&first, second));

    let typed = moved
        .update([TransactionSpec::new().changes([super::support::insert_text(&schema, 2, "X")])])
        .expect("resolves")
        .state()
        .clone();
    let third = typed.field(projection_field()).expect("configured");
    assert!(!Arc::ptr_eq(&first, third));
    assert_eq!(third.plain_text(), "aXbc");
}

#[test]
fn a_state_without_the_extension_still_gets_a_projection() {
    let schema = shared_schema();
    let start = state(
        doc(&schema, [n(&schema, "paragraph", [t(&schema, "abc")])]),
        Extension::none(),
    );
    assert!(cached_projection(&start).is_none());
    assert_eq!(projection_of(&start).plain_text(), "abc");
}

#[test]
fn an_inline_atom_occupies_one_position_and_one_char() {
    let schema = shared_schema();
    let document = doc(
        &schema,
        [n(
            &schema,
            "paragraph",
            [t(&schema, "a"), img(&schema, "pic.png"), t(&schema, "b")],
        )],
    );
    let projection = Projection::of(&document, &schema);
    assert_eq!(projection.plain_text(), "a\u{fffc}b");
    assert_eq!(projection.pos_to_line_offset(3), Some((0, 2)));
    assert!(projection.is_grapheme_boundary(2));
    assert!(projection.is_grapheme_boundary(3));
}
