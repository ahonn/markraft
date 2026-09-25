//! Block edits: lists, task items, code blocks, dividers and the block shortcuts.

use super::harness::{open, open_with};
use gpui::TestAppContext;

// A code block fenced in a new task item saves, and shows what is typed in
// it. On a real Mac the autosave lands between keystrokes; each step here is
// saved the same way, so every intermediate source has to write back.
#[gpui::test]
fn a_code_block_fenced_in_a_task_item_saves(cx: &mut TestAppContext) {
    let mut h = open(cx, |_| {});
    let steps = [
        "t", "t", "enter", "-", " ", "[", " ", "]", " ", "t", "1", "enter", "`", "`", "`", "enter",
        "c", "o", "d", "e",
    ];
    for step in steps {
        if step == "enter" {
            h.keys("enter");
        } else {
            h.type_text(step);
        }
        h.save();
        assert_eq!(h.error(), None, "after {step:?}: {:?}", h.markdown());
    }
    assert_eq!(h.markdown(), "tt\n\n- [ ] t1\n- [ ] \n  ```\n  code\n  ```");
}

// Return at the end of an item's first paragraph that more blocks of the
// item follow splits the item: the new item takes those
// blocks. Until something is typed there the file holds its marker alone on
// its line, which reads back as the same item.
#[gpui::test]
fn return_before_more_blocks_of_an_item_splits_it(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("item.md", "- first\n\n  para2\n- next\n")], |_| {});
    h.keys("cmd-up cmd-right enter");
    h.save();
    assert_eq!(h.error(), None);
    let split = "- first\n\n- \n  para2\n- next\n";
    assert_eq!(h.wait_for_file("item.md", |text| text == split), split);
    h.type_text("new");
    assert_eq!(h.markdown(), "- first\n\n- new\n\n  para2\n\n- next");
    h.save();
    let text = h.wait_for_file("item.md", |text| text.contains("new"));
    assert_eq!(text, "- first\n\n- new\n\n  para2\n- next\n");
}

// ⌥⌘C makes a code block with the fence the preferences ask for: on an
// empty line in place of it, and after a line with text, with the caret in
// it.
#[gpui::test]
fn the_code_block_shortcut_takes_the_preferred_fence(cx: &mut TestAppContext) {
    let mut h = open(cx, |p| p.code_fence = crate::storage::CodeFence::Tildes);
    h.keys("alt-cmd-c");
    h.type_text("x");
    assert_eq!(h.markdown(), "~~~\nx\n~~~");
    let mut h = open(cx, |p| p.code_fence = crate::storage::CodeFence::Tildes);
    h.type_text("x");
    h.keys("alt-cmd-c");
    h.type_text("y");
    assert_eq!(h.markdown(), "x\n\n~~~\ny\n~~~");
}

// With Markdown shortcuts off, a fence typed at the start of a line stays text.
#[gpui::test]
fn a_fence_stays_text_with_markdown_shortcuts_off(cx: &mut TestAppContext) {
    let mut h = open(cx, |p| p.markdown_shortcuts = false);
    h.type_text("```");
    h.keys("enter");
    h.type_text("x");
    assert_eq!(h.markdown(), "\\```\n\nx");
    // The fence is punctuation, not a name: the note is filed after `x`.
    h.save();
    let text = h.wait_for_file("x.md", |text| text == "\\```\n\nx\n");
    assert_eq!(text, "\\```\n\nx\n", "files: {:?}", h.files());
}

// The list shortcuts turn the whole list into the other kind.
#[gpui::test]
fn a_list_shortcut_converts_the_list_it_is_in(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("list.md", "1. a\n2. b\n")], |_| {});
    h.keys("cmd-down");
    h.keys("cmd-*");
    assert_eq!(h.markdown(), "- a\n- b");
}

// A new line in a code block starts where the one above it did.
#[gpui::test]
fn return_in_a_code_block_keeps_the_indent(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("code.md", "```\nfn a() {\n    x\n```\n")], |_| {});
    h.keys("cmd-up down down end enter");
    h.type_text("y");
    assert_eq!(h.markdown(), "```\nfn a() {\n    x\n    y\n```");
}

// Deleting across the edge of a list or a quote joins what it meets:
// Delete at the end of a textblock and Backspace at the start of one after a
// list or a quote join their text; Backspace at an item's start joins it to
// the item before, or to the item its nested list is in.
#[gpui::test]
fn deleting_across_list_and_quote_edges_joins_the_text(cx: &mut TestAppContext) {
    let cases: &[(&str, &str, &str)] = &[
        ("- 1\n- 2\n", "cmd-up cmd-right delete", "- 192"),
        ("- 1\n\n2\n", "cmd-down cmd-left backspace", "- 192"),
        ("- 1\n", "cmd-up cmd-right enter backspace", "- 1\n\n  9"),
        ("- 1\n- 2\n", "cmd-down cmd-left backspace", "- 1\n\n  92"),
        ("- 1\n- 2\n", "cmd-up backspace", "91\n\n- 2"),
        ("0\n\n- 1\n", "cmd-up cmd-right delete", "091"),
        ("- 1\n\n2\n", "cmd-up cmd-right delete", "- 192"),
        ("- 1\n  - 2\n", "cmd-down cmd-left backspace", "- 1\n\n  92"),
        ("- 1\n  - 2\n", "cmd-up cmd-right delete", "- 192"),
        (
            "1. 1\n2. 2\n",
            "cmd-down cmd-left backspace",
            "1. 1\n\n   92",
        ),
        (
            "- [ ] 1\n- [ ] 2\n",
            "cmd-down cmd-left backspace",
            "- [ ] 1\n\n  92",
        ),
        ("> 1\n\n2\n", "cmd-down cmd-left backspace", "> 192"),
        (
            "```\n1\n```\n\n2\n",
            "cmd-down cmd-left backspace",
            "```\n192\n```",
        ),
        (
            "1\n\n```\n2\n```\n",
            "cmd-up cmd-right delete",
            "19\n\n```\n2\n```",
        ),
    ];
    for (source, keys, expected) in cases {
        let mut h = open_with(cx, &[("x.md", source)], |_| {});
        h.keys(keys);
        h.type_text("9");
        assert_eq!(h.markdown(), *expected, "{source:?} {keys}");
        h.assert_round_trip(keys);
    }
}

// Around a divider, Backspace and Delete take it with one press, and no
// empty line is left where it was.
#[gpui::test]
fn backspace_and_delete_take_a_divider_with_one_press(cx: &mut TestAppContext) {
    for (keys, expected) in [
        ("cmd-down cmd-left backspace", "a\n\nb\n"),
        ("cmd-up cmd-right delete", "ab\n"),
    ] {
        let mut h = open_with(cx, &[("d.md", "a\n\n---\n\nb\n")], |_| {});
        h.keys(keys);
        h.save();
        let text = h.wait_for_file("d.md", |text| text == expected);
        assert_eq!(text, expected, "{keys}");
    }
}

// Backspace right after a block shortcut takes the format off, rather
// than giving back the characters that made it. A shortcut
// inside the text is still undone to what was typed.
#[gpui::test]
fn backspace_after_a_block_shortcut_takes_the_format_off(cx: &mut TestAppContext) {
    for typed in ["- ", "1. ", "# ", "## ", "> ", "- [ ] "] {
        let mut h = open(cx, |_| {});
        h.type_text(typed);
        h.keys("backspace");
        h.type_text("x");
        assert_eq!(h.markdown(), "x", "{typed:?}");
    }
    // In a list already, the item leaves the list, as Backspace at any
    // first item's start does.
    let mut h = open_with(cx, &[("l.md", "a\n")], |_| {});
    h.keys("cmd-down enter");
    h.type_text("- ");
    h.keys("backspace");
    h.type_text("x");
    assert_eq!(h.markdown(), "a\n\nx");
}

// A divider typed with stars or underscores keeps them.
#[gpui::test]
fn a_typed_divider_keeps_its_characters(cx: &mut TestAppContext) {
    for divider in ["***", "___", "---"] {
        let mut h = open_with(cx, &[("d.md", "0\n")], |_| {});
        h.keys("cmd-down enter");
        h.type_text(divider);
        h.keys("enter");
        h.type_text("5");
        h.save();
        let expected = format!("0\n\n{divider}\n\n5\n");
        let text = h.wait_for_file("d.md", |text| text == expected);
        assert_eq!(text, expected, "{divider}");
    }
}

// ⌥⌘C at a caret in text opens a new code
// block there — splitting the paragraph in its middle, and inside a task
// item too — and on an empty line or over a selection it makes that a
// code block, the caret at its start.
#[gpui::test]
fn the_code_block_shortcut_opens_a_block_at_the_caret(cx: &mut TestAppContext) {
    let cases: &[(&str, (usize, usize), &str)] = &[
        ("1234\n", (3, 3), "12\n\n```\n9\n```\n\n34"),
        ("1234\n", (5, 5), "1234\n\n```\n9\n```"),
        ("1234\n", (1, 1), "```\n9\n```\n\n1234"),
        ("1234\n", (1, 5), "```\n91234\n```"),
        (
            "- [ ] 12\n- [ ] 3\n",
            (5, 5),
            "- [ ] 12\n  ```\n  9\n  ```\n- [ ] 3",
        ),
    ];
    for (source, (anchor, head), expected) in cases {
        let mut h = open_with(cx, &[("c.md", source)], |_| {});
        h.select(*anchor, *head);
        h.keys("alt-cmd-c");
        h.type_text("9");
        assert_eq!(h.markdown(), *expected, "{source:?} at {anchor}..{head}");
        h.assert_round_trip("a code block at the caret");
    }
}

// A task box written `[X]` ticks off like any other.
#[gpui::test]
fn an_uppercase_task_box_toggles(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("t.md", "- [X] done\n")], |_| {});
    h.keys("cmd-down cmd-enter");
    assert_eq!(h.markdown(), "- [ ] done");
    h.assert_round_trip("unticking [X]");
}

// Every block format applied to a task item's text.
#[gpui::test]
fn block_formats_inside_a_task_item(cx: &mut TestAppContext) {
    // A task's line may be a heading or a quote after its box,
    // but not a fence, which goes under the box's line.
    let cases = [
        ("cmd-1", "- [ ] # t"),
        ("cmd-2", "- [ ] ## t"),
        ("cmd-6", "- [ ] ###### t"),
        ("cmd-0", "- [ ] t"),
        ("cmd-shift-b", "- [ ] > t"),
        ("alt-cmd-c", "- [ ] t\n  ```\n  ```"),
        ("cmd-&", "1. t"),
        ("cmd-*", "- t"),
        ("cmd-(", "t"),
    ];
    for (key, expected) in cases {
        let mut h = open_with(cx, &[("t.md", "- [ ] t\n")], |_| {});
        h.keys("cmd-down");
        h.keys(key);
        assert_eq!(h.markdown(), expected, "{key}");
        h.assert_round_trip(key);
    }
}

// The first item of a list, made by Return and lifted out again, is a
// paragraph of its own that typing goes into, not the heading above.
#[gpui::test]
fn typing_into_a_new_first_item_lifted_out_stays_below_the_heading(cx: &mut TestAppContext) {
    for (list, rest) in [
        ("1. one\n2. two\n", "1. one\n2. two"),
        ("- one\n- two\n", "- one\n- two"),
    ] {
        let source = format!("# Lists\n\n{list}");
        let mut h = open_with(cx, &[("l.md", source.as_str())], |_| {});
        h.keys("cmd-up cmd-right down cmd-left enter up backspace");
        h.type_text("5");
        assert_eq!(h.markdown(), format!("# Lists\n\n5\n\n{rest}"));
    }
}

// Return inside a code line's indent drops the blanks after the caret.
#[gpui::test]
fn return_in_a_code_indent_drops_the_blanks_after_the_caret(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("c.md", "```\n    x\n```\n")], |_| {});
    h.keys("cmd-up right right enter");
    h.type_text("y");
    assert_eq!(h.markdown(), "```\n  \n  yx\n```");
}

// A new item numbers the ones after it again in the file, unless the list
// is written with one number throughout.
#[gpui::test]
fn an_item_added_to_an_ordered_list_renumbers_the_file(cx: &mut TestAppContext) {
    for (source, keys, expected) in [
        (
            "1. one\n2. two\n",
            "cmd-up cmd-left enter up",
            "1. 5\n2. one\n3. two\n",
        ),
        (
            "1. a\n1. b\n",
            "cmd-down cmd-right enter",
            "1. a\n1. b\n1. 5\n",
        ),
    ] {
        let mut h = open_with(cx, &[("o.md", source)], |_| {});
        h.keys(keys);
        h.type_text("5");
        h.save();
        let text = h.wait_for_file("o.md", |text| text != source);
        assert_eq!(text, expected, "{source:?}");
    }
}
