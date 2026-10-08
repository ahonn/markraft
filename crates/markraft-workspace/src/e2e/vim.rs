//! Vim through the bindings a person presses and the editor the app hosts.

use super::harness::{Harness, open_with};
use crate::app::HideWindow;
use gpui::{Action, KeyContext, Keystroke, TestAppContext};

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

// Text objects are two-key bindings that only stand while an operator waits or a
// visual mode is on, so `i` and `a` still enter Insert mode on their own.
#[gpui::test]
fn vim_text_objects_reach_the_note_through_the_real_bindings(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("o.md", "one two f(x, y) say \"hi\"\n")], |p| {
        p.vim_mode = true
    });
    h.keys("cmd-up w d i w");
    assert_eq!(h.markdown(), "one  f(x, y) say \"hi\"");
    h.keys("w l l c i (");
    h.type_text("z");
    h.keys("escape");
    assert_eq!(h.markdown(), "one  f(z) say \"hi\"");
    h.keys("$ d a \"");
    assert_eq!(h.markdown(), "one  f(z) say");
    h.keys("0 w v i w d");
    assert_eq!(h.markdown(), "one  (z) say");
    h.keys("i");
    h.type_text("a");
    assert_eq!(h.markdown(), "one  a(z) say");
    h.assert_round_trip("vim text objects");
}

/// The labels the actions panel lists, or `None` while it is closed.
fn action_labels(h: &mut super::harness::Harness<'_>) -> Option<Vec<String>> {
    h.app.update(h.cx, |app, cx| app.test_action_labels(cx))
}

// `:` in Normal mode opens the actions panel as a command line: a `:` query lists
// only the commands vim has names for, the one it names first, so Return runs it.
#[gpui::test]
fn vim_colon_runs_the_command_it_names(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("c.md", "one two\n")], |p| p.vim_mode = true);
    h.keys("cmd-up :");
    let all = action_labels(&mut h).expect("the panel is open");
    assert!(all.contains(&"Save Now".to_owned()) && all.contains(&"Hide Window".to_owned()));
    assert!(
        !all.contains(&"Bold".to_owned()),
        "only commands vim has a name for"
    );
    h.type_text("q");
    assert_eq!(action_labels(&mut h).unwrap()[0], "Hide Window");
    h.keys("escape");
    assert_eq!(action_labels(&mut h), None);

    // `:u` undoes and `:red` redoes, as the editor's own keys do.
    h.keys("d w :");
    h.type_text("u");
    h.keys("enter");
    assert_eq!(action_labels(&mut h), None);
    assert_eq!(h.markdown(), "one two");
    h.keys(":");
    h.type_text("red");
    h.keys("enter");
    assert_eq!(h.markdown(), "two");

    // `:w` names Save Now before the longer `:wq`, `:wqa` it begins.
    h.keys(":");
    h.type_text("w");
    assert_eq!(action_labels(&mut h).unwrap()[0], "Save Now");
    h.keys("enter");
    h.wait_for_io();
    assert_eq!(h.markdown(), "two");
    h.assert_round_trip("vim colon commands");
}

// The commands that exist only for `:` stay out of the panel's own search, and
// `:` is text, not a command line, in Insert mode.
#[gpui::test]
fn vim_colon_commands_keep_to_the_command_line(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("c.md", "one\n")], |p| p.vim_mode = true);
    h.keys("cmd-k");
    h.type_text("Hide Window");
    assert_eq!(action_labels(&mut h), Some(Vec::new()));
    // One Escape closes the panel; a second would hide the window.
    h.keys("escape cmd-up A");
    h.type_text(":");
    assert_eq!(action_labels(&mut h), None);
    assert_eq!(h.markdown(), "one:");
}

// In vim Escape leaves every command, so one pressed too often in Normal mode keeps
// the window, and quietly: vim users press it by habit. The test platform cannot
// hide a window, so a hide here would fail the test outright.
#[gpui::test]
fn vim_escape_in_normal_mode_stays_put_quietly(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("e.md", "one\n")], |p| p.vim_mode = true);
    h.keys("cmd-up i escape escape escape");
    let notice = h.app.update(h.cx, |app, _| app.test_notice());
    assert_eq!(notice, None);
    h.type_text("x");
    assert_eq!(h.markdown(), "ne");
}

// Vim's own tests read its bindings on their own; here they sit in the app's whole
// keymap, beside the editor's and the app's, under the context the focused editor
// really has. In each mode every vim binding that applies must be what its keys
// run — none taken by another binding first, none left waiting for a longer one.
#[gpui::test]
fn every_vim_binding_is_what_its_keys_run_in_each_mode(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("m.md", "one (two) \"three\"\n")], |p| {
        p.vim_mode = true
    });
    h.keys("cmd-up");
    for (enter, mode, operator) in [
        ("", "normal", None),
        ("d", "normal", Some("d")),
        ("c", "normal", Some("c")),
        ("v", "visual", None),
        ("shift-v", "visual_line", None),
        ("i", "insert", None),
    ] {
        if !enter.is_empty() {
            h.keys(enter);
        }
        h.cx.update(|window, _| window.refresh());
        h.cx.run_until_parked();
        let (stack, keymap) =
            h.cx.update(|window, cx| (window.context_stack(), cx.key_bindings()));
        let read = |key: &str| {
            stack
                .iter()
                .rev()
                .find_map(|context| context.get(key).map(|value| value.to_string()))
        };
        assert_eq!(read("vim_mode").as_deref(), Some(mode), "after {enter:?}");
        assert_eq!(read("vim_operator").as_deref(), operator, "after {enter:?}");
        let keymap = keymap.borrow();
        let mut checked = 0;
        for binding in keymap
            .bindings()
            .filter(|binding| binding.action().name().starts_with("markraft_vim::"))
            .filter(|binding| binding.predicate().is_some_and(|when| when.eval(&stack)))
        {
            let (found, pending) = keymap.bindings_for_input(binding.keystrokes(), &stack);
            let keys = binding
                .keystrokes()
                .iter()
                .map(|key| key.inner().unparse())
                .collect::<Vec<_>>()
                .join(" ");
            assert!(
                !pending,
                "after {enter:?}, {keys:?} waits for a longer binding"
            );
            assert!(
                found
                    .first()
                    .is_some_and(|first| first.action().partial_eq(binding.action())),
                "after {enter:?}, {keys:?} runs {:?} rather than {}",
                found.first().map(|first| first.action().name()),
                binding.action().name()
            );
            checked += 1;
        }
        assert!(checked > 0, "after {enter:?} no vim binding applies");
        drop(keymap);
        h.keys("escape");
    }
    assert_eq!(
        h.markdown(),
        "one (two) \"three\"",
        "the sweep edits nothing"
    );
}

// Escape walks back out through whatever is open, and in vim it never hides the
// window, so ⌘W is the key that hides it outright, in every mode. The settings
// window binds ⌘W to closing itself, and that must still win while it is open.
#[gpui::test]
fn cmd_w_hides_the_window_in_every_mode_but_closes_settings_first(cx: &mut TestAppContext) {
    /// What ⌘W runs under `stack`, or `None` while it waits for a longer binding.
    fn cmd_w_runs(h: &mut Harness, stack: &[KeyContext]) -> Option<Box<dyn Action>> {
        let keymap = h.cx.update(|_, cx| cx.key_bindings());
        let keystrokes = [Keystroke::parse("cmd-w").expect("a keystroke")];
        let (found, pending) = keymap.borrow().bindings_for_input(&keystrokes, stack);
        let first = found.first().filter(|_| !pending)?;
        Some(first.action().boxed_clone())
    }
    /// The focused editor's context stack, and the vim mode it reports.
    fn settled_stack(h: &mut Harness) -> (Vec<KeyContext>, Option<String>) {
        h.cx.update(|window, _| window.refresh());
        h.cx.run_until_parked();
        let stack = h.cx.update(|window, _| window.context_stack());
        let mode = stack
            .iter()
            .rev()
            .find_map(|context| context.get("vim_mode").map(|value| value.to_string()));
        (stack, mode)
    }
    let hides_window = |runs: &Option<Box<dyn Action>>| {
        runs.as_ref()
            .is_some_and(|action| action.partial_eq(&HideWindow))
    };

    let mut h = open_with(cx, &[("w.md", "abc\n")], |p| p.vim_mode = true);
    h.keys("cmd-up");
    for (enter, mode) in [("", "normal"), ("i", "insert")] {
        if !enter.is_empty() {
            h.keys(enter);
        }
        let (stack, vim_mode) = settled_stack(&mut h);
        assert_eq!(vim_mode.as_deref(), Some(mode), "after {enter:?}");
        let runs = cmd_w_runs(&mut h, &stack);
        assert!(hides_window(&runs), "vim {mode}: {runs:?}");
    }
    // Settings is a window of its own, so the note's context never reaches it; its
    // root context sits under the same keymap, and there ⌘W must close it.
    #[cfg(feature = "bundled-settings")]
    {
        let settings = [KeyContext::parse("MarkraftSettings").expect("a context")];
        let runs = cmd_w_runs(&mut h, &settings);
        assert_eq!(
            runs.as_ref().map(|action| action.name()),
            Some("markraft_settings::CloseSettings")
        );
    }
    drop(h);

    let mut h = open_with(cx, &[("w.md", "abc\n")], |p| p.vim_mode = false);
    let (stack, vim_mode) = settled_stack(&mut h);
    assert_eq!(vim_mode, None);
    let runs = cmd_w_runs(&mut h, &stack);
    assert!(hides_window(&runs), "without vim: {runs:?}");
}
