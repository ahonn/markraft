//! Images, links and code blocks: what a note holds besides its text, put in
//! and changed through the app's own controls.

use super::harness::{Harness, open, open_with};
use gpui::TestAppContext;

/// A one-pixel PNG.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0,
    0x1f, 0x00, 0x05, 0x00, 0x01, 0xff, 0x89, 0x99, 0x3d, 0x1d, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

fn drop_paths(h: &mut Harness, paths: Vec<std::path::PathBuf>) {
    let app = h.app.clone();
    h.cx.update(|window, cx| app.update(cx, |app, cx| app.test_drop_paths(paths, window, cx)));
    h.cx.run_until_parked();
    h.wait_for_io();
}

fn paste_image(h: &mut Harness) {
    let image = gpui::Image::from_bytes(gpui::ImageFormat::Png, PNG.to_vec());
    h.cx.write_to_clipboard(gpui::ClipboardItem::new_image(&image));
    h.keys("cmd-v");
    settle(h);
}

/// Let an insertion finish: it waits for a save, then copies on a background
/// thread, then edits the note.
fn settle(h: &mut Harness) {
    h.wait_for_io();
    h.pass_time(std::time::Duration::from_secs(1));
    h.wait_for_io();
}

/// Every file under the notes folder, by its path there.
fn files_under(root: &std::path::Path) -> Vec<String> {
    fn walk(folder: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(folder).expect("a folder").flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else {
                out.push(path);
            }
        }
    }
    let mut paths = Vec::new();
    walk(root, &mut paths);
    let mut names: Vec<_> = paths
        .iter()
        .map(|path| {
            path.strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

/// The target of the one image the note shows.
fn image_target(markdown: &str) -> String {
    let start = markdown.find("](").expect("an image") + 2;
    let end = start + markdown[start..].find(')').expect("a closed image");
    markdown[start..end].to_owned()
}

// A pasted image is copied beside the note and shown where the caret was; the
// file it points at holds the pasted bytes, and undo takes the image out of
// the note but never deletes the copy.
#[gpui::test]
fn a_pasted_image_is_copied_beside_the_note(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("n.md", "a\n")], |_| {});
    h.keys("cmd-down cmd-right enter");
    paste_image(&mut h);
    assert_eq!(h.error(), None);
    let markdown = h.markdown();
    assert!(
        markdown.starts_with("a\n\n![image](assets/"),
        "{markdown:?}"
    );
    let target = image_target(&markdown);
    assert_eq!(std::fs::read(h.notes.join(&target)).unwrap(), PNG);
    assert_eq!(files_under(&h.notes), [target.clone(), "n.md".to_owned()]);
    h.assert_round_trip("a pasted image");

    h.keys("cmd-z");
    assert_eq!(h.markdown(), "a");
    assert!(
        h.notes.join(&target).exists(),
        "undo deleted the copied image"
    );
}

// An image dropped from outside the notes folder is copied in; one already in
// the folder is pointed at where it is, not copied again.
#[gpui::test]
fn a_dropped_image_is_copied_in_only_from_outside(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("n.md", "a\n")], |_| {});
    let outside = h.root().join("我的 图.png");
    std::fs::write(&outside, PNG).unwrap();
    h.keys("cmd-down cmd-right enter");
    drop_paths(&mut h, vec![outside.clone()]);
    settle(&mut h);
    let target = image_target(&h.markdown());
    assert!(target.starts_with("assets/"), "{target:?}");
    assert_eq!(std::fs::read(h.notes.join(&target)).unwrap(), PNG);
    assert!(outside.exists(), "the dropped original was moved");
    h.assert_round_trip("a dropped image");

    let mut h = open_with(cx, &[("n.md", "a\n")], |_| {});
    std::fs::create_dir_all(h.notes.join("pics")).unwrap();
    std::fs::write(h.notes.join("pics/x.png"), PNG).unwrap();
    h.keys("cmd-down cmd-right enter");
    let inside = h.notes.join("pics/x.png");
    drop_paths(&mut h, vec![inside]);
    settle(&mut h);
    assert_eq!(h.markdown(), "a\n\n![image](pics/x.png)");
    assert_eq!(files_under(&h.notes), ["n.md", "pics/x.png"]);
}

// A note with no file yet says why an image cannot go in, and writes nothing.
#[gpui::test]
fn an_image_into_a_note_with_no_file_is_refused_with_a_reason(cx: &mut TestAppContext) {
    let mut h = open(cx, |_| {});
    h.keys("cmd-n");
    paste_image(&mut h);
    assert_eq!(h.markdown(), "");
    assert_eq!(files_under(&h.notes), Vec::<String>::new());
    assert!(
        h.notices()
            .iter()
            .any(|notice| notice.contains("saved note")),
        "{:?}",
        h.notices()
    );
}

// ⌘L on a selection asks for an address and links it; on a link it offers to
// edit or remove it; removing keeps the text, and undo brings the link back.
#[gpui::test]
fn a_link_is_added_changed_and_removed_from_the_keyboard(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("n.md", "a b c\n")], |_| {});
    h.select(3, 4);
    h.keys("cmd-l");
    h.type_text("https://x.org");
    h.keys("enter");
    assert_eq!(h.markdown(), "a [b](https://x.org) c");

    // Edit is the first control of the link's pill.
    h.select(4, 4);
    h.keys("cmd-l tab enter cmd-a");
    h.type_text("https://y.org");
    h.keys("enter");
    assert_eq!(h.markdown(), "a [b](https://y.org) c");
    h.assert_round_trip("a changed link");

    // Remove is the pill's fourth.
    h.select(4, 4);
    h.keys("cmd-l tab tab tab tab enter");
    assert_eq!(h.markdown(), "a b c");
    h.keys("cmd-z");
    assert_eq!(h.markdown(), "a [b](https://y.org) c");

    // An address cleared to nothing removes the link as well.
    h.select(4, 4);
    h.keys("cmd-l tab enter cmd-a backspace enter");
    assert_eq!(h.markdown(), "a b c");
}

// The code block's language is chosen from a filtered list and written on its
// fence; undo takes it back.
#[gpui::test]
fn a_code_blocks_language_is_chosen_from_the_list(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("c.md", "```\nfn x\n```\n")], |_| {});
    h.keys("cmd-up");
    let editor = h.app.update(h.cx, |app, _| app.test_editor());
    // What clicking the block's language label reports.
    editor.update(h.cx, |_, cx| {
        cx.emit(markraft_gpui::EditorEvent::CodeLanguageRequested { pos: 0 })
    });
    h.cx.run_until_parked();
    h.type_text("rust");
    h.keys("enter");
    assert_eq!(h.markdown(), "```rust\nfn x\n```");
    h.assert_round_trip("a chosen language");
    h.keys("cmd-z");
    assert_eq!(h.markdown(), "```\nfn x\n```");
}
