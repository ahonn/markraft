//! Image menu completion uses the selection that opened the native file panel.

use super::{ContextTarget, MarkdownAction};
use crate::e2e::harness::{Harness, open_with};
use gpui::{Point, TestAppContext};
use markraft_core::{Node, history};
use std::path::PathBuf;

fn choose_image(h: &mut Harness<'_>) {
    let app = h.app.clone();
    h.cx.update(|window, cx| {
        app.update(cx, |app, cx| {
            let request = app
                .editor()
                .read(cx)
                .context_snapshot(Point::default(), ContextTarget::Text);
            assert!(app.markdown_action_enabled(MarkdownAction::InsertImage, cx));
            app.run_markdown_action(MarkdownAction::InsertImage, &request, window, cx);
        });
    });
    h.cx.run_until_parked();
    assert!(h.cx.did_prompt_for_paths());
}

fn image_file(h: &Harness<'_>) -> PathBuf {
    // Outside the notes folder, so success must copy the image before inserting.
    let path = h.root().join("菜单图片.png");
    std::fs::write(
        &path,
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icon/markraft-menubar.png"
        )),
    )
    .unwrap();
    path
}

fn document(h: &mut Harness<'_>) -> Node {
    h.app.update(h.cx, |app, cx| app.active_document(cx))
}

fn undo_depth(h: &mut Harness<'_>) -> usize {
    h.app.update(h.cx, |app, cx| {
        history::undo_depth(app.editor().read(cx).state())
    })
}

#[gpui::test]
fn image_menu_import_replaces_original_selection_and_undo_restores_it(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("note.md", "hello world\n")], |_| {});
    h.select(1, 6);
    let original = document(&mut h);
    let selection = h.selection();
    let image = image_file(&h);
    choose_image(&mut h);
    h.cx.simulate_path_prompt_response(|options| {
        assert!(options.files);
        assert!(!options.directories);
        assert!(options.multiple);
        Some(vec![image])
    });
    h.wait_until(|h| document(h) != original);
    let markdown = h.markdown();
    assert!(markdown.contains("![image](assets/"), "{markdown}");
    assert!(markdown.ends_with(" world"), "{markdown}");
    assert!(!markdown.contains("hello"), "{markdown}");
    let copied: Vec<_> = std::fs::read_dir(h.notes.join("assets"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(copied.len(), 1);
    assert_eq!(
        std::fs::read(&copied[0]).unwrap(),
        std::fs::read(h.root().join("菜单图片.png")).unwrap()
    );
    assert_eq!(undo_depth(&mut h), 1);
    assert!(h.edit(|editor, cx| {
        let spec = history::undo(editor.state()).unwrap();
        editor.dispatch([spec], cx)
    }));
    assert_eq!(document(&mut h), original);
    assert_eq!(h.selection(), selection);
    assert_eq!(undo_depth(&mut h), 0);
}

#[gpui::test]
fn image_menu_cancel_keeps_document_selection_and_history(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("note.md", "hello world\n")], |_| {});
    h.select(1, 6);
    let original = document(&mut h);
    let selection = h.selection();
    choose_image(&mut h);
    h.cx.simulate_path_prompt_response(|_| None);
    h.cx.run_until_parked();
    assert!(!h.cx.did_prompt_for_paths());
    assert_eq!(document(&mut h), original);
    assert_eq!(h.selection(), selection);
    assert_eq!(undo_depth(&mut h), 0);
    assert!(!h.notes.join("assets").exists());
}

#[gpui::test]
fn image_menu_changed_selection_during_dialog_refuses_import(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("note.md", "hello world\n")], |_| {});
    h.select(1, 6);
    let original = document(&mut h);
    let image = image_file(&h);
    choose_image(&mut h);
    h.select(7, 12);
    let moved = h.selection();
    h.cx.simulate_path_prompt_response(|_| Some(vec![image]));
    h.cx.run_until_parked();
    assert!(!h.cx.did_prompt_for_paths());
    assert_eq!(document(&mut h), original);
    assert_eq!(h.selection(), moved);
    assert_eq!(undo_depth(&mut h), 0);
    assert!(!h.notes.join("assets").exists());
}
