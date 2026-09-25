//! What reaches the disk: saves, the guard that holds edits to what a file can
//! say, undo after a save, and a file changed or removed by another program.

use super::harness::{open, open_with};
use gpui::TestAppContext;

/// `note` as the file reads when it says `markdown`.
fn on_disk(note: &crate::storage::Note, markdown: &str) -> crate::storage::Note {
    crate::storage::Note {
        document: crate::doc::from_markdown(markdown),
        ..note.clone()
    }
}

// Another app rewrote the file while the note had edits of its own: the file
// wins, and the edits are kept beside it as a conflicted copy, said once.
#[gpui::test]
fn edits_meeting_a_rewritten_file_are_kept_as_a_conflicted_copy(cx: &mut TestAppContext) {
    use crate::vault::External;
    let mut h = open_with(cx, &[("n.md", "hello\n")], |_| {});
    let before = h.active_note();
    h.keys("cmd-down");
    h.type_text(" mine");
    h.external(vec![External::Updated {
        previous: Some(before.clone()),
        note: on_disk(&before, "theirs"),
    }]);
    assert_eq!(h.markdown(), "theirs");
    let copies = h.other_files("n.md");
    assert_eq!(copies.len(), 1, "{copies:?}");
    assert_eq!(copies[0].1, "hello mine\n");
    assert_eq!(h.notices(), [crate::storage::CONFLICT_KEPT]);
}

// The file was rewritten while the note held what the file used to say: the
// new text is taken, with nothing to keep and nothing to say.
#[gpui::test]
fn an_untouched_note_takes_a_rewritten_file(cx: &mut TestAppContext) {
    use crate::vault::External;
    let mut h = open_with(cx, &[("n.md", "hello\n")], |_| {});
    let before = h.active_note();
    h.external(vec![External::Updated {
        previous: Some(before.clone()),
        note: on_disk(&before, "theirs"),
    }]);
    assert_eq!(h.markdown(), "theirs");
    assert_eq!(h.other_files("n.md"), []);
    assert_eq!(h.notices(), Vec::<String>::new());
}

// Only the file's permissions changed: the edits on screen stay, not a copy,
// and the note takes the new read-only state.
#[gpui::test]
fn a_permission_change_keeps_the_edits_on_screen(cx: &mut TestAppContext) {
    use crate::vault::External;
    let mut h = open_with(cx, &[("n.md", "hello\n")], |_| {});
    let before = h.active_note();
    h.keys("cmd-down");
    h.type_text(" mine");
    let locked = crate::storage::Note {
        read_only: Some("locked".into()),
        ..before.clone()
    };
    h.external(vec![External::Updated {
        previous: Some(before),
        note: locked,
    }]);
    assert_eq!(h.markdown(), "hello mine");
    assert_eq!(h.active_note().read_only.as_deref(), Some("locked"));
    assert_eq!(h.other_files("n.md"), []);
    assert_eq!(h.notices(), Vec::<String>::new());
}

// The file was deleted: a note with nothing unsaved goes with it; one with
// edits keeps them as a copy where the file was. Either way it is said.
#[gpui::test]
fn a_deleted_file_takes_its_note_and_keeps_its_edits(cx: &mut TestAppContext) {
    use crate::vault::External;
    let mut h = open_with(cx, &[("n.md", "hello\n")], |_| {});
    let note = h.active_note();
    h.external(vec![External::Removed(note.clone())]);
    let gone = h.app.update(h.cx, |app, _| app.test_note(&note.id));
    assert!(gone.is_none(), "the note left with its file");
    assert_eq!(h.other_files("n.md"), []);
    assert_eq!(h.notices(), ["A note's file was deleted outside Markraft."]);

    let mut h = open_with(cx, &[("n.md", "hello\n")], |_| {});
    let note = h.active_note();
    h.keys("cmd-down");
    h.type_text(" mine");
    h.external(vec![External::Removed(note.clone())]);
    let gone = h.app.update(h.cx, |app, _| app.test_note(&note.id));
    assert!(gone.is_none(), "the note left with its file");
    let copies = h.other_files("n.md");
    assert_eq!(copies.len(), 1, "{copies:?}");
    assert_eq!(copies[0].1, "hello mine\n");
}

#[gpui::test]
fn a_note_typed_and_saved_reaches_its_file(cx: &mut TestAppContext) {
    let mut h = open(cx, |_| {});
    h.type_text("hello");
    h.save();
    assert_eq!(h.markdown(), "hello");
    let text = h.wait_for_file("hello.md", |text| text == "hello\n");
    assert_eq!(text, "hello\n", "files: {:?}", h.files());
}

// A new note has no file for the guard to hold edits to until its first
// save, and the first save writes the document whole. Whatever the editor takes
// must still come back from that file.
#[gpui::test]
fn a_new_note_saves_what_it_holds_on_its_first_save(cx: &mut TestAppContext) {
    let mut h = open(cx, |_| {});
    h.type_text("a");
    h.keys("shift-enter");
    h.assert_round_trip("a line break at the end of a new note");
    h.type_text("b");
    h.assert_round_trip("text after it");
}

// A line break at the end of a line in a note already on disk.
#[gpui::test]
fn a_line_break_at_the_end_of_a_saved_line(cx: &mut TestAppContext) {
    for spaces in [false, true] {
        let mut h = open_with(cx, &[("a.md", "a\n")], |p| {
            if spaces {
                p.hard_break = crate::storage::HardBreakStyle::Spaces;
            }
        });
        h.keys("cmd-down cmd-right shift-enter");
        h.assert_round_trip("shift-return at the end");
        h.type_text("b");
        h.assert_round_trip("typing after the break");
    }
}

// Bold asked for on an empty line of a saved note, then typed into.
#[gpui::test]
fn bold_on_an_empty_line_of_a_saved_note(cx: &mut TestAppContext) {
    for underscore in [false, true] {
        let mut h = open_with(cx, &[("x.md", "x\n")], |p| {
            if underscore {
                p.emphasis_marker = crate::storage::EmphasisMarker::Underscore;
            }
        });
        h.keys("cmd-down cmd-right enter cmd-b");
        h.assert_round_trip("an empty bold pair");
        h.type_text("y");
        h.assert_round_trip("typed into it");
        h.keys("cmd-b");
        h.type_text(" z");
        h.assert_round_trip("typed after leaving it");
    }
}

// After several saves the guard and the save still agree on what can be
// written back.
#[gpui::test]
fn the_guard_and_the_save_agree_after_several_saves(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("n.md", "a\n\n- b\n")], |_| {});
    for text in ["1", "2", "3"] {
        h.keys("cmd-down cmd-right");
        h.type_text(text);
        h.assert_round_trip("typing");
    }
    h.keys("cmd-up cmd-2");
    h.assert_round_trip("a heading after several saves");
    h.keys("cmd-down enter");
    h.type_text("c");
    h.assert_round_trip("a new item after several saves");
}

// Undoing an edit already saved writes the file back byte for byte.
#[gpui::test]
fn undo_after_a_save_restores_the_file(cx: &mut TestAppContext) {
    let original = "Title\n\n* one\n*  two\n\n|a|b|\n|-|-|\n";
    let mut h = open_with(cx, &[("u.md", original)], |_| {});
    h.keys("cmd-up cmd-1");
    h.assert_round_trip("a heading");
    h.keys("cmd-z");
    h.assert_round_trip("undone");
    let text = h.wait_for_file("u.md", |text| text == original);
    assert_eq!(text, original);
}

// An edit reaches its file with nothing asked of the person: the autosave a
// moment after the last keystroke, which is how most edits are saved. A new
// note is filed under its title the same way.
#[gpui::test]
fn an_edit_reaches_its_file_without_a_save(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("n.md", "a\n")], |_| {});
    h.keys("cmd-down cmd-right");
    h.type_text("b");
    assert_eq!(h.wait_for_file("n.md", |text| text == "ab\n"), "ab\n");
    assert_eq!(h.error(), None);

    let mut h = open(cx, |_| {});
    h.type_text("hello");
    let text = h.wait_for_file("hello.md", |text| text == "hello\n");
    assert_eq!(text, "hello\n", "files: {:?}", h.files());
}

// A save the folder refuses is said, leaves the file as it was, and is gone
// once a save gets through; the edits wait on screen in between.
#[gpui::test]
fn a_refused_save_is_shown_until_a_save_gets_through(cx: &mut TestAppContext) {
    use std::os::unix::fs::PermissionsExt;
    /// The folder made read-only, and writable again however the test ends, so
    /// the temporary directory can still be removed.
    struct Locked(std::path::PathBuf);
    impl Locked {
        fn set(&self, mode: u32) {
            std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(mode))
                .expect("the folder's permissions");
        }
    }
    impl Drop for Locked {
        fn drop(&mut self) {
            self.set(0o755);
        }
    }
    let mut h = open_with(cx, &[("n.md", "a\n")], |_| {});
    let locked = Locked(h.notes.clone());
    locked.set(0o555);
    h.keys("cmd-down cmd-right");
    h.type_text("b");
    h.save();
    assert!(h.error().is_some(), "a refused save said nothing");
    assert_eq!(h.markdown(), "ab");
    let on_disk = std::fs::read_to_string(h.notes.join("n.md")).expect("the note's file");
    assert_eq!(on_disk, "a\n");

    locked.set(0o755);
    h.save();
    assert_eq!(h.error(), None);
    assert_eq!(h.wait_for_file("n.md", |text| text == "ab\n"), "ab\n");
}

// Another program writing a note's file reaches the note through the watcher,
// the path the reconciliations above stand in for: taken as it is when the
// note has nothing unsaved, and with the edits kept beside it when it has.
#[gpui::test]
fn a_file_written_by_another_program_reaches_its_note(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("n.md", "hello\n")], |_| {});
    // The window's first activation checks the folder again, which would find
    // the write without the watcher. Let it happen first; a save after it is
    // answered only once the vault's thread has done that check.
    h.pass_time(std::time::Duration::from_millis(100));
    h.save();
    std::fs::write(h.notes.join("n.md"), "theirs\n").expect("another program's write");
    h.wait_until(|h| h.markdown() == "theirs");
    assert_eq!(h.markdown(), "theirs");
    assert_eq!(h.other_files("n.md"), []);

    // The autosave may reach the file before the watcher's report reaches the
    // note; either way the file wins and the edits are kept as a copy.
    let mut h = open_with(cx, &[("n.md", "hello\n")], |_| {});
    h.keys("cmd-down cmd-right");
    h.type_text(" mine");
    std::fs::write(h.notes.join("n.md"), "theirs\n").expect("another program's write");
    h.wait_until(|h| h.markdown() == "theirs" && !h.other_files("n.md").is_empty());
    assert_eq!(h.markdown(), "theirs");
    let copies = h.other_files("n.md");
    assert_eq!(copies.len(), 1, "{copies:?}");
    assert_eq!(copies[0].1, "hello mine\n");
    assert_eq!(
        std::fs::read_to_string(h.notes.join("n.md")).unwrap(),
        "theirs\n"
    );
}
