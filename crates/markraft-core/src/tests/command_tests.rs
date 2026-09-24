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

/// An empty block after a container joins it by being deleted, and the caret
/// goes to the end of the container's content — not on to the next text,
/// which here is in the list after the paragraph.
#[test]
fn join_backward_from_an_empty_block_after_a_container_ends_up_before_the_cut() {
    let schema = shared_schema();
    let list = |kind: &str, text: &str| {
        n(
            &schema,
            kind,
            [n(
                &schema,
                "list_item",
                [n(&schema, "paragraph", [t(&schema, text)])],
            )],
        )
    };
    let start = state(
        doc(
            &schema,
            [
                list("ordered_list", "ab"),
                n(&schema, "paragraph", []),
                list("bullet_list", "cd"),
            ],
        ),
        Extension::none(),
    );
    let after = run(&at(&start, 9), &join_backward());
    assert_eq!(
        schema.describe(after.doc()),
        r#"doc(ordered_list(list_item(paragraph("ab"))), bullet_list(list_item(paragraph("cd"))))"#
    );
    assert_eq!(after.selection(), &Selection::cursor(5));

    let start = state(
        doc(
            &schema,
            [
                n(
                    &schema,
                    "blockquote",
                    [n(&schema, "paragraph", [t(&schema, "ab")])],
                ),
                n(&schema, "paragraph", []),
                n(&schema, "paragraph", [t(&schema, "cd")]),
            ],
        ),
        Extension::none(),
    );
    let after = run(&at(&start, 7), &join_backward());
    assert_eq!(
        schema.describe(after.doc()),
        r#"doc(blockquote(paragraph("ab")), paragraph("cd"))"#
    );
    assert_eq!(after.selection(), &Selection::cursor(4));
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
    let marks = after.stored_marks().expect("stored marks");
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

#[test]
fn typing_over_all_keeps_the_caret_inside_the_fitted_paragraph() {
    let schema = shared_schema();
    let start = state(
        doc(&schema, [n(&schema, "paragraph", [t(&schema, "hello")])]),
        Extension::none(),
    );
    let selected = start
        .update([TransactionSpec::new().selection(Selection::All)])
        .unwrap()
        .state()
        .clone();
    let typed = run(
        &run(
            &run(&selected, &crate::commands::insert_text("a")),
            &crate::commands::insert_text("b"),
        ),
        &crate::commands::insert_text("c"),
    );
    assert_eq!(schema.describe(typed.doc()), r#"doc(paragraph("abc"))"#);
    assert_eq!(typed.selection(), &Selection::cursor(4));
}

#[test]
fn enter_deletes_a_cross_quote_selection_before_splitting() {
    let schema = shared_schema();
    let start = state(
        doc(
            &schema,
            [
                n(&schema, "paragraph", [t(&schema, "abc")]),
                n(
                    &schema,
                    "blockquote",
                    [n(&schema, "paragraph", [t(&schema, "def")])],
                ),
            ],
        ),
        Extension::none(),
    );
    let selected = text_selection(&start, 2, 8);
    let split = run(&selected, &split_block());
    assert_eq!(
        schema.describe(split.doc()),
        r#"doc(paragraph("a"), paragraph("ef"))"#
    );
    assert_eq!(split.selection(), &Selection::cursor(4));
}

#[test]
fn enter_inside_an_inline_container_splits_its_textblock() {
    let schema = shared_schema();
    let start = state(
        doc(
            &schema,
            [n(
                &schema,
                "paragraph",
                [n(&schema, "inline_span", [t(&schema, "abcd")])],
            )],
        ),
        Extension::none(),
    );
    let split = run(&at(&start, 4), &split_block());
    assert_eq!(
        schema.describe(split.doc()),
        r#"doc(paragraph(inline_span("ab")), paragraph(inline_span("cd")))"#
    );
    assert_eq!(split.selection(), &Selection::cursor(8));
}

#[test]
fn enter_inside_an_inline_container_splits_its_list_item() {
    let schema = shared_schema();
    let start = state(
        doc(
            &schema,
            [n(
                &schema,
                "bullet_list",
                [n(
                    &schema,
                    "list_item",
                    [n(
                        &schema,
                        "paragraph",
                        [n(&schema, "inline_span", [t(&schema, "abcd")])],
                    )],
                )],
            )],
        ),
        Extension::none(),
    );
    let split = run(
        &at(&start, 6),
        &split_list_item(schema.node_id("list_item").unwrap()),
    );
    assert_eq!(
        schema.describe(split.doc()),
        r#"doc(bullet_list(list_item(paragraph(inline_span("ab"))), list_item(paragraph(inline_span("cd")))))"#
    );
}

#[test]
fn block_commands_and_join_skip_inline_container_boundaries() {
    let schema = shared_schema();
    let start = state(
        doc(
            &schema,
            [
                n(&schema, "paragraph", [t(&schema, "a")]),
                n(
                    &schema,
                    "paragraph",
                    [n(&schema, "inline_span", [t(&schema, "b")])],
                ),
            ],
        ),
        Extension::none(),
    );
    let joined = run(&at(&start, 5), &join_backward());
    assert_eq!(
        schema.describe(joined.doc()),
        r#"doc(paragraph("a", inline_span("b")))"#
    );
    let wrapped = run(
        &at(&start, 5),
        &wrap_in(schema.node_id("blockquote").unwrap(), Attrs::empty()),
    );
    assert_eq!(
        schema.describe(wrapped.doc()),
        r#"doc(paragraph("a"), blockquote(paragraph(inline_span("b"))))"#
    );
}

#[test]
fn grapheme_motion_and_deletion_skip_inline_scope_boundaries() {
    let schema = shared_schema();
    let start = state(
        doc(
            &schema,
            [n(
                &schema,
                "paragraph",
                [
                    t(&schema, "a"),
                    n(&schema, "inline_span", [t(&schema, "bc")]),
                    t(&schema, "d"),
                ],
            )],
        ),
        Extension::none(),
    );
    let mut cursor = at(&start, 1);
    let mut offsets = Vec::new();
    for _ in 0..4 {
        cursor = run(&cursor, &move_by_grapheme(Direction::Forward, false));
        offsets.push(
            crate::projection::projection_of(&cursor)
                .pos_to_line_offset(cursor.selection().head(cursor.doc()))
                .unwrap()
                .1,
        );
    }
    assert_eq!(offsets, [1, 2, 3, 4]);
    let deleted = run(&at(&start, 1), &delete_by_grapheme(Direction::Forward));
    assert_eq!(
        crate::projection::projection_of(&deleted).plain_text(),
        "bcd"
    );
    let deleted = run(&at(&start, 6), &delete_by_grapheme(Direction::Backward));
    assert_eq!(
        crate::projection::projection_of(&deleted).plain_text(),
        "abd"
    );
}

#[test]
fn copying_from_an_inline_scope_retains_its_marks_when_pasted_into_plain_text() {
    let schema = shared_schema();
    let marks = crate::MarkSet::from_marks(&schema, [m(&schema, "strong")]);
    let span = n(&schema, "inline_span", [t(&schema, "abcd")]).mark(marks);
    let source = doc(&schema, [n(&schema, "paragraph", [span])]);
    let copied = Selection::text(3, 5).content_with_schema(&source, &schema);
    let target = state(
        doc(&schema, [n(&schema, "paragraph", [t(&schema, "xy")])]),
        Extension::none(),
    );
    let pasted = run(&at(&target, 2), &replace_selection(copied));
    assert_eq!(
        schema.describe(pasted.doc()),
        r#"doc(paragraph("x", inline_span{strong}("bc"), "y"))"#
    );
}

#[test]
fn converting_inline_scopes_to_code_keeps_visible_text_and_caret() {
    let schema = shared_schema();
    let span = n(&schema, "inline_span", [t(&schema, "abcd")])
        .mark(crate::MarkSet::from_marks(&schema, [m(&schema, "strong")]));
    let start = state(
        doc(&schema, [n(&schema, "paragraph", [span])]),
        Extension::none(),
    );
    let code = run(
        &at(&start, 4),
        &set_block_type(schema.node_id("code_block").unwrap(), Attrs::empty()),
    );
    assert_eq!(schema.describe(code.doc()), r#"doc(code_block("abcd"))"#);
    assert_eq!(code.selection().head(code.doc()), 3);
}

#[test]
fn arrows_leave_a_selected_rule_the_way_they_point() {
    let schema = shared_schema();
    let start = state(
        doc(
            &schema,
            [
                n(&schema, "paragraph", [t(&schema, "ab")]),
                n(&schema, "horizontal_rule", []),
                n(&schema, "paragraph", [t(&schema, "cd")]),
            ],
        ),
        Extension::none(),
    );
    // paragraph("ab") spans 0..4, the rule 4..5, paragraph("cd") 5..9.
    let selected = start
        .update([TransactionSpec::new().selection(Selection::node(4))])
        .expect("valid")
        .state()
        .clone();
    let forward = run(&selected, &move_by_grapheme(Direction::Forward, false));
    assert_eq!(
        forward.selection(),
        &Selection::cursor(6),
        "the start of the next line"
    );
    let backward = run(&selected, &move_by_grapheme(Direction::Backward, false));
    assert_eq!(
        backward.selection(),
        &Selection::cursor(3),
        "the end of the line before"
    );
}
