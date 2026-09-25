//! The table commands: moving between cells, growing and shrinking a table,
//! and the invariant that keeps a table a grid.

use std::sync::OnceLock;

use crate::attr::{AttrKind, AttrSpec, AttrValue};
use crate::change::TrackMode;
use crate::commands::*;
use crate::fragment::Fragment;
use crate::history::{HistoryConfig, history, undo};
use crate::node::Node;
use crate::schema::{NodeTypeSpec, Schema, SchemaSpec};
use crate::selection::Selection;
use crate::slice::Slice;
use crate::state::{EditorState, EditorStateConfig, Extension, TransactionSpec};

/// A schema with the table shape the commands describe, plus the paragraphs a
/// table has to live among.
fn table_schema() -> Schema {
    static SCHEMA: OnceLock<Schema> = OnceLock::new();
    SCHEMA
        .get_or_init(|| {
            Schema::new(
                SchemaSpec::new()
                    .node(NodeTypeSpec::new("doc", "block+"))
                    .node(NodeTypeSpec::new("paragraph", "inline*").group("block"))
                    .node(
                        NodeTypeSpec::new("table", "table_row+")
                            .group("block")
                            .attr(AttrSpec::new(
                                "alignments",
                                AttrKind::Str,
                                AttrValue::Str(String::new()),
                            )),
                    )
                    .node(NodeTypeSpec::new("table_row", "table_cell+"))
                    .node(NodeTypeSpec::new("table_cell", "inline*"))
                    .node(NodeTypeSpec::text("text").group("inline")),
            )
            .expect("the table schema is valid")
        })
        .clone()
}

fn types() -> TableTypes {
    let schema = table_schema();
    TableTypes::new(
        schema.node_id("table").expect("known"),
        schema.node_id("table_row").expect("known"),
        schema.node_id("table_cell").expect("known"),
        "alignments",
    )
}

/// A table node from a grid of cell texts; an empty string is an empty cell.
fn table(alignments: &str, grid: &[&[&str]]) -> Node {
    let schema = table_schema();
    let rows: Vec<Node> = grid
        .iter()
        .map(|row| {
            let cells: Vec<Node> = row
                .iter()
                .map(|text| {
                    let content: Vec<Node> = if text.is_empty() {
                        Vec::new()
                    } else {
                        vec![schema.text(text)]
                    };
                    schema.node("table_cell", content).expect("a valid cell")
                })
                .collect();
            schema.node("table_row", cells).expect("a valid row")
        })
        .collect();
    schema
        .node_with("table", crate::attrs! {"alignments" => alignments}, rows)
        .expect("a valid table")
}

fn paragraph(text: &str) -> Node {
    let schema = table_schema();
    let content: Vec<Node> = if text.is_empty() {
        Vec::new()
    } else {
        vec![schema.text(text)]
    };
    schema
        .node("paragraph", content)
        .expect("a valid paragraph")
}

fn document(blocks: impl IntoIterator<Item = Node>) -> Node {
    table_schema().doc(blocks).expect("a valid document")
}

fn state_of(document: Node, extensions: Extension) -> EditorState {
    EditorState::create(
        EditorStateConfig::new(table_schema())
            .doc(document)
            .extensions(extensions),
    )
    .expect("a valid starting state")
}

/// `doc(table("a" "b" / "c" "d"))`, the fixture most tests start from.
///
/// Cell content starts at 3, 6, 11 and 14; the table spans 0..18.
fn grid() -> EditorState {
    state_of(
        document([table("none,none", &[&["a", "b"], &["c", "d"]])]),
        Extension::none(),
    )
}

fn at(state: &EditorState, pos: usize) -> EditorState {
    state
        .update([TransactionSpec::new().selection(Selection::cursor(pos))])
        .expect("the selection is valid")
        .state()
        .clone()
}

fn run(state: &EditorState, command: &Command) -> EditorState {
    run_command(state, command)
        .expect("the command applies")
        .expect("the transaction resolves")
        .state()
        .clone()
}

fn shape(state: &EditorState) -> String {
    table_schema().describe(state.doc())
}

fn cursor(state: &EditorState) -> usize {
    state.selection().head(state.doc())
}

/// The document the fixture starts as, for the undo tests to compare against.
const GRID: &str = r#"doc(table[alignments=Str("none,none")](table_row(table_cell("a"), table_cell("b")), table_row(table_cell("c"), table_cell("d"))))"#;

#[test]
fn the_fixture_has_the_positions_the_tests_assume() {
    assert_eq!(shape(&grid()), GRID);
    assert_eq!(grid().doc().content_size(), 18);
}

#[test]
fn cell_at_resolves_the_cursor_and_nothing_else() {
    let state = grid();
    assert_eq!(
        cell_at(types(), &at(&state, 6)),
        Some(CellPos {
            table: 0,
            row: 0,
            column: 1,
            cell: 5,
        })
    );
    assert_eq!(
        cell_at(types(), &at(&state, 14)),
        Some(CellPos {
            table: 0,
            row: 1,
            column: 1,
            cell: 13,
        })
    );
    let outside = state_of(document([paragraph("a")]), Extension::none());
    assert_eq!(cell_at(types(), &at(&outside, 1)), None);
}

/// The range a state's selection covers.
fn selected(state: &EditorState) -> (usize, usize) {
    let range = state.selection().replacement_range(state.doc());
    (range.from, range.to)
}

/// Moving into a cell selects what it holds, so typing
/// replaces it.
#[test]
fn tab_walks_the_cells_in_row_major_order() {
    let state = grid();
    assert_eq!(
        selected(&run(&at(&state, 3), &goto_next_cell(types()))),
        (6, 7)
    );
    // The end of a row wraps into the first cell of the next one.
    assert_eq!(
        selected(&run(&at(&state, 7), &goto_next_cell(types()))),
        (11, 12)
    );
    assert_eq!(
        selected(&run(&at(&state, 11), &goto_next_cell(types()))),
        (14, 15)
    );
}

#[test]
fn tab_in_the_last_cell_appends_a_row() {
    let after = run(&at(&grid(), 14), &goto_next_cell(types()));
    assert_eq!(
        shape(&after),
        r#"doc(table[alignments=Str("none,none")](table_row(table_cell("a"), table_cell("b")), table_row(table_cell("c"), table_cell("d")), table_row(table_cell(), table_cell())))"#
    );
    assert_eq!(cursor(&after), 19);
}

#[test]
fn insert_row_below_puts_an_empty_row_under_the_cursor_and_moves_into_it() {
    let empty_row = "table_row(table_cell(), table_cell())";
    for (from, rows, caret) in [
        (
            6,
            [empty_row, "table_row(table_cell(\"c\"), table_cell(\"d\"))"],
            11,
        ),
        (
            11,
            ["table_row(table_cell(\"c\"), table_cell(\"d\"))", empty_row],
            19,
        ),
    ] {
        let after = run(&at(&grid(), from), &insert_row_below(types()));
        assert_eq!(
            shape(&after),
            format!(
                r#"doc(table[alignments=Str("none,none")](table_row(table_cell("a"), table_cell("b")), {}, {}))"#,
                rows[0], rows[1]
            ),
            "from {from}"
        );
        assert_eq!(cursor(&after), caret, "from {from}");
    }
}

#[test]
fn shift_tab_walks_back_and_stops_at_the_first_cell() {
    let state = grid();
    assert_eq!(
        selected(&run(&at(&state, 14), &goto_prev_cell(types()))),
        (11, 12)
    );
    // The start of a row wraps into the last cell of the one above.
    assert_eq!(
        selected(&run(&at(&state, 11), &goto_prev_cell(types()))),
        (6, 7)
    );
    assert_eq!(
        selected(&run(&at(&state, 6), &goto_prev_cell(types()))),
        (3, 4)
    );
    let prev = goto_prev_cell(types());
    assert!(prev(&at(&state, 3)).is_none());
}

#[test]
fn enter_moves_down_the_column_and_appends_on_the_last_row() {
    let state = grid();
    assert_eq!(
        selected(&run(&at(&state, 6), &goto_cell_below(types()))),
        (14, 15)
    );
    let appended = run(&at(&state, 11), &goto_cell_below(types()));
    assert_eq!(
        shape(&appended),
        r#"doc(table[alignments=Str("none,none")](table_row(table_cell("a"), table_cell("b")), table_row(table_cell("c"), table_cell("d")), table_row(table_cell(), table_cell())))"#
    );
    assert_eq!(cursor(&appended), 19);
}

#[test]
fn down_from_the_last_row_adds_a_block_only_when_nothing_follows() {
    let state = grid();
    let exit = exit_table_below(types());
    // Above the last row it is ordinary motion.
    assert!(exit(&at(&state, 6)).is_none());
    let exited = run(&at(&state, 14), &exit);
    assert_eq!(
        shape(&exited),
        r#"doc(table[alignments=Str("none,none")](table_row(table_cell("a"), table_cell("b")), table_row(table_cell("c"), table_cell("d"))), paragraph())"#
    );
    assert_eq!(cursor(&exited), 19);
    // A block after the table is where ↓ goes by itself.
    let followed = state_of(
        document([
            table("none,none", &[&["a", "b"], &["c", "d"]]),
            paragraph("after"),
        ]),
        Extension::none(),
    );
    assert!(exit(&at(&followed, 14)).is_none());
}

#[test]
fn goto_cell_above_never_creates_a_row() {
    let state = grid();
    assert_eq!(cursor(&run(&at(&state, 14), &goto_cell_above(types()))), 6);
    let above = goto_cell_above(types());
    assert!(above(&at(&state, 3)).is_none());
}

#[test]
fn a_row_added_before_the_header_becomes_the_header() {
    let after = run(&at(&grid(), 3), &add_row_before(types()));
    assert_eq!(
        shape(&after),
        r#"doc(table[alignments=Str("none,none")](table_row(table_cell(), table_cell()), table_row(table_cell("a"), table_cell("b")), table_row(table_cell("c"), table_cell("d"))))"#
    );
    // The cursor stayed with the text it was in, six tokens further on.
    assert_eq!(cursor(&after), 9);
}

#[test]
fn add_row_after_puts_the_row_below_the_cursors_row() {
    let after = run(&at(&grid(), 3), &add_row_after(types()));
    assert_eq!(
        shape(&after),
        r#"doc(table[alignments=Str("none,none")](table_row(table_cell("a"), table_cell("b")), table_row(table_cell(), table_cell()), table_row(table_cell("c"), table_cell("d"))))"#
    );
    assert_eq!(cursor(&after), 3);
}

#[test]
fn add_column_widens_every_row_and_the_alignment_list() {
    let state = state_of(
        document([table("left,right", &[&["a", "b"], &["c", "d"]])]),
        Extension::none(),
    );
    let after = run(&at(&state, 3), &add_column_after(types()));
    assert_eq!(
        shape(&after),
        r#"doc(table[alignments=Str("left,none,right")](table_row(table_cell("a"), table_cell(), table_cell("b")), table_row(table_cell("c"), table_cell(), table_cell("d"))))"#
    );
    let before = run(&at(&state, 3), &add_column_before(types()));
    assert_eq!(
        shape(&before),
        r#"doc(table[alignments=Str("none,left,right")](table_row(table_cell(), table_cell("a"), table_cell("b")), table_row(table_cell(), table_cell("c"), table_cell("d"))))"#
    );
    for doc in [after.doc(), before.doc()] {
        let widths: Vec<usize> = doc
            .child(0)
            .children()
            .map(|row| row.child_count())
            .collect();
        assert_eq!(widths, vec![3, 3]);
    }
}

#[test]
fn delete_column_narrows_every_row_and_the_alignment_list() {
    let state = state_of(
        document([table(
            "left,center,right",
            &[&["a", "b", "c"], &["d", "e", "f"]],
        )]),
        Extension::none(),
    );
    // Cursor in "b", the middle column.
    let after = run(&at(&state, 6), &delete_column(types()));
    assert_eq!(
        shape(&after),
        r#"doc(table[alignments=Str("left,right")](table_row(table_cell("a"), table_cell("c")), table_row(table_cell("d"), table_cell("f"))))"#
    );
    // The cursor followed the column that took its place.
    assert_eq!(cursor(&after), 6);
    let widths: Vec<usize> = after
        .doc()
        .child(0)
        .children()
        .map(|row| row.child_count())
        .collect();
    assert_eq!(widths, vec![2, 2]);
}

#[test]
fn delete_row_takes_the_cursors_row() {
    let after = run(&at(&grid(), 3), &delete_row(types()));
    assert_eq!(
        shape(&after),
        r#"doc(table[alignments=Str("none,none")](table_row(table_cell("c"), table_cell("d"))))"#
    );
    assert_eq!(cursor(&after), 3);
}

#[test]
fn deleting_the_only_row_deletes_the_table() {
    let state = state_of(
        document([paragraph("before"), table("none,none", &[&["a", "b"]])]),
        Extension::none(),
    );
    let after = run(&at(&state, 11), &delete_row(types()));
    assert_eq!(shape(&after), r#"doc(paragraph("before"))"#);
}

#[test]
fn deleting_the_only_column_deletes_the_table() {
    let state = state_of(
        document([paragraph("before"), table("none", &[&["a"], &["b"]])]),
        Extension::none(),
    );
    let after = run(&at(&state, 11), &delete_column(types()));
    assert_eq!(shape(&after), r#"doc(paragraph("before"))"#);
}

#[test]
fn delete_table_keeps_its_neighbours_and_leaves_no_blank_block() {
    let state = state_of(
        document([
            paragraph("before"),
            table("none,none", &[&["a", "b"]]),
            paragraph("after"),
        ]),
        Extension::none(),
    );
    let after = run(&at(&state, 11), &delete_table(types()));
    assert_eq!(
        shape(&after),
        r#"doc(paragraph("before"), paragraph("after"))"#
    );
    assert_eq!(cursor(&after), 7);
}

#[test]
fn delete_table_on_its_own_leaves_an_empty_paragraph() {
    let after = run(&at(&grid(), 3), &delete_table(types()));
    assert_eq!(shape(&after), r#"doc(paragraph())"#);
    assert_eq!(cursor(&after), 1);
}

#[test]
fn set_column_alignment_rewrites_one_entry() {
    let state = grid();
    let after = run(
        &at(&state, 6),
        &set_column_alignment(types(), ColumnAlignment::Center),
    );
    assert_eq!(
        shape(&after),
        r#"doc(table[alignments=Str("none,center")](table_row(table_cell("a"), table_cell("b")), table_row(table_cell("c"), table_cell("d"))))"#
    );
    assert_eq!(
        column_alignments(after.doc().child(0), "alignments", 2),
        vec![ColumnAlignment::None, ColumnAlignment::Center]
    );
    // Setting the alignment it already has does not apply.
    let again = set_column_alignment(types(), ColumnAlignment::Center);
    assert!(again(&at(&after, 6)).is_none());
}

#[test]
fn insert_table_replaces_an_empty_paragraph() {
    let state = state_of(
        document([paragraph("keep"), paragraph("")]),
        Extension::none(),
    );
    let after = run(&at(&state, 7), &insert_table(types(), 2, 2));
    assert_eq!(
        shape(&after),
        r#"doc(paragraph("keep"), table[alignments=Str("none,none")](table_row(table_cell(), table_cell()), table_row(table_cell(), table_cell())))"#
    );
    assert_eq!(cursor(&after), 9);
}

#[test]
fn insert_table_goes_after_a_block_that_has_content() {
    let after = run(
        &at(
            &state_of(document([paragraph("text")]), Extension::none()),
            3,
        ),
        &insert_table(types(), 1, 2),
    );
    assert_eq!(
        shape(&after),
        r#"doc(paragraph("text"), table[alignments=Str("none,none")](table_row(table_cell(), table_cell())))"#
    );
    assert_eq!(cursor(&after), 9);
}

#[test]
fn insert_table_replaces_the_selection() {
    let state = state_of(document([paragraph("abcd")]), Extension::none());
    let selected = state
        .update([TransactionSpec::new().selection(Selection::text(2, 4))])
        .expect("the selection is valid")
        .state()
        .clone();
    let after = run(&selected, &insert_table(types(), 1, 1));
    assert_eq!(
        shape(&after),
        r#"doc(paragraph("ad"), table[alignments=Str("none")](table_row(table_cell())))"#
    );
    assert_eq!(cursor(&after), 7);
}

#[test]
fn backspace_in_an_empty_table_removes_it() {
    let state = state_of(
        document([paragraph("before"), table("none,none", &[&["", ""]])]),
        Extension::none(),
    );
    let after = run(&at(&state, 11), &delete_empty_table(types()));
    assert_eq!(shape(&after), r#"doc(paragraph("before"), paragraph())"#);
    assert_eq!(cursor(&after), 9);
}

#[test]
fn delete_empty_table_only_applies_at_the_start_of_an_empty_table() {
    let command = delete_empty_table(types());
    // Not at the start of the first cell.
    let empty = state_of(
        document([table("none,none", &[&["", ""]])]),
        Extension::none(),
    );
    assert!(command(&at(&empty, 3)).is_some());
    assert!(command(&at(&empty, 4)).is_none());
    // A cell holds something.
    let filled = state_of(
        document([table("none,none", &[&["", "b"]])]),
        Extension::none(),
    );
    assert!(command(&at(&filled, 3)).is_none());
}

/// The grid fixture with the invariant configured.
fn guarded() -> EditorState {
    state_of(
        document([table("none,none", &[&["a", "b"], &["c", "d"]])]),
        table_invariant(types()),
    )
}

#[test]
fn without_the_invariant_backspace_would_join_two_cells() {
    // Pinned so the invariant's reason for existing stays visible: at the
    // start of a cell the general chain joins it with the cell before it,
    // which leaves the row one cell short.
    let joined = run(
        &at(&grid(), 6),
        &chain([delete_selection(), join_backward(), select_node_backward()]),
    );
    assert_eq!(
        shape(&joined),
        r#"doc(table[alignments=Str("none,none")](table_row(table_cell("ab")), table_row(table_cell("c"), table_cell("d"))))"#
    );
}

#[test]
fn the_invariant_stops_backspace_and_delete_at_a_cell_edge() {
    let state = guarded();
    let backspace = chain([
        delete_selection(),
        delete_by_grapheme(Direction::Backward),
        join_backward(),
        select_node_backward(),
    ]);
    // The chain knows nothing of cells: the join applies, and its transaction
    // is refused, leaving the grid and the caret where they were — and no
    // undo step behind.
    let after = run(&at(&state, 6), &backspace);
    assert_eq!(shape(&after), GRID);
    assert_eq!(cursor(&after), 6);
    assert_eq!(crate::history::undo_depth(&after), 0);

    let delete_forward = chain([
        delete_selection(),
        delete_by_grapheme(Direction::Forward),
        join_forward(),
        select_node_forward(),
    ]);
    // The end of cell "a"; without the invariant this joins "a" and "b".
    let after = run(&at(&state, 4), &delete_forward);
    assert_eq!(shape(&after), GRID);

    // In the middle of a cell the chain still deletes a character.
    let two = state_of(
        document([table("none,none", &[&["ab", "c"]])]),
        table_invariant(types()),
    );
    let after = run(&at(&two, 4), &backspace);
    assert_eq!(
        shape(&after),
        r#"doc(table[alignments=Str("none,none")](table_row(table_cell("b"), table_cell("c"))))"#
    );
}

#[test]
fn the_invariant_stops_enter_from_splitting_a_cell() {
    let unguarded = state_of(
        document([table("none,none", &[&["ab", "c"]])]),
        Extension::none(),
    );
    // Without the invariant, splitting a block inside a cell makes a second cell.
    let split = run(&at(&unguarded, 4), &split_block());
    assert_eq!(
        shape(&split),
        r#"doc(table[alignments=Str("none,none")](table_row(table_cell("a"), table_cell("b"), table_cell("c"))))"#
    );
    let state = state_of(
        document([table("none,none", &[&["ab", "c"]])]),
        table_invariant(types()),
    );
    let refused = run(&at(&state, 4), &split_block());
    assert_eq!(
        shape(&refused),
        r#"doc(table[alignments=Str("none,none")](table_row(table_cell("ab"), table_cell("c"))))"#
    );
    // Enter as a view binds it never reaches the split either.
    let enter = run(
        &at(&state, 4),
        &chain([goto_cell_below(types()), split_block()]),
    );
    assert_eq!(
        shape(&enter),
        r#"doc(table[alignments=Str("none,none")](table_row(table_cell("ab"), table_cell("c")), table_row(table_cell(), table_cell())))"#
    );
}

#[test]
fn every_mutation_is_one_undo_step() {
    let grid_doc = || document([table("none,none", &[&["a", "b"], &["c", "d"]])]);
    // `insert_table` is the one command that does not apply inside a table, so
    // it gets a document of its own.
    let cases: Vec<(&str, Command, Node, usize)> = vec![
        ("goto_next_cell", goto_next_cell(types()), grid_doc(), 14),
        ("goto_cell_below", goto_cell_below(types()), grid_doc(), 11),
        ("add_row_before", add_row_before(types()), grid_doc(), 3),
        ("add_row_after", add_row_after(types()), grid_doc(), 3),
        (
            "add_column_before",
            add_column_before(types()),
            grid_doc(),
            3,
        ),
        ("add_column_after", add_column_after(types()), grid_doc(), 3),
        ("delete_row", delete_row(types()), grid_doc(), 3),
        ("delete_column", delete_column(types()), grid_doc(), 3),
        ("delete_table", delete_table(types()), grid_doc(), 3),
        (
            "set_column_alignment",
            set_column_alignment(types(), ColumnAlignment::Right),
            grid_doc(),
            3,
        ),
        (
            "delete_empty_table",
            delete_empty_table(types()),
            document([table("none,none", &[&["", ""]])]),
            3,
        ),
        (
            "insert_table",
            insert_table(types(), 2, 2),
            document([paragraph("text")]),
            3,
        ),
    ];
    for (name, command, start_doc, pos) in cases {
        let before = table_schema().describe(&start_doc);
        let start = at(&state_of(start_doc, history(HistoryConfig::default())), pos);
        let after = run(&start, &command);
        assert_ne!(shape(&after), before, "{name} changed nothing");
        let undone = after
            .update([undo(&after).expect("one entry to undo")])
            .expect("the undo resolves")
            .state()
            .clone();
        assert_eq!(shape(&undone), before, "{name} did not undo in one step");
    }
}

#[test]
fn a_position_elsewhere_in_the_table_maps_through_an_edit() {
    let state = at(&grid(), 3);
    // Just after the "d" in the last cell, before the edit.
    let watched = 15;
    let transaction = run_command(&state, &add_row_after(types()))
        .expect("the command applies")
        .expect("the transaction resolves");
    let mapped = transaction
        .changes()
        .map_pos(watched, 1, TrackMode::Simple)
        .expect("the position survives");
    // One row of two empty cells — six tokens — went in above it.
    assert_eq!(mapped, 21);
    assert_eq!(
        transaction
            .new_doc()
            .text_between(&table_schema(), mapped - 1, mapped, Some(""), None),
        "d"
    );
}

#[test]
fn the_invariant_refuses_an_edit_that_spans_two_cells() {
    let state = guarded();
    let across = state
        .update([TransactionSpec::new().selection(Selection::text(4, 6))])
        .expect("the selection is valid")
        .state()
        .clone();
    // Deleting the selection would merge the two cells: refused, grid and
    // selection standing.
    let refused = run(&across, &delete_selection());
    assert_eq!(shape(&refused), GRID);
    assert_eq!(refused.selection(), across.selection());

    // A selection inside one cell is left alone.
    let inside = state_of(
        document([table("none,none", &[&["abc", "d"]])]),
        table_invariant(types()),
    );
    let within = inside
        .update([TransactionSpec::new().selection(Selection::text(3, 5))])
        .expect("the selection is valid")
        .state()
        .clone();
    let after = run(&within, &delete_selection());
    assert_eq!(
        shape(&after),
        r#"doc(table[alignments=Str("none,none")](table_row(table_cell("c"), table_cell("d"))))"#
    );
}

#[test]
fn a_table_that_was_ragged_already_stays_editable() {
    // Only an importer can make one; the edit did not cause it, so it is not
    // held against the edit.
    let state = state_of(
        document([table("none,none", &[&["a", "b"], &["c"]])]),
        table_invariant(types()),
    );
    let typed = run(&at(&state, 12), &insert_text("x"));
    assert_eq!(
        shape(&typed),
        r#"doc(table[alignments=Str("none,none")](table_row(table_cell("a"), table_cell("b")), table_row(table_cell("cx"))))"#
    );
}

#[test]
fn spans_cells_tells_a_range_inside_one_cell_from_one_reaching_out() {
    let state = grid();
    let doc = state.doc();
    assert!(!spans_cells(types(), doc, 3, 4), "inside a cell");
    assert!(!spans_cells(types(), doc, 4, 4), "empty");
    assert!(
        spans_cells(types(), doc, 4, 6),
        "from one cell into the next"
    );
    assert!(
        spans_cells(types(), doc, 0, 4),
        "from outside the table into a cell"
    );
    let around = state_of(
        document([paragraph("p"), paragraph("q")]),
        Extension::none(),
    );
    assert!(
        !spans_cells(types(), around.doc(), 1, 4),
        "no table involved"
    );
}

#[test]
fn pasting_a_ragged_table_lands_it_as_it_is() {
    // Nothing squares a pasted table whose rows differ in width: without the
    // invariant configured it lands as it is.
    let state = state_of(document([paragraph("x")]), Extension::none());
    let ragged = table("none,none", &[&["a", "b"], &["c"]]);
    let pasted = run(
        &at(&state, 2),
        &replace_selection(Slice::new(Fragment::from_node(ragged), 0, 0)),
    );
    assert_eq!(
        shape(&pasted),
        r#"doc(paragraph("x"), table[alignments=Str("none,none")](table_row(table_cell("a"), table_cell("b")), table_row(table_cell("c"))))"#
    );
}

#[test]
fn pasting_a_ragged_table_is_refused_where_the_invariant_holds() {
    let state = state_of(document([paragraph("x")]), table_invariant(types()));
    let ragged = table("none,none", &[&["a", "b"], &["c"]]);
    let refused = run(
        &at(&state, 2),
        &replace_selection(Slice::new(Fragment::from_node(ragged), 0, 0)),
    );
    assert_eq!(shape(&refused), r#"doc(paragraph("x"))"#);
}

/// A table whose alignments live under a name other than the kind default.
fn custom_attr_schema() -> Schema {
    Schema::new(
        SchemaSpec::new()
            .node(NodeTypeSpec::new("doc", "block+"))
            .node(NodeTypeSpec::new("paragraph", "inline*").group("block"))
            .node(
                NodeTypeSpec::new("table", "table_row+")
                    .group("block")
                    .attr(AttrSpec::new(
                        "align",
                        AttrKind::Str,
                        AttrValue::Str(String::new()),
                    )),
            )
            .node(NodeTypeSpec::new("table_row", "table_cell+"))
            .node(NodeTypeSpec::new("table_cell", "inline*"))
            .node(NodeTypeSpec::text("text").group("inline")),
    )
    .expect("the table schema is valid")
}

#[test]
fn column_alignments_read_the_named_attribute() {
    let schema = custom_attr_schema();
    let cell = |text: &str| {
        schema
            .node("table_cell", [schema.text(text)])
            .expect("a valid cell")
    };
    let row = schema
        .node("table_row", [cell("a"), cell("b")])
        .expect("a valid row");
    let table = schema
        .node_with("table", crate::attrs! {"align" => "right,left"}, [row])
        .expect("a valid table");
    assert_eq!(
        column_alignments(&table, "align", 2),
        vec![ColumnAlignment::Right, ColumnAlignment::Left]
    );
    // An attribute the table does not carry reads as unaligned.
    assert_eq!(
        column_alignments(&table, "alignments", 2),
        vec![ColumnAlignment::None, ColumnAlignment::None]
    );
}

#[test]
fn set_column_alignment_writes_the_named_attribute() {
    let schema = custom_attr_schema();
    let types = TableTypes::new(
        schema.node_id("table").expect("known"),
        schema.node_id("table_row").expect("known"),
        schema.node_id("table_cell").expect("known"),
        "align",
    );
    let cell = |text: &str| {
        schema
            .node("table_cell", [schema.text(text)])
            .expect("a valid cell")
    };
    let row = schema
        .node("table_row", [cell("a"), cell("b")])
        .expect("a valid row");
    let table = schema
        .node_with("table", crate::attrs! {"align" => "none,none"}, [row])
        .expect("a valid table");
    let document = schema.node("doc", [table]).expect("a valid doc");
    let state = EditorState::create(EditorStateConfig::new(schema.clone()).doc(document))
        .expect("a valid state");
    // Cell content starts at 3 and 6, as in `grid`.
    let after = run(
        &at(&state, 6),
        &set_column_alignment(types, ColumnAlignment::Center),
    );
    assert_eq!(
        column_alignments(after.doc().child(0), "align", 2),
        vec![ColumnAlignment::None, ColumnAlignment::Center]
    );
}
