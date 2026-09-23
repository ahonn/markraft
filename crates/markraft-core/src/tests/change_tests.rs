//! Change-set algebra: mapping, composition, inversion and transformation.

use super::support::*;
use crate::change::{Change, ChangeRange, ChangeSet, TrackMode};
use crate::error::ChangeError;
use crate::fragment::Fragment;
use crate::mark::MarkSet;
use crate::node::Node;
use crate::schema::Schema;
use crate::slice::Slice;

fn text_slice(schema: &Schema, text: &str) -> Slice {
    Slice::from_fragment(Fragment::from_node(t(schema, text)))
}

fn one_paragraph(schema: &Schema) -> Node {
    doc(schema, [n(schema, "paragraph", [t(schema, "abcdef")])])
}

fn set(schema: &Schema, d: &Node, changes: Vec<Change>) -> ChangeSet {
    ChangeSet::create(schema, d, changes).expect("a valid change set")
}

#[test]
fn ranges_are_validated() {
    let schema = test_schema();
    let d = one_paragraph(&schema);
    assert!(matches!(
        ChangeSet::create(&schema, &d, vec![Change::delete(3, 2)]),
        Err(ChangeError::BadRange { .. })
    ));
    assert!(matches!(
        ChangeSet::create(&schema, &d, vec![Change::delete(0, 99)]),
        Err(ChangeError::BadRange { .. })
    ));
    assert!(matches!(
        ChangeSet::create(
            &schema,
            &d,
            vec![Change::delete(2, 5), Change::delete(4, 6)]
        ),
        Err(ChangeError::Overlapping { .. })
    ));
    // Touching ranges are fine.
    assert!(
        ChangeSet::create(
            &schema,
            &d,
            vec![Change::delete(2, 4), Change::delete(4, 6)]
        )
        .is_ok()
    );
}

#[test]
fn lengths_and_emptiness() {
    let schema = test_schema();
    let d = one_paragraph(&schema);
    assert_eq!(d.content_size(), 8);
    let empty = ChangeSet::empty(&schema, 8);
    assert!(empty.is_empty());
    assert_eq!(empty.apply(&d).expect("applies"), d);

    let cs = set(
        &schema,
        &d,
        vec![Change::replace(2, 4, text_slice(&schema, "XYZ"))],
    );
    assert!(!cs.is_empty());
    assert_eq!(cs.length_before(), 8);
    assert_eq!(cs.length_after(), 9);
    assert!(matches!(
        cs.apply(&doc(&schema, [n(&schema, "paragraph", [t(&schema, "a")])])),
        Err(ChangeError::LengthMismatch { .. })
    ));
}

#[test]
fn map_pos_honours_associativity() {
    let schema = test_schema();
    let d = one_paragraph(&schema);
    // Insert "XY" at 3.
    let cs = set(
        &schema,
        &d,
        vec![Change::insert(3, text_slice(&schema, "XY"))],
    );
    assert_eq!(cs.map_pos(2, -1, TrackMode::Simple), Some(2));
    assert_eq!(cs.map_pos(3, -1, TrackMode::Simple), Some(3));
    assert_eq!(cs.map_pos(3, 1, TrackMode::Simple), Some(5));
    assert_eq!(cs.map_pos(4, -1, TrackMode::Simple), Some(6));
    assert_eq!(cs.map_pos(8, -1, TrackMode::Simple), Some(10));
    assert_eq!(cs.map_pos(9, -1, TrackMode::Simple), None, "out of range");
}

#[test]
fn map_pos_tracks_deletions() {
    let schema = test_schema();
    let d = one_paragraph(&schema);
    // Delete 2..5 ("bcd").
    let cs = set(&schema, &d, vec![Change::delete(2, 5)]);
    assert_eq!(cs.map_pos(3, -1, TrackMode::Simple), Some(2));
    assert_eq!(cs.map_pos(3, 1, TrackMode::Simple), Some(2));
    assert_eq!(cs.map_pos(3, -1, TrackMode::Around), None);
    assert_eq!(cs.map_pos(2, -1, TrackMode::Around), Some(2));
    assert_eq!(
        cs.map_pos(2, -1, TrackMode::After),
        None,
        "the token after is gone"
    );
    assert_eq!(cs.map_pos(2, -1, TrackMode::Before), Some(2));
    assert_eq!(
        cs.map_pos(5, -1, TrackMode::Before),
        None,
        "the token before is gone"
    );
    assert_eq!(cs.map_pos(5, -1, TrackMode::After), Some(2));
    assert_eq!(cs.map_pos(6, -1, TrackMode::Around), Some(3));
}

#[test]
fn map_range_and_touches() {
    let schema = test_schema();
    let d = one_paragraph(&schema);
    let cs = set(&schema, &d, vec![Change::delete(3, 5)]);
    let range = cs.map_range(2, 6);
    assert_eq!((range.from, range.to), (2, 4));
    assert!(range.deleted);
    let range = cs.map_range(6, 7);
    assert_eq!((range.from, range.to), (4, 5));
    assert!(cs.touches(3, 3));
    assert!(cs.touches(0, 3), "adjacency counts");
    assert!(!cs.touches(6, 8));
}

#[test]
fn desc_maps_the_same_way_without_content() {
    let schema = test_schema();
    let d = one_paragraph(&schema);
    let cs = set(
        &schema,
        &d,
        vec![
            Change::insert(1, text_slice(&schema, "AB")),
            Change::delete(4, 6),
        ],
    );
    let desc = cs.desc();
    assert_eq!(desc.length_before(), cs.length_before());
    assert_eq!(desc.length_after(), cs.length_after());
    for pos in 0..=d.content_size() {
        for assoc in [-1, 1] {
            assert_eq!(
                desc.map_pos(pos, assoc, TrackMode::Simple),
                cs.map_pos(pos, assoc, TrackMode::Simple)
            );
        }
    }
}

#[test]
fn iter_changes_reports_both_coordinate_frames() {
    let schema = test_schema();
    let d = one_paragraph(&schema);
    let strong = m(&schema, "strong");
    let cs = set(
        &schema,
        &d,
        vec![
            Change::replace(1, 3, text_slice(&schema, "X")),
            Change::add_mark(5, 7, strong.clone()),
        ],
    );
    let changes = cs.iter_changes();
    assert_eq!(changes.len(), 2);
    match &changes[0] {
        ChangeRange::Replaced {
            from_a,
            to_a,
            from_b,
            to_b,
            inserted,
        } => {
            assert_eq!((*from_a, *to_a, *from_b, *to_b), (1, 3, 1, 2));
            assert_eq!(inserted.text_content(None), "X");
        }
        other => panic!("expected a replacement, got {other:?}"),
    }
    match &changes[1] {
        ChangeRange::Marked {
            from_a,
            to_a,
            from_b,
            to_b,
            mods,
        } => {
            assert_eq!((*from_a, *to_a, *from_b, *to_b), (5, 7, 4, 6));
            assert_eq!(mods.len(), 1);
        }
        other => panic!("expected a mark change, got {other:?}"),
    }
}

#[test]
fn invert_restores_content_and_marks() {
    let schema = test_schema();
    let d = doc(
        &schema,
        [
            n(
                &schema,
                "paragraph",
                [t(&schema, "ab"), tm(&schema, "cd", &["strong"])],
            ),
            n(&schema, "paragraph", [t(&schema, "ef")]),
        ],
    );
    let strong = m(&schema, "strong");
    let cases: Vec<Vec<Change>> = vec![
        vec![Change::delete(2, 4)],
        vec![Change::insert(3, text_slice(&schema, "ZZ"))],
        vec![Change::replace(1, 5, text_slice(&schema, "Q"))],
        vec![Change::add_mark(1, 5, strong.clone())],
        vec![Change::remove_mark(1, 5, strong.clone())],
        vec![Change::delete(5, 7)],
        vec![
            Change::insert(1, text_slice(&schema, "(")),
            Change::add_mark(3, 5, strong.clone()),
        ],
    ];
    for changes in cases {
        let cs = set(&schema, &d, changes.clone());
        let out = cs.apply(&d).expect("applies");
        let inverse = cs.invert(&d).expect("invertible");
        assert_eq!(inverse.length_before(), cs.length_after());
        assert_eq!(
            inverse.apply(&out).expect("applies"),
            d,
            "inverting {changes:?} did not restore the document"
        );
    }
}

#[test]
fn invert_of_a_mark_change_is_exact_over_mixed_runs() {
    let schema = test_schema();
    // Half the range already carries the mark, so a blanket removal is wrong.
    let d = doc(
        &schema,
        [n(
            &schema,
            "paragraph",
            [tm(&schema, "ab", &["strong"]), t(&schema, "cd")],
        )],
    );
    let strong = m(&schema, "strong");
    let cs = set(&schema, &d, vec![Change::add_mark(1, 5, strong)]);
    let out = cs.apply(&d).expect("applies");
    assert_eq!(schema.describe(&out), r#"doc(paragraph("abcd"{strong}))"#);
    let inverse = cs.invert(&d).expect("invertible");
    assert_eq!(inverse.apply(&out).expect("applies"), d);
}

fn marks(schema: &Schema, names: &[&str]) -> MarkSet {
    MarkSet::from_marks(schema, names.iter().map(|name| m(schema, name)))
}

fn mixed_paragraph(schema: &Schema) -> Node {
    doc(
        schema,
        [
            n(
                schema,
                "paragraph",
                [
                    t(schema, "ab"),
                    tm(schema, "cd", &["strong"]),
                    tm(schema, "ef", &["em", "strong"]),
                    tm(schema, "gh", &["em"]),
                ],
            ),
            n(schema, "code_block", [t(schema, "ij")]),
        ],
    )
}

#[test]
fn set_marks_makes_a_range_carry_exactly_one_set() {
    let schema = test_schema();
    let d = mixed_paragraph(&schema);
    // One change over four differently marked runs.
    let cs = set(
        &schema,
        &d,
        vec![Change::set_marks(1, 9, marks(&schema, &["em"]))],
    );
    let out = cs.apply(&d).expect("applies");
    assert_eq!(
        schema.describe(&out),
        r#"doc(paragraph("abcdefgh"{em}), code_block("ij"))"#
    );
    // The empty set clears; a range reaching into the code block leaves it
    // alone because the code block allows no marks.
    let cs = set(
        &schema,
        &d,
        vec![Change::set_marks(1, 13, MarkSet::empty())],
    );
    let out = cs.apply(&d).expect("applies");
    assert_eq!(
        schema.describe(&out),
        r#"doc(paragraph("abcdefgh"), code_block("ij"))"#
    );
    // Runs that already carry the target are not recorded as changed.
    let cs = set(
        &schema,
        &d,
        vec![Change::set_marks(3, 5, marks(&schema, &["strong"]))],
    );
    assert!(cs.is_empty());
}

#[test]
fn set_marks_inverts_exactly() {
    let schema = test_schema();
    let d = mixed_paragraph(&schema);
    for target in [&[][..], &["strong"], &["em", "strong"], &["em"]] {
        for (from, to) in [(1, 9), (2, 6), (4, 8), (1, 13)] {
            let cs = set(
                &schema,
                &d,
                vec![Change::set_marks(from, to, marks(&schema, target))],
            );
            let out = cs.apply(&d).expect("applies");
            let inverse = cs.invert(&d).expect("invertible");
            assert_eq!(
                inverse.apply(&out).expect("applies"),
                d,
                "set {target:?} over {from}..{to}"
            );
        }
    }
}

#[test]
fn set_marks_sit_beside_other_changes_and_map_positions_unchanged() {
    let schema = test_schema();
    let d = mixed_paragraph(&schema);
    let cs = set(
        &schema,
        &d,
        vec![
            Change::set_marks(1, 5, marks(&schema, &["em"])),
            Change::set_marks(5, 7, MarkSet::empty()),
            Change::insert(8, text_slice(&schema, "XY")),
        ],
    );
    let out = cs.apply(&d).expect("applies");
    assert_eq!(
        schema.describe(&out),
        r#"doc(paragraph("abcd"{em}, "ef", "g"{em}, "XY", "h"{em}), code_block("ij"))"#
    );
    // A mark section keeps its positions; only the insertion shifts them.
    assert_eq!(cs.map_pos(3, 1, TrackMode::Simple), Some(3));
    assert_eq!(cs.map_pos(9, 1, TrackMode::Simple), Some(11));
    assert!(!cs.touches(1, 7));
    let json = cs.to_json();
    assert_eq!(
        ChangeSet::from_json(&schema, &json).expect("round trips"),
        cs
    );
}

#[test]
fn set_marks_compose_with_edits_before_and_after() {
    let schema = test_schema();
    let d = mixed_paragraph(&schema);
    let a = set(
        &schema,
        &d,
        vec![Change::insert(3, text_slice(&schema, "XY"))],
    );
    let mid = a.apply(&d).expect("applies");
    let b = set(
        &schema,
        &mid,
        vec![Change::set_marks(2, 9, marks(&schema, &["strong"]))],
    );
    let end = b.apply(&mid).expect("applies");
    let composed = a.compose(&b).expect("composable");
    assert_eq!(composed.apply(&d).expect("applies"), end);
    // And the other way round: marks first, then an edit inside the range.
    let b2 = set(&schema, &end, vec![Change::delete(4, 6)]);
    let composed = b.compose(&b2).expect("composable");
    assert_eq!(
        composed.apply(&mid).expect("applies"),
        b2.apply(&end).expect("applies")
    );
    let inverse = composed.invert(&mid).expect("invertible");
    assert_eq!(
        inverse
            .apply(&composed.apply(&mid).expect("applies"))
            .expect("applies"),
        mid
    );
}

#[test]
fn compose_matches_sequential_application() {
    let schema = test_schema();
    let d = one_paragraph(&schema);
    let a = set(
        &schema,
        &d,
        vec![Change::insert(1, text_slice(&schema, "XY"))],
    );
    let mid = a.apply(&d).expect("applies");
    let b = set(&schema, &mid, vec![Change::delete(4, 6)]);
    let composed = a.compose(&b).expect("composable");
    assert_eq!(composed.length_before(), a.length_before());
    assert_eq!(composed.length_after(), b.length_after());
    assert_eq!(
        composed.apply(&d).expect("applies"),
        b.apply(&mid).expect("applies")
    );
}

#[test]
fn compose_handles_edits_inside_inserted_content() {
    let schema = test_schema();
    let d = one_paragraph(&schema);
    let a = set(
        &schema,
        &d,
        vec![Change::insert(1, text_slice(&schema, "insertme"))],
    );
    let mid = a.apply(&d).expect("applies");
    let strong = m(&schema, "strong");
    let b = set(
        &schema,
        &mid,
        vec![Change::delete(3, 5), Change::add_mark(6, 8, strong)],
    );
    let composed = a.compose(&b).expect("composable");
    assert_eq!(
        composed.apply(&d).expect("applies"),
        b.apply(&mid).expect("applies")
    );
    // The composed set replaces a single stretch of the original document.
    assert_eq!(composed.length_before(), 8);
}

#[test]
fn compose_rejects_mismatched_lengths() {
    let schema = test_schema();
    let d = one_paragraph(&schema);
    let a = set(&schema, &d, vec![Change::delete(1, 3)]);
    assert!(matches!(
        a.compose(&ChangeSet::empty(&schema, 99)),
        Err(ChangeError::LengthMismatch { .. })
    ));
}

#[test]
fn transform_converges_for_independent_edits() {
    let schema = test_schema();
    let d = one_paragraph(&schema);
    let a = set(
        &schema,
        &d,
        vec![Change::insert(1, text_slice(&schema, "A"))],
    );
    let b = set(
        &schema,
        &d,
        vec![Change::insert(7, text_slice(&schema, "B"))],
    );
    let (a2, b2) = a.transform(&d, &b, true).expect("transformable");
    let left = a2.apply(&b.apply(&d).expect("applies")).expect("applies");
    let right = b2.apply(&a.apply(&d).expect("applies")).expect("applies");
    assert_eq!(left, right);
    assert_eq!(schema.describe(&left), r#"doc(paragraph("AabcdefB"))"#);
}

#[test]
fn transform_orders_insertions_at_the_same_position() {
    let schema = test_schema();
    let d = one_paragraph(&schema);
    let a = set(
        &schema,
        &d,
        vec![Change::insert(4, text_slice(&schema, "A"))],
    );
    let b = set(
        &schema,
        &d,
        vec![Change::insert(4, text_slice(&schema, "B"))],
    );
    let (a2, b2) = a.transform(&d, &b, true).expect("transformable");
    let left = a2.apply(&b.apply(&d).expect("applies")).expect("applies");
    let right = b2.apply(&a.apply(&d).expect("applies")).expect("applies");
    assert_eq!(left, right);
    assert_eq!(schema.describe(&left), r#"doc(paragraph("abcABdef"))"#);

    let (a2, b2) = a.transform(&d, &b, false).expect("transformable");
    let left = a2.apply(&b.apply(&d).expect("applies")).expect("applies");
    let right = b2.apply(&a.apply(&d).expect("applies")).expect("applies");
    assert_eq!(left, right);
    assert_eq!(schema.describe(&left), r#"doc(paragraph("abcBAdef"))"#);
}

#[test]
fn transform_keeps_the_other_sides_insertion() {
    let schema = test_schema();
    let d = one_paragraph(&schema);
    // A deletes a range that B inserts into.
    let a = set(&schema, &d, vec![Change::delete(2, 6)]);
    let b = set(
        &schema,
        &d,
        vec![Change::insert(4, text_slice(&schema, "B"))],
    );
    let (a2, b2) = a.transform(&d, &b, true).expect("transformable");
    let left = a2.apply(&b.apply(&d).expect("applies")).expect("applies");
    let right = b2.apply(&a.apply(&d).expect("applies")).expect("applies");
    assert_eq!(left, right);
    assert_eq!(
        schema.describe(&left),
        r#"doc(paragraph("aBf"))"#,
        "a deletion never removes the other change's insertion"
    );
}

#[test]
fn transform_with_overlapping_replacements() {
    let schema = test_schema();
    let d = one_paragraph(&schema);
    let a = set(
        &schema,
        &d,
        vec![Change::replace(2, 5, text_slice(&schema, "A"))],
    );
    let b = set(
        &schema,
        &d,
        vec![Change::replace(3, 7, text_slice(&schema, "B"))],
    );
    let (a2, b2) = a.transform(&d, &b, true).expect("transformable");
    let left = a2.apply(&b.apply(&d).expect("applies")).expect("applies");
    let right = b2.apply(&a.apply(&d).expect("applies")).expect("applies");
    assert_eq!(left, right, "both orders converge");
}

#[test]
fn transform_preserves_mark_changes() {
    let schema = test_schema();
    let d = one_paragraph(&schema);
    let strong = m(&schema, "strong");
    let a = set(&schema, &d, vec![Change::add_mark(1, 7, strong)]);
    let b = set(&schema, &d, vec![Change::delete(3, 5)]);
    let (a2, b2) = a.transform(&d, &b, true).expect("transformable");
    let left = a2.apply(&b.apply(&d).expect("applies")).expect("applies");
    let right = b2.apply(&a.apply(&d).expect("applies")).expect("applies");
    assert_eq!(left, right);
    assert_eq!(schema.describe(&left), r#"doc(paragraph("abef"{strong}))"#);
}

#[test]
fn transform_resolves_conflicting_mark_changes() {
    let schema = test_schema();
    let d = doc(
        &schema,
        [n(
            &schema,
            "paragraph",
            [tm(&schema, "abcdef", &["strong"])],
        )],
    );
    let strong = m(&schema, "strong");
    // One side removes the mark where the other adds it.
    let a = set(&schema, &d, vec![Change::remove_mark(1, 5, strong.clone())]);
    let b = set(&schema, &d, vec![Change::add_mark(3, 7, strong.clone())]);
    for before in [true, false] {
        let (a2, b2) = a.transform(&d, &b, before).expect("transformable");
        let left = a2.apply(&b.apply(&d).expect("applies")).expect("applies");
        let right = b2.apply(&a.apply(&d).expect("applies")).expect("applies");
        assert_eq!(left, right, "before = {before}");
    }

    // Unrelated mark types commute, so neither side gives way.
    let em = m(&schema, "em");
    let b = set(&schema, &d, vec![Change::add_mark(3, 7, em)]);
    let (a2, b2) = a.transform(&d, &b, true).expect("transformable");
    let left = a2.apply(&b.apply(&d).expect("applies")).expect("applies");
    let right = b2.apply(&a.apply(&d).expect("applies")).expect("applies");
    assert_eq!(left, right);
    assert_eq!(
        schema.describe(&left),
        r#"doc(paragraph("ab", "cd"{em}, "ef"{strong,em}))"#
    );
}

#[test]
fn map_pos_finds_content_again_after_a_change() {
    let schema = test_schema();
    let d = one_paragraph(&schema);
    // Mark a single character, then edit elsewhere and check it still maps to
    // the same character.
    let cs = set(
        &schema,
        &d,
        vec![Change::insert(1, text_slice(&schema, "12345"))],
    );
    let out = cs.apply(&d).expect("applies");
    let target = 4; // the character "c" starts at 3 and ends at 4
    let mapped = cs.map_pos(target, -1, TrackMode::Simple).expect("mapped");
    assert_eq!(
        out.text_between(&schema, mapped - 1, mapped, None, None),
        d.text_between(&schema, target - 1, target, None, None)
    );
}

#[test]
fn an_editing_session_composes_and_undoes_as_one_step() {
    let schema = test_schema();
    let start = doc(&schema, [n(&schema, "paragraph", [t(&schema, "hello")])]);

    // Type " world", split the paragraph, bold a word, then wrap the whole
    // thing in a blockquote -- the kind of sequence a history entry holds.
    let mut current = start.clone();
    let mut composed: Option<ChangeSet> = None;
    type Step = Box<dyn Fn(&Node) -> Vec<Change>>;
    let steps: Vec<Step> = vec![
        Box::new(|_: &Node| vec![Change::insert(6, text_slice(&test_schema(), " world"))]),
        Box::new(|_: &Node| {
            let schema = test_schema();
            let paragraph = schema.node_id("paragraph").expect("known");
            vec![Change::insert(
                7,
                Slice::from_tokens(&[
                    crate::slice::Token::Close(crate::node::Markup::new(paragraph)),
                    crate::slice::Token::Open(crate::node::Markup::new(paragraph)),
                ]),
            )]
        }),
        Box::new(|_: &Node| vec![Change::add_mark(9, 14, m(&test_schema(), "strong"))]),
        Box::new(|d: &Node| {
            let schema = test_schema();
            let quote = schema.node_id("blockquote").expect("known");
            let markup = crate::node::Markup::new(quote);
            vec![
                Change::insert(
                    0,
                    Slice::from_tokens(&[crate::slice::Token::Open(markup.clone())]),
                ),
                Change::insert(
                    d.content_size(),
                    Slice::from_tokens(&[crate::slice::Token::Close(markup)]),
                ),
            ]
        }),
    ];
    for step in steps {
        let cs = set(&schema, &current, step(&current));
        current = cs.apply(&current).expect("applies");
        current.check(&schema).expect("valid at every step");
        composed = Some(match composed {
            None => cs,
            Some(previous) => previous.compose(&cs).expect("composable"),
        });
    }
    assert_eq!(
        schema.describe(&current),
        r#"doc(blockquote(paragraph("hello "), paragraph("world"{strong})))"#
    );

    let composed = composed.expect("at least one step");
    assert_eq!(composed.apply(&start).expect("applies"), current);
    // Undoing the whole session restores the starting document exactly.
    let undo = composed.invert(&start).expect("invertible");
    assert_eq!(undo.apply(&current).expect("applies"), start);
}

#[test]
fn degenerate_change_sets_behave() {
    let schema = test_schema();
    let d = one_paragraph(&schema);

    // No changes at all.
    let none = set(&schema, &d, Vec::new());
    assert!(none.is_empty());
    assert_eq!(none.apply(&d).expect("applies"), d);
    assert_eq!(
        none.invert(&d)
            .expect("invertible")
            .apply(&d)
            .expect("applies"),
        d
    );
    assert_eq!(none.compose(&none).expect("composable"), none);
    let (a, b) = none.transform(&d, &none, true).expect("transformable");
    assert!(a.is_empty() && b.is_empty());

    // A zero-width replacement with empty content is dropped.
    let noop = set(&schema, &d, vec![Change::replace(3, 3, Slice::empty())]);
    assert!(noop.is_empty());

    // A mark change over an empty range is dropped too.
    let noop = set(
        &schema,
        &d,
        vec![Change::add_mark(3, 3, m(&schema, "strong"))],
    );
    assert!(noop.is_empty());

    // An empty document still round-trips.
    let empty = schema.doc([]).expect("built");
    assert_eq!(empty.content_size(), 0);
    let cs = set(&schema, &empty, Vec::new());
    assert_eq!(cs.length_before(), 0);
    assert_eq!(cs.apply(&empty).expect("applies"), empty);
}

#[test]
fn change_sets_round_trip_through_json() {
    let schema = test_schema();
    let d = doc(
        &schema,
        [
            n(&schema, "paragraph", [t(&schema, "abcdef")]),
            n(&schema, "paragraph", [t(&schema, "ghi")]),
        ],
    );
    let strong = m(&schema, "strong");
    let cs = set(
        &schema,
        &d,
        vec![
            Change::replace(2, 4, text_slice(&schema, "XY")),
            Change::add_mark(5, 7, strong),
            Change::remove_mark_type(9, 11, schema.mark_id("link").expect("known")),
        ],
    );
    let json = cs.to_json();
    let back = ChangeSet::from_json(&schema, &json).expect("round trips");
    assert_eq!(back, cs);
    assert_eq!(
        back.apply(&d).expect("applies"),
        cs.apply(&d).expect("applies")
    );
    let text = serde_json::to_string(&json).expect("serialises");
    let parsed: serde_json::Value = serde_json::from_str(&text).expect("parses");
    assert_eq!(
        ChangeSet::from_json(&schema, &parsed).expect("round trips"),
        cs
    );

    // A payload whose sections do not add up is rejected.
    let mut broken = json.clone();
    broken["length"] = serde_json::Value::from(999u64);
    assert!(ChangeSet::from_json(&schema, &broken).is_err());
}

#[test]
fn map_range_never_grows_past_its_content() {
    let schema = test_schema();
    let d = one_paragraph(&schema);

    // Inserting two tokens at 3.
    let cs = set(
        &schema,
        &d,
        vec![Change::insert(3, text_slice(&schema, "XY"))],
    );
    // A cursor stays a cursor and does not swallow the new text.
    let cursor = cs.map_range(3, 3);
    assert_eq!((cursor.from, cursor.to), (3, 3));
    // A range around the insertion keeps the inserted text outside its edges.
    let around = cs.map_range(2, 4);
    assert_eq!((around.from, around.to), (2, 6));
    let before = cs.map_range(1, 3);
    assert_eq!((before.from, before.to), (1, 3));
    let after = cs.map_range(3, 5);
    assert_eq!((after.from, after.to), (5, 7));

    // A range whose content is fully deleted collapses.
    let cs = set(&schema, &d, vec![Change::delete(2, 6)]);
    let gone = cs.map_range(3, 5);
    assert_eq!((gone.from, gone.to), (2, 2));
    assert!(gone.deleted);
    let cursor = cs.map_range(4, 4);
    assert_eq!((cursor.from, cursor.to), (2, 2));
}

#[test]
fn a_repair_that_swallows_another_change_says_so() {
    let schema = test_schema();
    let d = doc(
        &schema,
        [
            n(&schema, "paragraph", [t(&schema, "abcdef")]),
            n(
                &schema,
                "blockquote",
                [n(&schema, "paragraph", [t(&schema, "gh")])],
            ),
            n(&schema, "paragraph", [t(&schema, "ijk")]),
        ],
    );
    assert_eq!(d.content_size(), 19);
    let changes = || vec![Change::delete(0, 13), Change::delete(14, 14)];
    // The two ranges do not overlap, so without repair the set is fine.
    ChangeSet::create(&schema, &d, changes()).expect("no repair, no conflict");

    // Repairing the first change widens it over the second. The error names the
    // range the caller wrote and the range the repair produced.
    let fitted: Vec<Change> = changes()
        .into_iter()
        .map(|c| c.with_fit(crate::fit::Fit::Auto))
        .collect();
    match ChangeSet::create(&schema, &d, fitted) {
        Err(ChangeError::FitConflict {
            from,
            to,
            fitted_from,
            fitted_to,
        }) => {
            assert_eq!((from, to), (0, 13));
            assert!(
                fitted_to > to,
                "the repair widened {from}..{to} to {fitted_from}..{fitted_to}"
            );
        }
        other => panic!("expected a fit conflict, got {other:?}"),
    }
}

#[test]
fn change_sets_from_different_schemas_do_not_mix() {
    let a_schema = test_schema();
    let b_schema = test_schema();
    assert!(!a_schema.same(&b_schema));
    let d = doc(&a_schema, [n(&a_schema, "paragraph", [t(&a_schema, "ab")])]);
    let a = ChangeSet::empty(&a_schema, 4);
    let b = ChangeSet::empty(&b_schema, 4);
    assert!(matches!(a.compose(&b), Err(ChangeError::SchemaMismatch)));
    assert!(matches!(
        a.transform_over(&d, &b, true),
        Err(ChangeError::SchemaMismatch)
    ));
    assert!(matches!(
        a.transform(&d, &b, true),
        Err(ChangeError::SchemaMismatch)
    ));
    // Clones of the same schema are interchangeable.
    let same = ChangeSet::empty(&a_schema.clone(), 4);
    assert!(a.compose(&same).is_ok());
}

#[test]
fn transform_repairs_a_rebase_that_would_not_balance() {
    let schema = test_schema();
    let d = doc(
        &schema,
        [n(
            &schema,
            "blockquote",
            [n(&schema, "paragraph", [t(&schema, "abcd")])],
        )],
    );
    // 0 quote 1 para 2 a 3 b 4 c 5 d 6 /para 7 /quote 8
    assert_eq!(d.content_size(), 8);
    let end = d.content_size();

    // One side splits the paragraph; the other lifts it out of the blockquote.
    let paragraph = schema.node_id("paragraph").expect("known");
    let split = Slice::from_tokens(&[
        crate::slice::Token::Close(crate::node::Markup::new(paragraph)),
        crate::slice::Token::Open(crate::node::Markup::new(paragraph)),
    ]);
    let a = set(&schema, &d, vec![Change::insert(4, split)]);
    let b = set(
        &schema,
        &d,
        vec![Change::delete(0, 1), Change::delete(end - 1, end)],
    );
    let after_a = a.apply(&d).expect("applies");
    let after_b = b.apply(&d).expect("applies");
    assert_eq!(
        schema.describe(&after_a),
        r#"doc(blockquote(paragraph("ab"), paragraph("cd")))"#
    );
    assert_eq!(schema.describe(&after_b), r#"doc(paragraph("abcd"))"#);

    // Rebasing either way yields a set that applies and leaves a valid
    // document, which a naive rebase would not.
    let (a2, b2) = a.transform(&d, &b, true).expect("transformable");
    let left = a2.apply(&after_b).expect("a' applies");
    let right = b2.apply(&after_a).expect("b' applies");
    left.check(&schema).expect("a' leaves a valid document");
    right.check(&schema).expect("b' leaves a valid document");
    assert_eq!(
        schema.describe(&left),
        r#"doc(paragraph("ab"), paragraph("cd"))"#,
        "the split survives the lift"
    );
    assert_eq!(left, right, "this pair still converges");
}

#[test]
fn transform_diverges_only_on_overlapping_conflicts() {
    let schema = test_schema();
    // A single code block. Both changes replace a range that starts at its
    // opening token, so each of them dissolves the block and puts a paragraph
    // in its place -- and they disagree about how much of the text goes.
    let d = doc(
        &schema,
        [n(&schema, "code_block", [t(&schema, "abcdefghij")])],
    );
    assert_eq!(d.content_size(), 12);
    let para = |text: &str| {
        Slice::from_fragment(Fragment::from_node(n(
            &schema,
            "paragraph",
            [t(&schema, text)],
        )))
    };
    let a = set(
        &schema,
        &d,
        vec![Change::replace(0, 5, para("PP")).with_fit(crate::fit::Fit::Auto)],
    );
    let b = set(
        &schema,
        &d,
        vec![Change::replace(0, 9, para("QQ")).with_fit(crate::fit::Fit::Auto)],
    );
    let after_a = a.apply(&d).expect("applies");
    let after_b = b.apply(&d).expect("applies");
    after_a.check(&schema).expect("valid");
    after_b.check(&schema).expect("valid");

    let (a2, b2) = a.transform(&d, &b, true).expect("transformable");
    let left = a2.apply(&after_b).expect("a' applies");
    let right = b2.apply(&after_a).expect("b' applies");
    // The guarantee that always holds: both orders apply and stay valid.
    left.check(&schema).expect("a' leaves a valid document");
    right.check(&schema).expect("b' leaves a valid document");
    // The guarantee that does not: this conflict class does not converge,
    // because each side repairs against a document the other never saw.
    assert_ne!(
        left,
        right,
        "this conflict class is documented as divergent; if it now converges, \
         update the crate documentation. left = {}, right = {}",
        schema.describe(&left),
        schema.describe(&right)
    );
    // Neither order loses anyone's text.
    let text = |node: &Node| node.text_between(&schema, 0, node.content_size(), Some(""), None);
    for side in [&left, &right] {
        let text = text(side);
        assert!(text.contains("PP"), "kept a's insertion: {text}");
        assert!(text.contains("QQ"), "kept b's insertion: {text}");
    }

    // The same pair of changes over ranges that do not meet converges.
    let far = doc(
        &schema,
        [
            n(&schema, "paragraph", [t(&schema, "abcd")]),
            n(&schema, "paragraph", [t(&schema, "efgh")]),
            n(&schema, "paragraph", [t(&schema, "ijkl")]),
        ],
    );
    let head = set(&schema, &far, vec![Change::delete(1, 2)]);
    let tail = set(&schema, &far, vec![Change::delete(13, 14)]);
    let (head2, tail2) = head.transform(&far, &tail, true).expect("transformable");
    assert_eq!(
        head2
            .apply(&tail.apply(&far).expect("applies"))
            .expect("applies"),
        tail2
            .apply(&head.apply(&far).expect("applies"))
            .expect("applies")
    );
}
