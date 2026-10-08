//! What reaches the editor from around it: the preferences, the menus, the
//! input method and a panel holding the keyboard.

use super::harness::{Harness, open, open_with};
use gpui::TestAppContext;

// The `/` menu searches past a space: `/code bl` still finds Code Block.
#[gpui::test]
fn the_slash_menu_searches_past_a_space(cx: &mut TestAppContext) {
    let mut h = open(cx, |_| {});
    h.type_text("/code bl");
    h.keys("enter");
    // The query is taken away with the menu: nothing of `/code bl` is left.
    assert_eq!(h.markdown(), "```\n```");
}

// Each preference reaches the note's editor.
#[gpui::test]
fn preferences_reach_the_editor(cx: &mut TestAppContext) {
    use crate::storage::*;
    let mut h = open(cx, |p| p.auto_pair = false);
    h.type_text("(a");
    assert_eq!(h.markdown(), "(a");
    // Pairing is on by default.
    let mut h = open(cx, |_| {});
    h.type_text("(a");
    assert_eq!(h.markdown(), "(a)");
    // Tab in a code block writes the spaces asked for.
    let mut h = open_with(cx, &[("c.md", "```\nx\n```\n")], |p| {
        p.tab_key = TabKey::FourSpaces
    });
    h.keys("cmd-up tab");
    assert!(h.markdown().contains("\n    x"), "{:?}", h.markdown());
    // The list shortcuts take the preferred markers.
    let mut h = open(cx, |p| {
        p.bullet_marker = BulletMarker::Plus;
        p.ordered_delimiter = OrderedDelimiter::Parenthesis;
    });
    h.type_text("a");
    h.keys("cmd-*");
    assert_eq!(h.markdown(), "+ a");
    h.keys("cmd-&");
    assert_eq!(h.markdown(), "1) a");
    let mut h = open(cx, |p| p.emphasis_marker = EmphasisMarker::Underscore);
    h.type_text("a ");
    h.keys("cmd-i");
    h.type_text("b");
    assert_eq!(h.markdown(), "a _b_");
    let mut h = open(cx, |p| p.emoji_characters = false);
    h.type_text(":smile:");
    assert_eq!(h.markdown(), ":smile:");
    let mut h = open(cx, |p| p.emoji_characters = true);
    h.type_text(":smile:");
    assert_eq!(h.markdown(), "😄");
}

// The input method sees the note in UTF-16 units, as macOS counts them: an
// emoji is two. A range that starts or ends inside one is taken back to its
// start; marked text and its caret are placed in those units; empty marked
// text cancels; a marked range can replace text; a commit writes it.
#[gpui::test]
fn the_input_method_sees_the_note_in_utf16_units(cx: &mut TestAppContext) {
    use gpui::EntityInputHandler;
    let mut h = open_with(cx, &[("i.md", "a\u{1F600}b\n")], |_| {});
    h.keys("cmd-down");
    let editor = h.app.update(h.cx, |app, _| app.test_editor());
    let text = |h: &mut Harness, range: std::ops::Range<usize>| {
        editor.update_in(h.cx, |view, window, cx| {
            let mut actual = None;
            let text = view.text_for_range(range, &mut actual, window, cx);
            (text, actual)
        })
    };
    assert_eq!(text(&mut h, 0..4), (Some("a\u{1F600}b".into()), Some(0..4)));
    assert_eq!(text(&mut h, 1..3), (Some("\u{1F600}".into()), Some(1..3)));
    assert_eq!(text(&mut h, 1..2), (Some(String::new()), Some(1..1)));
    assert_eq!(text(&mut h, 2..4), (Some("\u{1F600}b".into()), Some(1..4)));
    let state = |h: &mut Harness| {
        editor.update_in(h.cx, |view, window, cx| {
            (
                view.marked_text_range(window, cx),
                view.selected_text_range(false, window, cx).map(|s| s.range),
            )
        })
    };
    assert_eq!(state(&mut h), (None, Some(4..4)));

    // A candidate whose caret is after its emoji: two units in.
    editor.update_in(h.cx, |view, window, cx| {
        view.replace_and_mark_text_in_range(None, "\u{1F600}x", Some(2..2), window, cx)
    });
    assert_eq!(state(&mut h), (Some(4..7), Some(6..6)));
    // Empty marked text is how macOS says the candidate was cancelled.
    editor.update_in(h.cx, |view, window, cx| {
        view.replace_and_mark_text_in_range(None, "", None, window, cx)
    });
    assert_eq!(state(&mut h), (None, Some(4..4)));
    assert_eq!(h.markdown(), "a\u{1F600}b");

    // A candidate over `b`, then committed in its place.
    editor.update_in(h.cx, |view, window, cx| {
        view.replace_and_mark_text_in_range(Some(3..4), "x", None, window, cx)
    });
    assert_eq!(state(&mut h).0, Some(3..4));
    editor.update_in(h.cx, |view, window, cx| {
        view.replace_text_in_range(None, "\u{597d}", window, cx)
    });
    assert_eq!(state(&mut h), (None, Some(4..4)));
    assert_eq!(h.markdown(), "a\u{1F600}\u{597d}");

    // Where a candidate window goes: further along the line is further
    // right. The test platform lays text out with a placeholder font, so
    // only the order is judged.
    let bounds = |h: &mut Harness, range: std::ops::Range<usize>| {
        editor
            .update_in(h.cx, |view, window, cx| {
                view.bounds_for_range(range, gpui::Bounds::default(), window, cx)
            })
            .expect("bounds for text on screen")
    };
    let first = bounds(&mut h, 0..1);
    let last = bounds(&mut h, 3..4);
    assert!(first.origin.x < last.origin.x, "{first:?} {last:?}");
    assert!(first.size.height > gpui::px(0.));
    h.assert_round_trip("a committed candidate");
}

// The wiki-link and emoji menus open as the trigger is typed and write what
// is chosen.
#[gpui::test]
fn the_link_and_emoji_menus_open_as_they_are_typed(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("Target note.md", "t\n"), ("here.md", "h\n")], |_| {});
    let link = h.wiki_link("Target note.md");
    h.keys("cmd-n");
    h.type_text("see [[Targ");
    h.keys("enter");
    assert_eq!(h.markdown(), format!("see {link}"));
    h.type_text(" :smil");
    h.keys("enter");
    assert_eq!(h.markdown(), format!("see {link} :smile:"));
    h.assert_round_trip("a link and an emoji from the menus");
}

// With a panel open Tab walks its controls and leaves the note alone.
#[gpui::test]
fn tab_walks_an_open_panel_rather_than_indenting(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("l.md", "- a\n- b\n")], |_| {});
    h.keys("cmd-down cmd-k");
    let before = h.markdown();
    h.keys("tab tab");
    assert_eq!(h.markdown(), before);
    h.keys("escape tab");
    assert_eq!(h.markdown(), "- a\n  - b");
}
