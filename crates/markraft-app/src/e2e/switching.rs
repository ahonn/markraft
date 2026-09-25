//! Moving between notes: each keeps its own edits and its own undo.

use super::harness::{Harness, open_with};
use gpui::TestAppContext;

/// Open the note titled `title` from Browse, as a person does.
fn browse_to(h: &mut Harness, title: &str) {
    h.keys("cmd-p");
    h.type_text(title);
    h.keys("enter");
    h.wait_for_io();
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

// Opening an external Markdown file leaves the edited note safe and changes
// the save destination to that file, without importing a duplicate into the vault.
#[gpui::test]
fn opening_an_external_file_preserves_edits_and_saves_to_the_opened_path(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("alpha.md", "alpha\n")], |_| {});
    h.keys("cmd-down cmd-right");
    h.type_text(" local");
    let outside = tempfile::tempdir().unwrap();
    let path = outside.path().join("external.md");
    std::fs::write(&path, "outside\n").unwrap();
    h.open_path(&path);
    assert_eq!(h.markdown(), "outside");
    assert_eq!(
        h.wait_for_file("alpha.md", |s| s == "alpha local\n"),
        "alpha local\n"
    );
    h.keys("cmd-down cmd-right");
    h.type_text(" changed");
    h.save();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "outside changed\n");
    assert_eq!(h.files(), ["alpha.md"]);

    browse_to(&mut h, "alpha");
    assert_eq!(h.markdown(), "alpha local");
    h.keys("cmd-z");
    assert_eq!(h.markdown(), "alpha");
}

#[gpui::test]
fn rename_after_a_name_conflict_updates_open_backlinks_and_keeps_them_editable(
    cx: &mut TestAppContext,
) {
    let mut h = open_with(
        cx,
        &[
            ("Welcome.md", "Welcome\n"),
            ("Backlinks.md", "Backlinks\n\n[[Welcome]]\n"),
        ],
        |_| {},
    );
    // Keep the backlink's editor alive before the rename, including its source guard.
    browse_to(&mut h, "Backlinks");
    browse_to(&mut h, "Welcome.md");
    h.keys("cmd-k");
    h.type_text("Rename");
    h.keys("enter");
    h.type_text("Backlinks");
    h.keys("enter");
    h.wait_for_io();
    assert_eq!(active_file(&mut h), "Welcome.md");

    h.keys("cmd-k");
    h.type_text("Rename");
    h.keys("enter");
    h.type_text("Renamed");
    h.keys("enter cmd-s cmd-p");
    h.type_text("Backlinks");
    h.keys("enter");
    h.wait_for_io();
    assert_eq!(active_file(&mut h), "Backlinks.md");
    assert_eq!(h.markdown(), "Backlinks\n\n[[Renamed]]");
    h.keys("cmd-down cmd-right");
    h.type_text(" retained");
    h.save();
    assert_eq!(h.markdown(), "Backlinks\n\n[[Renamed]] retained");
    assert_eq!(
        std::fs::read_to_string(h.notes.join("Backlinks.md")).unwrap(),
        "Backlinks\n\n[[Renamed]] retained\n"
    );
    assert_eq!(h.files(), ["Backlinks.md", "Renamed.md"]);
}
