//! IME composition: marking, replacing and clearing.

use super::support::*;
use crate::composition::{
    CompositionRange, composition, composition_range, finish_composition, is_composing,
    start_composition, update_composition,
};
use crate::mark::MarkSet;
use crate::selection::Selection;
use crate::state::{EditorState, TransactionSpec};

fn run(state: &EditorState, spec: TransactionSpec) -> EditorState {
    state
        .update([spec])
        .expect("a valid transaction")
        .state()
        .clone()
}

#[test]
fn a_composition_marks_replaces_and_clears() {
    let schema = shared_schema();
    let start = state(
        doc(&schema, [n(&schema, "paragraph", [t(&schema, "ab")])]),
        composition(),
    );
    assert!(!is_composing(&start));

    let marked = run(&start, start_composition(CompositionRange::new(2, 2)));
    assert_eq!(
        composition_range(&marked),
        Some(CompositionRange::new(2, 2))
    );
    assert_eq!(schema.describe(marked.doc()), r#"doc(paragraph("ab"))"#);

    let typing = run(&marked, update_composition(&marked, "ni", 2).unwrap());
    assert_eq!(schema.describe(typing.doc()), r#"doc(paragraph("anib"))"#);
    assert_eq!(
        composition_range(&typing),
        Some(CompositionRange::new(2, 4))
    );
    assert_eq!(typing.selection(), &Selection::cursor(4));

    let committed = run(&typing, update_composition(&typing, "\u{4f60}", 1).unwrap());
    assert_eq!(
        schema.describe(committed.doc()),
        "doc(paragraph(\"a\u{4f60}b\"))"
    );
    let done = run(&committed, finish_composition());
    assert_eq!(composition_range(&done), None);
    assert!(!is_composing(&done));
}

#[test]
fn the_marked_range_maps_through_an_unrelated_change() {
    let schema = shared_schema();
    let start = state(
        doc(&schema, [n(&schema, "paragraph", [t(&schema, "hello")])]),
        composition(),
    );
    let marked = run(&start, start_composition(CompositionRange::new(4, 6)));
    let edited = run(
        &marked,
        TransactionSpec::new().changes([insert_text(&schema, 1, "ZZ")]),
    );
    assert_eq!(
        composition_range(&edited),
        Some(CompositionRange::new(6, 8))
    );
}

#[test]
fn an_unmarked_composition_replaces_the_selection() {
    let schema = shared_schema();
    let start = state(
        doc(&schema, [n(&schema, "paragraph", [t(&schema, "hello")])]),
        composition(),
    );
    let selected = run(
        &start,
        TransactionSpec::new().selection(Selection::text(2, 4)),
    );
    let typed = run(&selected, update_composition(&selected, "X", 1).unwrap());
    assert_eq!(schema.describe(typed.doc()), r#"doc(paragraph("hXlo"))"#);
    assert_eq!(composition_range(&typed), Some(CompositionRange::new(2, 3)));
}

#[test]
fn composed_text_takes_the_marks_at_the_insertion_point() {
    let schema = shared_schema();
    let start = state(
        doc(
            &schema,
            [n(&schema, "paragraph", [tm(&schema, "bold", &["strong"])])],
        ),
        composition(),
    );
    let marked = run(&start, start_composition(CompositionRange::new(3, 3)));
    let typed = run(&marked, update_composition(&marked, "X", 1).unwrap());
    assert_eq!(
        schema.describe(typed.doc()),
        r#"doc(paragraph("boXld"{strong}))"#
    );

    // Stored marks on the selection win over the surrounding content.
    let plain = run(
        &start,
        TransactionSpec::new().selection(Selection::cursor_with_marks(3, MarkSet::empty())),
    );
    let plain = run(&plain, update_composition(&plain, "X", 1).unwrap());
    assert_eq!(
        schema.describe(plain.doc()),
        r#"doc(paragraph("bo"{strong}, "X", "ld"{strong}))"#
    );
}
