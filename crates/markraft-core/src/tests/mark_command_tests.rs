//! Marks, insertion, selection and motion commands.

use crate::attr::Attrs;
use crate::commands::*;
use crate::fragment::Fragment;
use crate::mark::MarkSet;
use crate::selection::Selection;
use crate::slice::Slice;
use crate::state::{EditorState, Extension, TransactionSpec};

use super::support::*;

// Disambiguates from the change-building helper of the same name in `support`.
use crate::commands::insert_text;

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

fn text_selection(state: &EditorState, anchor: usize, head: usize) -> EditorState {
    state
        .update([TransactionSpec::new().selection(Selection::text(anchor, head))])
        .expect("selection is valid")
        .state()
        .clone()
}

// --- marks ----------------------------------------------------------------

#[test]
fn toggle_mark_over_a_range_adds_then_removes() {
    let schema = shared_schema();
    let strong = schema.mark_id("strong").expect("known");
    let start = state(
        doc(&schema, [n(&schema, "paragraph", [t(&schema, "abcd")])]),
        Extension::none(),
    );
    let selected = text_selection(&start, 2, 4);
    let bold = run(&selected, &toggle_mark(strong, Attrs::empty()));
    assert_eq!(
        schema.describe(bold.doc()),
        r#"doc(paragraph("a", "bc"{strong}, "d"))"#
    );
    let plain = run(&bold, &toggle_mark(strong, Attrs::empty()));
    assert_eq!(schema.describe(plain.doc()), r#"doc(paragraph("abcd"))"#);
}

#[test]
fn toggle_mark_at_a_cursor_sets_stored_marks_that_typing_uses() {
    let schema = shared_schema();
    let strong = schema.mark_id("strong").expect("known");
    let start = state(
        doc(&schema, [n(&schema, "paragraph", [t(&schema, "ab")])]),
        Extension::none(),
    );
    let armed = run(&at(&start, 3), &toggle_mark(strong, Attrs::empty()));
    assert_eq!(schema.describe(armed.doc()), r#"doc(paragraph("ab"))"#);
    assert!(
        armed
            .stored_marks()
            .is_some_and(|marks| marks.contains_type(strong))
    );
    // Arming a mark leaves the selection alone.
    assert_eq!(armed.selection(), &Selection::cursor(3));
    let typed = run(&armed, &insert_text("X"));
    assert_eq!(
        schema.describe(typed.doc()),
        r#"doc(paragraph("ab", "X"{strong}))"#
    );
    assert!(
        typed
            .stored_marks()
            .is_some_and(|marks| marks.contains_type(strong))
    );
    // The stored marks survive, so the next character is bold too.
    let again = run(&typed, &insert_text("Y"));
    assert_eq!(
        schema.describe(again.doc()),
        r#"doc(paragraph("ab", "XY"{strong}))"#
    );
    assert!(
        again
            .stored_marks()
            .is_some_and(|marks| marks.contains_type(strong))
    );
    // Moving the cursor drops them.
    let moved = at(&again, 1);
    assert_eq!(moved.stored_marks(), None);
}

#[test]
fn set_and_remove_mark_at_a_cursor_only_store_marks() {
    let schema = shared_schema();
    let strong = schema.mark_id("strong").expect("known");
    let start = at(
        &state(
            doc(
                &schema,
                [n(&schema, "paragraph", [tm(&schema, "ab", &["strong"])])],
            ),
            Extension::none(),
        ),
        2,
    );
    let removed = run(&start, &remove_mark(strong));
    assert_eq!(removed.doc(), start.doc());
    assert_eq!(removed.selection(), start.selection());
    assert_eq!(removed.stored_marks(), Some(&MarkSet::empty()));
    let typed = run(&removed, &insert_text("X"));
    assert_eq!(
        schema.describe(typed.doc()),
        r#"doc(paragraph("a"{strong}, "X", "b"{strong}))"#
    );

    let set = run(&removed, &set_mark(strong, Attrs::empty()));
    assert_eq!(set.selection(), start.selection());
    assert!(
        set.stored_marks()
            .is_some_and(|marks| marks.contains_type(strong))
    );
}

#[test]
fn mark_applies_and_range_has_mark_report_the_schema() {
    let schema = shared_schema();
    let strong = schema.mark_id("strong").expect("known");
    let document = doc(
        &schema,
        [
            n(&schema, "paragraph", [tm(&schema, "ab", &["strong"])]),
            n(&schema, "code_block", [t(&schema, "cd")]),
        ],
    );
    assert!(range_has_mark(&document, 1, 3, strong));
    assert!(!range_has_mark(&document, 5, 7, strong));
    assert!(mark_applies(
        &schema,
        &document,
        &[crate::selection::SelectionRange::new(1, 3)],
        strong
    ));
    assert!(!mark_applies(
        &schema,
        &document,
        &[crate::selection::SelectionRange::new(5, 7)],
        strong
    ));
}

#[test]
fn insert_text_inside_a_code_block_ignores_stored_marks() {
    let schema = shared_schema();
    let strong = schema.mark_id("strong").expect("known");
    let start = state(
        doc(&schema, [n(&schema, "code_block", [t(&schema, "ab")])]),
        Extension::none(),
    );
    let marks = MarkSet::from_marks(&schema, [m(&schema, "strong")]);
    let armed = start
        .update([TransactionSpec::new()
            .selection(Selection::cursor(3))
            .stored_marks(Some(marks))])
        .expect("valid")
        .state()
        .clone();
    let typed = run(&armed, &insert_text("X"));
    assert_eq!(schema.describe(typed.doc()), r#"doc(code_block("abX"))"#);
    assert!(!range_has_mark(typed.doc(), 1, 4, strong));
}

// --- inserting ------------------------------------------------------------

#[test]
fn insert_hard_break_and_insert_node() {
    let schema = shared_schema();
    let start = state(
        doc(&schema, [n(&schema, "paragraph", [t(&schema, "ab")])]),
        Extension::none(),
    );
    let br = schema.node_id("hard_break").expect("known");
    let after = run(&at(&start, 2), &insert_hard_break(br));
    assert_eq!(
        schema.describe(after.doc()),
        r#"doc(paragraph("a", hard_break, "b"))"#
    );
    let picture = img(&schema, "pic.png");
    let with_image = run(&after, &insert_node(picture));
    assert!(schema.describe(with_image.doc()).contains("image"));
}

#[test]
fn replace_selection_merges_inline_edges_and_keeps_block_nodes() {
    let schema = shared_schema();
    let start = state(
        doc(&schema, [n(&schema, "paragraph", [t(&schema, "ad")])]),
        Extension::none(),
    );
    let slice = Slice::new(
        Fragment::from_nodes([
            n(&schema, "paragraph", [t(&schema, "b")]),
            n(&schema, "heading", [t(&schema, "c")]),
        ]),
        1,
        0,
    );
    let after = run(&at(&start, 2), &replace_selection(slice));
    assert_eq!(
        schema.describe(after.doc()),
        r#"doc(paragraph("ab"), heading[level=Int(1)]("c"), paragraph("d"))"#
    );
}

#[test]
fn replace_selection_with_a_closed_block_splits_the_paragraph() {
    let schema = shared_schema();
    let start = state(
        doc(&schema, [n(&schema, "paragraph", [t(&schema, "ad")])]),
        Extension::none(),
    );
    let slice = Slice::from_fragment(Fragment::from_node(n(
        &schema,
        "blockquote",
        [n(&schema, "paragraph", [t(&schema, "b")])],
    )));
    let after = run(&at(&start, 2), &replace_selection(slice));
    assert_eq!(
        schema.describe(after.doc()),
        r#"doc(paragraph("a"), blockquote(paragraph("b")), paragraph("d"))"#
    );
}

#[test]
fn a_list_pasted_into_an_empty_item_adds_its_items_beside_it() {
    let schema = shared_schema();
    let item = |text: &str| {
        n(
            &schema,
            "list_item",
            [if text.is_empty() {
                n(&schema, "paragraph", [])
            } else {
                n(&schema, "paragraph", [t(&schema, text)])
            }],
        )
    };
    let start = state(
        doc(&schema, [n(&schema, "bullet_list", [item("a"), item("")])]),
        Extension::none(),
    );
    for pasted in ["bullet_list", "ordered_list"] {
        let slice = Slice::from_fragment(Fragment::from_node(n(
            &schema,
            pasted,
            [item("b"), item("c")],
        )));
        let after = run(&at(&start, 8), &replace_selection(slice));
        assert_eq!(
            schema.describe(after.doc()),
            r#"doc(bullet_list(list_item(paragraph("a")), list_item(paragraph("b")), list_item(paragraph("c"))))"#,
            "{pasted}"
        );
    }
}

// --- selection and motion -------------------------------------------------

#[test]
fn selection_commands() {
    let schema = shared_schema();
    let start = state(
        doc(
            &schema,
            [n(
                &schema,
                "blockquote",
                [n(&schema, "paragraph", [t(&schema, "abc")])],
            )],
        ),
        Extension::none(),
    );
    let all = run(&start, &select_all());
    assert_eq!(all.selection(), &Selection::All);
    let parent = run(&at(&start, 3), &select_parent_node());
    assert_eq!(parent.selection(), &Selection::node(1));
    let home = run(&at(&start, 3), &select_textblock_start());
    assert_eq!(home.selection(), &Selection::cursor(2));
    let end = run(&home, &select_textblock_end());
    assert_eq!(end.selection(), &Selection::cursor(5));
}

#[test]
fn join_up_merges_two_blockquotes() {
    let schema = shared_schema();
    let start = state(
        doc(
            &schema,
            [
                n(
                    &schema,
                    "blockquote",
                    [n(&schema, "paragraph", [t(&schema, "a")])],
                ),
                n(
                    &schema,
                    "blockquote",
                    [n(&schema, "paragraph", [t(&schema, "b")])],
                ),
            ],
        ),
        Extension::none(),
    );
    let after = run(&at(&start, 7), &join_up());
    assert_eq!(
        schema.describe(after.doc()),
        r#"doc(blockquote(paragraph("a"), paragraph("b")))"#
    );
    let down = run(&at(&start, 2), &join_down());
    assert_eq!(
        schema.describe(down.doc()),
        r#"doc(blockquote(paragraph("a"), paragraph("b")))"#
    );
}

#[test]
fn move_by_grapheme_and_word() {
    let schema = shared_schema();
    let start = state(
        doc(
            &schema,
            [
                n(&schema, "paragraph", [t(&schema, "one two")]),
                n(&schema, "paragraph", [t(&schema, "three")]),
            ],
        ),
        Extension::none(),
    );
    let moved = run(&at(&start, 1), &move_by_grapheme(Direction::Forward, false));
    assert_eq!(moved.selection(), &Selection::cursor(2));
    let word = run(&at(&start, 1), &move_by_word(Direction::Forward, false));
    assert_eq!(word.selection(), &Selection::cursor(4));
    // Grapheme motion crosses a block boundary.
    let across = run(&at(&start, 8), &move_by_grapheme(Direction::Forward, false));
    assert_eq!(across.selection(), &Selection::cursor(10));
    let extended = run(&at(&start, 1), &move_by_word(Direction::Forward, true));
    assert_eq!(extended.selection(), &Selection::text(1, 4));
}

#[test]
fn chain_takes_the_first_command_that_applies() {
    let schema = shared_schema();
    let start = state(
        doc(&schema, [n(&schema, "paragraph", [t(&schema, "ab")])]),
        Extension::none(),
    );
    // `delete_selection` does not apply to a cursor, so the chain falls through.
    let enter = chain([delete_selection(), split_block()]);
    let after = run(&at(&start, 2), &enter);
    assert_eq!(
        schema.describe(after.doc()),
        r#"doc(paragraph("a"), paragraph("b"))"#
    );
}

#[test]
fn removing_part_of_an_inherited_mark_preserves_both_unselected_sides() {
    let schema = shared_schema();
    let strong = schema.mark_id("strong").unwrap();
    let marks = MarkSet::from_marks(&schema, [m(&schema, "strong")]);
    let span = n(&schema, "inline_span", [t(&schema, "abcd")]).mark(marks);
    let start = state(
        doc(&schema, [n(&schema, "paragraph", [span])]),
        Extension::none(),
    );
    assert!(
        start
            .doc()
            .resolve(3)
            .unwrap()
            .marks(&schema)
            .contains_type(strong)
    );
    let selected = text_selection(&start, 3, 5);
    let plain = run(&selected, &toggle_mark(strong, Attrs::empty()));
    assert_eq!(
        schema.describe(plain.doc()),
        r#"doc(paragraph(inline_span{strong}("a"), inline_span("bc"), inline_span{strong}("d")))"#
    );
    let range = plain.selection().replacement_range(plain.doc());
    assert!(!range_has_mark(plain.doc(), range.from, range.to, strong));
}

#[test]
fn cursor_toggle_exits_the_inherited_mark_when_typing() {
    let schema = shared_schema();
    let strong = schema.mark_id("strong").unwrap();
    let marks = MarkSet::from_marks(&schema, [m(&schema, "strong")]);
    let span = n(&schema, "inline_span", [t(&schema, "ab")]).mark(marks);
    let start = state(
        doc(&schema, [n(&schema, "paragraph", [span])]),
        Extension::none(),
    );
    let toggled = run(&at(&start, 3), &toggle_mark(strong, Attrs::empty()));
    let typed = run(&toggled, &insert_text("X"));
    let typed = run(&typed, &insert_text("Y"));
    assert_eq!(
        schema.describe(typed.doc()),
        r#"doc(paragraph(inline_span{strong}("a"), "XY", inline_span{strong}("b")))"#
    );
}

#[test]
fn ordinary_typing_inherits_the_scope_without_duplicating_its_mark() {
    let schema = shared_schema();
    let marks = MarkSet::from_marks(&schema, [m(&schema, "strong")]);
    let span = n(&schema, "inline_span", [t(&schema, "ab")]).mark(marks);
    let start = state(
        doc(&schema, [n(&schema, "paragraph", [span])]),
        Extension::none(),
    );
    let typed = run(&at(&start, 3), &insert_text("X"));
    assert_eq!(
        schema.describe(typed.doc()),
        r#"doc(paragraph(inline_span{strong}("aXb")))"#
    );
}

#[test]
fn setting_a_mark_across_a_code_block_keeps_forbidden_marks_out() {
    let schema = shared_schema();
    let strong = schema.mark_id("strong").unwrap();
    let start = state(
        doc(
            &schema,
            [
                n(&schema, "paragraph", [t(&schema, "a")]),
                n(&schema, "code_block", [t(&schema, "b")]),
            ],
        ),
        Extension::none(),
    );
    let selected = text_selection(&start, 1, 5);
    let typed = run(&selected, &set_mark(strong, Attrs::empty()));
    assert_eq!(
        schema.describe(typed.doc()),
        r#"doc(paragraph("a"{strong}), code_block("b"))"#
    );
}

#[test]
fn removing_a_whole_inherited_link_does_not_leave_empty_links() {
    let schema = shared_schema();
    let link = schema.mark_id("link").unwrap();
    let marks = MarkSet::from_marks(
        &schema,
        [crate::Mark::with_attrs(
            link,
            Attrs::from_pairs([("href", "https://old")]),
        )],
    );
    let span = n(&schema, "inline_span", [t(&schema, "ab")]).mark(marks);
    let start = state(
        doc(&schema, [n(&schema, "paragraph", [span])]),
        Extension::none(),
    );
    let plain = run(&text_selection(&start, 2, 4), &remove_mark(link));
    assert!(!range_has_mark(
        plain.doc(),
        0,
        plain.doc().content_size(),
        link
    ));
    assert_eq!(
        schema.describe(plain.doc()),
        r#"doc(paragraph(inline_span("ab")))"#
    );
}
