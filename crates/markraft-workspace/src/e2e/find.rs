//! Finding in the note: the bar, the visible word, and stepping through hits.

use super::harness::open_with;
use gpui::{EntityInputHandler, TestAppContext};

/// What the selection covers in the document, which for a concealed span is
/// the word and not the markers around it.
fn selected_source(h: &mut super::harness::Harness<'_>) -> String {
    let editor = h.app.update(h.cx, |app, _| app.test_editor());
    editor.update(h.cx, |editor, _| {
        let doc = editor.doc();
        let selection = editor.state().selection();
        doc.text_between(
            editor.schema(),
            selection.from(doc),
            selection.to(doc),
            None,
            None,
        )
    })
}

fn selected_from(h: &mut super::harness::Harness<'_>) -> usize {
    let selection = h.selection();
    match selection {
        markraft_core::Selection::Text { anchor, head } => anchor.min(head),
        other => panic!("a find hit is a text selection, not {other:?}"),
    }
}

// ⌘F, a query, and ⌘G: the hit is the word a reader sees, and the next one
// is the later copy. Escape leaves that selection and clears the query.
#[gpui::test]
fn find_selects_the_visible_word_and_steps(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("n.md", "alpha **bold** and bold\n")], |_| {});
    h.keys("cmd-f");
    h.type_text("bold");
    assert_eq!(selected_source(&mut h), "bold");
    let editor = h.app.update(h.cx, |app, _| app.test_editor());
    let status = editor.update(h.cx, |editor, _| editor.find_status());
    assert_eq!(status.query, "bold");
    assert_eq!(status.total, 2);
    assert_eq!(status.current, Some(0));
    let first = selected_from(&mut h);
    h.keys("cmd-g");
    assert_eq!(selected_source(&mut h), "bold");
    assert!(selected_from(&mut h) > first);
    h.keys("cmd-shift-g");
    assert_eq!(selected_from(&mut h), first);
    h.keys("escape");
    let status = editor.update(h.cx, |editor, _| editor.find_status());
    assert!(status.query.is_empty());
    assert_eq!(selected_source(&mut h), "bold");
    assert_eq!(selected_from(&mut h), first);
}

// A new note opened while finding keeps the query, on a document that has
// no copy of it.
#[gpui::test]
fn a_new_note_keeps_the_open_query(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("n.md", "alpha **bold** and bold\n")], |_| {});
    h.keys("cmd-f");
    h.type_text("bold");
    h.keys("cmd-n");
    let editor = h.app.update(h.cx, |app, _| app.test_editor());
    let status = editor.update(h.cx, |editor, _| editor.find_status());
    assert_eq!(status.query, "bold");
    assert_eq!(status.total, 0);
}

#[gpui::test]
fn find_selects_complete_chinese_matches_and_wraps(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("n.md", "中文搜索：你好世界，你好朋友。\n")], |_| {});
    h.keys("cmd-f");
    h.type_text("你好");
    assert_eq!(selected_source(&mut h), "你好");
    let editor = h.app.update(h.cx, |app, _| app.test_editor());
    let status = editor.update(h.cx, |editor, _| editor.find_status());
    assert_eq!(status.query, "你好");
    assert_eq!(status.total, 2);
    assert_eq!(status.current, Some(0));
    let first = selected_from(&mut h);
    h.keys("cmd-g");
    assert_eq!(selected_source(&mut h), "你好");
    assert!(selected_from(&mut h) > first);
    h.keys("cmd-g");
    assert_eq!(selected_from(&mut h), first);
}

fn assert_cursor(h: &mut super::harness::Harness<'_>, position: usize) {
    assert_eq!(h.selection(), markraft_core::Selection::cursor(position));
}

// Every query edit starts at the original caret, and Return accepts that
// preview without taking another step or leaving the keyboard in the field.
#[gpui::test]
fn vim_find_confirms_the_preview_and_steps_from_the_current_caret(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("n.md", "one two one two one\n")], |p| {
        p.vim_mode = true
    });
    h.keys("g g /");
    for (character, preview) in [("o", 7), ("n", 9), ("e", 9)] {
        h.type_text(character);
        assert_cursor(&mut h, preview);
    }
    h.keys("enter");
    assert_cursor(&mut h, 9);
    h.keys("n");
    assert_cursor(&mut h, 17);
    h.keys("N");
    assert_cursor(&mut h, 9);
    h.keys("g g n");
    assert_cursor(&mut h, 9);
    h.keys("N");
    assert_cursor(&mut h, 1);
    h.keys("N");
    assert_cursor(&mut h, 17);
    h.keys("n");
    assert_cursor(&mut h, 1);
    assert_eq!(h.markdown(), "one two one two one");
}

// The Vim caret is at the start of a hit: x removes its first character,
// while i inserts before it. The next search sees those edits.
#[gpui::test]
fn vim_find_edits_at_the_hit_start_and_recomputes_matches(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("n.md", "one two one two one\n")], |p| {
        p.vim_mode = true
    });
    h.keys("g g /");
    h.type_text("one");
    h.keys("enter x");
    assert_eq!(h.markdown(), "one two ne two one");
    h.keys("n");
    assert_cursor(&mut h, 16);
    h.keys("i");
    h.type_text("new ");
    assert_eq!(h.markdown(), "one two ne two new one");
    h.keys("escape u");
    assert_eq!(h.markdown(), "one two ne two one");
    h.keys("u");
    assert_eq!(h.markdown(), "one two one two one");
}

// Reopening selects the remembered query. Cancelling restores both the
// original caret and the accepted query used by the next n.
#[gpui::test]
fn vim_find_escape_restores_the_caret_and_previous_query(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("n.md", "one two one two one\n")], |p| {
        p.vim_mode = true
    });
    h.keys("g g /");
    h.type_text("one");
    h.keys("enter");
    h.select(5, 5);
    h.keys("/");
    h.type_text("two");
    assert_cursor(&mut h, 13);
    h.keys("escape");
    assert_cursor(&mut h, 5);
    h.keys("n");
    assert_cursor(&mut h, 9);
    assert_eq!(h.markdown(), "one two one two one");
}

#[gpui::test]
fn vim_find_handles_unicode_and_missing_queries_without_losing_note_focus(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("n.md", "你好 🙂 你好 🙂\n")], |p| {
        p.vim_mode = true
    });
    h.keys("g g n N");
    assert_cursor(&mut h, 1);
    assert!(h.app.update(h.cx, |app, _| app.test_notice()).is_some());
    h.keys("/");
    h.type_text("你好");
    let second = selected_from(&mut h);
    assert!(second > 1);
    assert_cursor(&mut h, second);
    h.keys("enter n");
    assert_cursor(&mut h, 1);
    h.keys("/");
    h.type_text("🙂");
    let first_emoji = selected_from(&mut h);
    h.keys("enter n");
    assert!(selected_from(&mut h) > first_emoji);
    h.keys("N");
    assert_cursor(&mut h, first_emoji);
    h.keys("/");
    h.type_text("absent");
    assert_cursor(&mut h, first_emoji);
    h.keys("enter n N");
    assert_cursor(&mut h, first_emoji);
    h.keys("i");
    h.type_text("X");
    assert_eq!(h.markdown(), "你好 X🙂 你好 🙂");
}

// A preview belongs to one note. Changing notes rolls back its query; an
// accepted query still works from the current caret in the newly opened note.
#[gpui::test]
fn vim_find_cancels_a_preview_when_changing_notes(cx: &mut TestAppContext) {
    let mut h = open_with(
        cx,
        &[("a.md", "one two one\n"), ("b.md", "one four one\n")],
        |p| p.vim_mode = true,
    );
    h.browse_to("a");
    h.keys("g g /");
    h.type_text("one");
    h.keys("enter /");
    h.type_text("two");
    h.browse_to("b");
    h.keys("g g n");
    assert_cursor(&mut h, 10);
    h.keys("/");
    h.type_text("four");
    h.keys("cmd-n");
    h.keys("n");
    assert_eq!(h.markdown(), "");
    h.browse_to("a");
    h.keys("g g n");
    assert_cursor(&mut h, 9);
    assert_eq!(h.markdown(), "one two one");
}

#[gpui::test]
fn vim_find_bindings_only_apply_in_idle_normal_mode(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("n.md", "one two one\n")], |p| p.vim_mode = true);
    for (enter, enabled) in [
        ("", true),
        ("i", false),
        ("v", false),
        ("shift-v", false),
        ("d", false),
        ("2", false),
    ] {
        h.keys(enter);
        h.cx.update(|window, _| window.refresh());
        h.cx.run_until_parked();
        let (stack, keymap) =
            h.cx.update(|window, cx| (window.context_stack(), cx.key_bindings()));
        let keymap = keymap.borrow();
        for action in ["VimFind", "VimFindNext", "VimFindPrevious"] {
            let suffix = format!("::{action}");
            let binding = keymap
                .bindings()
                .find(|binding| binding.action().name().ends_with(&suffix))
                .unwrap_or_else(|| panic!("{action} has a binding"));
            assert_eq!(
                binding.predicate().is_some_and(|when| when.eval(&stack)),
                enabled,
                "{action} after {enter:?}"
            );
        }
        drop(keymap);
        h.keys("escape");
    }
    h.keys("g g i");
    h.type_text("/nN");
    assert_eq!(h.markdown(), "/nNone two one");
}

// Replacing a document destroys its preview bookmark. The input session must
// end too, returning the keyboard without restoring a position from the old note.
#[gpui::test]
fn vim_find_document_replacement_ends_the_preview_and_returns_focus(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("n.md", "one two one two one\n")], |p| {
        p.vim_mode = true
    });
    h.select(9, 9);
    h.keys("/");
    h.type_text("one");
    assert_cursor(&mut h, 17);
    h.edit(|editor, cx| {
        editor.replace_doc(crate::doc::from_markdown("new"), cx);
        true
    });
    assert_cursor(&mut h, 1);
    h.keys("escape x");
    assert_eq!(h.markdown(), "ew");
}

#[gpui::test]
fn vim_find_external_reload_ends_the_old_session_preview(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("n.md", "one two one two one\n")], |p| {
        p.vim_mode = true
    });
    let note_id = h.active_note().id;
    let old_editor = h.app.update(h.cx, |app, _| app.test_editor());
    h.keys("g g /");
    h.type_text("one");
    h.keys("enter /");
    h.type_text("two");
    std::fs::write(h.notes.join("n.md"), "one new one\n").expect("rewrite the note");
    h.refresh_files();
    h.wait_until(|h| h.markdown() == "one new one");
    assert_eq!(h.markdown(), "one new one");
    assert_eq!(h.active_note().id, note_id);
    let new_editor = h.app.update(h.cx, |app, _| app.test_editor());
    assert_ne!(new_editor.entity_id(), old_editor.entity_id());
    assert!(!new_editor.update(h.cx, |editor, _| editor.has_find_preview()));
    // The old preview's "two" is cancelled, leaving the accepted "one".
    h.keys("n");
    assert_cursor(&mut h, 9);
    h.keys("x");
    assert_eq!(h.markdown(), "one new ne");
}

#[gpui::test]
fn vim_find_turning_vim_off_restores_the_caret_and_returns_focus(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("n.md", "one two one\n")], |p| p.vim_mode = true);
    h.keys("g g /");
    h.type_text("one");
    assert_cursor(&mut h, 9);
    h.set_preference(crate::storage::Pref::VimMode(false));
    assert_cursor(&mut h, 1);
    h.type_text("X");
    assert_eq!(h.markdown(), "Xone two one");
}

// Native candidate confirmation owns Return; cancelling the candidate takes
// one Escape, and cancelling the surrounding search takes another.
#[gpui::test]
fn vim_find_composition_keeps_return_and_escape_in_the_input(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("n.md", "one 你好 one\n")], |p| p.vim_mode = true);
    h.keys("g g /");
    h.type_text("one");
    h.keys("enter g g /");
    let field = h.app.update(h.cx, |app, _| app.test_find_editor());
    let note = h.app.update(h.cx, |app, _| app.test_editor());
    field.update_in(h.cx, |editor, window, cx| {
        editor.replace_and_mark_text_in_range(None, "你好", Some(2..2), window, cx);
    });
    h.cx.run_until_parked();
    assert!(field.update(h.cx, |editor, _| editor.is_composing()));
    h.keys("enter");
    assert!(field.update(h.cx, |editor, _| editor.is_composing()));
    assert!(note.update(h.cx, |editor, _| editor.has_find_preview()));
    h.keys("escape");
    assert!(!field.update(h.cx, |editor, _| editor.is_composing()));
    assert!(note.update(h.cx, |editor, _| editor.has_find_preview()));
    assert_eq!(
        field.update(h.cx, |editor, _| editor.text().to_owned()),
        "one"
    );
    h.keys("escape");
    assert!(!note.update(h.cx, |editor, _| editor.has_find_preview()));
    assert_cursor(&mut h, 1);
    h.keys("x");
    assert_eq!(h.markdown(), "ne 你好 one");
}
