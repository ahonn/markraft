//! Writes that fail partway: a full disk, a temporary file that cannot be made,
//! a rename that is refused. Whatever step fails, the file keeps what it said,
//! nothing half-written is left beside it, the edits stay on screen with the
//! reason shown, and the next save once the disk recovers writes them.

use super::harness::{Harness, open, open_with};
use crate::fs::faults::{Stage, inject};
use crate::storage::Pref;
use gpui::TestAppContext;
use std::io::ErrorKind;

const STAGES: [Stage; 3] = [Stage::Create, Stage::Write, Stage::Persist];

/// Every entry in the notes folder, hidden ones included: a temporary file left
/// behind by a failed write is hidden.
fn entries(h: &Harness) -> Vec<String> {
    let mut names: Vec<_> = std::fs::read_dir(&h.notes)
        .expect("the notes folder")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[gpui::test]
fn a_full_disk_keeps_the_file_and_the_edits_until_there_is_room(cx: &mut TestAppContext) {
    for stage in STAGES {
        let mut h = open_with(cx, &[("n.md", "a\n")], |_| {});
        let fault = inject(&h.notes, stage, ErrorKind::StorageFull);
        h.keys("cmd-down cmd-right");
        h.type_text("b");
        h.save();
        let error = h
            .error()
            .unwrap_or_else(|| panic!("{stage:?}: a full disk said nothing"));
        assert!(error.contains("no room left"), "{stage:?}: {error}");
        assert_eq!(h.markdown(), "ab", "{stage:?}");
        assert_eq!(
            std::fs::read_to_string(h.notes.join("n.md")).unwrap(),
            "a\n",
            "{stage:?}: the file changed"
        );
        assert_eq!(
            entries(&h),
            ["n.md"],
            "{stage:?}: a partial write was left behind"
        );

        // More typing while the disk is still full keeps the error and the edits.
        h.type_text("c");
        h.wait_for_io();
        assert!(
            h.error().is_some(),
            "{stage:?}: the error went away by itself"
        );

        drop(fault);
        h.save();
        assert_eq!(h.error(), None, "{stage:?}");
        assert_eq!(
            h.wait_for_file("n.md", |text| text == "abc\n"),
            "abc\n",
            "{stage:?}"
        );
        assert_eq!(entries(&h), ["n.md"], "{stage:?}");
    }
}

// Autosave meets the full disk with no ⌘S: the failure is still shown, and once
// there is room, the next edit's autosave writes everything typed meanwhile.
#[gpui::test]
fn autosave_on_a_full_disk_is_shown_and_catches_up(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("n.md", "a\n")], |_| {});
    let fault = inject(&h.notes, Stage::Write, ErrorKind::StorageFull);
    h.keys("cmd-down cmd-right");
    h.type_text("b");
    h.wait_until(|h| h.error().is_some());
    assert!(h.error().is_some(), "a failed autosave said nothing");
    assert_eq!(
        std::fs::read_to_string(h.notes.join("n.md")).unwrap(),
        "a\n"
    );

    drop(fault);
    h.type_text("c");
    assert_eq!(h.wait_for_file("n.md", |text| text == "abc\n"), "abc\n");
    h.wait_until(|h| h.error().is_none());
    assert_eq!(h.error(), None);
}

// A new note whose first write fails is not filed under a second name when the
// write is tried again: one file, under the name the note asked for.
#[gpui::test]
fn a_new_notes_failed_first_write_leaves_one_file_once_it_goes_through(cx: &mut TestAppContext) {
    for stage in STAGES {
        let mut h = open(cx, |_| {});
        let before = entries(&h);
        let fault = inject(&h.notes, stage, ErrorKind::StorageFull);
        h.type_text("Plans");
        h.save();
        assert!(h.error().is_some(), "{stage:?}");
        assert_eq!(entries(&h), before, "{stage:?}: something was written");

        drop(fault);
        h.save();
        assert_eq!(h.error(), None, "{stage:?}");
        let added: Vec<_> = entries(&h)
            .into_iter()
            .filter(|name| !before.contains(name))
            .collect();
        assert_eq!(added, ["Plans.md"], "{stage:?}");
        assert_eq!(
            std::fs::read_to_string(h.notes.join("Plans.md")).unwrap(),
            "Plans\n"
        );
    }
}

// The settings file failing to write does not touch the notes, and a later
// change is written whole once it can be.
#[gpui::test]
fn a_settings_file_that_cannot_be_written_is_shown_and_retried(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("n.md", "a\n")], |_| {});
    let root = h.root().to_owned();
    let fault = inject(&root, Stage::Persist, ErrorKind::StorageFull);
    h.set_preference(Pref::VimMode(true));
    h.wait_until(|h| h.error().is_some());
    assert!(
        h.error().is_some(),
        "a settings write that failed said nothing"
    );
    assert_eq!(
        std::fs::read_to_string(h.notes.join("n.md")).unwrap(),
        "a\n"
    );

    drop(fault);
    h.set_preference(Pref::AutoPair(false));
    let saved = h
        .saved_preferences(|saved| saved.vim_mode && !saved.auto_pair)
        .expect("a readable settings file");
    assert!(saved.vim_mode && !saved.auto_pair);
    h.wait_until(|h| h.error().is_none());
    assert_eq!(h.error(), None);
}
