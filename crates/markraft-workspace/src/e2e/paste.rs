//! Pastes: Markdown into items, cells and paragraphs, and an image beside its note.

use super::harness::{open, open_with};
use gpui::TestAppContext;

// A pasted image is written beside the note and referenced with nothing after it;
// named for a note named for today, its name holds the date once.
#[gpui::test]
fn a_pasted_image_lands_beside_the_note_under_its_name(cx: &mut TestAppContext) {
    use crate::storage::{ImageNaming, NoteNaming};
    let mut h = open(cx, |_| {});
    h.app.update(h.cx, |app, _| {
        app.set_workspace_naming(NoteNaming::DateTime, ImageNaming::NoteAndDate)
    });
    h.type_text("x");
    h.save();
    let png = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/icon/markraft-menubar.png"
    ))
    .expect("an image to paste");
    h.cx.write_to_clipboard(gpui::ClipboardItem::new_image(&gpui::Image::from_bytes(
        gpui::ImageFormat::Png,
        png,
    )));
    h.keys("cmd-v");
    h.wait_until(|h| h.markdown().ends_with(".png)"));
    let markdown = h.markdown();
    // At the caret, inline: `x![image](…)`, with nothing typed after it.
    assert!(markdown.starts_with("x![image](assets/"), "{markdown:?}");
    assert!(
        markdown.ends_with(".png)"),
        "text after the image: {markdown:?}"
    );
    let image = std::fs::read_dir(h.notes.join("assets"))
        .expect("the assets folder")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .next()
        .expect("the pasted image");
    let date = &image[..10];
    assert_eq!(image.matches(date).count(), 1, "{image}");
}

// Markdown pasted into a list item and into a table cell. A GFM row is one
// line, so what is pasted into a cell goes in as its inline content, a
// `<br/>` for each line ending, and the table keeps its
// columns.
#[gpui::test]
fn markdown_pasted_into_a_list_item_and_a_table_cell(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("p.md", "- a\n")], |_| {});
    h.keys("cmd-down cmd-right");
    h.cx.write_to_clipboard(gpui::ClipboardItem::new_string("**b** and *c*".into()));
    h.keys("cmd-v");
    assert_eq!(h.markdown(), "- a**b** and *c*");
    h.assert_round_trip("inline Markdown pasted into an item");
    let table = "| a | b |\n| --- | --- |\n| 1 | 2 |\n";
    for (pasted, expected) in [
        ("x\ny", "| a | b |\n| --- | --- |\n| 1 | 2x<br/>y |\n"),
        (
            "x\n\ny",
            "| a | b |\n| --- | --- |\n| 1 | 2x<br/><br/>y |\n",
        ),
        ("x | y\n", "| a | b |\n| --- | --- |\n| 1 | 2x \\| y |\n"),
        (
            "**p** one\n- b",
            "| a | b |\n| --- | --- |\n| 1 | 2**p** one<br/>b |\n",
        ),
    ] {
        let mut h = open_with(cx, &[("t.md", table)], |_| {});
        h.keys("cmd-down");
        h.cx.write_to_clipboard(gpui::ClipboardItem::new_string(pasted.into()));
        h.keys("cmd-v");
        h.assert_round_trip("blocks pasted into a cell");
        let text = h.wait_for_file("t.md", |text| text == expected);
        assert_eq!(text, expected, "{pasted:?}");
    }
}

// Paragraphs pasted in the middle of a paragraph carry on the text either
// side of the caret; a heading at an end stays a block.
#[gpui::test]
fn paragraphs_pasted_mid_paragraph_join_the_text_around_them(cx: &mut TestAppContext) {
    for (pasted, expected) in [
        ("one\n\ntwo", "AAA one\n\ntwoBBB"),
        ("one\n\nmid\n\ntwo", "AAA one\n\nmid\n\ntwoBBB"),
        ("one\n\n# Head", "AAA one\n\n# Head\n\nBBB"),
    ] {
        let mut h = open_with(cx, &[("p.md", "AAA BBB\n")], |_| {});
        h.keys("cmd-up alt-right right");
        h.cx.write_to_clipboard(gpui::ClipboardItem::new_string(pasted.into()));
        h.keys("cmd-v");
        assert_eq!(h.markdown(), expected, "{pasted:?}");
        h.assert_round_trip("paragraphs pasted mid-paragraph");
    }
}

// A list pasted into an empty item adds its items beside the ones around it,
// whatever kind of list it was, rather than nesting a list in the item.
#[gpui::test]
fn a_list_pasted_into_an_empty_item_joins_the_list(cx: &mut TestAppContext) {
    for (source, pasted, expected) in [
        ("- a\n", "- b\n- c", "- a\n- b\n- c"),
        ("- a\n", "1. b\n2. c", "- a\n- b\n- c"),
        ("1. a\n", "- b\n- c", "1. a\n2. b\n3. c"),
        ("- [ ] a\n", "- b\n- c", "- [ ] a\n- b\n- c"),
    ] {
        let mut h = open_with(cx, &[("l.md", source)], |_| {});
        h.keys("cmd-down enter");
        h.cx.write_to_clipboard(gpui::ClipboardItem::new_string(pasted.into()));
        h.keys("cmd-v");
        assert_eq!(h.markdown(), expected, "{source:?} + {pasted:?}");
        h.assert_round_trip("a list pasted into an empty item");
    }
}

// A paste keeps the space it starts with.
#[gpui::test]
fn a_paste_keeps_its_leading_space(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("p.md", "x\n")], |_| {});
    h.keys("cmd-down cmd-right");
    h.cx.write_to_clipboard(gpui::ClipboardItem::new_string(" ![a](b) and".into()));
    h.keys("cmd-v");
    assert_eq!(h.markdown(), "x ![a](b) and");
}

// What is copied across two list items pastes back as two items.
#[gpui::test]
fn a_copy_across_two_items_pastes_as_items(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("c.md", "- one\n- two\n")], |_| {});
    h.keys("cmd-up cmd-left right shift-down shift-right cmd-c cmd-down enter enter cmd-v");
    let markdown = h.markdown();
    let items: Vec<_> = markdown.lines().filter(|line| !line.is_empty()).collect();
    assert_eq!(items, ["- one", "- two", "- ne", "- tw"], "{markdown:?}");
}
