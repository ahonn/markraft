//! Inline edits: styles at a caret, word motion, pairing, the Emacs keys and
//! inline HTML.

use super::harness::open_with;
use gpui::TestAppContext;

// ⌘B at a caret: in a word it bolds the word,
// in a bold span it takes the bold off the whole span, and elsewhere it
// leaves a pair to type into.
#[gpui::test]
fn bold_at_a_caret_styles_the_word_or_the_span(cx: &mut TestAppContext) {
    for (source, caret, expected) in [
        ("123 456\n", 6, "123 **4X56**"),
        ("0 **12** 3\n", 6, "0 1X2 3"),
        ("12  34\n", 4, "12 **X** 34"),
    ] {
        let mut h = open_with(cx, &[("b.md", source)], |_| {});
        h.select(caret, caret);
        h.keys("cmd-b");
        h.type_text("X");
        assert_eq!(h.markdown(), expected, "{source:?}");
        h.assert_round_trip("bold at a caret");
    }
}

// The Emacs keys every macOS text view takes: ⌃A and ⌃E go to the ends of
// the paragraph, ⌃K deletes to its end, ⌃D and ⌃H delete a character.
#[gpui::test]
fn the_emacs_keys_move_and_delete(cx: &mut TestAppContext) {
    let cases: &[(&str, &str, &str)] = &[
        ("hello **world**\n", "cmd-up ctrl-e", "hello **world**X"),
        ("hello world\n", "cmd-up cmd-right ctrl-a", "Xhello world"),
        ("hello world\n", "cmd-up ctrl-f ctrl-f ctrl-k", "heX"),
        ("hello\n", "cmd-up ctrl-d", "Xello"),
        ("hello\n", "cmd-up ctrl-f ctrl-h", "Xello"),
    ];
    for (source, keys, expected) in cases {
        let mut h = open_with(cx, &[("e.md", source)], |_| {});
        h.keys(keys);
        h.type_text("X");
        assert_eq!(h.markdown(), *expected, "{source:?} {keys}");
    }
}

// Inline HTML nothing reads is text like any other: the caret reaching a tag finds its
// source, which is edited character by character and saved as typed, and no
// shortcut opens a separate HTML editor.
#[gpui::test]
fn inline_html_is_edited_as_text(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("h.md", "press <var>K</var> now\n")], |_| {});
    h.select(7, 7);
    h.keys("right right");
    h.type_text("a");
    h.keys("cmd-down");
    h.save();
    let expected = "press <vaar>K</var> now\n";
    assert_eq!(h.wait_for_file("h.md", |text| text == expected), expected);
    // ⌥⌘R is bound to nothing.
    h.keys("alt-cmd-r");
    assert_eq!(h.markdown(), "press <vaar>K</var> now");
}

// ⌥← from the end of a line that ends in hidden markup reaches the
// start of the word inside it, as it does before plain text.
#[gpui::test]
fn word_motion_crosses_hidden_markup_at_a_line_edge(cx: &mut TestAppContext) {
    for (source, keys, expected) in [
        ("**abc**\n", "cmd-up cmd-right alt-left", "**Xabc**"),
        ("`abc`\n", "cmd-up cmd-right alt-left", "`Xabc`"),
        ("*a b c*\n", "cmd-up cmd-right alt-left", "*a b Xc*"),
        ("**a** **b**\n", "cmd-up cmd-left alt-right", "**aX** **b**"),
    ] {
        let mut h = open_with(cx, &[("w.md", source)], |_| {});
        h.keys(keys);
        h.type_text("X");
        assert_eq!(h.markdown(), expected, "{source:?}");
    }
}

// An opener typed into a note just emptied still writes its closer.
#[gpui::test]
fn an_emptied_note_pairs_the_first_opener(cx: &mut TestAppContext) {
    for (opener, expected) in [("(", "()"), ("[", "[]"), ("{", "{}"), ("\"", "\"\"")] {
        let mut h = open_with(cx, &[("p.md", "x\n")], |_| {});
        h.keys("cmd-a backspace");
        h.type_text(opener);
        assert_eq!(h.markdown(), expected, "{opener}");
    }
}

// A word deletion over hidden markup keeps what is typed after it.
#[gpui::test]
fn typing_after_deleting_words_over_hidden_markup_reaches_the_note(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("w.md", "hello **world** end\n")], |_| {});
    h.keys("cmd-up cmd-right alt-backspace alt-backspace");
    h.type_text("X");
    assert_eq!(h.markdown(), "hello X");
}

// A reference definition typed on a line of its own is a definition.
#[gpui::test]
fn a_typed_reference_definition_defines(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("r.md", "[a][ref]\n")], |_| {});
    h.keys("cmd-down cmd-right enter");
    h.type_text("[ref]: /wx");
    h.keys("cmd-up");
    assert_eq!(h.markdown(), "[a][ref]\n\n[ref]: /wx");
}

// A bracket pairs before whitespace or a closer, not before punctuation.
#[gpui::test]
fn a_bracket_before_punctuation_does_not_pair(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("p.md", "see , then\n")], |_| {});
    h.keys("cmd-up cmd-left right right right right");
    h.type_text("(a");
    assert_eq!(h.markdown(), "see (a, then");
}

// ⌘B over part of a bold span takes the bold off the whole span; over
// part of a code span it does nothing.
#[gpui::test]
fn strong_over_part_of_a_span_acts_on_the_whole_or_not_at_all(cx: &mut TestAppContext) {
    for (source, expected) in [("**abc**\n", "abc"), ("`abc`\n", "`abc`")] {
        let mut h = open_with(cx, &[("s.md", source)], |_| {});
        h.keys("cmd-up cmd-right alt-left right shift-right cmd-b");
        assert_eq!(h.markdown(), expected, "{source:?}");
    }
}
