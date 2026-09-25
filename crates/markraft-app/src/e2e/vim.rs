//! Vim through the bindings a person presses and the editor the app hosts.

use super::harness::open_with;
use gpui::TestAppContext;

// Vim's normal mode edits the note, and what it does is saved.
#[gpui::test]
fn vim_edits_reach_the_file(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("v.md", "abc\n")], |p| p.vim_mode = true);
    // Vim starts in normal mode; Escape there would hide the window.
    h.keys("cmd-up");
    h.type_text("x");
    assert_eq!(h.markdown(), "bc");
    h.assert_round_trip("vim x");
}

// Vim's own tests drive a stand-in host with a keymap of their own; these keys
// go through the bindings a person presses and the editor the app hosts: an
// operator with a motion, undo and redo, a count, and a visual delete.
#[gpui::test]
fn vim_keys_reach_the_note_through_the_real_bindings(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("v.md", "one two three\n")], |p| p.vim_mode = true);
    h.keys("cmd-up");
    h.keys("d w");
    assert_eq!(h.markdown(), "two three");
    h.keys("u");
    assert_eq!(h.markdown(), "one two three");
    h.keys("ctrl-r");
    assert_eq!(h.markdown(), "two three");
    h.keys("2 x");
    assert_eq!(h.markdown(), "o three");
    h.keys("v l d");
    assert_eq!(h.markdown(), "three");
    h.assert_round_trip("vim operators");
}

// vim's `j` leaves a code block or a table that ends the note for a new
// block, as ↓ does — through the real bindings and a laid-out editor, which
// is the path the unit tests over a bare state cannot take.
#[gpui::test]
fn vim_j_leaves_a_final_code_block_and_table_as_the_arrow_does(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("c.md", "```\n1\n2\n```\n")], |p| p.vim_mode = true);
    h.keys("cmd-up j j i");
    h.type_text("8");
    assert_eq!(h.markdown(), "```\n1\n2\n```\n\n8");
    let mut h = open_with(cx, &[("t.md", "| 1 | 2 |\n| - | - |\n| 3 | 4 |\n")], |p| {
        p.vim_mode = true
    });
    h.keys("cmd-up j j i");
    h.type_text("8");
    assert!(h.markdown().ends_with("|\n\n8"), "{:?}", h.markdown());
}
