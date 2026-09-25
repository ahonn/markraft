//! Tables: the toolbar's edits, moving between cells and line breaks in them.

use super::harness::{Harness, open_with};
use gpui::TestAppContext;

// The table toolbar on a table written by hand, not as the writer would.
// A row added — spaced as the header is — or deleted leaves the other rows
// as they were spelled; a
// column changed respells the table, since every row changes with it.
#[gpui::test]
fn table_edits_on_a_hand_written_table(cx: &mut TestAppContext) {
    use markraft_gpui::{ColumnAlignment, TableOp};
    type Edit =
        fn(&mut markraft_gpui::EditorView, &mut gpui::Context<markraft_gpui::EditorView>) -> bool;
    let edits: [(&str, Edit, &str); 7] = [
        (
            "row after",
            |e, cx| e.table(TableOp::AddRowAfter, cx),
            "|a|b|\n|-|-|\n|1|2|\n| | |\n",
        ),
        (
            "row before",
            |e, cx| e.table(TableOp::AddRowBefore, cx),
            "|a|b|\n|-|-|\n| | |\n|1|2|\n",
        ),
        (
            "column after",
            |e, cx| e.table(TableOp::AddColumnAfter, cx),
            "| a   | b   |     |\n| --- | --- | --- |\n| 1   | 2   |     |\n",
        ),
        (
            "column before",
            |e, cx| e.table(TableOp::AddColumnBefore, cx),
            "| a   |     | b   |\n| --- | --- | --- |\n| 1   |     | 2   |\n",
        ),
        (
            "align centre",
            |e, cx| e.table(TableOp::SetAlignment(ColumnAlignment::Center), cx),
            "| a   | b   |\n| --- | :-: |\n| 1   | 2   |\n",
        ),
        (
            "delete row",
            |e, cx| e.table(TableOp::DeleteRow, cx),
            "|a|b|\n|-|-|\n",
        ),
        (
            "delete column",
            |e, cx| e.table(TableOp::DeleteColumn, cx),
            "| a   |\n| --- |\n| 1   |\n",
        ),
    ];
    let source = "|a|b|\n|-|-|\n|1|2|\n";
    for (name, edit, expected) in edits {
        let mut h = open_with(cx, &[("t.md", source)], |_| {});
        h.keys("cmd-down");
        let done = h.edit(edit);
        assert!(done, "{name} was refused on a hand-written table");
        h.assert_round_trip(name);
        let text = h.wait_for_file("t.md", |text| text == expected);
        assert_eq!(text, expected, "{name}");
    }
}

// ⌘Enter in a table opens a row below the caret's and moves into its first
// cell. The rows written by hand keep their bytes, the
// edit typed into the new row included.
#[gpui::test]
fn command_return_in_a_table_opens_a_row_to_type_in(cx: &mut TestAppContext) {
    let source = "|a|b|\n|-|-|\n|1|2|\n|3|4|\n";
    let mut h = open_with(cx, &[("t.md", source)], |_| {});
    h.keys("cmd-up down cmd-right cmd-enter");
    h.type_text("z");
    h.keys("down");
    h.type_text("y");
    h.save();
    let expected = "|a|b|\n|-|-|\n|1|2|\n|z| |\n|3y|4|\n";
    let text = h.wait_for_file("t.md", |text| text == expected);
    assert_eq!(text, expected);
}

// A `<br/>` in a table cell is the cell's line break: the caret
// reaching it does not spell it out, and text typed on either side of it
// stays on that side.
#[gpui::test]
fn a_break_in_a_table_cell_stays_a_break_under_the_caret(cx: &mut TestAppContext) {
    let table = "| a |\n| --- |\n| 1<br/>2 |\n";
    let doc = |h: &mut Harness<'_>| h.app.update(h.cx, |app, cx| app.active_document(cx));
    for (caret, expected) in [(9, "| 1x<br/>2 |"), (10, "| 1<br/>x2 |")] {
        let mut h = open_with(cx, &[("t.md", table)], |_| {});
        let before = doc(&mut h);
        h.select(caret, caret);
        assert_eq!(doc(&mut h), before, "the break is not spelled out");
        h.type_text("x");
        h.save();
        let text = h.wait_for_file("t.md", |text| text.contains('x'));
        assert!(text.contains(expected), "{text:?}");
    }
}

// Shift-Return in a table cell writes `<br />`.
#[gpui::test]
fn shift_return_in_a_table_cell_writes_a_break_tag(cx: &mut TestAppContext) {
    let table = "| 1 | 2 |\n| --- | --- |\n| 3 | 4 |\n";
    let mut h = open_with(cx, &[("t.md", table)], |_| {});
    h.keys("cmd-down shift-enter");
    h.type_text("9");
    h.save();
    let expected = "| 1 | 2 |\n| --- | --- |\n| 3 | 4<br />9 |\n";
    assert_eq!(h.wait_for_file("t.md", |text| text == expected), expected);
}

// Moving into another cell with Tab, Shift-Tab or Return selects what it
// holds, so typing replaces it.
#[gpui::test]
fn moving_into_a_cell_selects_its_content(cx: &mut TestAppContext) {
    let table = "| a | b |\n| - | - |\n| c | d |\n";
    for (keys, cells) in [
        ("cmd-up tab", ["a", "x", "c", "d"]),
        ("cmd-up tab shift-tab", ["x", "b", "c", "d"]),
        ("cmd-up enter", ["a", "b", "x", "d"]),
    ] {
        let mut h = open_with(cx, &[("t.md", table)], |_| {});
        h.keys(keys);
        h.type_text("x");
        let markdown = h.markdown();
        let found: Vec<_> = markdown
            .lines()
            .filter(|line| !line.contains('-'))
            .flat_map(|line| {
                line.split('|')
                    .map(str::trim)
                    .filter(|cell| !cell.is_empty())
            })
            .collect();
        assert_eq!(found, cells, "{keys}: {markdown:?}");
    }
}
