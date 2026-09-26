//! A Chinese input method as the note sees it: letters marked as a candidate,
//! then committed as characters, or cancelled. There is no real input method on
//! the test platform; these calls are the ones macOS makes through
//! `NSTextInputClient`, in the order pinyin makes them.

use super::harness::{Harness, open_with};
use gpui::{EntityInputHandler, TestAppContext};

/// Mark each of `steps` in turn as the candidate grows, as pinyin shows `n`,
/// then `ni`, with the caret after the last letter.
fn mark(h: &mut Harness, steps: &[&str]) {
    let editor = h.app.update(h.cx, |app, _| app.test_editor());
    for step in steps {
        let caret = step.encode_utf16().count();
        editor.update_in(h.cx, |view, window, cx| {
            view.replace_and_mark_text_in_range(None, step, Some(caret..caret), window, cx)
        });
        h.cx.run_until_parked();
    }
}

/// Commit `text` in place of the candidate.
fn commit(h: &mut Harness, text: &str) {
    let editor = h.app.update(h.cx, |app, _| app.test_editor());
    editor.update_in(h.cx, |view, window, cx| {
        view.replace_text_in_range(None, text, window, cx)
    });
    h.cx.run_until_parked();
}

/// Cancel the candidate, as Escape in the candidate window does.
fn cancel(h: &mut Harness) {
    let editor = h.app.update(h.cx, |app, _| app.test_editor());
    editor.update_in(h.cx, |view, window, cx| {
        view.replace_and_mark_text_in_range(None, "", None, window, cx)
    });
    h.cx.run_until_parked();
}

fn marked(h: &mut Harness) -> Option<std::ops::Range<usize>> {
    let editor = h.app.update(h.cx, |app, _| app.test_editor());
    editor.update_in(h.cx, |view, window, cx| view.marked_text_range(window, cx))
}

/// Compose 你好 from its pinyin, letter by letter, and commit it.
fn compose_nihao(h: &mut Harness) {
    mark(h, &["n", "ni", "nih", "niha", "nihao"]);
    commit(h, "你好");
}

// A candidate still being composed is not the note's text: neither ⌘S nor
// autosave writes it, and it stays marked through the save. Committed, it is.
#[gpui::test]
fn a_candidate_being_composed_never_reaches_the_file(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("n.md", "a\n")], |_| {});
    h.keys("cmd-down cmd-right");
    h.type_text("b");
    mark(&mut h, &["n", "ni"]);
    h.pass_time(std::time::Duration::from_secs(5));
    h.save();
    assert_eq!(h.error(), None);
    assert_eq!(
        std::fs::read_to_string(h.notes.join("n.md")).unwrap(),
        "ab\n"
    );
    assert_eq!(marked(&mut h), Some(2..4), "the save ended the composition");

    commit(&mut h, "你");
    h.save();
    assert_eq!(h.wait_for_file("n.md", |text| text == "ab你\n"), "ab你\n");
}

// A word committed at the end of each kind of block lands in that block, and
// what is written is only the word: the rest of the file keeps its spelling.
#[gpui::test]
fn a_committed_word_lands_in_every_kind_of_block(cx: &mut TestAppContext) {
    for (file, expected) in [
        ("- a\n", "- a你好\n"),
        ("1. a\n", "1. a你好\n"),
        ("- [ ] a\n", "- [ ] a你好\n"),
        ("# a\n", "# a你好\n"),
        ("> a\n", "> a你好\n"),
        ("```\na\n```\n", "```\na你好\n```\n"),
        (
            "| x | y |\n| - | - |\n| 1 | a |\n",
            "| x | y |\n| - | - |\n| 1 | a你好 |\n",
        ),
        ("a [l](u)\n", "a [l](u)你好\n"),
    ] {
        let mut h = open_with(cx, &[("b.md", file)], |_| {});
        h.keys("cmd-down cmd-right");
        compose_nihao(&mut h);
        h.save();
        assert_eq!(h.error(), None, "{file:?}");
        assert_eq!(
            h.wait_for_file("b.md", |text| text == expected),
            expected,
            "{file:?}"
        );
        h.assert_round_trip(file);
    }
}

// Undo takes a committed word back whole, never a letter of its pinyin or
// half of it, and redo puts it back.
#[gpui::test]
fn a_committed_word_undoes_and_redoes_whole(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("n.md", "a\n")], |_| {});
    h.keys("cmd-down cmd-right");
    compose_nihao(&mut h);
    h.keys("cmd-z");
    assert_eq!(h.markdown(), "a");
    h.keys("cmd-shift-z");
    assert_eq!(h.markdown(), "a你好");
    h.assert_round_trip("undo and redo of a committed word");
}

// A commit is typing: it lands on the same side of a bold edge as a key would,
// a full-width bracket pairs, and `-` then a space made through the input
// method starts a list.
#[gpui::test]
fn a_commit_behaves_as_the_same_text_typed(cx: &mut TestAppContext) {
    for (file, at) in [("**a**\n", None), ("a **b** c\n", Some(3))] {
        let place = |h: &mut Harness| match at {
            Some(position) => h.select(position, position),
            None => h.keys("cmd-down cmd-right"),
        };
        let mut h = open_with(cx, &[("n.md", file)], |_| {});
        place(&mut h);
        h.type_text("x");
        let typed = h.markdown();
        let mut h = open_with(cx, &[("n.md", file)], |_| {});
        place(&mut h);
        mark(&mut h, &["x"]);
        commit(&mut h, "x");
        assert_eq!(h.markdown(), typed, "{file:?}");
    }

    let mut h = open_with(cx, &[("n.md", "a\n")], |_| {});
    h.keys("cmd-down cmd-right enter");
    commit(&mut h, "（");
    assert_eq!(h.markdown(), "a\n\n（）");

    let mut h = open_with(cx, &[("n.md", "a\n")], |_| {});
    h.keys("cmd-down cmd-right enter");
    commit(&mut h, "-");
    commit(&mut h, " ");
    mark(&mut h, &["n"]);
    commit(&mut h, "你");
    assert_eq!(h.markdown(), "a\n\n- 你");
    h.assert_round_trip("a list started through the input method");
}

// Cancelling a candidate composed over a selection leaves the note as it was.
#[gpui::test]
fn a_cancelled_candidate_leaves_the_note_as_it_was(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("n.md", "abc\n")], |_| {});
    h.select(1, 2);
    mark(&mut h, &["n", "ni"]);
    cancel(&mut h);
    assert_eq!(marked(&mut h), None);
    assert_eq!(h.markdown(), "abc");
    h.save();
    assert_eq!(
        std::fs::read_to_string(h.notes.join("n.md")).unwrap(),
        "abc\n"
    );
}

// Leaving a note mid-composition keeps its pinyin out of both notes' files, and
// the note returned to is not left composing.
#[gpui::test]
fn leaving_a_note_mid_composition_leaks_nothing(cx: &mut TestAppContext) {
    let mut h = open_with(
        cx,
        &[("alpha.md", "alpha\n"), ("beta.md", "beta\n")],
        |_| {},
    );
    h.browse_to("alpha");
    h.keys("cmd-down cmd-right");
    mark(&mut h, &["n", "ni"]);
    h.browse_to("beta");
    assert_eq!(h.markdown(), "beta");
    h.save();
    assert_eq!(
        std::fs::read_to_string(h.notes.join("alpha.md")).unwrap(),
        "alpha\n"
    );
    assert_eq!(
        std::fs::read_to_string(h.notes.join("beta.md")).unwrap(),
        "beta\n"
    );
    h.browse_to("alpha");
    assert_eq!(h.markdown(), "alpha");
    assert_eq!(marked(&mut h), None);
}
