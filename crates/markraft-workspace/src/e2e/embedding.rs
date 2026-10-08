//! Public integration contracts exercised without standalone process services.
use super::harness::open;
use crate::{WorkspaceOptions, WorkspaceView};
use gpui::{AppContext, TestAppContext};
use std::{cell::RefCell, rc::Rc};

#[gpui::test]
fn explicit_close_saves_and_releases_the_notes_directory(cx: &mut TestAppContext) {
    let mut h = open(cx, |_| {});
    h.type_text("A note saved by the host close barrier");
    h.wait_for_io();
    let result = Rc::new(RefCell::new(None));
    let completed = result.clone();
    h.cx.update(|window, cx| {
        h.app.update(cx, |view, cx| {
            view.prepare_close(window, cx, move |saved, _, _| {
                *completed.borrow_mut() = Some(saved)
            });
        })
    });
    h.wait_until(|_| result.borrow().is_some());
    assert!(result.borrow_mut().take().expect("close completes").is_ok());
    let state = tempfile::tempdir().unwrap();
    let reopened = crate::vault::Store::open(
        h.notes.clone(),
        state.path().join("workspace.json"),
        Default::default(),
    );
    assert!(
        reopened.is_ok(),
        "close acknowledgement releases the directory lock"
    );
    let (_, library) = reopened.unwrap();
    assert!(
        library
            .notes
            .iter()
            .any(|note| crate::doc::to_markdown(&note.document).contains("host close barrier"))
    );
}

#[gpui::test]
fn mounting_another_workspace_does_not_change_existing_format_preferences(cx: &mut TestAppContext) {
    let mut h = open(cx, |preferences| {
        preferences.bullet_marker = crate::storage::BulletMarker::Plus
    });
    let other_root = tempfile::tempdir().unwrap();
    let folders = markraft_notes::NotesConfig::new(
        other_root.path().join("notes"),
        other_root.path().join("state"),
    );
    let mut options = WorkspaceOptions::default();
    options.preferences.bullet_marker = crate::storage::BulletMarker::Star;
    let other = h.cx.update(|window, cx| {
        cx.new(|cx| WorkspaceView::open(folders, options, window, cx).unwrap())
    });
    h.focus_note();
    h.type_text("First workspace");
    h.keys("cmd-*");
    assert_eq!(h.markdown(), "+ First workspace");
    assert_eq!(
        other.read_with(h.cx, |view, _| view.preferences().bullet_marker),
        crate::storage::BulletMarker::Star
    );
}

#[gpui::test]
fn close_rejects_pending_open_and_remains_retryable(cx: &mut TestAppContext) {
    let mut h = open(cx, |_| {});
    h.wait_for_io();
    let path = h.root().join("external.md");
    std::fs::write(&path, "An accepted file open").unwrap();
    let result = Rc::new(RefCell::new(None));
    let completed = result.clone();
    h.cx.update(|window, cx| {
        h.app.update(cx, |view, cx| {
            view.open_files(vec![path], window, cx);
            view.prepare_close(window, cx, move |saved, _, _| {
                *completed.borrow_mut() = Some(saved)
            });
        })
    });
    assert_eq!(
        result.borrow_mut().take(),
        Some(Err(crate::host::WorkspaceError::Busy))
    );
    h.wait_for_io();
    assert!(h.markdown().contains("An accepted file open"));
    let completed = result.clone();
    h.cx.update(|window, cx| {
        h.app.update(cx, |view, cx| {
            view.prepare_close(window, cx, move |saved, _, _| {
                *completed.borrow_mut() = Some(saved)
            });
        })
    });
    h.wait_until(|_| result.borrow().is_some());
    assert!(result.borrow_mut().take().expect("retry closes").is_ok());
}

#[gpui::test]
fn embedded_rendering_preserves_the_host_window_title_and_size(cx: &mut TestAppContext) {
    let mut h = open(cx, |preferences| preferences.auto_height = true);
    let bounds = h.cx.update(|window, _| {
        window.set_window_title("Host application");
        window.bounds()
    });
    h.type_text("A workspace note must not rename the host");
    h.wait_for_io();
    assert_eq!(h.cx.window_title().as_deref(), Some("Host application"));
    h.cx.update(|window, _| assert_eq!(window.bounds(), bounds));
}

#[gpui::test]
fn constructing_an_embedded_workspace_preserves_host_focus(cx: &mut TestAppContext) {
    let h = open(cx, |_| {});
    let other_root = tempfile::tempdir().unwrap();
    let folders = markraft_notes::NotesConfig::new(
        other_root.path().join("notes"),
        other_root.path().join("state"),
    );
    let options = WorkspaceOptions::default();
    h.cx.update(|window, cx| {
        let host_focus = cx.focus_handle();
        window.focus(&host_focus, cx);
        let _workspace = cx.new(|cx| WorkspaceView::open(folders, options, window, cx).unwrap());
        assert!(host_focus.is_focused(window));
    });
}

use markraft_notes::memory::MemoryBackend;

#[gpui::test]
fn database_workspace_saves_without_creating_markdown_files(cx: &mut TestAppContext) {
    let backend = MemoryBackend::default();
    let mut h = super::harness::open_backend(cx, Box::new(backend.clone()));
    h.type_text("Saved through a host backend");
    h.save();
    assert!(
        backend
            .notes()
            .iter()
            .any(|note| note.markdown.contains("host backend"))
    );
    assert!(!h.notes.exists());
    let result = Rc::new(RefCell::new(None));
    let completed = result.clone();
    h.cx.update(|window, cx| {
        h.app.update(cx, |view, cx| {
            view.prepare_close(window, cx, move |saved, _, _| {
                *completed.borrow_mut() = Some(saved)
            });
        })
    });
    h.wait_until(|_| result.borrow().is_some());
    result.borrow_mut().take().unwrap().unwrap();
    assert!(h.app.read_with(h.cx, |view, _| view.is_closed()));
}

#[gpui::test]
fn database_workspace_preserves_unedited_source_on_close(cx: &mut TestAppContext) {
    use markraft_notes::*;
    let source = "---\ncustom: preserved\n---\n\nTitle\n=====\n\nA  paragraph.\n";
    let backend = MemoryBackend::default();
    backend.state().records.insert(
        "original".into(),
        BackendNote {
            id: NoteId::new("original"),
            markdown: source.into(),
            title: None,
            logical_key: None,
            revision: StorageRevision("original-revision".into()),
            created_at: 1,
            updated_at: 1,
            pinned: false,
        },
    );
    let mut h = super::harness::open_backend(cx, Box::new(backend.clone()));
    h.save();
    assert_eq!(backend.notes()[0].markdown, source);
    assert!(h.active_note().path.is_none());
    h.keys("cmd-down");
    h.type_text(" Edited.");
    h.save();
    let saved = backend.notes()[0].markdown.clone();
    assert!(saved.starts_with("---\ncustom: preserved\n---\n\nTitle\n=====\n"));
    assert!(saved.contains("Edited."));
}

#[gpui::test]
fn database_workspace_rename_changes_metadata_without_rewriting_markdown(cx: &mut TestAppContext) {
    let backend = MemoryBackend::default();
    let mut h = super::harness::open_backend(cx, Box::new(backend.clone()));
    h.type_text("Original body");
    h.save();
    let original = backend.notes()[0].markdown.clone();
    h.keys("cmd-k");
    h.type_text("Rename");
    h.keys("enter");
    h.keys("cmd-a");
    h.type_text("Renamed record");
    h.keys("enter");
    h.wait_for_io();
    let notes = backend.notes();
    assert_eq!(notes[0].title.as_deref(), Some("Renamed record"));
    assert_eq!(notes[0].markdown, original);
}

#[gpui::test]
fn database_daily_notes_use_a_stable_logical_key(cx: &mut TestAppContext) {
    let backend = MemoryBackend::default();
    let mut h = super::harness::open_backend(cx, Box::new(backend.clone()));
    for _ in 0..2 {
        h.keys("cmd-k");
        h.type_text("today");
        h.keys("enter");
        h.wait_for_io();
    }
    let expected = format!("daily:{}", chrono::Local::now().date_naive());
    let notes = backend.notes();
    assert_eq!(
        notes
            .iter()
            .filter(|note| note.logical_key.as_deref() == Some(&expected))
            .count(),
        1
    );
    let daily = notes
        .iter()
        .find(|note| note.logical_key.as_deref() == Some(&expected))
        .unwrap();
    assert_eq!(daily.id, markraft_notes::NoteId::for_logical_key(&expected));
}
