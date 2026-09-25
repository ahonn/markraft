//! What reaches the disk: saves, the guard that holds edits to what a file can
//! say, undo after a save, and a file changed or removed by another program.

use super::harness::{open, open_with};
use gpui::TestAppContext;

/// Restore the folder even when a fault-injection assertion panics.
struct Locked(std::path::PathBuf);

impl Locked {
    fn set(&self, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(mode))
            .expect("the folder's permissions");
    }
}

impl Drop for Locked {
    fn drop(&mut self) {
        self.set(0o755);
    }
}

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

// A refused conflict copy is not permission to discard the only remaining
// local version. The same outside change can be retried after storage recovers.
#[gpui::test]
fn failed_recovery_keeps_local_edits_until_a_rewrite_can_be_reconciled(cx: &mut TestAppContext) {
    use crate::vault::External;
    let mut h = open_with(cx, &[("n.md", "hello\n")], |_| {});
    let before = h.active_note();
    h.keys("cmd-down cmd-right");
    h.type_text(" mine");
    std::fs::write(h.notes.join("n.md"), "theirs\n").unwrap();
    let change = || External::Updated {
        previous: Some(before.clone()),
        note: on_disk(&before, "theirs"),
    };
    let locked = Locked(h.notes.clone());
    locked.set(0o555);
    h.external(vec![change()]);
    assert_eq!(h.markdown(), "hello mine");
    assert_eq!(h.active_note().id, before.id);
    assert!(h.error().is_some(), "a refused recovery said nothing");
    assert_eq!(h.other_files("n.md"), []);

    locked.set(0o755);
    h.external(vec![change()]);
    assert_eq!(h.markdown(), "theirs");
    let copies = h.other_files("n.md");
    assert_eq!(copies.len(), 1, "{copies:?}");
    assert_eq!(copies[0].1, "hello mine\n");
    assert_eq!(
        std::fs::read_to_string(h.notes.join("n.md")).unwrap(),
        "theirs\n"
    );
}

#[gpui::test]
fn failed_recovery_keeps_a_deleted_notes_session_until_its_edits_are_safe(cx: &mut TestAppContext) {
    use crate::vault::External;
    let mut h = open_with(cx, &[("n.md", "hello\n")], |_| {});
    let before = h.active_note();
    h.keys("cmd-down cmd-right");
    h.type_text(" mine");
    std::fs::remove_file(h.notes.join("n.md")).unwrap();
    let locked = Locked(h.notes.clone());
    locked.set(0o555);
    h.external(vec![External::Removed(before.clone())]);
    assert_eq!(h.markdown(), "hello mine");
    assert!(
        h.app
            .update(h.cx, |app, _| app.test_note(&before.id))
            .is_some()
    );
    assert!(h.error().is_some(), "a refused recovery said nothing");
    assert_eq!(h.files(), Vec::<String>::new());

    locked.set(0o755);
    h.external(vec![External::Removed(before.clone())]);
    assert!(
        h.app
            .update(h.cx, |app, _| app.test_note(&before.id))
            .is_none()
    );
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

// Two explicit saves before either receipt arrives must share progress rather
// than continually invalidating one another's durability barrier.
#[gpui::test]
fn overlapping_manual_saves_finish_with_the_latest_edits(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("n.md", "hello\n")], |_| {});
    h.keys("cmd-down cmd-right");
    h.type_text(" changed");
    h.cx.update(|window, cx| {
        window.dispatch_action(Box::new(crate::app::Save), cx);
        window.dispatch_action(Box::new(crate::app::Save), cx);
    });
    h.wait_for_io();
    assert_eq!(h.error(), None);
    assert_eq!(
        std::fs::read_to_string(h.notes.join("n.md")).unwrap(),
        "hello changed\n"
    );
}

#[gpui::test]
fn edits_between_manual_saves_reach_the_final_file(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("n.md", "hello\n")], |_| {});
    h.keys("cmd-down cmd-right");
    h.type_text(" first");
    h.keys("cmd-s");
    h.type_text(" second");
    h.keys("cmd-s");
    h.wait_for_io();
    assert_eq!(h.error(), None);
    assert_eq!(h.markdown(), "hello first second");
    assert_eq!(
        std::fs::read_to_string(h.notes.join("n.md")).unwrap(),
        "hello first second\n"
    );
}

// A second deletion while the first save is in flight belongs to a newer
// workspace revision and must receive its own durable save before both finish.
#[gpui::test]
fn consecutive_deletions_are_both_saved_before_the_first_flush_returns(cx: &mut TestAppContext) {
    let mut h = open_with(
        cx,
        &[("alpha.md", "alpha\n"), ("beta.md", "beta\n")],
        |preferences| preferences.confirm_delete = false,
    );
    h.save();
    let first = h.active_note().id;
    h.keys("cmd-k");
    h.type_text("Move to Trash");
    h.keys("enter");
    assert!(h.app.update(h.cx, |app, _| app.test_note(&first)).is_none());
    assert!(h.app.update(h.cx, |app, _| app.test_io_pending()));

    let second = h.active_note().id;
    assert_ne!(first, second);
    h.keys("cmd-k");
    h.type_text("Move to Trash");
    h.keys("enter");
    assert!(
        h.app
            .update(h.cx, |app, _| app.test_note(&second))
            .is_none()
    );
    h.wait_for_io();

    assert_eq!(h.error(), None);
    assert_eq!(h.files(), Vec::<String>::new());
    assert_eq!(h.markdown(), "");
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
    let mut h = open_with(cx, &[("n.md", "a\n")], |_| {});
    let locked = Locked(h.notes.clone());
    locked.set(0o555);
    h.keys("cmd-down cmd-right");
    h.type_text("b");
    h.save();
    assert!(
        h.error().is_some(),
        "a refused save said nothing: markdown={:?}, read_only={:?}, disk={:?}",
        h.markdown(),
        h.active_note().read_only,
        std::fs::read_to_string(h.notes.join("n.md"))
    );
    assert_eq!(h.markdown(), "ab");
    let on_disk = std::fs::read_to_string(h.notes.join("n.md")).expect("the note's file");
    assert_eq!(on_disk, "a\n");

    locked.set(0o755);
    h.save();
    assert_eq!(h.error(), None);
    assert_eq!(h.wait_for_file("n.md", |text| text == "ab\n"), "ab\n");
}

#[gpui::test]
fn newer_edits_do_not_hide_an_earlier_save_failure(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("n.md", "a\n")], |_| {});
    let locked = Locked(h.notes.clone());
    locked.set(0o555);
    h.keys("cmd-down cmd-right");
    h.type_text("b");
    h.keys("cmd-s");
    h.type_text("c");
    h.wait_for_io();
    assert!(
        h.error().is_some(),
        "new edits hid the pending write's failure"
    );
    assert_eq!(h.markdown(), "abc");
    assert_eq!(
        std::fs::read_to_string(h.notes.join("n.md")).unwrap(),
        "a\n"
    );

    locked.set(0o755);
    h.save();
    assert_eq!(h.error(), None);
    assert_eq!(
        std::fs::read_to_string(h.notes.join("n.md")).unwrap(),
        "abc\n"
    );
}

// Another program writing a note's file reaches the note through a refresh,
// the same reconciliation path used by the watcher: taken as it is when the
// note has nothing unsaved, and with the edits kept beside it when it has.
#[gpui::test]
fn a_file_written_by_another_program_reaches_its_note(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("n.md", "hello\n")], |_| {});
    // Let the window's first activation refresh finish before this write.
    h.pass_time(std::time::Duration::from_millis(100));
    h.save();
    std::fs::write(h.notes.join("n.md"), "theirs\n").expect("another program's write");
    h.refresh_files();
    h.wait_until(|h| h.markdown() == "theirs");
    assert_eq!(h.markdown(), "theirs");
    assert_eq!(h.other_files("n.md"), []);

    // The autosave may reach the file before the refresh's report reaches the
    // note; either way the file wins and the edits are kept as a copy.
    let mut h = open_with(cx, &[("n.md", "hello\n")], |_| {});
    h.keys("cmd-down cmd-right");
    h.type_text(" mine");
    std::fs::write(h.notes.join("n.md"), "theirs\n").expect("another program's write");
    h.refresh_files();
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
