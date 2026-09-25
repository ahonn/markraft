//! The list commands, and the key bindings built on them.

use crate::attr::Attrs;
use crate::commands::*;
use crate::schema::NodeTypeId;
use crate::selection::Selection;
use crate::state::{EditorState, Extension, TransactionSpec};

use super::support::*;

fn run(state: &EditorState, command: &Command) -> EditorState {
    run_command(state, command)
        .expect("the command applies")
        .expect("the transaction resolves")
        .state()
        .clone()
}

fn at(state: &EditorState, pos: usize) -> EditorState {
    state
        .update([TransactionSpec::new().selection(Selection::cursor(pos))])
        .expect("selection is valid")
        .state()
        .clone()
}

fn ids() -> (NodeTypeId, NodeTypeId, NodeTypeId) {
    let schema = shared_schema();
    (
        schema.node_id("bullet_list").expect("known"),
        schema.node_id("list_item").expect("known"),
        schema.node_id("task_item").expect("known"),
    )
}

/// `doc(bullet_list(list_item(paragraph("a")), list_item(paragraph("b"))))`
fn two_item_list() -> EditorState {
    let schema = shared_schema();
    let item = |text: &str| {
        n(
            &schema,
            "list_item",
            [n(&schema, "paragraph", [t(&schema, text)])],
        )
    };
    state(
        doc(&schema, [n(&schema, "bullet_list", [item("a"), item("b")])]),
        Extension::none(),
    )
}

#[test]
fn wrap_in_list_makes_one_item_per_block() {
    let schema = shared_schema();
    let (list, _, _) = ids();
    let start = state(
        doc(
            &schema,
            [
                n(&schema, "paragraph", [t(&schema, "a")]),
                n(&schema, "paragraph", [t(&schema, "b")]),
            ],
        ),
        Extension::none(),
    );
    let selected = start
        .update([TransactionSpec::new().selection(Selection::text(1, 5))])
        .expect("valid")
        .state()
        .clone();
    let after = run(&selected, &wrap_in_list(list, Attrs::empty()));
    assert_eq!(
        schema.describe(after.doc()),
        r#"doc(bullet_list(list_item(paragraph("a")), list_item(paragraph("b"))))"#
    );
}

#[test]
fn wrap_in_list_inside_an_item_nests_under_the_item_before_it() {
    let schema = shared_schema();
    let (list, _, _) = ids();
    let start = two_item_list();
    let after = run(&at(&start, 8), &wrap_in_list(list, Attrs::empty()));
    assert_eq!(
        schema.describe(after.doc()),
        r#"doc(bullet_list(list_item(paragraph("a"), bullet_list(list_item(paragraph("b"))))))"#
    );
}

#[test]
fn split_list_item_makes_a_new_item() {
    let schema = shared_schema();
    let (_, item, _) = ids();
    let start = two_item_list();
    let after = run(&at(&start, 9), &split_list_item(item));
    assert_eq!(
        schema.describe(after.doc()),
        r#"doc(bullet_list(list_item(paragraph("a")), list_item(paragraph("b")), list_item(paragraph())))"#
    );
    assert_eq!(after.selection(), &Selection::cursor(13));
}

#[test]
fn split_list_item_keeps_a_task_item_type() {
    let schema = shared_schema();
    let (_, _, task) = ids();
    let start = state(
        doc(
            &schema,
            [n(
                &schema,
                "bullet_list",
                [na(
                    &schema,
                    "task_item",
                    crate::attrs! {"checked" => true},
                    [n(&schema, "paragraph", [t(&schema, "a")])],
                )],
            )],
        ),
        Extension::none(),
    );
    let after = run(&at(&start, 4), &split_list_item(task));
    assert_eq!(
        schema.describe(after.doc()),
        r#"doc(bullet_list(task_item[checked=Bool(true)](paragraph("a")), task_item[checked=Bool(true)](paragraph())))"#
    );
}

#[test]
fn enter_in_an_empty_list_item_lifts_it_out() {
    let schema = shared_schema();
    let (_, item, _) = ids();
    let start = two_item_list();
    // Add an empty third item, then press Enter in it.
    let with_empty = run(&at(&start, 9), &split_list_item(item));
    let enter = chain([split_list_item(item), lift_list_item(item), split_block()]);
    let after = run(&with_empty, &enter);
    assert_eq!(
        schema.describe(after.doc()),
        r#"doc(bullet_list(list_item(paragraph("a")), list_item(paragraph("b"))), paragraph())"#
    );
}

#[test]
fn lift_list_item_moves_an_item_out_of_a_top_level_list() {
    let schema = shared_schema();
    let (_, item, _) = ids();
    let start = two_item_list();
    let after = run(&at(&start, 8), &lift_list_item(item));
    assert_eq!(
        schema.describe(after.doc()),
        r#"doc(bullet_list(list_item(paragraph("a"))), paragraph("b"))"#
    );
    let first = run(&at(&start, 3), &lift_list_item(item));
    assert_eq!(
        schema.describe(first.doc()),
        r#"doc(paragraph("a"), bullet_list(list_item(paragraph("b"))))"#
    );
}

#[test]
fn sink_and_lift_a_list_item_round_trip() {
    let schema = shared_schema();
    let (_, item, _) = ids();
    let start = two_item_list();
    let sunk = run(&at(&start, 8), &sink_list_item(item));
    assert_eq!(
        schema.describe(sunk.doc()),
        r#"doc(bullet_list(list_item(paragraph("a"), bullet_list(list_item(paragraph("b"))))))"#
    );
    // The cursor followed the item; lifting it puts it back.
    let lifted = run(&at(&sunk, 10), &lift_list_item(item));
    assert_eq!(
        schema.describe(lifted.doc()),
        r#"doc(bullet_list(list_item(paragraph("a")), list_item(paragraph("b"))))"#
    );
}

#[test]
fn sink_appends_to_an_existing_nested_list() {
    let schema = shared_schema();
    let (_, item, _) = ids();
    let plain = |text: &str| {
        n(
            &schema,
            "list_item",
            [n(&schema, "paragraph", [t(&schema, text)])],
        )
    };
    let start = state(
        doc(
            &schema,
            [n(
                &schema,
                "bullet_list",
                [
                    n(
                        &schema,
                        "list_item",
                        [
                            n(&schema, "paragraph", [t(&schema, "a")]),
                            n(&schema, "bullet_list", [plain("b")]),
                        ],
                    ),
                    plain("c"),
                ],
            )],
        ),
        Extension::none(),
    );
    // A third top-level item joins the existing nested list rather than making
    // a second one.
    let after = run(&at(&start, 15), &sink_list_item(item));
    assert_eq!(
        schema.describe(after.doc()),
        r#"doc(bullet_list(list_item(paragraph("a"), bullet_list(list_item(paragraph("b")), list_item(paragraph("c"))))))"#
    );
}

#[test]
fn sink_does_not_apply_to_the_first_item() {
    let (_, item, _) = ids();
    let start = two_item_list();
    let sink = sink_list_item(item);
    assert!(sink(&at(&start, 3)).is_none());
}

#[test]
fn lift_nested_items_into_the_outer_list() {
    let schema = shared_schema();
    let (_, item, _) = ids();
    let inner = |text: &str| {
        n(
            &shared_schema(),
            "list_item",
            [n(
                &shared_schema(),
                "paragraph",
                [t(&shared_schema(), text)],
            )],
        )
    };
    let start = state(
        doc(
            &schema,
            [n(
                &schema,
                "bullet_list",
                [n(
                    &schema,
                    "list_item",
                    [
                        n(&schema, "paragraph", [t(&schema, "a")]),
                        n(&schema, "bullet_list", [inner("b"), inner("c")]),
                    ],
                )],
            )],
        ),
        Extension::none(),
    );
    // Cursor inside "b".
    let after = run(&at(&start, 8), &lift_list_item(item));
    assert_eq!(
        schema.describe(after.doc()),
        r#"doc(bullet_list(list_item(paragraph("a")), list_item(paragraph("b"), bullet_list(list_item(paragraph("c"))))))"#
    );
}

#[test]
fn backspace_at_the_start_of_a_later_item_joins_it_with_the_one_above() {
    let schema = shared_schema();
    let (_, item, _) = ids();
    let start = two_item_list();
    let backspace = chain([delete_selection(), join_backward(), select_node_backward()]);
    let after = run(&at(&start, 8), &backspace);
    assert_eq!(
        schema.describe(after.doc()),
        r#"doc(bullet_list(list_item(paragraph("a"), paragraph("b"))))"#
    );
    let _ = item;
}

#[test]
fn backspace_at_the_start_of_the_first_item_lifts_it() {
    let schema = shared_schema();
    let (_, item, _) = ids();
    let start = two_item_list();
    let backspace = chain([
        delete_selection(),
        lift_list_item(item),
        join_backward(),
        select_node_backward(),
    ]);
    let after = run(&at(&start, 3), &backspace);
    assert_eq!(
        schema.describe(after.doc()),
        r#"doc(paragraph("a"), bullet_list(list_item(paragraph("b"))))"#
    );
}
