//! Finding in the note: the bar, the visible word, and stepping through hits.

use super::harness::open_with;
use gpui::TestAppContext;

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
