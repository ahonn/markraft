//! System service rewrites operate on reading text, preserving source styles.

mod common;

use common::Codec;
use markraft_commonmark::{CommonMarkCodecs, Formatter, commonmark_extensions};
use markraft_core::{
    EditorState, EditorStateConfig, Extension, Selection, Slice,
    history::{HistoryConfig, history, redo, undo},
    kind::{Codecs, ReadingReplacementPolicy},
};

fn editor(codec: &Codec, source: &str) -> EditorState {
    EditorState::create(
        EditorStateConfig::new(codec.schema.clone())
            .doc(codec.parse(source))
            .selection(Selection::All)
            .extensions(Extension::all([
                commonmark_extensions(&codec.schema),
                history(HistoryConfig::default()),
            ])),
    )
    .unwrap()
}

fn reading(codec: &Codec, state: &EditorState) -> String {
    CommonMarkCodecs::new(codec.schema.clone(), codec.house.clone())
        .to_text(&Slice::from_fragment(state.doc().content().clone()))
}

fn replace(codec: &Codec, state: &EditorState, text: &str) -> EditorState {
    let range = state.selection().from(state.doc())..state.selection().to(state.doc());
    let command = Formatter::new(codec.house.clone()).replace_reading(range, text);
    let spec = command(state)
        .unwrap()
        .expect("supported reading replacement");
    let changed = state.update([spec]).unwrap().state().clone();
    assert_eq!(
        reading(codec, &changed),
        text,
        "{}",
        codec.write(changed.doc())
    );
    assert_eq!(codec.parse(&codec.write(changed.doc())), *changed.doc());
    let undone = changed
        .update([undo(&changed).unwrap()])
        .unwrap()
        .state()
        .clone();
    assert_eq!(*undone.doc(), *state.doc());
    let redone = undone
        .update([redo(&undone).unwrap()])
        .unwrap()
        .state()
        .clone();
    assert_eq!(*redone.doc(), *changed.doc());
    changed
}

#[test]
fn plain_service_partial_replacement_preserves_unselected_styles_and_source() {
    let codec = Codec::new();
    let source = "before &amp; **red** _blue_ after &copy; ![image](x.png)";
    let state = editor(&codec, source);
    let start = source.find("red").unwrap() + 1;
    let end = source.find("blue").unwrap() + 1 + 4;
    let command = Formatter::new(codec.house.clone()).replace_reading_with_policy(
        start..end,
        "red blue",
        ReadingReplacementPolicy::InheritSelectionStart,
    );
    let changed = state
        .update([command(&state).unwrap().unwrap()])
        .unwrap()
        .state()
        .clone();
    let written = codec.write(changed.doc());
    assert_eq!(
        written,
        "before &amp; **red blue** after &copy; ![image](x.png)"
    );
    assert_eq!(codec.parse(&written), *changed.doc());
    assert_eq!(
        *changed.update([undo(&changed).unwrap()]).unwrap().new_doc(),
        *state.doc()
    );
}

#[test]
fn variable_length_unicode_edits_preserve_unchanged_inline_source() {
    let codec = Codec::new();
    let before = editor(
        &codec,
        "Keep &amp; **café** then _later_.\n\nOther paragraph.",
    );
    let after = replace(
        &codec,
        &before,
        "Keep & café 😀 then later.\nOther paragraphs are longer.",
    );
    let source = codec.write(after.doc());
    assert!(
        source.contains("Keep &amp; **café** 😀 then _later_."),
        "{source}"
    );
}

#[test]
fn changed_words_and_equal_graphemes_keep_their_character_styles() {
    let codec = Codec::new();
    let before = editor(&codec, "**hello** _world_ 中文 👩‍💻");
    let after = replace(&codec, &before, "greetings world 中文 👩‍🚀");
    let source = codec.write(after.doc());
    assert!(source.contains("_world_"), "{source}");
    assert!(!source.contains("👩‍💻"));
    assert!(source.contains("greetings"));
}

#[test]
fn paragraph_insertion_and_deletion_support_full_rewrites() {
    let codec = Codec::new();
    let before = editor(&codec, "**first**\n\n_second_\n\nthird");
    replace(&codec, &before, "Replacement 😀\nMore words");
    replace(&codec, &before, "Only one paragraph");
    replace(&codec, &before, "");
}

#[test]
fn partial_selection_keeps_the_unselected_source_and_styles() {
    let codec = Codec::new();
    let source = "before **hello world** after &amp;";
    let initial = editor(&codec, source);
    let state = initial
        .update([markraft_core::TransactionSpec::new().selection(Selection::text(10, 15))])
        .unwrap()
        .state()
        .clone();
    let command = Formatter::new(codec.house.clone()).replace_reading(10..15, "hi 😀");
    let after = state
        .update([command(&state).unwrap().unwrap()])
        .unwrap()
        .state()
        .clone();
    assert_eq!(
        codec.write(after.doc()),
        "before **hi 😀 world** after &amp;"
    );
    assert_eq!(codec.parse(&codec.write(after.doc())), *after.doc());
}

#[test]
fn inserted_combining_marks_keep_selection_on_new_grapheme_boundaries() {
    let codec = Codec::new();
    let state = editor(&codec, "ab");
    let command = Formatter::new(codec.house.clone()).replace_reading(2..3, "\u{301}");
    let after = state
        .update([command(&state).unwrap().unwrap()])
        .unwrap()
        .state()
        .clone();
    assert_eq!(reading(&codec, &after), "a\u{301}");
    assert_eq!(after.selection(), &Selection::text(1, 3));
}

#[test]
fn many_separated_edits_preserve_reading_and_styles_with_bounded_local_spelling() {
    let codec = Codec::new();
    let source = format!("**{}**", "a word ".repeat(1000).trim_end());
    let state = editor(&codec, &source);
    let output = "b word ".repeat(1000).trim_end().to_owned();
    let after = replace(&codec, &state, &output);
    let written = codec.write(after.doc());
    assert!(
        written.starts_with("**") || written.starts_with("<strong"),
        "{}",
        &written[..written.len().min(100)]
    );
}

#[test]
fn dense_replacements_keep_unselected_source_and_atoms() {
    let codec = Codec::new();
    let prefix = "before &amp; <em>kept</em> **";
    let suffix = "** ![image](x.png) &copy;";
    let body = "a word ".repeat(10_000).trim_end().to_owned();
    let replacement = "b word ".repeat(10_000).trim_end().to_owned();
    let source = format!("{prefix}{body}{suffix}");
    let state = editor(&codec, &source);
    let start = prefix.chars().count() + 1;
    let command = Formatter::new(codec.house.clone())
        .replace_reading(start..start + body.chars().count(), &replacement);
    let started = std::time::Instant::now();
    let spec = command(&state).unwrap().unwrap();
    let duration = started.elapsed();
    eprintln!("70k reading chars / 10k edits planned in {duration:?}");
    let after = state.update([spec]).unwrap().state().clone();
    assert_eq!(
        codec.write(after.doc()),
        format!("{prefix}{replacement}{suffix}")
    );
    assert_eq!(codec.parse(&codec.write(after.doc())), *after.doc());
}

#[test]
fn unsupported_structures_and_partial_graphemes_refuse_without_changes() {
    let codec = Codec::new();
    for source in ["plain `code`", "![image](x.png)"] {
        let state = editor(&codec, source);
        assert!(
            Formatter::new(codec.house.clone())
                .replace_reading(0..state.doc().content_size(), "new")(&state)
            .is_err(),
            "{source}"
        );
    }
    for source in ["- list", "```\ncode\n```"] {
        let state = editor(&codec, source);
        assert!(
            Formatter::new(codec.house.clone())
                .replace_reading(0..state.doc().content_size(), "new")(&state)
            .unwrap()
            .is_none()
        );
    }
    let state = editor(&codec, "e\u{301} text");
    assert!(Formatter::new(codec.house.clone()).replace_reading(1..2, "e")(&state).is_err());
}

#[test]
fn service_results_collapse_after_the_complete_new_grapheme() {
    let codec = Codec::new();
    let state = editor(&codec, "ab");
    let command = Formatter::new(codec.house.clone()).replace_reading_with_policy(
        2..3,
        "\u{301}",
        ReadingReplacementPolicy::InheritSelectionStart,
    );
    {
        let changed = state
            .update([command(&state).unwrap().unwrap()])
            .unwrap()
            .state()
            .clone();
        assert_eq!(reading(&codec, &changed), "a\u{301}");
        assert_eq!(changed.selection().from(changed.doc()), 3);
        assert_eq!(changed.selection().to(changed.doc()), 3);
    }
}
