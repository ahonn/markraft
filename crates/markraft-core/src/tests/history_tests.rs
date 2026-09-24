//! Undo history: grouping, explicit groups, compositions, remote changes.

use super::support::*;
use crate::change::Change;
use crate::composition::CompositionRange;
use crate::composition::{composition, finish_composition, start_composition, update_composition};
use crate::fit::Fit;
use crate::history::{
    HistoryConfig, begin_undo_group, end_undo_group, history, redo, redo_depth, redo_selection,
    undo, undo_depth, undo_selection,
};
use crate::node::{Markup, Node};
use crate::protocol::{IsolateHistory, isolate};
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
fn typing_in_two_places_in_quick_succession_makes_two_entries() {
    let schema = shared_schema();
    let start = history_state(doc(
        &schema,
        [n(&schema, "paragraph", [t(&schema, "hello world")])],
    ));
    // Close in time and the same kind of edit, but not touching: moving the
    // caret between them starts a new step, so undo takes back one at a time.
    let state = run(&start, typed(&schema, 1, "A", 0));
    let state = run(&state, typed(&schema, 9, "B", 10));
    assert_eq!(undo_depth(&state), 2);
    let undone = run(&state, undo(&state).expect("something to undo"));
    assert_eq!(
        schema.describe(undone.doc()),
        r#"doc(paragraph("Ahello world"))"#
    );
}

#[test]
fn a_composition_after_a_pause_is_its_own_entry() {
    let schema = shared_schema();
    let start = state(
        doc(&schema, [n(&schema, "paragraph", [t(&schema, "ab")])]),
        Extension::all([history(HistoryConfig::default()), composition()]),
    );
    // Composing is typing, so it folds into typing right before it; after a
    // pause it starts its own entry like any other typing would.
    let quick = run(&start, typed(&schema, 2, "X", 0));
    let quick = run(&quick, start_composition(CompositionRange::new(3, 3)));
    let quick = run(&quick, update_composition(&quick, "n", 1).unwrap().time(1));
    assert_eq!(schema.describe(quick.doc()), r#"doc(paragraph("aXnb"))"#);
    assert_eq!(undo_depth(&quick), 1);

    let paused = run(&start, typed(&schema, 2, "X", 0));
    let paused = run(&paused, start_composition(CompositionRange::new(3, 3)));
    let paused = run(
        &paused,
        update_composition(&paused, "n", 1).unwrap().time(5_000),
    );
    assert_eq!(undo_depth(&paused), 2);
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
fn a_redo_lands_where_a_remote_change_moved_its_place() {
    let schema = shared_schema();
    let start = history_state(doc(
        &schema,
        [n(&schema, "paragraph", [t(&schema, "hello")])],
    ));
    let state = run(&start, typed(&schema, 3, "X", 0));
    let undone = run(&state, undo(&state).expect("something to undo"));
    // Someone else inserts before the place the undone edit was made.
    let moved = run(
        &undone,
        TransactionSpec::new()
            .changes([insert_text(&schema, 1, "Z")])
            .add_to_history(false)
            .remote(true)
            .time(1),
    );
    let redone = run(&moved, redo(&moved).expect("something to redo"));
    assert_eq!(
        schema.describe(redone.doc()),
        r#"doc(paragraph("ZheXllo"))"#
    );
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

/// A remote change that deletes everything an entry did leaves nothing to
/// undo in it: the entry goes, and undo reaches the one below.
#[test]
fn an_entry_a_remote_change_empties_is_dropped() {
    let schema = shared_schema();
    let start = history_state(doc(
        &schema,
        [
            n(&schema, "paragraph", [t(&schema, "hello")]),
            n(&schema, "paragraph", [t(&schema, "world")]),
        ],
    ));
    let state = run(&start, typed(&schema, 3, "X", 0));
    let state = run(&state, typed(&schema, 10, "Y", 10_000));
    assert_eq!(undo_depth(&state), 2);
    // The remote side deletes the second paragraph's text, "Y" with it.
    let state = run(
        &state,
        TransactionSpec::new()
            .changes([Change::delete(9, 15)])
            .add_to_history(false)
            .remote(true)
            .time(20_000),
    );
    assert_eq!(
        schema.describe(state.doc()),
        r#"doc(paragraph("heXllo"), paragraph())"#
    );
    assert_eq!(undo_depth(&state), 1);
    let undone = run(&state, undo(&state).expect("something to undo"));
    assert_eq!(
        schema.describe(undone.doc()),
        r#"doc(paragraph("hello"), paragraph())"#
    );
}

/// Moving the caret after an edit records where it went, for selection undo;
/// a plain undo still takes the edit back.
#[test]
fn undo_after_moving_the_caret_undoes_the_edit() {
    let schema = shared_schema();
    let start = history_state(doc(
        &schema,
        [n(&schema, "paragraph", [t(&schema, "hello")])],
    ));
    let state = run(&start, typed(&schema, 3, "A", 0));
    let state = run(
        &state,
        TransactionSpec::new()
            .selection(Selection::cursor(6))
            .user_event("select.pointer")
            .time(10),
    );
    let undone = run(&state, undo(&state).expect("something to undo"));
    assert_eq!(schema.describe(undone.doc()), r#"doc(paragraph("hello"))"#);
}

#[test]
fn undo_and_redo_say_which_they_are() {
    let schema = shared_schema();
    let start = history_state(doc(
        &schema,
        [n(&schema, "paragraph", [t(&schema, "hello")])],
    ));
    let state = run(&start, typed(&schema, 3, "A", 0));
    let event = |state: &EditorState, spec: TransactionSpec| {
        state
            .update([spec])
            .unwrap()
            .annotation(crate::state::protocol::user_event())
            .cloned()
    };
    assert_eq!(
        event(&state, undo(&state).unwrap()).as_deref(),
        Some("undo")
    );
    let undone = run(&state, undo(&state).unwrap());
    assert_eq!(
        event(&undone, redo(&undone).unwrap()).as_deref(),
        Some("redo")
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
            .user_event(crate::protocol::COMPOSE_USER_EVENT)
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
    assert_eq!(
        tr.annotation(crate::protocol::add_to_history()),
        Some(&true)
    );
    let state = tr.state().clone();
    assert_eq!(undo_depth(&state), 1);
    let undone = run(&state, undo(&state).expect("something to undo"));
    assert_eq!(schema.describe(undone.doc()), r#"doc(paragraph("ab"))"#);
}

#[test]
fn a_failed_rebase_is_reported_once_and_cleared_by_the_next_transaction() {
    use crate::history::history_lost;
    use crate::state::{EditorStateConfig, StateJsonFields};

    let schema = shared_schema();
    let document = doc(&schema, [n(&schema, "paragraph", [t(&schema, "hello")])]);
    // A persisted entry that belongs to some other document: its change set
    // starts from 99 tokens, so no edit of this one can be rebased under it.
    let fields = StateJsonFields::new().add("history", crate::history::history_field());
    let mut json = history_state(document.clone()).to_json(&fields);
    json["history"] = serde_json::json!({
        "done": [{"changes": {"length": 99, "sections": [{"len": 99}]}}],
        "undone": [],
    });
    let start = EditorState::from_json(
        &json,
        EditorStateConfig::new(schema.clone()).extensions(history(HistoryConfig::default())),
        &fields,
    )
    .unwrap();
    assert_eq!(undo_depth(&start), 1);
    assert!(!history_lost(&start));

    let remote = run(
        &start,
        TransactionSpec::new()
            .changes([insert_text(&schema, 1, "X")])
            .add_to_history(false),
    );
    assert!(history_lost(&remote), "the entry could not be rebased");
    assert_eq!(undo_depth(&remote), 0);
    assert!(undo(&remote).is_none());

    // The next transaction, of any kind, clears the report.
    let moved = run(
        &remote,
        TransactionSpec::new().selection(Selection::cursor(2)),
    );
    assert!(!history_lost(&moved));
    let typed_after = run(&remote, typed(&schema, 2, "y", 0));
    assert!(!history_lost(&typed_after));

    // A rebase that succeeds reports nothing.
    let fine = history_state(document);
    let fine = run(&fine, typed(&schema, 1, "a", 0));
    let fine = run(
        &fine,
        TransactionSpec::new()
            .changes([insert_text(&schema, 1, "Z")])
            .add_to_history(false),
    );
    assert!(!history_lost(&fine));
    assert_eq!(undo_depth(&fine), 1);
}

#[test]
fn two_compositions_apart_in_time_are_two_entries() {
    let schema = shared_schema();
    let start = state(
        doc(&schema, [n(&schema, "paragraph", [t(&schema, "ab")])]),
        Extension::all([history(HistoryConfig::default()), composition()]),
    );
    // Each marked range is started explicitly, as an input method that names
    // the range it replaces does.
    let state = run(&start, start_composition(CompositionRange::new(2, 2)));
    let state = run(&state, update_composition(&state, "n", 1).unwrap().time(0));
    let state = run(
        &state,
        update_composition(&state, "\u{4f60}", 1).unwrap().time(1),
    );
    let state = run(&state, finish_composition());
    let state = run(&state, start_composition(CompositionRange::new(3, 3)));
    let state = run(
        &state,
        update_composition(&state, "h", 1).unwrap().time(10_000),
    );
    let state = run(
        &state,
        update_composition(&state, "\u{597d}", 1)
            .unwrap()
            .time(10_001),
    );
    let state = run(&state, finish_composition());
    assert_eq!(
        schema.describe(state.doc()),
        "doc(paragraph(\"a\u{4f60}\u{597d}b\"))"
    );
    assert_eq!(undo_depth(&state), 2);
    let undone = run(&state, undo(&state).expect("something to undo"));
    assert_eq!(
        schema.describe(undone.doc()),
        "doc(paragraph(\"a\u{4f60}b\"))"
    );
}
