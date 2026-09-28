//! Preferences changed while notes are open, as the Settings window changes them:
//! each reaches every open note, not only the one on screen, applies from the next
//! edit on, and is what the next launch reads.

use super::harness::{open, open_with};
use crate::storage::*;
use gpui::TestAppContext;

#[gpui::test]
fn locale_changes_preserve_search_document_and_undo(cx: &mut TestAppContext) {
    use crate::locale::{I18n, LanguagePreference};

    let mut h = open(cx, |_| {});
    h.type_text("unchanged note");
    let source = h.markdown();
    h.keys("cmd-k");
    h.type_text("new note");
    h.app.update(h.cx, |app, cx| {
        app.test_set_locale(I18n::fixture("zh-Hans"), cx);
    });
    h.cx.run_until_parked();
    assert_eq!(
        h.app.update(h.cx, |app, cx| app.test_query_text(cx)),
        "new note"
    );
    assert_eq!(
        h.app.update(h.cx, |app, cx| app.test_action_labels(cx)),
        Some(vec!["新建测试笔记".to_owned()])
    );
    assert_eq!(h.markdown(), source);

    h.set_preference(Pref::Language(LanguagePreference::Locale("en".into())));
    assert_eq!(
        h.app.update(h.cx, |app, cx| app.test_query_text(cx)),
        "new note"
    );
    assert_eq!(
        h.app.update(h.cx, |app, cx| app.test_action_labels(cx)),
        Some(vec!["New Note".to_owned()])
    );
    h.keys("escape cmd-z");
    assert_eq!(h.markdown(), "");
}

#[gpui::test]
fn locale_changes_reach_a_slash_provider_in_an_existing_editor(cx: &mut TestAppContext) {
    let mut h = open(cx, |_| {});
    h.app.update(h.cx, |app, cx| {
        app.test_set_locale(crate::locale::I18n::fixture("zh-Hans"), cx);
    });
    h.cx.run_until_parked();
    h.type_text("/测试标题");
    h.keys("enter");
    h.type_text("Title");
    assert_eq!(h.markdown(), "# Title");
}

// A note left open in the background takes a change made while another is on
// screen: coming back to it, typing follows the new preferences.
#[gpui::test]
fn a_changed_preference_reaches_every_open_note(cx: &mut TestAppContext) {
    let mut h = open_with(
        cx,
        &[("alpha.md", "alpha\n"), ("beta.md", "beta\n")],
        |_| {},
    );
    h.browse_to("alpha");
    h.browse_to("beta");
    h.set_preference(Pref::AutoPair(false));
    h.set_preference(Pref::MarkdownShortcuts(false));

    h.browse_to("alpha");
    h.keys("cmd-down cmd-right enter");
    h.type_text("(a");
    assert_eq!(h.markdown(), "alpha\n\n(a");
    h.keys("enter");
    h.type_text("- b");
    assert_eq!(h.markdown(), "alpha\n\n(a\n\n\\- b");
    h.assert_round_trip("typing after the change");

    // Turning them back on applies as promptly.
    h.set_preference(Pref::AutoPair(true));
    h.set_preference(Pref::MarkdownShortcuts(true));
    h.browse_to("beta");
    h.keys("cmd-down cmd-right enter");
    h.type_text("- (c");
    assert_eq!(h.markdown(), "beta\n\n- (c)");
}

// Numbering reaches background sessions and notes opened after the change,
// without rewriting the Markdown source.
#[gpui::test]
fn equation_numbering_reaches_existing_and_new_editors(cx: &mut TestAppContext) {
    let mut h = open_with(
        cx,
        &[
            ("alpha.md", "alpha\n\n$$x$$\n"),
            ("beta.md", "beta\n\n$$y$$\n"),
        ],
        |preferences| preferences.auto_number_equations = true,
    );
    let first = h.app.update(h.cx, |app, _| app.test_editor());
    assert!(first.update(h.cx, |editor, _| editor.auto_number_equations()));
    h.browse_to("alpha");
    h.browse_to("beta");
    let source = h.markdown();
    for enabled in [false, true] {
        h.set_preference(Pref::AutoNumberEquations(enabled));
        let editors = h.app.update(h.cx, |app, _| app.test_editors());
        assert!(editors.len() >= 2, "both notes stay open");
        for editor in editors {
            assert_eq!(
                editor.update(h.cx, |editor, _| editor.auto_number_equations()),
                enabled
            );
        }
        assert_eq!(h.markdown(), source);
    }
    h.keys("cmd-n");
    let created = h.app.update(h.cx, |app, _| app.test_editor());
    assert!(created.update(h.cx, |editor, _| editor.auto_number_equations()));
}

// Tab in a code block writes what the preference now names, in an open note.
#[gpui::test]
fn the_tab_key_preference_applies_to_an_open_code_block(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("c.md", "```\nx\n```\n")], |_| {});
    h.set_preference(Pref::TabKey(TabKey::TwoSpaces));
    h.keys("cmd-up tab");
    assert!(h.markdown().contains("\n  x"), "{:?}", h.markdown());
    h.set_preference(Pref::TabKey(TabKey::Tab));
    h.keys("tab");
    assert!(h.markdown().contains("\n  \tx"), "{:?}", h.markdown());
}

// Vim turned on while a note is open starts in normal mode; turned off, keys type.
#[gpui::test]
fn vim_turned_on_and_off_while_a_note_is_open(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("v.md", "abc\n")], |_| {});
    h.set_preference(Pref::VimMode(true));
    h.keys("cmd-up");
    h.type_text("x");
    assert_eq!(h.markdown(), "bc");
    h.set_preference(Pref::VimMode(false));
    h.type_text("x");
    assert_eq!(h.markdown(), "xbc");
    h.assert_round_trip("vim off");
}

// The Markdown written by a command follows a style changed after the note opened.
#[gpui::test]
fn markdown_style_changes_apply_to_the_next_command(cx: &mut TestAppContext) {
    let mut h = open(cx, |_| {});
    h.set_preference(Pref::Emphasis(EmphasisMarker::Underscore));
    h.type_text("a ");
    h.keys("cmd-i");
    h.type_text("b");
    assert_eq!(h.markdown(), "a _b_");

    let mut h = open(cx, |_| {});
    h.set_preference(Pref::Bullet(BulletMarker::Plus));
    h.set_preference(Pref::OrderedDelimiter(OrderedDelimiter::Parenthesis));
    h.type_text("a");
    h.keys("cmd-*");
    assert_eq!(h.markdown(), "+ a");
    h.keys("cmd-&");
    assert_eq!(h.markdown(), "1) a");
    h.assert_round_trip("list markers");

    let mut h = open(cx, |_| {});
    h.set_preference(Pref::EmojiCharacters(true));
    h.type_text(":smile:");
    assert_eq!(h.markdown(), "😄");
}

// Theme and typography restyle every open note, the one off screen as well.
#[gpui::test]
fn theme_and_typography_restyle_every_open_note(cx: &mut TestAppContext) {
    let mut h = open_with(
        cx,
        &[("alpha.md", "alpha\n"), ("beta.md", "beta\n")],
        |_| {},
    );
    h.browse_to("alpha");
    h.browse_to("beta");
    h.set_preference(Pref::Theme(Some(true)));
    assert!(h.app.update(h.cx, |app, _| app.test_dark()));
    h.set_preference(Pref::Font(EditorFont::Serif));
    h.set_preference(Pref::LineHeight(LineHeight::Relaxed));
    h.set_preference(Pref::TextSize(18.));
    h.set_preference(Pref::LineWidth(LineWidth::Full));

    let editors = h.app.update(h.cx, |app, _| app.test_editors());
    assert!(editors.len() >= 2, "both notes stay open");
    for editor in editors {
        editor.update(h.cx, |editor, _| {
            let style = editor.style();
            assert_eq!(style.font_family.as_ref(), EditorFont::Serif.family());
            assert_eq!(style.line_height_ratio, LineHeight::Relaxed.ratio());
            assert_eq!(style.max_line_width, None);
        });
    }
    h.set_preference(Pref::Theme(Some(false)));
    assert!(!h.app.update(h.cx, |app, _| app.test_dark()));
}

// What was changed is what the next launch reads, with a text size kept to the
// sizes the note offers.
#[gpui::test]
fn a_changed_preference_is_what_the_next_launch_reads(cx: &mut TestAppContext) {
    let mut h = open(cx, |_| {});
    let language = crate::locale::LanguagePreference::Locale("future-Language".into());
    h.set_preference(Pref::Language(language.clone()));
    h.set_preference(Pref::VimMode(true));
    h.set_preference(Pref::Font(EditorFont::Mono));
    h.set_preference(Pref::AutoNumberEquations(true));
    h.set_preference(Pref::Bullet(BulletMarker::Star));
    h.set_preference(Pref::TextSize(99.));
    h.set_preference(Pref::SettingsPage("editor".into()));
    let expected = h.app.update(h.cx, |app, _| app.test_preferences());
    assert_eq!(expected.text_size, *Preferences::TEXT_SIZES.end());

    let saved = h.saved_preferences(|saved| {
        saved.vim_mode && saved.settings_page == "editor" && saved.language == language
    });
    let saved = saved.expect("a readable settings file");
    assert!(saved.vim_mode);
    assert!(saved.auto_number_equations);
    assert_eq!(saved.font, EditorFont::Mono);
    assert_eq!(saved.bullet_marker, BulletMarker::Star);
    assert_eq!(saved.text_size, expected.text_size);
    assert_eq!(saved.settings_page, "editor");
    assert_eq!(saved.language, language);
}
