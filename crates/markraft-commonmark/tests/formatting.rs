//! Formatting commands, which edit the source a style is spelled in.
//!
//! Every command is judged by the file it leaves behind — which a reader must
//! read as the formatting asked for — by what stays selected, and by one undo
//! taking it back.

mod common;

use common::{Codec, html};
use markraft_commonmark::schema as md;
use markraft_commonmark::{
    CommandRefusal, FormatCommand, Inexpressible, clear_formatting, commonmark_extensions,
    set_link, split_block_keeping_styles, to_markdown, toggle_style, unlink,
};
use markraft_core::commands::{insert_text, run_command};
use markraft_core::{
    EditorState, EditorStateConfig, Extension, HistoryConfig, Node, Selection, history, undo,
    undo_depth,
};

/// The document position of source character `offset` in the first block.
fn at(offset: usize) -> usize {
    1 + offset
}

fn editor(codec: &Codec, source: &str, selection: Selection) -> EditorState {
    EditorState::create(
        EditorStateConfig::new(codec.schema.clone())
            .doc(codec.parse(source))
            .selection(selection)
            .extensions(Extension::all([
                commonmark_extensions(&codec.schema),
                history(HistoryConfig::default()),
            ])),
    )
    .expect("a valid editor state")
}

/// `source` with the characters `from..to` of its first block selected.
fn selecting(codec: &Codec, source: &str, from: usize, to: usize) -> EditorState {
    editor(codec, source, Selection::text(at(from), at(to)))
}

fn formatted(state: &EditorState, command: &FormatCommand) -> EditorState {
    let spec = command(state)
        .expect("the command is not refused")
        .expect("the command applies");
    state
        .update([spec])
        .expect("the edit applies")
        .state()
        .clone()
}

fn toggle(codec: &Codec, mark: &str) -> FormatCommand {
    toggle_style(codec.schema.mark_id(mark).expect("the mark type"))
}

fn written(codec: &Codec, state: &EditorState) -> String {
    to_markdown(&codec.schema, state.doc())
}

/// The file is exactly `expected`, and reads as it does.
#[track_caller]
fn assert_saves(codec: &Codec, state: &EditorState, expected: &str) {
    let actual = written(codec, state);
    assert_eq!(
        actual,
        expected,
        "the tree is {}",
        codec.describe(state.doc())
    );
    assert_eq!(html(&actual), html(expected));
}

/// What the selection covers, as the text a reader sees.
fn selected_text(codec: &Codec, state: &EditorState) -> String {
    let doc = state.doc();
    let (from, to) = (state.selection().from(doc), state.selection().to(doc));
    let slice = doc.slice(from, to).expect("a slice");
    markraft_commonmark::slice_to_plain_text(&codec.schema, &slice)
}

/// One undo gives back exactly the document the command started from.
#[track_caller]
fn assert_one_undo(before: &EditorState, after: &EditorState) {
    assert_eq!(undo_depth(after), undo_depth(before) + 1);
    let spec = undo(after).expect("something to undo");
    let back = after.update([spec]).expect("undo applies").state().clone();
    assert_eq!(back.doc(), before.doc());
}

fn block_source(state: &EditorState, index: usize) -> String {
    let block: &Node = state.doc().child(index);
    block
        .children()
        .map(|child| child.text().unwrap_or("\u{fffc}").to_string())
        .collect()
}

// -- toggling a style ---------------------------------------------------------

#[test]
fn toggling_strong_on_a_word_and_off_again() {
    let codec = Codec::new();
    let state = selecting(&codec, "a b c", 2, 3);
    let bold = formatted(&state, &toggle(&codec, md::STRONG));
    assert_saves(&codec, &bold, "a **b** c");
    assert_eq!(selected_text(&codec, &bold), "b");
    assert_one_undo(&state, &bold);
    let plain = formatted(&bold, &toggle(&codec, md::STRONG));
    assert_saves(&codec, &plain, "a b c");
    assert_eq!(selected_text(&codec, &plain), "b");
    assert_one_undo(&bold, &plain);
}

#[test]
fn every_delimited_style_toggles() {
    let codec = Codec::new();
    for (mark, expected) in [
        (md::EM, "a *b* c"),
        (md::STRIKETHROUGH, "a ~~b~~ c"),
        (md::CODE, "a `b` c"),
    ] {
        let state = selecting(&codec, "a b c", 2, 3);
        let on = formatted(&state, &toggle(&codec, mark));
        assert_saves(&codec, &on, expected);
        let off = formatted(&on, &toggle(&codec, mark));
        assert_saves(&codec, &off, "a b c");
    }
}

/// A style inside another keeps the outer span as it was written.
#[test]
fn a_style_nests_inside_another() {
    let codec = Codec::new();
    let state = selecting(&codec, "*a b c*", 3, 4);
    let nested = formatted(&state, &toggle(&codec, md::STRONG));
    assert_saves(&codec, &nested, "*a **b** c*");
    assert_eq!(selected_text(&codec, &nested), "b");
    assert_one_undo(&state, &nested);
}

/// Taking the inner style off leaves the outer one; taking the outer one
/// off, over everything it covers, leaves the inner one.
#[test]
fn nested_styles_come_off_one_at_a_time() {
    let codec = Codec::new();
    // `*a **b** c*`: the b sits at 5.
    let inner = formatted(
        &selecting(&codec, "*a **b** c*", 5, 6),
        &toggle(&codec, md::STRONG),
    );
    assert_saves(&codec, &inner, "*a b c*");
    let outer = formatted(
        &selecting(&codec, "*a **b** c*", 1, 10),
        &toggle(&codec, md::EM),
    );
    assert_saves(&codec, &outer, "a **b** c");
    assert_eq!(selected_text(&codec, &outer), "a b c");
}

/// Emphasis taken off the middle of a span leaves two spans.
#[test]
fn toggling_off_the_middle_of_a_span_splits_it() {
    let codec = Codec::new();
    let state = selecting(&codec, "**abc**", 3, 4);
    let split = formatted(&state, &toggle(&codec, md::STRONG));
    assert_saves(&codec, &split, "**a**b**c**");
    assert_eq!(selected_text(&codec, &split), "b");
}

/// A selection over two blocks styles each, as one edit.
#[test]
fn toggling_across_blocks_styles_each() {
    let codec = Codec::new();
    let state = editor(&codec, "one\n\ntwo", Selection::text(at(0), 9));
    let bold = formatted(&state, &toggle(&codec, md::STRONG));
    assert_saves(&codec, &bold, "**one**\n\n**two**");
    assert_eq!(selected_text(&codec, &bold), "one\ntwo");
    assert_one_undo(&state, &bold);
}

/// Code content is literal, so a backtick inside it needs a longer fence, and
/// the fence is what a reader sees as one code span.
#[test]
fn code_around_a_backtick_takes_a_longer_fence() {
    let codec = Codec::new();
    let state = selecting(&codec, "x a`b y", 2, 5);
    let code = formatted(&state, &toggle(&codec, md::CODE));
    assert_saves(&codec, &code, "x ``a`b`` y");
    assert_eq!(selected_text(&codec, &code), "a`b");
    let plain = formatted(&code, &toggle(&codec, md::CODE));
    assert_eq!(selected_text(&codec, &plain), "a`b");
    assert_eq!(html(&written(&codec, &plain)), html("x a\\`b y"));
}

/// Strong over part of a code span keeps the code whole around it.
#[test]
fn strong_over_part_of_a_code_span() {
    let codec = Codec::new();
    let state = selecting(&codec, "`ab`", 2, 3);
    let bold = formatted(&state, &toggle(&codec, md::STRONG));
    let file = written(&codec, &bold);
    assert_eq!(html(&file), html("`a`**`b`**"), "wrote {file:?}");
    assert_eq!(selected_text(&codec, &bold), "b");
}

/// Literal characters stay literal: an escaped `*` that ends up inside a new
/// span is still an escaped `*`.
#[test]
fn escapes_inside_a_new_span_stay_escaped() {
    let codec = Codec::new();
    let state = selecting(&codec, r"a \*b\* c", 2, 7);
    let bold = formatted(&state, &toggle(&codec, md::STRONG));
    assert_saves(&codec, &bold, r"a **\*b\*** c");
    assert_eq!(selected_text(&codec, &bold), "*b*");
}

/// `**"a"**b` is not strong in CommonMark: the closing `**` has punctuation
/// before it and a letter after it. The delimiters move inward past the
/// quotes instead, which stay plain and stay selected.
#[test]
fn delimiters_a_reader_would_not_take_move_inside_the_punctuation() {
    let codec = Codec::new();
    let state = selecting(&codec, "\"a\"b", 0, 3);
    let bold = formatted(&state, &toggle(&codec, md::STRONG));
    assert_saves(&codec, &bold, "\"**a**\"b");
    assert_eq!(selected_text(&codec, &bold), "\"a\"");
    assert_one_undo(&state, &bold);
}

/// A selection of nothing but punctuation between two letters has nothing
/// left once its punctuation is set aside: `a**"**b` is not strong, and the
/// command refuses, leaving the document alone.
#[test]
fn a_style_markdown_cannot_spell_here_is_refused() {
    let codec = Codec::new();
    let state = selecting(&codec, "a\"b", 1, 2);
    let refusal = toggle(&codec, md::STRONG)(&state).expect_err("the toggle is refused");
    assert_eq!(
        refusal,
        CommandRefusal::NotExpressible {
            reason: Inexpressible::Delimiters { mark: md::STRONG }
        }
    );
    assert!(refusal.to_string().contains("strong"), "{refusal}");
}

/// A cursor toggle writes an empty pair, and typing lands inside it.
#[test]
fn a_cursor_toggle_leaves_a_pair_to_type_into() {
    let codec = Codec::new();
    let state = editor(&codec, "ab", Selection::cursor(at(1)));
    let paired = formatted(&state, &toggle(&codec, md::EM));
    assert_eq!(block_source(&paired, 0), "a**b");
    let typed = run_command(&paired, &insert_text("X"))
        .expect("typing runs")
        .expect("typing applies")
        .state()
        .clone();
    assert_saves(&codec, &typed, "a*X*b");
}

// -- links ----------------------------------------------------------------------

/// The href and title of the link in the first block of `state`'s file, as a
/// reader reads them back.
fn link_read_back(codec: &Codec, state: &EditorState) -> Option<(String, String)> {
    let file = written(codec, state);
    let doc = codec.parse(&file);
    let link = codec.schema.mark_id(md::LINK)?;
    let mut found = None;
    doc.nodes_between(0, doc.content_size(), &mut |node, _, _, _| {
        if let Some(mark) = node.marks().get(link) {
            let attr = |name: &str| {
                mark.attrs
                    .get(name)
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_string()
            };
            found.get_or_insert((attr("href"), attr("title")));
        }
        true
    });
    found
}

#[test]
fn linking_a_selection_spells_the_destination_and_title() {
    let codec = Codec::new();
    for (href, title) in [
        ("https://example.com", ""),
        ("https://e.com/a)b", ""),
        ("https://e.com/a(b", "a \"quoted\" title"),
        ("https://e.com/?q=\"x\"&y=1", "it's (fine)"),
    ] {
        let state = selecting(&codec, "see word here", 4, 8);
        let linked = formatted(&state, &set_link(href, title));
        assert_eq!(
            link_read_back(&codec, &linked),
            Some((href.to_string(), title.to_string())),
            "wrote {:?}",
            written(&codec, &linked)
        );
        assert_eq!(selected_text(&codec, &linked), "word");
        assert_one_undo(&state, &linked);
    }
}

/// A caret or a selection inside a link changes that link's destination and
/// keeps its text.
#[test]
fn a_caret_in_a_link_changes_its_destination() {
    let codec = Codec::new();
    let state = editor(&codec, "a [b **c**](u) d", Selection::cursor(at(4)));
    let changed = formatted(&state, &set_link("v", ""));
    assert_saves(&codec, &changed, "a [b **c**](v) d");
    assert_one_undo(&state, &changed);
    let state = selecting(&codec, "a [b **c**](u) d", 3, 4);
    assert_saves(
        &codec,
        &formatted(&state, &set_link("v", "")),
        "a [b **c**](v) d",
    );
}

#[test]
fn a_caret_outside_any_link_inserts_the_url_as_a_link() {
    let codec = Codec::new();
    let state = editor(&codec, "see x", Selection::cursor(at(4)));
    let inserted = formatted(&state, &set_link("https://example.com", ""));
    assert_eq!(
        link_read_back(&codec, &inserted).map(|(href, _)| href),
        Some("https://example.com".to_string())
    );
    // The URL runs straight into the `x`, so a bare one would take it in.
    let link = "[https://example.com](https://example.com)";
    assert_saves(&codec, &inserted, &format!("see {link}x"));
    // The caret lands after the whole link, where typing is not linked.
    assert_eq!(inserted.selection(), &Selection::cursor(at(4 + link.len())));
    // Between spaces the URL stands bare.
    let state = editor(&codec, "see  x", Selection::cursor(at(4)));
    let bare = formatted(&state, &set_link("https://example.com", ""));
    assert_saves(&codec, &bare, "see https://example.com x");
}

#[test]
fn unlinking_keeps_the_text_and_its_other_styles() {
    let codec = Codec::new();
    let state = editor(&codec, "a [b **c**](u) d", Selection::cursor(at(4)));
    let unlinked = formatted(&state, &unlink());
    assert_saves(&codec, &unlinked, "a b **c** d");
    assert_one_undo(&state, &unlinked);
    // A selection inside a link unlinks all of it.
    let state = selecting(&codec, "[abc](u)", 2, 3);
    assert_saves(&codec, &formatted(&state, &unlink()), "abc");
    // One across its edge unlinks what it covers.
    let state = selecting(&codec, "[abc](u) d", 3, 10);
    assert_saves(&codec, &formatted(&state, &unlink()), "[ab](u)c d");
}

#[test]
fn a_reference_link_is_a_link_to_the_commands() {
    let codec = Codec::new();
    // Unlinking one leaves its text and the definition.
    let state = editor(&codec, "a [b][r] c\n\n[r]: /u", Selection::cursor(at(4)));
    let unlinked = formatted(&state, &unlink());
    assert_saves(&codec, &unlinked, "a b c\n\n[r]: /u");
    // Bold inside one keeps it the reference it is.
    let state = selecting(&codec, "[bc][r] d\n\n[r]: /u", 1, 3);
    let bold = formatted(&state, &toggle(&codec, md::STRONG));
    assert_saves(&codec, &bold, "[**bc**][r] d\n\n[r]: /u");
}

// -- clearing -------------------------------------------------------------------

#[test]
fn clearing_takes_every_style_off() {
    let codec = Codec::new();
    let source = "**a** *b* [c](u) `d` ~~e~~";
    let state = selecting(&codec, source, 0, source.chars().count());
    let cleared = formatted(&state, &clear_formatting());
    assert_saves(&codec, &cleared, "a b c d e");
    assert_eq!(selected_text(&codec, &cleared), "a b c d e");
    assert_one_undo(&state, &cleared);
}

// -- Enter ------------------------------------------------------------------------

fn entered(state: &EditorState) -> EditorState {
    run_command(state, &split_block_keeping_styles())
        .expect("Enter runs")
        .expect("Enter applies")
        .state()
        .clone()
}

fn type_x(state: &EditorState) -> EditorState {
    run_command(state, &insert_text("X"))
        .expect("typing runs")
        .expect("typing applies")
        .state()
        .clone()
}

/// Enter inside a span closes it before the cut and opens it again after, and
/// typing carries on inside the reopened span.
#[test]
fn enter_inside_a_span_keeps_both_halves_styled() {
    let codec = Codec::new();
    let state = editor(&codec, "**abcd**", Selection::cursor(at(4)));
    let split = entered(&state);
    assert_saves(&codec, &split, "**ab**\n\n**cd**");
    assert_saves(&codec, &type_x(&split), "**ab**\n\n**Xcd**");
    assert_one_undo(&state, &split);
}

#[test]
fn enter_inside_nested_spans_closes_each() {
    let codec = Codec::new();
    // `*a **b|c** d*`
    let state = editor(&codec, "*a **bc** d*", Selection::cursor(at(6)));
    assert_saves(&codec, &entered(&state), "*a **b***\n\n***c** d*");
}

#[test]
fn enter_inside_a_link_keeps_both_halves_linked() {
    let codec = Codec::new();
    let state = editor(&codec, "[abcd](u \"t\")", Selection::cursor(at(3)));
    assert_saves(&codec, &entered(&state), "[ab](u \"t\")\n\n[cd](u \"t\")");
}

/// A cut beside a space closes the span before the space, where a reader
/// takes the delimiter for a closing one. The space would end the first
/// block, where a reader does not see it, and the caret is no longer there
/// to keep it: it goes, so the tree holds what a reload reads.
#[test]
fn enter_after_a_space_closes_the_span_before_it() {
    let codec = Codec::new();
    let state = editor(&codec, "**ab cd**", Selection::cursor(at(5)));
    let split = entered(&state);
    assert_eq!(block_source(&split, 0), "**ab**");
    assert_eq!(block_source(&split, 1), "**cd**");
    assert_saves(&codec, &split, "**ab**\n\n**cd**");
    assert_eq!(codec.parse(&written(&codec, &split)), *split.doc());
    assert_one_undo(&state, &split);
}

/// Right after an opening delimiter there is nothing to close: the whole span
/// moves to the new block.
#[test]
fn enter_at_the_start_of_a_span_moves_it_whole() {
    let codec = Codec::new();
    let state = editor(&codec, "a **bc**", Selection::cursor(at(4)));
    let split = entered(&state);
    assert_eq!(block_source(&split, 0), "a");
    assert_eq!(block_source(&split, 1), "**bc**");
}

/// Outside any style Enter is a plain split.
#[test]
fn enter_outside_a_style_is_a_plain_split() {
    let codec = Codec::new();
    let state = editor(&codec, "ab **c**", Selection::cursor(at(1)));
    assert_saves(&codec, &entered(&state), "a\n\nb **c**");
}
