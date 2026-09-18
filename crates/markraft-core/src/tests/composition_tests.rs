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
    assert_eq!(
        typing.selection(),
        &Selection::cursor_with_marks(4, MarkSet::empty())
    );

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
fn an_ordinary_edit_commits_the_candidate() {
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
    assert_eq!(composition_range(&edited), None);
    assert_eq!(crate::committed_document(&edited), edited.doc());
    assert!(crate::cancel_composition(&edited).is_none());
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

#[test]
fn replacing_all_tracks_the_candidate_inside_the_fitted_paragraph() {
    let schema = shared_schema();
    let start = state(
        doc(&schema, [n(&schema, "paragraph", [t(&schema, "hello")])]),
        composition(),
    );
    let selected = run(&start, TransactionSpec::new().selection(Selection::All));
    let mut typed = selected.clone();
    for candidate in ["n", "ni", "你"] {
        typed = run(
            &typed,
            update_composition(&typed, candidate, candidate.chars().count()).unwrap(),
        );
        assert_eq!(crate::committed_document(&typed), start.doc());
    }
    assert_eq!(schema.describe(typed.doc()), "doc(paragraph(\"你\"))");
    assert_eq!(composition_range(&typed), Some(CompositionRange::new(1, 2)));
    assert_eq!(
        typed.selection(),
        &Selection::cursor_with_marks(2, MarkSet::empty())
    );
    let cancelled = run(&typed, crate::cancel_composition(&typed).unwrap());
    assert_eq!(cancelled.doc(), start.doc());
    assert_eq!(cancelled.selection(), &Selection::All);
}

#[test]
fn cancel_restores_replaced_content_selection_and_history() {
    let schema = shared_schema();
    let start = state(
        doc(&schema, [n(&schema, "paragraph", [t(&schema, "hello")])]),
        crate::Extension::all([composition(), crate::history(Default::default())]),
    );
    let selected = run(
        &start,
        TransactionSpec::new().selection(Selection::text(1, 6)),
    );
    let marked = run(&selected, update_composition(&selected, "ni", 2).unwrap());
    let cancelled = run(&marked, crate::cancel_composition(&marked).unwrap());
    assert_eq!(cancelled.doc(), selected.doc());
    assert_eq!(cancelled.selection(), selected.selection());
    assert_eq!(crate::undo_depth(&cancelled), crate::undo_depth(&selected));
    assert_eq!(crate::redo_depth(&cancelled), crate::redo_depth(&selected));
    assert!(!is_composing(&cancelled));
}

#[test]
fn unmark_commits_a_replacement_and_undo_restores_the_original() {
    let schema = shared_schema();
    let start = state(
        doc(&schema, [n(&schema, "paragraph", [t(&schema, "hello")])]),
        crate::Extension::all([composition(), crate::history(Default::default())]),
    );
    let selected = run(
        &start,
        TransactionSpec::new().selection(Selection::text(1, 6)),
    );
    let marked = run(&selected, update_composition(&selected, "ni", 2).unwrap());
    let committed = run(&marked, finish_composition());
    assert_eq!(crate::committed_document(&committed), committed.doc());
    let undone = run(&committed, crate::undo(&committed).unwrap());
    assert_eq!(undone.doc(), start.doc());
}

#[test]
fn composing_after_turning_off_an_inherited_mark_exits_its_scope() {
    let schema = shared_schema();
    let strong = schema.mark_id("strong").unwrap();
    let span = n(&schema, "inline_span", [t(&schema, "ab")])
        .mark(MarkSet::from_marks(&schema, [m(&schema, "strong")]));
    let start = state(
        doc(&schema, [n(&schema, "paragraph", [span])]),
        composition(),
    );
    let selected = run(
        &start,
        TransactionSpec::new().selection(Selection::cursor(3)),
    );
    let toggled = run(
        &selected,
        crate::commands::toggle_mark(strong, Default::default())(&selected).unwrap(),
    );
    let marked = run(&toggled, update_composition(&toggled, "ni", 2).unwrap());
    let marked = run(&marked, update_composition(&marked, "你", 1).unwrap());
    assert_eq!(
        schema.describe(marked.doc()),
        "doc(paragraph(inline_span{strong}(\"a\"), \"你\", inline_span{strong}(\"b\")))"
    );
    let cancelled = run(&marked, crate::cancel_composition(&marked).unwrap());
    assert_eq!(cancelled.doc(), start.doc());
}
