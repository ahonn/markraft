//! Moving between notes: each keeps its own edits and its own undo.

use super::harness::{Harness, open_with};
use gpui::TestAppContext;

/// Open the note titled `title` from Browse, as a person does.
fn browse_to(h: &mut Harness, title: &str) {
    h.keys("cmd-p");
    h.type_text(title);
    h.keys("enter");
}

/// The file name of the note the window shows.
fn active_file(h: &mut Harness) -> String {
    let path = h.active_note().path.expect("a note with a file");
    path.file_name().unwrap().to_string_lossy().into_owned()
}

// The note left is written, the one moved to shows what its file says, and
// ⌘Z in either reaches only that note's own edits.
#[gpui::test]
fn switching_notes_keeps_each_notes_edits_and_undo(cx: &mut TestAppContext) {
    let mut h = open_with(
        cx,
        &[("alpha.md", "alpha\n"), ("beta.md", "beta\n")],
        |_| {},
    );
    browse_to(&mut h, "alpha");
    assert_eq!(active_file(&mut h), "alpha.md");
    h.keys("cmd-down cmd-right");
    h.type_text("1");

    browse_to(&mut h, "beta");
    assert_eq!(active_file(&mut h), "beta.md");
    assert_eq!(h.markdown(), "beta");
    assert_eq!(
        h.wait_for_file("alpha.md", |text| text == "alpha1\n"),
        "alpha1\n"
    );
    h.keys("cmd-z");
    assert_eq!(h.markdown(), "beta", "undo reached into the note left");
    h.keys("cmd-down cmd-right");
    h.type_text("2");

    browse_to(&mut h, "alpha");
    assert_eq!(h.markdown(), "alpha1");
    h.keys("cmd-z");
    assert_eq!(h.markdown(), "alpha");
    h.save();
    assert_eq!(
        h.wait_for_file("alpha.md", |text| text == "alpha\n"),
        "alpha\n"
    );
    assert_eq!(
        h.wait_for_file("beta.md", |text| text == "beta2\n"),
        "beta2\n"
    );
}
