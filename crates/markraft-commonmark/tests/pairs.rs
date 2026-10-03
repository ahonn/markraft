//! Brackets and quotes that close themselves: what typing, Backspace and a
//! selection do with auto-pairing on, and that it is plain typing when off.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use markraft_commonmark::{
    commonmark_auto_pairs_with_quotes, commonmark_extensions, commonmark_schema, from_markdown,
    from_markdown_fragment, to_markdown,
};
use markraft_core::commands::{
    Command, Direction, delete_by_grapheme, delete_range, insert_text, replace_selection,
    run_command,
};
use markraft_core::composition::{CompositionRange, start_composition, update_composition};
use markraft_core::history::{HistoryConfig, history, undo};
use markraft_core::{EditorState, EditorStateConfig, Extension, Selection, TransactionSpec};

/// An editor over `markdown` with the caret where `|` is, auto-pairing
/// switched by the returned flag.
fn editor(markdown: &str) -> (EditorState, Arc<AtomicBool>) {
    let (state, enabled, _) = editor_with_quotes(markdown, true);
    (state, enabled)
}

fn editor_with_quotes(
    markdown: &str,
    quotes: bool,
) -> (EditorState, Arc<AtomicBool>, Arc<AtomicBool>) {
    let (source, caret) = split_caret(markdown);
    let schema = commonmark_schema();
    let doc = from_markdown(&schema, &source).expect("the source parses");
    let enabled = Arc::new(AtomicBool::new(true));
    let quotes_enabled = Arc::new(AtomicBool::new(quotes));
    let extensions = Extension::all([
        commonmark_extensions(&schema),
        commonmark_auto_pairs_with_quotes(enabled.clone(), quotes_enabled.clone()),
        history(HistoryConfig::default()),
        markraft_core::composition::composition(),
    ]);
    let state = EditorState::create(
        EditorStateConfig::new(schema.clone())
            .doc(doc)
            .extensions(extensions),
    )
    .expect("a valid state");
    let state = match caret {
        Some((anchor, head)) => {
            let anchor = pos_of(&state, anchor);
            let head = pos_of(&state, head);
            apply(
                &state,
                vec![TransactionSpec::new().selection(Selection::text(anchor, head))],
            )
        }
        // The end of the first textblock.
        None => {
            let end = pos_of(&state, block_len(&state));
            apply(
                &state,
                vec![TransactionSpec::new().selection(Selection::cursor(end))],
            )
        }
    };
    (state, enabled, quotes_enabled)
}

/// `markdown` without its caret mark `|` or selection marks `‹…›`, and where
/// they stood as `char` offsets into it — which is the first paragraph's text
/// where that is all the source is. Without either, the caret goes at the end
/// of the first textblock.
fn split_caret(markdown: &str) -> (String, Option<(usize, usize)>) {
    if let Some(at) = markdown.find('|') {
        let offset = markdown[..at].chars().count();
        return (markdown.replacen('|', "", 1), Some((offset, offset)));
    }
    if let (Some(open), Some(close)) = (markdown.find('‹'), markdown.find('›')) {
        let from = markdown[..open].chars().count();
        let to = markdown[..close].chars().count() - 1;
        let source = markdown.replacen('‹', "", 1).replacen('›', "", 1);
        return (source, Some((from, to)));
    }
    (markdown.to_string(), None)
}

/// The document position of `offset` into the first textblock's text.
fn pos_of(state: &EditorState, offset: usize) -> usize {
    let mut start = None;
    state.doc().descendants(&mut |node, pos, _, _| {
        if start.is_none() && node.is_textblock(state.schema()) {
            start = Some(pos + 1);
        }
        start.is_none()
    });
    start.expect("a textblock") + offset
}

fn apply(state: &EditorState, specs: Vec<TransactionSpec>) -> EditorState {
    state
        .update(specs)
        .expect("the transaction resolves")
        .state()
        .clone()
}

fn run(state: &EditorState, command: &Command) -> EditorState {
    run_command(state, command)
        .expect("the command applies")
        .expect("the transaction resolves")
        .state()
        .clone()
}

fn typed(state: &EditorState, text: &str) -> EditorState {
    text.chars().fold(state.clone(), |state, c| {
        run(&state, &insert_text(&c.to_string()))
    })
}

fn backspace(state: &EditorState) -> EditorState {
    run(state, &delete_by_grapheme(Direction::Backward))
}

/// The first textblock's text, with `|` at the caret, or `‹…›` around the
/// selection.
fn shown(state: &EditorState) -> String {
    let doc = state.doc();
    let start = pos_of(state, 0);
    let chars: Vec<char> = doc
        .text_between(
            state.schema(),
            start,
            doc.content_size().min(start + block_len(state)),
            None,
            Some(&|_| "\u{fffc}".to_string()),
        )
        .chars()
        .collect();
    let selection = state.selection();
    let (from, to) = (selection.from(doc) - start, selection.to(doc) - start);
    let text = |range: std::ops::Range<usize>| chars[range].iter().collect::<String>();
    if from == to {
        format!("{}|{}", text(0..from), text(from..chars.len()))
    } else {
        format!(
            "{}‹{}›{}",
            text(0..from),
            text(from..to),
            text(to..chars.len())
        )
    }
}

fn block_len(state: &EditorState) -> usize {
    let mut len = None;
    state.doc().descendants(&mut |node, _, _, _| {
        if len.is_none() && node.is_textblock(state.schema()) {
            len = Some(node.content_size());
        }
        len.is_none()
    });
    len.unwrap_or(0)
}

#[test]
fn an_opener_writes_its_closer_after_the_caret() {
    for (opener, pair) in [
        ("(", "(|)"),
        ("[", "[|]"),
        ("{", "{|}"),
        ("\"", "\"|\""),
        ("（", "（|）"),
        ("「", "「|」"),
        ("《", "《|》"),
        ("【", "【|】"),
    ] {
        let (state, _) = editor("");
        assert_eq!(shown(&typed(&state, opener)), pair, "{opener}");
    }
    // Before whitespace or a closing bracket it pairs; before punctuation or
    // a letter it does not.
    for (source, shown_after) in [
        ("see | then", "see (a|) then"),
        ("see |) then", "see (a|)) then"),
        ("see |] then", "see (a|)] then"),
        ("see |, then", "see (a|, then"),
        ("see |. then", "see (a|. then"),
        ("see |d then", "see (a|d then"),
    ] {
        let (state, _) = editor(source);
        assert_eq!(shown(&typed(&state, "(a")), shown_after, "{source}");
    }
}

#[test]
fn typing_the_closer_steps_over_the_one_auto_pairing_wrote() {
    let (state, _) = editor("");
    let state = typed(&state, "(ab)");
    assert_eq!(shown(&state), "(ab)|");
    // Stepped over once, it is text: the next `)` is a character of its own.
    assert_eq!(shown(&typed(&state, ")")), "(ab))|");
    // Nested pairs step out one at a time.
    let (state, _) = editor("");
    assert_eq!(shown(&typed(&state, "([a])")), "([a])|");
    let (state, _) = editor("");
    assert_eq!(shown(&typed(&state, "\"hi\" ")), "\"hi\" |");
    // A closer auto-pairing did not write is typed as usual.
    let (state, _) = editor("a|)");
    assert_eq!(shown(&typed(&state, ")")), "a)|)");
}

#[test]
fn backspace_in_an_empty_pair_deletes_both() {
    let (state, _) = editor("x|");
    let state = typed(&state, "(");
    assert_eq!(shown(&backspace(&state)), "x|");
    // Once something was typed in it and taken out again, it is still empty.
    let state = backspace(&typed(&state, "a"));
    assert_eq!(shown(&state), "x(|)");
    assert_eq!(shown(&backspace(&state)), "x|");
    // A pair auto-pairing did not write is two characters.
    let (state, _) = editor("x(|)");
    assert_eq!(shown(&backspace(&state)), "x|)");
    // Nor does Backspace before content take the closer after it.
    let (state, _) = editor("");
    let state = typed(&state, "(ab");
    let state = apply(
        &state,
        vec![TransactionSpec::new().selection(Selection::cursor(pos_of(&state, 1)))],
    );
    assert_eq!(shown(&backspace(&state)), "|ab)");
}

#[test]
fn an_opener_over_a_selection_wraps_it() {
    for (opener, wrapped) in [
        ("(", "a (‹bc›) d"),
        ("\"", "a \"‹bc›\" d"),
        ("[", "a [‹bc›] d"),
        ("「", "a 「‹bc›」 d"),
    ] {
        let (state, _) = editor("a ‹bc› d");
        assert_eq!(shown(&typed(&state, opener)), wrapped, "{opener}");
    }
    // Any other character replaces the selection.
    let (state, _) = editor("a ‹bc› d");
    assert_eq!(shown(&typed(&state, "x")), "a x| d");
}

#[test]
fn quotes_pair_only_after_something_that_is_not_a_word() {
    let (state, _) = editor("5|");
    assert_eq!(shown(&typed(&state, "\"")), "5\"|");
    let (state, _) = editor("say | now");
    assert_eq!(shown(&typed(&state, "\"")), "say \"|\" now");
    // The apostrophe never pairs.
    let (state, _) = editor("");
    assert_eq!(shown(&typed(&state, "'")), "'|");
}

#[test]
fn an_opener_before_a_word_or_after_a_backslash_does_not_pair() {
    let (state, _) = editor("|word");
    assert_eq!(shown(&typed(&state, "(")), "(|word");
    let (state, _) = editor("\\|");
    assert_eq!(shown(&typed(&state, "(")), "\\(|");
}

#[test]
fn markdown_delimiters_do_not_pair() {
    for delimiter in ["`", "*", "_", "~"] {
        let (state, _) = editor("");
        assert_eq!(
            shown(&typed(&state, delimiter)),
            format!("{delimiter}|"),
            "{delimiter}"
        );
    }
}

#[test]
fn nothing_pairs_in_code() {
    let (state, _) = editor("`a|`");
    assert_eq!(shown(&typed(&state, "(")), "`a(|`");
    let (state, _) = editor("a `‹bc›` d");
    assert_eq!(shown(&typed(&state, "(")), "a `(|` d");
    let (state, _) = editor("```\n```");
    let state = typed(&state, "(");
    assert_eq!(to_markdown(state.schema(), state.doc()), "```\n(\n```");
}

#[test]
fn switched_off_it_is_plain_typing() {
    let (state, enabled) = editor("");
    enabled.store(false, Ordering::Relaxed);
    assert_eq!(shown(&typed(&state, "(\"[")), "(\"[|");
    let (state, enabled) = editor("a ‹bc› d");
    enabled.store(false, Ordering::Relaxed);
    assert_eq!(shown(&typed(&state, "(")), "a (| d");
    // A closer written while it was on is left alone once it is off.
    let (state, enabled) = editor("");
    let state = typed(&state, "(");
    enabled.store(false, Ordering::Relaxed);
    assert_eq!(shown(&typed(&state, ")")), "()|)");
    assert_eq!(shown(&backspace(&state)), "|)");
}

#[test]
fn one_undo_takes_the_opener_back_with_its_closer() {
    let (state, _) = editor("x|");
    let state = typed(&state, "(");
    let undone = apply(&state, vec![undo(&state).expect("something to undo")]);
    assert_eq!(shown(&undone), "x|");
}

#[test]
fn a_composition_is_left_alone() {
    let (state, _) = editor("");
    let head = state.selection().head(state.doc());
    let started = apply(
        &state,
        vec![start_composition(CompositionRange::new(head, head))],
    );
    let marked = apply(
        &started,
        vec![update_composition(&started, "（", 1).expect("a composition update")],
    );
    assert_eq!(shown(&marked), "（|");
    let refined = apply(
        &marked,
        vec![update_composition(&marked, "（a", 2).expect("a composition update")],
    );
    assert_eq!(shown(&refined), "（a|");
}

/// A second `[` typed into the empty pair the first wrote takes its `]`
/// back: `[[` opens a wiki link, and the menu a host opens on it replaces the
/// `[[query` before the caret with the link it chose.
#[test]
fn a_wiki_link_opens_as_typed_for_its_menu() {
    for (typed_text, expected) in [("[[", "see [[| now"), ("【【", "see 【【| now")] {
        let (state, _) = editor("see | now");
        assert_eq!(shown(&typed(&state, typed_text)), expected);
    }
    // What the menu does on accepting: the trigger run goes, the link goes in.
    let (state, _) = editor("see | now");
    let state = typed(&state, "[[No");
    assert_eq!(shown(&state), "see [[No| now");
    let trigger = pos_of(&state, 4);
    let caret = state.selection().head(state.doc());
    let deleted = delete_range(trigger, caret)(&state).expect("a deletion");
    let base = apply(&state, vec![deleted.clone()]);
    let schema = state.schema().clone();
    let link = from_markdown_fragment(&schema, "[[Note]]").expect("a fragment");
    let chosen = replace_selection(link)(&base).expect("the link goes in");
    let accepted = apply(&state, vec![deleted, chosen.sequential()]);
    assert_eq!(
        to_markdown(accepted.schema(), accepted.doc()),
        "see [[Note]] now"
    );
    // Typed out by hand, it comes out as typed.
    let (state, _) = editor("see | now");
    let state = typed(&state, "[[Note]]");
    assert_eq!(to_markdown(state.schema(), state.doc()), "see [[Note]] now");
    // A `[` anywhere else pairs as usual.
    let (state, _) = editor("");
    assert_eq!(shown(&typed(&state, "[a[")), "[a[|]]");
}

/// Pairing `[` does not hide the typed `[` from the input rules that open
/// with it.
#[test]
fn a_footnote_and_a_task_type_through() {
    let (state, _) = editor("");
    let state = typed(&state, "[^1]: note");
    assert_eq!(to_markdown(state.schema(), state.doc()), "[^1]: note");
    let (state, _) = editor("- ");
    let state = typed(&state, "[ ] task");
    assert_eq!(to_markdown(state.schema(), state.doc()), "- [ ] task");
}

#[test]
fn smart_quote_mode_has_no_automatic_straight_closer_to_leave_behind() {
    fn replace_character(state: &EditorState, position: usize, text: &str) -> EditorState {
        let slice = markraft_core::Slice::from_fragment(markraft_core::Fragment::from_node(
            state.schema().text(text),
        ));
        apply(
            state,
            vec![
                TransactionSpec::new()
                    .changes([markraft_core::Change::replace(
                        position,
                        position + 1,
                        slice,
                    )])
                    .user_event(markraft_core::protocol::event::INPUT_REPLACE),
            ],
        )
    }
    let (state, _, quotes) = editor_with_quotes("", false);
    let state = typed(&state, "\"");
    assert_eq!(shown(&state), "\"|");
    let state = replace_character(&state, 1, "“");
    let state = typed(&state, "hello\"");
    let state = replace_character(&state, 7, "”");
    assert_eq!(shown(&state), "“hello”|");
    // Turning smart quotes off restores the original pairing immediately.
    quotes.store(true, Ordering::Relaxed);
    assert_eq!(shown(&typed(&state, " \"")), "“hello” \"|\"");
}

#[test]
fn suppressing_quote_pairs_keeps_brackets_code_and_existing_closers_working() {
    let (state, _, _) = editor_with_quotes("", false);
    assert_eq!(shown(&typed(&state, "(hi)")), "(hi)|");
    let (state, _, _) = editor_with_quotes("‹hi›", false);
    assert_eq!(shown(&typed(&state, "\"")), "\"|");
    let (state, _, quotes) = editor_with_quotes("", true);
    let state = typed(&state, "\"hello");
    quotes.store(false, Ordering::Relaxed);
    assert_eq!(shown(&typed(&state, "\"")), "\"hello\"|");
    for enabled in [false, true] {
        let (state, _, _) = editor_with_quotes("`code |`", enabled);
        assert_eq!(shown(&typed(&state, "\"")), "`code \"|`");
    }
}
