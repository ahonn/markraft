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
    assert_eq!(heading.kind(), LineKind::Textblock);
    assert_eq!((heading.from(), heading.to()), (1, 3));
    assert_eq!(heading.depth(), 1);
    assert_eq!(heading.node_type(), schema.node_id("heading"));

    let quoted = projection.line(1).expect("line");
    assert_eq!(quoted.depth(), 2);
    assert_eq!(
        quoted.ancestors()[0].node_type,
        schema.node_id("blockquote").expect("known")
    );
    assert_eq!(quoted.ancestors()[0].index, 1);

    let rule = projection.line(2).expect("line");
    assert_eq!(rule.kind(), LineKind::LeafBlock);
    assert!(rule.is_empty());

    let last = projection.line(3).expect("line");
    assert_eq!(last.runs().len(), 3);
    assert!(matches!(last.runs()[1].content, RunContent::Atom(_)));
    assert_eq!(last.rows().len(), 2);
    assert_eq!(last.rows()[0].char_from, 0);
    assert_eq!(last.rows()[0].char_to, 1);
    assert_eq!(last.rows()[1].char_from, 2);
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
        assert!(projection.is_grapheme_boundary(line.from()));
        assert!(projection.is_grapheme_boundary(line.to()));
        for pos in line.from() + 1..line.to() {
            assert!(
                !projection.is_grapheme_boundary(pos),
                "{case:?} at {pos} should be inside a cluster"
            );
            assert_eq!(projection.floor_grapheme(pos), line.from());
            assert_eq!(projection.ceil_grapheme(pos), line.to());
        }
        assert_eq!(
            projection.next_grapheme_boundary(line.from()),
            Some(line.to())
        );
        assert_eq!(
            projection.prev_grapheme_boundary(line.to()),
            Some(line.from())
        );
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
        projection.text_between(line.from(), line.to()),
        Some("one  two three")
    );
    // Inverted, past the line's end, and across a block boundary: nothing.
    assert_eq!(projection.text_between(4, 1), None);
    assert_eq!(projection.text_between(1, 16), None);
    assert_eq!(projection.text_between(16, 17), None);
    assert_eq!(
        projection.word_ranges(line.from(), line.to()),
        vec![1..4, 6..9, 10..15]
    );
    // The range is segmented on its own, so a bound inside a word cuts it.
    assert_eq!(projection.word_ranges(line.from(), 2), vec![1..2]);
    assert_eq!(
        projection.word_ranges(2, line.to()),
        vec![2..4, 6..9, 10..15]
    );
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

#[test]
fn inline_container_boundaries_do_not_add_text_or_caret_stops() {
    let schema = shared_schema();
    let document = doc(
        &schema,
        [
            n(
                &schema,
                "paragraph",
                [n(
                    &schema,
                    "inline_span",
                    [
                        t(&schema, "a😀"),
                        n(&schema, "inline_span", [t(&schema, "b")]),
                    ],
                )],
            ),
            n(
                &schema,
                "paragraph",
                [n(&schema, "inline_span", [t(&schema, "next")])],
            ),
        ],
    );
    let projection = Projection::of(&document, &schema);
    assert_eq!(projection.plain_text(), "a😀b\nnext");
    let first = projection.line(0).unwrap();
    assert_eq!(first.len(), 3);
    assert_eq!(
        projection.text_between(first.from(), first.to()),
        Some("a😀b")
    );
    for offset in 0..=first.len() {
        let pos = first.offset_to_pos(offset).unwrap();
        assert!(projection.is_caret_position(pos));
        assert_eq!(first.pos_to_offset(pos), Some(offset));
        assert_eq!(
            projection.utf16_to_pos(projection.pos_to_utf16(pos).unwrap()),
            Some(pos)
        );
    }
    // A surrogate half rounds down to the start of its scalar value.
    assert_eq!(projection.pos_from_utf16(0, 2), first.offset_to_pos(1));
    let end = first.offset_to_pos(first.len()).unwrap();
    let next = projection.line(1).unwrap().offset_to_pos(0).unwrap();
    assert_eq!(projection.next_grapheme_boundary(end), Some(next));
    assert_eq!(projection.next_word_boundary(end), Some(next));
    assert_eq!(projection.prev_word_boundary(next), Some(end));
    assert_eq!(
        projection
            .graphemes(0)
            .map(|(_, text)| text)
            .collect::<Vec<_>>(),
        ["a", "😀", "b"]
    );
    assert_eq!(
        projection
            .graphemes(0)
            .rev()
            .map(|(_, text)| text)
            .collect::<Vec<_>>(),
        ["b", "😀", "a"]
    );
    let words = projection.word_ranges(first.from(), first.to());
    assert_eq!(
        projection.text_between(words[0].start, words[0].end),
        Some("a")
    );
    assert_eq!(
        slice_to_plain_text(
            &schema,
            &document.slice(0, document.content_size()).unwrap()
        ),
        "a😀b\nnext"
    );
}

#[test]
fn an_empty_inline_container_has_one_editable_position() {
    let schema = shared_schema();
    let document = doc(
        &schema,
        [n(&schema, "paragraph", [n(&schema, "inline_span", [])])],
    );
    let projection = Projection::of(&document, &schema);
    let line = projection.line(0).unwrap();
    assert_eq!(projection.plain_text(), "");
    assert!(line.is_empty());
    assert_eq!(line.offset_to_pos(0), Some(2));
    assert!(projection.is_caret_position(2));
    assert!(!projection.is_caret_position(1));
    assert!(!projection.is_caret_position(3));
}

/// A schema with one break type of each kind, so the two can be told apart.
fn break_schema() -> crate::schema::Schema {
    use crate::schema::{BreakKind, NodeTypeSpec, Schema, SchemaSpec};
    Schema::new(
        SchemaSpec::new()
            .node(NodeTypeSpec::new("doc", "block+"))
            .node(NodeTypeSpec::new("paragraph", "inline*").group("block"))
            .node(NodeTypeSpec::text("text").group("inline"))
            .node(
                NodeTypeSpec::leaf("hard")
                    .inline(true)
                    .group("inline")
                    .break_kind(BreakKind::Hard),
            )
            .node(
                NodeTypeSpec::leaf("soft")
                    .inline(true)
                    .group("inline")
                    .break_kind(BreakKind::Soft),
            )
            .node(NodeTypeSpec::leaf("atom").inline(true).group("inline")),
    )
    .expect("the break schema is valid")
}

#[test]
fn a_soft_break_reads_as_a_space_and_a_hard_one_as_a_newline() {
    let schema = break_schema();
    let document = doc(
        &schema,
        [n(
            &schema,
            "paragraph",
            [
                t(&schema, "a"),
                n(&schema, "soft", []),
                t(&schema, "b"),
                n(&schema, "hard", []),
                t(&schema, "c"),
                n(&schema, "atom", []),
            ],
        )],
    );
    let projection = Projection::of(&document, &schema);
    assert_eq!(projection.plain_text(), "a b\nc\u{fffc}");
    // Only the hard break ends a row; the soft one stays inside the first.
    let line = projection.line(0).expect("line");
    assert_eq!(line.rows().len(), 2);
    assert_eq!((line.rows()[0].char_from, line.rows()[0].char_to), (0, 3));
    assert_eq!((line.rows()[1].char_from, line.rows()[1].char_to), (4, 6));

    let whole = document
        .slice(0, document.content_size())
        .expect("whole document");
    assert_eq!(
        slice_to_plain_text(&schema, &whole),
        projection.plain_text()
    );
}

/// Assert `updated` equals `fresh` field by field, so a mismatch names the
/// first line and field that differ rather than dumping both projections.
fn assert_same_projection(updated: &Projection, fresh: &Projection, context: &str) {
    assert_eq!(
        updated.plain_text(),
        fresh.plain_text(),
        "{context}: plain text"
    );
    assert_eq!(
        updated.utf16_len(),
        fresh.utf16_len(),
        "{context}: utf16 length"
    );
    assert_eq!(
        updated.doc_size(),
        fresh.doc_size(),
        "{context}: document size"
    );
    assert_eq!(
        updated.line_count(),
        fresh.line_count(),
        "{context}: line count"
    );
    for (index, (a, b)) in updated.lines().iter().zip(fresh.lines()).enumerate() {
        let at = format!("{context}: line {index}");
        assert_eq!(a.start(), b.start(), "{at}: start");
        assert_eq!((a.from(), a.to()), (b.from(), b.to()), "{at}: range");
        assert_eq!(a.kind(), b.kind(), "{at}: kind");
        assert_eq!(a.ancestors(), b.ancestors(), "{at}: ancestors");
        assert_eq!(a.runs(), b.runs(), "{at}: runs");
        assert_eq!(a.rows(), b.rows(), "{at}: rows");
        assert_eq!(a.byte_range(), b.byte_range(), "{at}: byte range");
        assert_eq!(a.len(), b.len(), "{at}: length");
        for offset in 0..=a.len() + 1 {
            assert_eq!(
                a.offset_to_pos(offset),
                b.offset_to_pos(offset),
                "{at}: position of offset {offset}"
            );
        }
        for i in 0..a.depth() {
            assert_eq!(
                a.ancestor_before(i),
                b.ancestor_before(i),
                "{at}: ancestor {i}"
            );
        }
    }
    assert!(updated == fresh, "{context}: projections differ");
}

/// The kinds of edit the update property draws from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditKind {
    Structural,
    Text,
    BlockDeletion,
    RootReplacement,
    /// Splitting or joining top-level textblocks, or inserting a block
    /// between two top-level blocks: the region is several children of the
    /// document, or none.
    Siblings,
    Emptying,
}

const EDIT_KINDS: [EditKind; 6] = [
    EditKind::Structural,
    EditKind::Text,
    EditKind::BlockDeletion,
    EditKind::RootReplacement,
    EditKind::Siblings,
    EditKind::Emptying,
];

/// Split a top-level textblock (at its start, middle or end), join two
/// adjacent top-level textblocks, or insert a block
/// between two top-level blocks. A join falls back to a split, which sets up a
/// later join, and a split to an insertion, when the document offers no place
/// for them.
fn random_sibling_edit(
    schema: &crate::schema::Schema,
    rng: &mut Rng,
    d: &crate::node::Node,
) -> Vec<crate::change::Change> {
    use crate::change::Change;
    use crate::fragment::Fragment;
    use crate::slice::{Slice, Token};
    let mut befores = vec![0];
    for child in d.children() {
        befores.push(befores.last().expect("seeded") + child.node_size());
    }
    let textblocks: Vec<usize> = (0..d.child_count())
        .filter(|&index| d.child(index).is_textblock(schema))
        .collect();
    let joinable: Vec<usize> = (1..d.child_count())
        .filter(|&index| {
            d.child(index).is_textblock(schema) && d.child(index - 1).is_textblock(schema)
        })
        .collect();
    // Joins need a pair that splits set up, so they are drawn more often.
    let op = rng.below(4);
    if op >= 2 && !joinable.is_empty() {
        let index = *rng.pick(&joinable);
        let boundary = befores[index];
        let join = Change::delete(boundary - 1, boundary + 1);
        // Joining blocks of different markup leaves the second block's close
        // token behind, which the fit repairs as Backspace would.
        return vec![match d.child(index).same_markup(d.child(index - 1)) {
            true => join,
            false => join.with_fit(crate::fit::Fit::Auto),
        }];
    }
    match op {
        0 | 2 | 3 if !textblocks.is_empty() => {
            let index = *rng.pick(&textblocks);
            let start = befores[index] + 1;
            let at = rng.range(start, start + d.child(index).content_size());
            let markup = d.child(index).markup().clone();
            vec![Change::insert(
                at,
                Slice::from_tokens(&[Token::Close(markup.clone()), Token::Open(markup)]),
            )]
        }
        _ => {
            let at = befores[rng.below(befores.len())];
            let block = random_block(schema, rng, 1);
            vec![Change::insert(
                at,
                Slice::from_fragment(Fragment::from_node(block)),
            )]
        }
    }
}

/// Assert `updated`, derived from `old` by an edit from `old_doc` to
/// `new_doc`, kept the body of every line in a top-level block the edit left
/// alone: every one before the first changed top-level block, and, when the
/// edit kept the number of top-level blocks, every one after the last.
fn assert_untouched_top_level_kept(
    old: &Projection,
    updated: &Projection,
    old_doc: &crate::node::Node,
    new_doc: &crate::node::Node,
    context: &str,
) {
    let (old_range, new_range) =
        crate::node::unshared_middles(old_doc.content().as_slice(), new_doc.content().as_slice());
    let top = |line: &Line| line.ancestors()[0].index;
    for (index, line) in old.lines().iter().enumerate() {
        if top(line) < old_range.start {
            assert!(
                line.same_body(&updated.lines()[index]),
                "{context}: line {index}, before the region, lost its body"
            );
        }
    }
    if old_range.len() == new_range.len() {
        let kept_after = old
            .lines()
            .iter()
            .rev()
            .take_while(|line| top(line) >= old_range.end)
            .count();
        for back in 1..=kept_after {
            let (old_line, new_line) = (
                &old.lines()[old.line_count() - back],
                &updated.lines()[updated.line_count() - back],
            );
            assert!(
                old_line.same_body(new_line),
                "{context}: line {back} from the end, after the region, lost its body"
            );
        }
    }
}

/// Every block container with at least two children, as the position its
/// content starts at and the sizes of its children.
fn multi_child_containers(
    schema: &crate::schema::Schema,
    d: &crate::node::Node,
) -> Vec<(usize, Vec<usize>)> {
    let sizes = |node: &crate::node::Node| {
        node.children()
            .map(|child| child.node_size())
            .collect::<Vec<_>>()
    };
    let mut out = Vec::new();
    if d.child_count() >= 2 {
        out.push((0, sizes(d)));
    }
    d.descendants(&mut |node, pos, _, _| {
        if node.is_block(schema) && !node.is_textblock(schema) && node.child_count() >= 2 {
            out.push((pos + 1, sizes(node)));
        }
        true
    });
    out
}

/// The changes for one random edit of `kind`, or `None` when `d` offers no
/// place for it.
fn random_edit(
    schema: &crate::schema::Schema,
    rng: &mut Rng,
    d: &crate::node::Node,
    kind: EditKind,
) -> Option<Vec<crate::change::Change>> {
    use crate::change::Change;
    use crate::fit::Fit;
    use crate::fragment::Fragment;
    use crate::slice::Slice;
    match kind {
        EditKind::Structural => random_structural_change(schema, rng, d),
        EditKind::Text => {
            // A document of dividers alone has no text to edit.
            let spots = textblock_positions(schema, d);
            if spots.is_empty() {
                return None;
            }
            let (start, at) = *rng.pick(&spots);
            let size = d.resolve(at).ok()?.parent().content_size();
            let end = rng.range(at, start + size);
            Some(vec![if rng.one_in(2) {
                Change::delete(at, end)
            } else {
                Change::replace(
                    at,
                    end,
                    Slice::from_fragment(Fragment::from_node(t(
                        schema,
                        rng.pick::<&str>(&["q", "é😀"]),
                    ))),
                )
            }])
        }
        EditKind::BlockDeletion => {
            let containers = multi_child_containers(schema, d);
            if containers.is_empty() {
                return None;
            }
            let (start, sizes) = rng.pick(&containers).clone();
            // Keep at least one child so the container stays valid.
            let first = rng.below(sizes.len());
            let last = rng.range(first, (first + sizes.len() - 2).min(sizes.len() - 1));
            let from = start + sizes[..first].iter().sum::<usize>();
            let to = start + sizes[..=last].iter().sum::<usize>();
            Some(vec![Change::delete(from, to)])
        }
        EditKind::RootReplacement => {
            let index = rng.below(d.child_count());
            let from: usize = d
                .children()
                .take(index)
                .map(|child| child.node_size())
                .sum();
            let to = from + d.child(index).node_size();
            let block = random_block(schema, rng, 2);
            Some(vec![Change::replace(
                from,
                to,
                Slice::from_fragment(Fragment::from_node(block)),
            )])
        }
        EditKind::Siblings => Some(random_sibling_edit(schema, rng, d)),
        EditKind::Emptying => Some(vec![
            Change::replace(
                0,
                d.content_size(),
                Slice::from_fragment(Fragment::from_node(n(schema, "paragraph", []))),
            )
            .with_fit(Fit::Auto),
        ]),
    }
}

#[test]
fn an_updated_projection_equals_a_fresh_one() {
    use crate::change::ChangeSet;
    const ROUNDS: u64 = 600;
    let schema = test_schema();
    let mut applied = [0usize; EDIT_KINDS.len()];
    let mut reused = [0usize; EDIT_KINDS.len()];
    let mut chained = 0;
    // Sibling edits that removed a top-level block (joins) and that added one
    // (splits and insertions).
    let (mut joins, mut additions) = (0, 0);
    for seed in 0..ROUNDS {
        let mut rng = Rng::new(seed + 7_368_787);
        let mut current = random_doc(&schema, &mut rng);
        let mut projection = Projection::of(&current, &schema);
        for step in 0..6 {
            // Emptying ends the interesting part of a chain, so it is rare.
            let kind_index = if rng.one_in(12) {
                EDIT_KINDS.len() - 1
            } else {
                rng.below(EDIT_KINDS.len() - 1)
            };
            let kind = EDIT_KINDS[kind_index];
            let Some(changes) = random_edit(&schema, &mut rng, &current, kind) else {
                continue;
            };
            let Ok(set) = ChangeSet::create(&schema, &current, changes) else {
                continue;
            };
            let Ok(next) = set.apply(&current) else {
                continue;
            };
            if next.ptr_eq(&current) {
                continue;
            }
            let updated = projection.update(&schema, &current, &next);
            let fresh = Projection::of(&next, &schema);
            let context = format!(
                "seed {seed} step {step} ({kind:?}): {} -> {}",
                schema.describe(&current),
                schema.describe(&next)
            );
            assert_same_projection(&updated, &fresh, &context);
            assert_untouched_top_level_kept(&projection, &updated, &current, &next, &context);
            applied[kind_index] += 1;
            if kind == EditKind::Siblings {
                match next.child_count().cmp(&current.child_count()) {
                    std::cmp::Ordering::Less => joins += 1,
                    std::cmp::Ordering::Greater => additions += 1,
                    std::cmp::Ordering::Equal => {}
                }
            }
            if updated
                .lines()
                .iter()
                .any(|line| projection.lines().iter().any(|old| old.same_body(line)))
            {
                reused[kind_index] += 1;
            }
            if step > 0 {
                chained += 1;
            }
            projection = updated;
            current = next;
        }
    }
    for (index, kind) in EDIT_KINDS.iter().enumerate() {
        assert!(
            applied[index] >= 200,
            "too few {kind:?} edits: {}",
            applied[index]
        );
        // Emptying the document leaves nothing to keep.
        if *kind != EditKind::Emptying {
            assert!(
                reused[index] >= 100,
                "too few {kind:?} edits kept a line: {} of {}",
                reused[index],
                applied[index]
            );
        }
    }
    assert!(chained >= 1000, "too few chained updates: {chained}");
    assert!(joins >= 50, "too few top-level joins: {joins}");
    assert!(additions >= 200, "too few top-level additions: {additions}");
}

#[test]
fn updating_across_an_unchanged_document_keeps_everything() {
    let (schema, document) = sample();
    let projection = Projection::of(&document, &schema);
    let same = projection.update(&schema, &document, &document);
    assert_eq!(same, projection);
    for (a, b) in same.lines().iter().zip(projection.lines()) {
        assert!(a.same_body(b));
    }
}

#[test]
fn a_one_character_insertion_keeps_every_other_lines_body() {
    use crate::change::ChangeSet;
    let schema = shared_schema();
    let document = doc(
        &schema,
        [
            n(&schema, "heading", [t(&schema, "Title")]),
            n(
                &schema,
                "blockquote",
                [
                    n(&schema, "paragraph", [t(&schema, "quoted")]),
                    n(&schema, "paragraph", [t(&schema, "more")]),
                ],
            ),
            n(&schema, "paragraph", [t(&schema, "edit me")]),
            n(&schema, "horizontal_rule", []),
            n(
                &schema,
                "bullet_list",
                [n(
                    &schema,
                    "list_item",
                    [n(&schema, "paragraph", [t(&schema, "item")])],
                )],
            ),
        ],
    );
    let projection = Projection::of(&document, &schema);
    let edited = 3;
    let at = projection.line(edited).expect("line").from() + 2;
    let set = ChangeSet::create(&schema, &document, [insert_text(&schema, at, "X")])
        .expect("a valid change");
    let next = set.apply(&document).expect("applies");
    let updated = projection.update(&schema, &document, &next);
    assert_same_projection(&updated, &Projection::of(&next, &schema), "insertion");
    assert_eq!(updated.line_text(edited), Some("edXit me"));
    for (index, (old, new)) in projection.lines().iter().zip(updated.lines()).enumerate() {
        assert_eq!(
            old.same_body(new),
            index != edited,
            "line {index} should keep its body unless it is the edited one"
        );
    }

    // Inside the quote, the quote's later paragraph moves away from the quote's
    // start, so its body changes; everything outside the quote keeps its own.
    let quoted = 1;
    let at = projection.line(quoted).expect("line").from() + 1;
    let set = ChangeSet::create(&schema, &document, [insert_text(&schema, at, "X")])
        .expect("a valid change");
    let next = set.apply(&document).expect("applies");
    let updated = projection.update(&schema, &document, &next);
    assert_same_projection(
        &updated,
        &Projection::of(&next, &schema),
        "quoted insertion",
    );
    let kept: Vec<bool> = projection
        .lines()
        .iter()
        .zip(updated.lines())
        .map(|(old, new)| old.same_body(new))
        .collect();
    assert_eq!(kept, [true, false, false, true, true, true]);
}

#[test]
fn splitting_a_paragraph_rebuilds_only_its_own_lines() {
    use crate::change::{Change, ChangeSet};
    use crate::slice::{Slice, Token};
    let schema = shared_schema();
    let paragraph = |text: &str| n(&schema, "paragraph", [t(&schema, text)]);
    let split = |document: &crate::node::Node, projection: &Projection, line: usize| {
        let at = projection.line(line).expect("line").from() + 2;
        let markup = document
            .resolve(at)
            .expect("resolves")
            .parent()
            .markup()
            .clone();
        let set = ChangeSet::create(
            &schema,
            document,
            [Change::insert(
                at,
                Slice::from_tokens(&[Token::Close(markup.clone()), Token::Open(markup)]),
            )],
        )
        .expect("a valid change");
        let next = set.apply(document).expect("applies");
        let updated = projection.update(&schema, document, &next);
        assert_same_projection(&updated, &Projection::of(&next, &schema), "split");
        updated
    };

    // At the top level, the lines before the split keep their bodies. The
    // lines after it keep their content, but their paragraphs' indexes in the
    // document grew by one, and the index is part of the body.
    let document = doc(
        &schema,
        ["one", "two", "three", "four", "five"].map(paragraph),
    );
    let projection = Projection::of(&document, &schema);
    let updated = split(&document, &projection, 2);
    let texts: Vec<_> = (0..updated.line_count())
        .map(|line| updated.line_text(line).expect("line"))
        .collect();
    assert_eq!(texts, ["one", "two", "th", "ree", "four", "five"]);
    let old = projection.lines();
    let new = updated.lines();
    assert!(old[0].same_body(&new[0]) && old[1].same_body(&new[1]));
    for (old, new) in old[3..].iter().zip(&new[4..]) {
        assert!(!old.same_body(new));
        assert_eq!(new.ancestors()[0].index, old.ancestors()[0].index + 1);
        assert_eq!(old.runs(), new.runs());
    }

    // Inside a blockquote, the region's parent is the quote: the four lines
    // outside it keep their bodies wherever they moved to.
    let document = doc(
        &schema,
        [
            paragraph("one"),
            paragraph("two"),
            n(&schema, "blockquote", [paragraph("three")]),
            paragraph("four"),
            paragraph("five"),
        ],
    );
    let projection = Projection::of(&document, &schema);
    let updated = split(&document, &projection, 2);
    let old = projection.lines();
    let new = updated.lines();
    assert_eq!(new.len(), 6);
    for (from, to) in [(0, 0), (1, 1), (3, 4), (4, 5)] {
        assert!(
            old[from].same_body(&new[to]),
            "line {from} should keep its body as line {to}"
        );
    }
    assert!(!old[2].same_body(&new[2]) && !old[2].same_body(&new[3]));
}

#[test]
fn the_projection_field_updates_to_what_a_fresh_build_gives() {
    let schema = shared_schema();
    let start = state(
        doc(
            &schema,
            [
                n(&schema, "paragraph", [t(&schema, "abc")]),
                n(&schema, "paragraph", [t(&schema, "def")]),
            ],
        ),
        projection(),
    );
    let first = start.field(projection_field()).expect("configured").clone();
    let typed = start
        .update([TransactionSpec::new().changes([super::support::insert_text(&schema, 7, "X")])])
        .expect("resolves")
        .state()
        .clone();
    let second = typed.field(projection_field()).expect("configured");
    assert_eq!(**second, Projection::of(typed.doc(), &schema));
    assert!(first.lines()[0].same_body(&second.lines()[0]));
    assert!(!first.lines()[1].same_body(&second.lines()[1]));
}
