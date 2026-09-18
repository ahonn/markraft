//! Undo history: grouping, explicit groups, compositions, remote changes.

use super::support::*;
use crate::change::Change;
use crate::composition::CompositionRange;
use crate::composition::{composition, finish_composition, start_composition, update_composition};
use crate::fit::Fit;
use crate::history::{
    HistoryConfig, IsolateHistory, begin_undo_group, end_undo_group, history, isolate, redo,
    redo_depth, redo_selection, undo, undo_depth, undo_selection,
};
use crate::node::{Markup, Node};
use crate::schema::Schema;
use crate::selection::Selection;
use crate::slice::{Slice, Token};
use crate::state::{EditorState, Extension, TransactionSpec};

fn run(state: &EditorState, spec: TransactionSpec) -> EditorState {
    state
        .update([spec])
        .expect("a valid transaction")
        .state()
        .clone()
}

fn typed(schema: &Schema, pos: usize, text: &str, at: u64) -> TransactionSpec {
    TransactionSpec::new()
        .changes([insert_text(schema, pos, text)])
        .selection(Selection::cursor(pos + text.chars().count()))
        .user_event("input.type")
        .time(at)
}

fn split(doc: &Node, at: usize) -> Change {
    let resolved = doc.resolve(at).expect("a position inside the document");
    let markup = Markup::with_attrs(
        resolved.parent().type_id(),
        resolved.parent().attrs().clone(),
    );
    Change::insert(
        at,
        Slice::from_tokens(&[Token::Close(markup.clone()), Token::Open(markup)]),
    )
    .with_fit(Fit::Auto)
}

fn wrap(schema: &Schema, before: usize, after: usize) -> Vec<Change> {
    let quote = Markup::new(schema.node_id("blockquote").expect("the test schema"));
    vec![
        Change::insert(before, Slice::from_tokens(&[Token::Open(quote.clone())])),
        Change::insert(after, Slice::from_tokens(&[Token::Close(quote)])),
    ]
}

fn history_state(document: Node) -> EditorState {
    state(document, history(HistoryConfig::default()))
}

#[test]
fn typing_in_quick_succession_makes_one_entry() {
    let schema = shared_schema();
    let start = history_state(doc(
        &schema,
        [n(&schema, "paragraph", [t(&schema, "hello")])],
    ));
    let mut state = start.clone();
    for (index, ch) in ["A", "B", "C"].iter().enumerate() {
        state = run(&state, typed(&schema, 3 + index, ch, 10 * index as u64));
    }
    assert_eq!(
        schema.describe(state.doc()),
        r#"doc(paragraph("heABCllo"))"#
    );
    assert_eq!(undo_depth(&state), 1);

    let undone = run(&state, undo(&state).expect("something to undo"));
    assert_eq!(schema.describe(undone.doc()), r#"doc(paragraph("hello"))"#);
    assert_eq!(undone.selection(), start.selection());
    assert_eq!(undo_depth(&undone), 0);
    assert_eq!(redo_depth(&undone), 1);

    let redone = run(&undone, redo(&undone).expect("something to redo"));
    assert_eq!(
        schema.describe(redone.doc()),
        r#"doc(paragraph("heABCllo"))"#
    );
    assert_eq!(redone.selection(), state.selection());
}

#[test]
fn a_pause_or_a_different_user_event_starts_a_new_entry() {
    let schema = shared_schema();
    let start = history_state(doc(
        &schema,
        [n(&schema, "paragraph", [t(&schema, "hello")])],
    ));

    let paused = run(&start, typed(&schema, 3, "A", 0));
    let paused = run(&paused, typed(&schema, 4, "B", 5_000));
    assert_eq!(undo_depth(&paused), 2);

    let other = run(&start, typed(&schema, 3, "A", 0));
    let other = run(
        &other,
        TransactionSpec::new()
            .changes([insert_text(&schema, 4, "B")])
            .user_event("input.paste")
            .time(10),
    );
    assert_eq!(undo_depth(&other), 2);
}

#[test]
fn isolate_history_forces_a_boundary() {
    let schema = shared_schema();
    let start = history_state(doc(
        &schema,
        [n(&schema, "paragraph", [t(&schema, "hello")])],
    ));
    let state = run(&start, typed(&schema, 3, "A", 0));
    let state = run(
        &state,
        typed(&schema, 4, "B", 10).annotate(isolate(IsolateHistory::Before)),
    );
    assert_eq!(undo_depth(&state), 2);
}

#[test]
fn an_explicit_group_folds_a_split_typing_and_a_wrap_into_one_entry() {
    let schema = shared_schema();
    let start = history_state(doc(
        &schema,
        [n(&schema, "paragraph", [t(&schema, "hello")])],
    ));

    let state = run(
        &start,
        TransactionSpec::new()
            .effect(begin_undo_group().of(()))
            .time(0),
    );
    let state = run(
        &state,
        TransactionSpec::new()
            .changes([split(state.doc(), 4)])
            .user_event("split")
            .time(1),
    );
    assert_eq!(
        schema.describe(state.doc()),
        r#"doc(paragraph("hel"), paragraph("lo"))"#
    );
    let state = run(&state, typed(&schema, 6, "X", 2));
    assert_eq!(
        schema.describe(state.doc()),
        r#"doc(paragraph("hel"), paragraph("Xlo"))"#
    );
    let state = run(
        &state,
        TransactionSpec::new()
            .changes(wrap(&schema, 5, 10))
            .user_event("wrap")
            .time(3),
    );
    assert_eq!(
        schema.describe(state.doc()),
        r#"doc(paragraph("hel"), blockquote(paragraph("Xlo")))"#
    );
    let state = run(
        &state,
        TransactionSpec::new()
            .effect(end_undo_group().of(()))
            .time(4),
    );

    assert_eq!(undo_depth(&state), 1);
    let undone = run(&state, undo(&state).expect("something to undo"));
    assert_eq!(schema.describe(undone.doc()), r#"doc(paragraph("hello"))"#);
    assert_eq!(undone.selection(), start.selection());
    assert_eq!(undo_depth(&undone), 0);

    let redone = run(&undone, redo(&undone).expect("something to redo"));
    assert_eq!(
        schema.describe(redone.doc()),
        r#"doc(paragraph("hel"), blockquote(paragraph("Xlo")))"#
    );
}

#[test]
fn a_group_starts_at_its_first_edit_and_does_not_swallow_earlier_entries() {
    let schema = shared_schema();
    let start = history_state(doc(
        &schema,
        [n(&schema, "paragraph", [t(&schema, "hello")])],
    ));
    let state = run(&start, typed(&schema, 3, "A", 0));
    let state = run(
        &state,
        TransactionSpec::new()
            .effect(begin_undo_group().of(()))
            .time(1_000),
    );
    let state = run(&state, typed(&schema, 4, "B", 1_001));
    let state = run(&state, typed(&schema, 5, "C", 9_999));
    let state = run(
        &state,
        TransactionSpec::new()
            .effect(end_undo_group().of(()))
            .time(10_000),
    );
    assert_eq!(undo_depth(&state), 2);
    let undone = run(&state, undo(&state).expect("something to undo"));
    assert_eq!(schema.describe(undone.doc()), r#"doc(paragraph("heAllo"))"#);
}

#[test]
fn a_composition_folds_its_updates_and_closes_on_a_plain_transaction() {
    let schema = shared_schema();
    let start = state(
        doc(&schema, [n(&schema, "paragraph", [t(&schema, "ab")])]),
        Extension::all([history(HistoryConfig::default()), composition()]),
    );
    let state = run(&start, start_composition(CompositionRange::new(2, 2)));
    let state = run(&state, update_composition(&state, "n", 1).unwrap().time(0));
    assert_eq!(schema.describe(state.doc()), r#"doc(paragraph("anb"))"#);
    let state = run(&state, update_composition(&state, "ni", 2).unwrap().time(1));
    assert_eq!(schema.describe(state.doc()), r#"doc(paragraph("anib"))"#);
    let state = run(
        &state,
        update_composition(&state, "\u{4f60}", 1).unwrap().time(2),
    );
    assert_eq!(
        schema.describe(state.doc()),
        "doc(paragraph(\"a\u{4f60}b\"))"
    );
    assert_eq!(undo_depth(&state), 1, "the whole composition is one entry");

    // A plain transaction closes the composition, so the next edit is its own
    // entry even though it is close in time.
    let state = run(&state, finish_composition());
    let state = run(&state, typed(&schema, 3, "!", 3));
    assert_eq!(undo_depth(&state), 2);

    let undone = run(&state, undo(&state).expect("something to undo"));
    assert_eq!(
        schema.describe(undone.doc()),
        "doc(paragraph(\"a\u{4f60}b\"))"
    );
    let undone = run(&undone, undo(&undone).expect("something to undo"));
    assert_eq!(schema.describe(undone.doc()), r#"doc(paragraph("ab"))"#);
    assert_eq!(undo_depth(&undone), 0);
}

#[test]
fn undo_closes_an_open_composition_and_group() {
    let schema = shared_schema();
    let start = state(
        doc(&schema, [n(&schema, "paragraph", [t(&schema, "ab")])]),
        Extension::all([history(HistoryConfig::default()), composition()]),
    );
    let state = run(
        &start,
        TransactionSpec::new().effect(begin_undo_group().of(())),
    );
    let state = run(&state, start_composition(CompositionRange::new(2, 2)));
    let state = run(&state, update_composition(&state, "n", 1).unwrap().time(0));
    assert!(crate::composition::is_composing(&state));

    let undone = run(&state, undo(&state).expect("something to undo"));
    assert_eq!(schema.describe(undone.doc()), r#"doc(paragraph("ab"))"#);
    assert!(
        !crate::composition::is_composing(&undone),
        "undo ends the composition"
    );

    // The group is closed too, so the next edit starts a fresh entry.
    let after = run(&undone, typed(&schema, 2, "X", 100));
    let after = run(&after, typed(&schema, 3, "Y", 10_000));
    assert_eq!(undo_depth(&after), 2);
}

#[test]
fn history_entries_survive_an_interleaved_remote_change() {
    let schema = shared_schema();
    let start = history_state(doc(
        &schema,
        [
            n(&schema, "paragraph", [t(&schema, "hello")]),
            n(&schema, "paragraph", [t(&schema, "world")]),
        ],
    ));
    let state = run(&start, typed(&schema, 3, "X", 0));
    assert_eq!(
        schema.describe(state.doc()),
        r#"doc(paragraph("heXllo"), paragraph("world"))"#
    );

    let state = run(
        &state,
        TransactionSpec::new()
            .changes([insert_text(&schema, 10, "Y")])
            .add_to_history(false)
            .remote(true)
            .time(1),
    );
    assert_eq!(
        schema.describe(state.doc()),
        r#"doc(paragraph("heXllo"), paragraph("wYorld"))"#
    );
    assert_eq!(undo_depth(&state), 1);

    let undone = run(&state, undo(&state).expect("something to undo"));
    assert_eq!(
        schema.describe(undone.doc()),
        r#"doc(paragraph("hello"), paragraph("wYorld"))"#,
        "the local edit is undone and the remote one survives"
    );
    undone.doc().check(&schema).unwrap();

    let redone = run(&undone, redo(&undone).expect("something to redo"));
    assert_eq!(
        schema.describe(redone.doc()),
        r#"doc(paragraph("heXllo"), paragraph("wYorld"))"#
    );
}

#[test]
fn min_depth_bounds_the_number_of_entries() {
    let schema = shared_schema();
    let start = state(
        doc(&schema, [n(&schema, "paragraph", [t(&schema, "hello")])]),
        history(HistoryConfig {
            min_depth: 2,
            ..HistoryConfig::default()
        }),
    );
    let mut state = start;
    for index in 0..4 {
        state = run(
            &state,
            typed(&schema, 3 + index, "x", 10_000 * index as u64),
        );
    }
    assert_eq!(undo_depth(&state), 2);
    let mut state = state;
    while let Some(spec) = undo(&state) {
        state = run(&state, spec);
    }
    assert_eq!(
        schema.describe(state.doc()),
        r#"doc(paragraph("hexxllo"))"#,
        "the two oldest edits stay applied because their entries were dropped"
    );
}

#[test]
fn selection_undo_restores_the_previous_selection() {
    let schema = shared_schema();
    let start = history_state(doc(
        &schema,
        [n(&schema, "paragraph", [t(&schema, "hello")])],
    ));
    let state = run(&start, typed(&schema, 3, "A", 0));
    let before = state.selection().clone();
    let state = run(
        &state,
        TransactionSpec::new()
            .selection(Selection::cursor(6))
            .user_event("select.pointer")
            .time(10),
    );
    assert_eq!(state.selection(), &Selection::cursor(6));

    let restored = run(&state, undo_selection(&state).expect("a selection to undo"));
    assert_eq!(restored.selection(), &before);
    assert_eq!(
        schema.describe(restored.doc()),
        r#"doc(paragraph("heAllo"))"#
    );

    let again = run(
        &restored,
        redo_selection(&restored).expect("a selection to redo"),
    );
    assert_eq!(again.selection(), &Selection::cursor(6));
}

#[test]
fn a_transaction_that_is_not_added_to_history_is_not_recorded() {
    let schema = shared_schema();
    let start = history_state(doc(
        &schema,
        [n(&schema, "paragraph", [t(&schema, "hello")])],
    ));
    let state = run(
        &start,
        TransactionSpec::new()
            .changes([insert_text(&schema, 3, "A")])
            .add_to_history(false),
    );
    assert_eq!(undo_depth(&state), 0);
    assert!(undo(&state).is_none());
}

#[test]
fn the_history_field_round_trips_through_json() {
    use crate::state::{EditorStateConfig, StateJsonFields};

    let schema = shared_schema();
    let start = history_state(doc(
        &schema,
        [n(&schema, "paragraph", [t(&schema, "hello")])],
    ));
    let state = run(&start, typed(&schema, 3, "A", 0));
    let fields = StateJsonFields::new().add("history", crate::history::history_field());
    let json = state.to_json(&fields);
    let restored = EditorState::from_json(
        &json,
        EditorStateConfig::new(schema.clone()).extensions(history(HistoryConfig::default())),
        &fields,
    )
    .unwrap();
    assert_eq!(undo_depth(&restored), 1);
    let undone = run(&restored, undo(&restored).expect("something to undo"));
    assert_eq!(schema.describe(undone.doc()), r#"doc(paragraph("hello"))"#);
}

/// What an input method does inside a modal editor's insert session: compose a
/// candidate, close the composition with the text it settled on, and keep
/// typing. The session is one entry and undoing it leaves nothing behind.
#[test]
fn a_composition_committed_inside_an_explicit_group_undoes_with_it() {
    let schema = shared_schema();
    let start = state(
        doc(&schema, [n(&schema, "paragraph", [t(&schema, "end")])]),
        Extension::all([history(HistoryConfig::default()), composition()]),
    );
    let state = run(
        &start,
        TransactionSpec::new()
            .effect(begin_undo_group().of(()))
            .time(0),
    );
    let state = run(&state, start_composition(CompositionRange::new(4, 4)));
    let state = run(&state, update_composition(&state, "h", 1).unwrap().time(1));
    let state = run(&state, update_composition(&state, "hi", 2).unwrap().time(2));
    assert_eq!(schema.describe(state.doc()), r#"doc(paragraph("endhi"))"#);

    // Closing the composition is its last step, so it says so rather than
    // starting an entry of its own.
    let state = run(
        &state,
        finish_composition()
            .user_event(crate::composition::COMPOSE_USER_EVENT)
            .time(3),
    );
    let state = run(&state, typed(&schema, 6, "!", 4));
    assert_eq!(schema.describe(state.doc()), r#"doc(paragraph("endhi!"))"#);

    let state = run(
        &state,
        TransactionSpec::new()
            .effect(end_undo_group().of(()))
            .time(5),
    );
    assert_eq!(undo_depth(&state), 1, "the whole session is one entry");
    let undone = run(&state, undo(&state).expect("something to undo"));
    assert_eq!(
        schema.describe(undone.doc()),
        r#"doc(paragraph("end"))"#,
        "the composed text is gone"
    );
    assert_eq!(undo_depth(&undone), 0);
}

/// A spec that only establishes the range a later one edits must not decide
/// whether the transaction is recorded.
#[test]
fn a_later_spec_has_the_last_word_on_an_annotation() {
    let schema = shared_schema();
    let start = history_state(doc(&schema, [n(&schema, "paragraph", [t(&schema, "ab")])]));
    // The first spec keeps itself out of the history; the second says the
    // transaction is typing after all, and the transaction is recorded.
    let state = run(
        &start,
        TransactionSpec::new()
            .selection(Selection::cursor(1))
            .add_to_history(false),
    );
    assert_eq!(undo_depth(&state), 0);
    let tr = state
        .update([
            TransactionSpec::new()
                .selection(Selection::text(1, 3))
                .add_to_history(false),
            typed(&schema, 3, "C", 0).add_to_history(true).sequential(),
        ])
        .expect("a valid transaction");
    assert_eq!(tr.annotation(crate::state::add_to_history()), Some(&true));
    let state = tr.state().clone();
    assert_eq!(undo_depth(&state), 1);
    let undone = run(&state, undo(&state).expect("something to undo"));
    assert_eq!(schema.describe(undone.doc()), r#"doc(paragraph("ab"))"#);
}
