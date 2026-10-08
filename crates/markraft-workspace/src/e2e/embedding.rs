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
    let mut options = WorkspaceOptions::new(
        other_root.path().join("notes"),
        other_root.path().join("state"),
    );
    options.preferences.bullet_marker = crate::storage::BulletMarker::Star;
    let other =
        h.cx.update(|window, cx| cx.new(|cx| WorkspaceView::open(options, window, cx).unwrap()));
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
    let options = WorkspaceOptions::new(
        other_root.path().join("notes"),
        other_root.path().join("state"),
    );
    h.cx.update(|window, cx| {
        let host_focus = cx.focus_handle();
        window.focus(&host_focus, cx);
        let _workspace = cx.new(|cx| WorkspaceView::open(options, window, cx).unwrap());
        assert!(host_focus.is_focused(window));
    });
}
