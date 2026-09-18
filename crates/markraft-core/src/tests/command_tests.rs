//! The general command catalogue, exercised against the shared test schema.

use crate::attr::Attrs;
use crate::commands::*;
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

fn try_run(state: &EditorState, command: &Command) -> Option<EditorState> {
    Some(
        run_command(state, command)?
            .expect("the transaction resolves")
            .state()
            .clone(),
    )
}

fn at(state: &EditorState, pos: usize) -> EditorState {
    state
        .update([TransactionSpec::new().selection(Selection::cursor(pos))])
        .expect("selection is valid")
        .state()
        .clone()
}

fn text_selection(state: &EditorState, anchor: usize, head: usize) -> EditorState {
    state
        .update([TransactionSpec::new().selection(Selection::text(anchor, head))])
        .expect("selection is valid")
        .state()
        .clone()
}

// --- deleting -------------------------------------------------------------

#[test]
fn delete_selection_removes_the_selected_range() {
    let schema = shared_schema();
    let start = state(
        doc(&schema, [n(&schema, "paragraph", [t(&schema, "abcd")])]),
        Extension::none(),
    );
    let selected = text_selection(&start, 2, 4);
    let after = run(&selected, &delete_selection());
    assert_eq!(schema.describe(after.doc()), r#"doc(paragraph("ad"))"#);
    assert!(try_run(&start, &delete_selection()).is_none());
}

#[test]
fn delete_range_drops_a_block_it_would_empty() {
    let schema = shared_schema();
    let start = state(
        doc(
            &schema,
            [
                n(&schema, "paragraph", [t(&schema, "ab")]),
                n(&schema, "paragraph", [t(&schema, "cd")]),
            ],
        ),
        Extension::none(),
    );
    // Emptying a paragraph is allowed, so only its content goes.
    let after = run(&start, &delete_range(5, 7));
    assert_eq!(
        schema.describe(after.doc()),
        r#"doc(paragraph("ab"), paragraph())"#
    );
    // A range that covers both blocks' content collapses them into one.
    let merged = run(&start, &delete_range(1, 7));
    assert_eq!(schema.describe(merged.doc()), r#"doc(paragraph())"#);
}

#[test]
fn join_backward_merges_two_paragraphs() {
    let schema = shared_schema();
    let start = state(
        doc(
            &schema,
            [
                n(&schema, "paragraph", [t(&schema, "ab")]),
                n(&schema, "paragraph", [t(&schema, "cd")]),
            ],
        ),
        Extension::none(),
    );
    let after = run(&at(&start, 5), &join_backward());
    assert_eq!(schema.describe(after.doc()), r#"doc(paragraph("abcd"))"#);
    assert_eq!(after.selection(), &Selection::cursor(3));
}

#[test]
fn join_backward_lifts_a_paragraph_out_of_a_blockquote() {
    let schema = shared_schema();
    let start = state(
        doc(
            &schema,
            [n(
                &schema,
                "blockquote",
                [n(&schema, "paragraph", [t(&schema, "ab")])],
            )],
        ),
        Extension::none(),
    );
    let after = run(&at(&start, 2), &join_backward());
    assert_eq!(schema.describe(after.doc()), r#"doc(paragraph("ab"))"#);
}

#[test]
fn join_backward_pulls_a_paragraph_into_the_blockquote_before_it() {
    let schema = shared_schema();
    let start = state(
        doc(
            &schema,
            [
                n(
                    &schema,
                    "blockquote",
                    [n(&schema, "paragraph", [t(&schema, "ab")])],
                ),
                n(&schema, "paragraph", [t(&schema, "cd")]),
            ],
        ),
        Extension::none(),
    );
    let after = run(&at(&start, 7), &join_backward());
    assert_eq!(
        schema.describe(after.doc()),
        r#"doc(blockquote(paragraph("ab"), paragraph("cd")))"#
    );
}

#[test]
fn join_forward_merges_with_the_next_paragraph() {
    let schema = shared_schema();
    let start = state(
        doc(
            &schema,
            [
                n(&schema, "paragraph", [t(&schema, "ab")]),
                n(&schema, "paragraph", [t(&schema, "cd")]),
            ],
        ),
        Extension::none(),
    );
    let after = run(&at(&start, 3), &join_forward());
    assert_eq!(schema.describe(after.doc()), r#"doc(paragraph("abcd"))"#);
}

#[test]
fn join_textblock_backward_merges_across_a_wrapper() {
    let schema = shared_schema();
    let start = state(
        doc(
            &schema,
            [
                n(
                    &schema,
                    "blockquote",
                    [n(&schema, "paragraph", [t(&schema, "ab")])],
                ),
                n(&schema, "paragraph", [t(&schema, "cd")]),
            ],
        ),
        Extension::none(),
    );
    let after = run(&at(&start, 7), &join_textblock_backward());
    assert_eq!(
        schema.describe(after.doc()),
        r#"doc(blockquote(paragraph("abcd")))"#
    );
}

#[test]
fn select_node_backward_selects_an_atom_block() {
    let schema = shared_schema();
    let start = state(
        doc(
            &schema,
            [
                n(&schema, "horizontal_rule", []),
                n(&schema, "paragraph", [t(&schema, "ab")]),
            ],
        ),
        Extension::none(),
    );
    let after = run(&at(&start, 2), &select_node_backward());
    assert_eq!(after.selection(), &Selection::node(0));
}

// --- splitting ------------------------------------------------------------

#[test]
fn split_block_in_the_middle_of_a_paragraph() {
    let schema = shared_schema();
    let start = state(
        doc(&schema, [n(&schema, "paragraph", [t(&schema, "abcd")])]),
        Extension::none(),
    );
    let after = run(&at(&start, 3), &split_block());
    assert_eq!(
        schema.describe(after.doc()),
        r#"doc(paragraph("ab"), paragraph("cd"))"#
    );
    assert_eq!(after.selection(), &Selection::cursor(5));
}

#[test]
fn split_block_at_the_end_of_a_heading_starts_a_paragraph() {
    let schema = shared_schema();
    let start = state(
        doc(
            &schema,
            [na(
                &schema,
                "heading",
                crate::attrs! {"level" => 2i64},
                [t(&schema, "Title")],
            )],
        ),
        Extension::none(),
    );
    let after = run(&at(&start, 6), &split_block());
    assert_eq!(
        schema.describe(after.doc()),
        r#"doc(heading[level=Int(2)]("Title"), paragraph())"#
    );
}

#[test]
fn split_block_in_the_middle_of_a_heading_keeps_both_headings() {
    let schema = shared_schema();
    let start = state(
        doc(&schema, [n(&schema, "heading", [t(&schema, "Title")])]),
        Extension::none(),
    );
    let after = run(&at(&start, 3), &split_block());
    assert_eq!(
        schema.describe(after.doc()),
        r#"doc(heading[level=Int(1)]("Ti"), heading[level=Int(1)]("tle"))"#
    );
}

#[test]
fn split_block_at_the_start_of_a_heading_leaves_a_paragraph_above() {
    let schema = shared_schema();
    let start = state(
        doc(&schema, [n(&schema, "heading", [t(&schema, "Title")])]),
        Extension::none(),
    );
    let after = run(&at(&start, 1), &split_block());
    assert_eq!(
        schema.describe(after.doc()),
        r#"doc(paragraph(), heading[level=Int(1)]("Title"))"#
    );
}

#[test]
fn split_block_keep_marks_carries_the_marks_at_the_cursor() {
    let schema = shared_schema();
    let start = state(
        doc(
            &schema,
            [n(&schema, "paragraph", [tm(&schema, "abcd", &["strong"])])],
        ),
        Extension::none(),
    );
    let after = run(&at(&start, 3), &split_block_keep_marks());
    let marks = after.selection().stored_marks().expect("stored marks");
    assert!(marks.contains_type(schema.mark_id("strong").expect("known")));
}

#[test]
fn lift_empty_block_leaves_a_blockquote() {
    let schema = shared_schema();
    let start = state(
        doc(
            &schema,
            [n(
                &schema,
                "blockquote",
                [
                    n(&schema, "paragraph", [t(&schema, "ab")]),
                    n(&schema, "paragraph", []),
                ],
            )],
        ),
        Extension::none(),
    );
    let after = run(&at(&start, 6), &lift_empty_block());
    assert_eq!(
        schema.describe(after.doc()),
        r#"doc(blockquote(paragraph("ab")), paragraph())"#
    );
}

#[test]
fn new_line_in_code_inserts_a_newline_and_exit_code_leaves_the_block() {
    let schema = shared_schema();
    let start = state(
        doc(&schema, [n(&schema, "code_block", [t(&schema, "ab")])]),
        Extension::none(),
    );
    let after = run(&at(&start, 3), &new_line_in_code());
    assert_eq!(schema.describe(after.doc()), "doc(code_block(\"ab\n\"))");
    let out = run(&after, &exit_code());
    assert_eq!(
        schema.describe(out.doc()),
        "doc(code_block(\"ab\n\"), paragraph())"
    );
}

#[test]
fn create_paragraph_near_a_selected_rule() {
    let schema = shared_schema();
    let start = state(
        doc(
            &schema,
            [
                n(&schema, "horizontal_rule", []),
                n(&schema, "paragraph", [t(&schema, "ab")]),
            ],
        ),
        Extension::none(),
    );
    let selected = start
        .update([TransactionSpec::new().selection(Selection::node(0))])
        .expect("valid")
        .state()
        .clone();
    let after = run(&selected, &create_paragraph_near());
    assert_eq!(
        schema.describe(after.doc()),
        r#"doc(paragraph(), horizontal_rule, paragraph("ab"))"#
    );
}

// --- wrapping and re-typing ----------------------------------------------

#[test]
fn wrap_in_and_lift_round_trip() {
    let schema = shared_schema();
    let start = state(
        doc(&schema, [n(&schema, "paragraph", [t(&schema, "ab")])]),
        Extension::none(),
    );
    let quote = schema.node_id("blockquote").expect("known");
    let wrapped = run(&at(&start, 2), &wrap_in(quote, Attrs::empty()));
    assert_eq!(
        schema.describe(wrapped.doc()),
        r#"doc(blockquote(paragraph("ab")))"#
    );
    let lifted = run(&wrapped, &lift());
    assert_eq!(schema.describe(lifted.doc()), r#"doc(paragraph("ab"))"#);
}

#[test]
fn set_block_type_to_a_heading_and_back() {
    let schema = shared_schema();
    let start = state(
        doc(&schema, [n(&schema, "paragraph", [t(&schema, "ab")])]),
        Extension::none(),
    );
    let heading = schema.node_id("heading").expect("known");
    let paragraph = schema.node_id("paragraph").expect("known");
    let after = run(
        &at(&start, 2),
        &set_block_type(heading, crate::attrs! {"level" => 3i64}),
    );
    assert_eq!(
        schema.describe(after.doc()),
        r#"doc(heading[level=Int(3)]("ab"))"#
    );
    let back = run(&after, &set_block_type(paragraph, Attrs::empty()));
    assert_eq!(schema.describe(back.doc()), r#"doc(paragraph("ab"))"#);
}

#[test]
fn set_block_type_to_code_strips_marks_the_new_type_forbids() {
    let schema = shared_schema();
    let start = state(
        doc(
            &schema,
            [n(
                &schema,
                "paragraph",
                [t(&schema, "a"), tm(&schema, "bc", &["strong", "em"])],
            )],
        ),
        Extension::none(),
    );
    let code = schema.node_id("code_block").expect("known");
    let after = run(&at(&start, 2), &set_block_type(code, Attrs::empty()));
    assert_eq!(schema.describe(after.doc()), r#"doc(code_block("abc"))"#);
}

#[test]
fn backspace_before_a_rule_selects_it_rather_than_deleting_it() {
    let schema = shared_schema();
    let start = state(
        doc(
            &schema,
            [
                n(&schema, "horizontal_rule", []),
                n(&schema, "paragraph", [t(&schema, "ab")]),
            ],
        ),
        Extension::none(),
    );
    let backspace = chain([delete_selection(), join_backward(), select_node_backward()]);
    let after = run(&at(&start, 2), &backspace);
    assert_eq!(
        schema.describe(after.doc()),
        r#"doc(horizontal_rule, paragraph("ab"))"#
    );
    assert_eq!(after.selection(), &Selection::node(0));
    // Pressing it again deletes the now-selected rule.
    let gone = run(&after, &backspace);
    assert_eq!(schema.describe(gone.doc()), r#"doc(paragraph("ab"))"#);
}

#[test]
fn backspace_removes_an_empty_paragraph_before_the_cursor() {
    let schema = shared_schema();
    let start = state(
        doc(
            &schema,
            [
                n(&schema, "paragraph", []),
                n(&schema, "paragraph", [t(&schema, "ab")]),
            ],
        ),
        Extension::none(),
    );
    let after = run(&at(&start, 3), &join_backward());
    assert_eq!(schema.describe(after.doc()), r#"doc(paragraph("ab"))"#);
}

// --- deleting by grapheme and by word -------------------------------------

#[test]
fn delete_by_grapheme_takes_one_cluster_and_stops_at_a_block_boundary() {
    let schema = shared_schema();
    let start = state(
        doc(
            &schema,
            [
                n(
                    &schema,
                    "paragraph",
                    [t(&schema, "a👩\u{200d}👩\u{200d}👧")],
                ),
                n(&schema, "paragraph", [t(&schema, "b")]),
            ],
        ),
        crate::projection::projection(),
    );
    // The whole cluster goes, not one scalar of it.
    let end = at(&start, 1 + "a👩\u{200d}👩\u{200d}👧".chars().count());
    let after = run(&end, &delete_by_grapheme(Direction::Backward));
    assert_eq!(
        schema.describe(after.doc()),
        r#"doc(paragraph("a"), paragraph("b"))"#
    );
    // At the start of the second block there is no character to delete: joining
    // the two blocks is `join_backward`'s job, so the command does not apply.
    let block_start = at(&start, 1 + "a👩\u{200d}👩\u{200d}👧".chars().count() + 2);
    assert!(try_run(&block_start, &delete_by_grapheme(Direction::Backward)).is_none());
    // Forward from the end of the first block likewise.
    assert!(try_run(&end, &delete_by_grapheme(Direction::Forward)).is_none());
}

#[test]
fn delete_by_word_takes_a_word_and_a_selection_wins_over_both() {
    let schema = shared_schema();
    let start = state(
        doc(&schema, [n(&schema, "paragraph", [t(&schema, "one two")])]),
        crate::projection::projection(),
    );
    let end = at(&start, 8);
    let after = run(&end, &delete_by_word(Direction::Backward));
    assert_eq!(schema.describe(after.doc()), r#"doc(paragraph("one "))"#);
    let begin = at(&start, 1);
    let after = run(&begin, &delete_by_word(Direction::Forward));
    assert_eq!(schema.describe(after.doc()), r#"doc(paragraph(" two"))"#);
    // With a range selected, the range goes whatever the direction.
    let selected = text_selection(&start, 2, 5);
    let after = run(&selected, &delete_by_word(Direction::Backward));
    assert_eq!(schema.describe(after.doc()), r#"doc(paragraph("otwo"))"#);
}
