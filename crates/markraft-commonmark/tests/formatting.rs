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
    keeping_styles, set_link, split_block_keeping_styles, to_markdown, toggle_style, unlink,
};
use markraft_core::commands::{Direction, delete_by_grapheme, insert_text, run_command};
use markraft_core::{
    EditorState, EditorStateConfig, Extension, Node, Selection, TransactionSpec,
    history::{HistoryConfig, history, redo, undo, undo_depth},
    protocol::add_to_history,
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
        (md::HIGHLIGHT, "a ==b== c"),
        (md::SUPERSCRIPT, "a ^b^ c"),
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

/// A formula is literal like a code span: strong over part of one keeps its
/// TeX whole, and strong over all of it wraps the fences.
#[test]
fn strong_over_a_formula_keeps_it_whole() {
    let codec = Codec::new();
    let state = selecting(&codec, "$ab$", 2, 3);
    let bold = formatted(&state, &toggle(&codec, md::STRONG));
    let file = written(&codec, &bold);
    assert_eq!(html(&file), html("$a$**$b$**"), "wrote {file:?}");
    let state = selecting(&codec, "x $$a^2$$ y", 0, 11);
    let bold = formatted(&state, &toggle(&codec, md::STRONG));
    assert_saves(&codec, &bold, "**x $$a^2$$ y**");
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

// -- a cursor -------------------------------------------------------------------

fn caret(state: &EditorState) -> usize {
    state.selection().head(state.doc())
}

/// The caret put at `pos`, the way an arrow key or a click puts it.
fn moved(state: &EditorState, pos: usize) -> EditorState {
    state
        .update([TransactionSpec::new()
            .selection(Selection::cursor(pos))
            .user_event("select")])
        .expect("the move applies")
        .state()
        .clone()
}

fn typed(state: &EditorState, text: &str) -> EditorState {
    run_command(state, &insert_text(text))
        .expect("typing runs")
        .expect("typing applies")
        .state()
        .clone()
}

fn toggled(codec: &Codec, state: &EditorState, mark: &str) -> EditorState {
    formatted(state, &toggle(codec, mark))
}

fn undone(state: &EditorState) -> EditorState {
    let spec = undo(state).expect("something to undo");
    state.update([spec]).expect("undo applies").state().clone()
}

/// The paragraph the device report started from, after one above it, with
/// the caret at its end.
fn two_paragraphs(codec: &Codec) -> EditorState {
    editor(codec, "one\n\nPara four echo.", Selection::cursor(21))
}

/// ⌘B at a caret, then ↑ into the paragraph above: the empty pair goes, as it
/// does for a click back into the same paragraph or → into the pair's second
/// half.
#[test]
fn an_empty_pair_goes_when_the_caret_leaves_it() {
    let codec = Codec::new();
    let state = two_paragraphs(&codec);
    let paired = toggled(&codec, &state, md::STRONG);
    assert_eq!(block_source(&paired, 1), "Para four echo.****");
    assert_eq!(caret(&paired), 23);

    let up = moved(&paired, 2);
    assert_saves(&codec, &up, "one\n\nPara four echo.");
    assert_eq!(up.doc(), state.doc());
    assert_eq!(caret(&up), 2);

    let clicked = moved(&paired, 10);
    assert_eq!(clicked.doc(), state.doc());
    assert_eq!(caret(&clicked), 10);

    let right = moved(&paired, 24);
    assert_eq!(right.doc(), state.doc());
    assert_eq!(caret(&right), 21);
}

/// In an empty paragraph the pair is the whole line — a thematic break to a
/// reader, which the canonicalising correction would guard with a backslash
/// once the caret left. It goes before that correction sees it.
#[test]
fn an_empty_pair_alone_on_its_line_goes_too() {
    let codec = Codec::new();
    let state = editor(&codec, "one", Selection::cursor(at(3)));
    let state = run_command(&state, &markraft_core::commands::split_block())
        .expect("Enter runs")
        .expect("Enter applies")
        .state()
        .clone();
    let paired = toggled(&codec, &state, md::STRONG);
    assert_eq!(block_source(&paired, 1), "****");
    let up = moved(&paired, 2);
    assert_eq!(up.doc(), state.doc());
    assert_saves(&codec, &up, "one");
}

/// The deletion joins the toggle's undo step: one undo after leaving the pair
/// gives back the document and the caret from before the toggle.
#[test]
fn undo_after_leaving_an_empty_pair_takes_back_the_toggle() {
    let codec = Codec::new();
    let state = two_paragraphs(&codec);
    let paired = toggled(&codec, &state, md::STRONG);
    let up = moved(&paired, 2);
    assert_eq!(undo_depth(&up), undo_depth(&state) + 1);
    let back = undone(&up);
    assert_eq!(back.doc(), state.doc());
    assert_eq!(caret(&back), 21);
}

/// A second toggle in the empty pair takes it off again.
#[test]
fn toggling_twice_at_a_caret_writes_nothing() {
    let codec = Codec::new();
    let state = two_paragraphs(&codec);
    let paired = toggled(&codec, &state, md::STRONG);
    let unpaired = toggled(&codec, &paired, md::STRONG);
    assert_eq!(unpaired.doc(), state.doc());
    assert_eq!(caret(&unpaired), 21);
    assert_eq!(moved(&unpaired, 2).doc(), state.doc());
}

/// What is typed in the pair makes it a span, which stays when the caret
/// leaves.
#[test]
fn typing_in_the_pair_keeps_it() {
    let codec = Codec::new();
    let state = two_paragraphs(&codec);
    let written = typed(&toggled(&codec, &state, md::STRONG), "7");
    let up = moved(&written, 2);
    assert_saves(&codec, &up, "one\n\nPara four echo.**7**");
}

/// The sequence from the device report — ⌘B, `7`, ⌘B, `8`, ↑ — writes a bold
/// `7` and a plain `8`, and no pair after them.
#[test]
fn a_second_toggle_after_typing_steps_out_of_the_span() {
    let codec = Codec::new();
    let state = two_paragraphs(&codec);
    let seven = typed(&toggled(&codec, &state, md::STRONG), "7");
    let out = toggled(&codec, &seven, md::STRONG);
    assert_eq!(out.doc(), seven.doc(), "stepping out writes nothing");
    let eight = typed(&out, "8");
    let up = moved(&eight, 2);
    assert_saves(&codec, &up, "one\n\nPara four echo.**7**8");
}

/// Undoing what was typed in the pair gives the empty pair back, and it still
/// goes when the caret leaves.
#[test]
fn undoing_the_typing_gives_back_a_pair_that_still_goes() {
    let codec = Codec::new();
    let state = two_paragraphs(&codec);
    let paired = toggled(&codec, &state, md::STRONG);
    let written = typed(&paired, "7");
    let back = undone(&written);
    assert_eq!(back.doc(), paired.doc());
    assert_eq!(caret(&back), 23);
    let up = moved(&back, 2);
    assert_eq!(up.doc(), state.doc());
}

/// Backspace in an empty pair deletes all of it, not just the `*` before the
/// caret.
#[test]
fn backspace_in_an_empty_pair_deletes_it() {
    let codec = Codec::new();
    let state = two_paragraphs(&codec);
    let paired = toggled(&codec, &state, md::STRONG);
    let deleted = run_command(&paired, &delete_by_grapheme(Direction::Backward))
        .expect("Backspace runs")
        .expect("Backspace applies")
        .state()
        .clone();
    assert_eq!(deleted.doc(), state.doc());
    assert_eq!(caret(&deleted), 21);
}

fn redone(state: &EditorState) -> EditorState {
    let spec = redo(state).expect("something to redo");
    state.update([spec]).expect("redo applies").state().clone()
}

fn backspaced(state: &EditorState) -> EditorState {
    run_command(state, &delete_by_grapheme(Direction::Backward))
        .expect("Backspace runs")
        .expect("Backspace applies")
        .state()
        .clone()
}

/// Undoing the Backspace that deleted an empty pair gives the pair back
/// pending: leaving it deletes it, and the cycle repeats.
#[test]
fn undoing_backspace_in_an_empty_pair_gives_back_a_pair_that_still_goes() {
    let codec = Codec::new();
    let state = two_paragraphs(&codec);
    let paired = toggled(&codec, &state, md::STRONG);
    let back = undone(&backspaced(&paired));
    assert_eq!(back.doc(), paired.doc());
    assert_eq!(caret(&back), 23);
    assert_eq!(moved(&back, 2).doc(), state.doc());

    let again = undone(&backspaced(&back));
    assert_eq!(again.doc(), paired.doc());
    assert_eq!(caret(&again), 23);
    let up = moved(&again, 2);
    assert_eq!(up.doc(), state.doc());
    assert_eq!(caret(&up), 2);
}

/// Undoing the Enter that broke an empty pair apart gives the pair back
/// pending, and a redo of that Enter splits without it again.
#[test]
fn undoing_enter_in_an_empty_pair_gives_back_a_pair_that_still_goes() {
    let codec = Codec::new();
    let state = editor(&codec, "ab", Selection::cursor(at(1)));
    let paired = toggled(&codec, &state, md::STRONG);
    let split = entered(&paired);
    let back = undone(&split);
    assert_eq!(back.doc(), paired.doc());
    assert_eq!(caret(&back), caret(&paired));
    let up = moved(&back, at(0));
    assert_eq!(up.doc(), state.doc());
    assert_saves(&codec, &up, "ab");

    assert_eq!(redone(&back).doc(), split.doc());
}

/// Undo steps past an edit made after the split still find the pair.
#[test]
fn undoing_past_typing_after_enter_gives_back_the_pair() {
    let codec = Codec::new();
    let state = editor(&codec, "ab", Selection::cursor(at(1)));
    let paired = toggled(&codec, &state, md::STRONG);
    let written = typed(&entered(&paired), "X");
    let back = undone(&undone(&written));
    assert_eq!(back.doc(), paired.doc());
    assert_eq!(moved(&back, at(0)).doc(), state.doc());
}

/// Undoing the typing in a pair gives it back still pending — tracking is
/// kept while the caret stays inside, so this needs no memory.
#[test]
fn undoing_typing_in_the_pair_then_leaving_deletes_it() {
    let codec = Codec::new();
    let state = two_paragraphs(&codec);
    let back = undone(&typed(&toggled(&codec, &state, md::STRONG), "7"));
    assert_eq!(block_source(&back, 1), "Para four echo.****");
    let up = moved(&back, 2);
    assert_eq!(up.doc(), state.doc());
}

/// Undoing the toggle itself leaves nothing pending, and what it remembers
/// comes back only on a redo: four asterisks typed by hand there stay.
#[test]
fn asterisks_typed_where_an_undone_pair_was_stay() {
    let codec = Codec::new();
    let state = two_paragraphs(&codec);
    let back = undone(&toggled(&codec, &state, md::STRONG));
    assert_eq!(back.doc(), state.doc());
    let asterisks = typed(&back, "****");
    let inside = moved(&asterisks, 23);
    let up = moved(&inside, 2);
    assert_eq!(block_source(&up, 1), "Para four echo.****");
}

/// Asterisks typed by hand where a deleted pair was never come back pending,
/// even when an undo leaves the caret between them at exactly those points.
#[test]
fn asterisks_typed_where_a_deleted_pair_was_stay() {
    let codec = Codec::new();
    let state = two_paragraphs(&codec);
    let deleted = backspaced(&toggled(&codec, &state, md::STRONG));
    let asterisks = typed(&deleted, "****");
    let written = typed(&moved(&asterisks, 23), "x");
    assert_eq!(block_source(&written, 1), "Para four echo.**x**");
    let back = undone(&written);
    assert_eq!(block_source(&back, 1), "Para four echo.****");
    assert_eq!(caret(&back), 23);
    let up = moved(&back, 2);
    assert_eq!(block_source(&up, 1), "Para four echo.****");
}

/// ⌘B, ⌘Z, ⌘⇧Z: the redo writes the pair back with the caret in it, and it
/// is pending again, so leaving the line leaves no asterisks behind.
#[test]
fn redoing_an_undone_toggle_gives_back_a_pair_that_still_goes() {
    let codec = Codec::new();
    let state = two_paragraphs(&codec);
    let paired = toggled(&codec, &state, md::STRONG);
    let again = redone(&undone(&paired));
    assert_eq!(again.doc(), paired.doc());
    assert_eq!(caret(&again), 23);
    let up = moved(&again, 2);
    assert_eq!(up.doc(), state.doc());
    assert_saves(&codec, &up, "one\n\nPara four echo.");

    // And the cycle repeats.
    let again = redone(&undone(&again));
    assert_eq!(moved(&again, 2).doc(), state.doc());
}

/// ⌘B, `7`, ⌘B, `8`, then undo all the way back and redo step by step: at
/// every step where the pair is empty with the caret in it, leaving the line
/// deletes it, and where it holds something it stays.
#[test]
fn redoing_through_a_toggle_and_typing_keeps_the_pair_pending() {
    let codec = Codec::new();
    let state = two_paragraphs(&codec);
    let eight = typed(
        &toggled(
            &codec,
            &typed(&toggled(&codec, &state, md::STRONG), "7"),
            md::STRONG,
        ),
        "8",
    );
    let mut back = eight;
    while undo(&back).is_some() {
        back = undone(&back);
    }
    assert_eq!(back.doc(), state.doc());
    let mut forth = back;
    let mut empty_pairs = 0;
    while redo(&forth).is_some() {
        forth = redone(&forth);
        let source = block_source(&forth, 1);
        let up = moved(&forth, 2);
        if source == "Para four echo.****" {
            assert_eq!(caret(&forth), 23);
            assert_eq!(up.doc(), state.doc(), "an empty pair goes");
            empty_pairs += 1;
        } else {
            assert_eq!(block_source(&up, 1), source, "a span stays");
        }
    }
    assert_saves(&codec, &forth, "one\n\nPara four echo.**7**8");
    assert_eq!(empty_pairs, 1);
}

/// ⌘B, ⌘Z, ⌘⇧Z, ↑: the file is as it was.
#[test]
fn leaving_a_redone_pair_leaves_a_clean_file() {
    let codec = Codec::new();
    let state = two_paragraphs(&codec);
    let again = redone(&undone(&toggled(&codec, &state, md::STRONG)));
    let up = moved(&again, 2);
    assert_saves(&codec, &up, "one\n\nPara four echo.");
}

/// ⌘B, ⌘Z, `5`, ⌘⇧Z: typing drops the redo, and nothing comes back pending.
#[test]
fn typing_after_undoing_a_toggle_never_rearms_it() {
    let codec = Codec::new();
    let state = two_paragraphs(&codec);
    let five = typed(&undone(&toggled(&codec, &state, md::STRONG)), "5");
    let after = match redo(&five) {
        Some(spec) => five.update([spec]).expect("redo applies").state().clone(),
        None => five.clone(),
    };
    assert_eq!(block_source(&after, 1), "Para four echo.5");
    let up = moved(&after, 2);
    assert_saves(&codec, &up, "one\n\nPara four echo.5");
    let stars = typed(&moved(&up, 22), "****");
    let inside = moved(&stars, 24);
    assert_eq!(
        block_source(&moved(&inside, 2), 1),
        "Para four echo.5****",
        "asterisks typed by hand stay"
    );
}

/// ⌘B, `ni `, then ↑ or a click: a reader takes no closing run after a
/// space, so the runs move inside the whitespace as the caret leaves, and
/// the file keeps a bold `ni`. One undo takes that back with the typing, and the pair it gives
/// back still goes.
#[test]
fn leaving_a_pair_that_ends_with_a_space_closes_it_before_the_space() {
    let codec = Codec::new();
    let state = two_paragraphs(&codec);
    let paired = toggled(&codec, &state, md::STRONG);
    let spaced = typed(&paired, "ni ");
    assert_eq!(block_source(&spaced, 1), "Para four echo.**ni **");
    // The space ends the line, where a reader does not see it, and it goes
    // with the caret as a trailing space always does.
    let up = moved(&spaced, 2);
    assert_saves(&codec, &up, "one\n\nPara four echo.**ni**");
    assert_eq!(caret(&up), 2);

    let clicked = moved(&spaced, 10);
    assert_eq!(block_source(&clicked, 1), "Para four echo.**ni** ");

    let back = undone(&up);
    assert_eq!(back.doc(), paired.doc());
    assert_eq!(caret(&back), 23);
    assert_eq!(moved(&back, 2).doc(), state.doc());
}

/// The same for leading whitespace, and with both.
#[test]
fn leaving_a_pair_that_starts_with_a_space_opens_it_after_the_space() {
    let codec = Codec::new();
    let state = editor(&codec, "ab", Selection::cursor(at(2)));
    let spaced = typed(&toggled(&codec, &state, md::STRONG), " c ");
    let left = moved(&spaced, at(0));
    assert_eq!(block_source(&left, 0), "ab **c** ");
}

/// Only whitespace between the runs: nothing reads, the runs go and the
/// space stays.
#[test]
fn leaving_a_pair_of_only_whitespace_deletes_its_runs() {
    let codec = Codec::new();
    let state = editor(&codec, "ab", Selection::cursor(at(2)));
    let spaced = typed(&toggled(&codec, &state, md::STRONG), " ");
    let left = moved(&spaced, at(0));
    assert_eq!(block_source(&left, 0), "ab ");
}

/// A pair that reads is left as it is, whitespace and all: a code span keeps
/// its spaces.
#[test]
fn leaving_a_code_pair_with_a_space_keeps_it() {
    let codec = Codec::new();
    let state = editor(&codec, "ab", Selection::cursor(at(2)));
    let spaced = typed(&toggled(&codec, &state, md::CODE), "c ");
    let left = moved(&spaced, at(0));
    assert_eq!(block_source(&left, 0), "ab`c `");
}

/// ⌘B, `ni `, ⌘B, `2`: the second toggle steps out past the closing run,
/// which moves before the space, and `2` is plain. Undo takes the typing and
/// the move back together.
#[test]
fn a_toggle_after_a_trailing_space_closes_the_pair_before_it() {
    let codec = Codec::new();
    let state = two_paragraphs(&codec);
    let spaced = typed(&toggled(&codec, &state, md::STRONG), "ni ");
    let out = toggled(&codec, &spaced, md::STRONG);
    assert_eq!(block_source(&out, 1), "Para four echo.**ni** ");
    assert_eq!(caret(&out), 28);
    let two = typed(&out, "2");
    assert_saves(&codec, &moved(&two, 2), "one\n\nPara four echo.**ni** 2");

    let back = undone(&out);
    assert_eq!(block_source(&back, 1), "Para four echo.****");
    assert_eq!(moved(&back, 2).doc(), state.doc());
}

/// A selection the history does not record — an undo restoring one — is not
/// the writer moving on, so the pair stays until the writer does.
#[test]
fn a_selection_the_history_does_not_record_settles_nothing() {
    let codec = Codec::new();
    let state = two_paragraphs(&codec);
    let paired = toggled(&codec, &state, md::STRONG);
    let restored = paired
        .update([TransactionSpec::new()
            .selection(Selection::cursor(2))
            .annotate(add_to_history().of(false))])
        .expect("the move applies")
        .state()
        .clone();
    assert_eq!(block_source(&restored, 1), "Para four echo.****");
    let typed_above = typed(&restored, "X");
    assert_saves(&codec, &typed_above, "oXne\n\nPara four echo.");
}

/// Every style a toggle can write has a pair to type into, which a second
/// toggle takes back and leaving deletes.
#[test]
fn every_delimited_style_pairs_at_a_caret() {
    let codec = Codec::new();
    for (mark, file) in [
        (md::STRONG, "ab**X**"),
        (md::EM, "ab*X*"),
        (md::STRIKETHROUGH, "ab~~X~~"),
        (md::CODE, "ab`X`"),
        (md::UNDERLINE, "ab<u>X</u>"),
    ] {
        let state = editor(&codec, "ab", Selection::cursor(at(2)));
        let paired = toggled(&codec, &state, mark);
        assert_saves(&codec, &typed(&paired, "X"), file);
        assert_eq!(toggled(&codec, &paired, mark).doc(), state.doc(), "{mark}");
        assert_eq!(moved(&paired, at(0)).doc(), state.doc(), "{mark}");
    }
}

/// At either edge of a span's content the toggle steps over its delimiter,
/// out of the span; right outside it, it steps back in.
#[test]
fn at_the_edge_of_a_span_the_toggle_steps_over_its_delimiter() {
    let codec = Codec::new();
    let state = editor(&codec, "**abc**", Selection::cursor(at(5)));
    let out = toggled(&codec, &state, md::STRONG);
    assert_eq!(out.doc(), state.doc());
    assert_eq!(caret(&out), at(7));
    assert_saves(&codec, &typed(&out, "X"), "**abc**X");
    let back = toggled(&codec, &out, md::STRONG);
    assert_eq!(caret(&back), at(5));

    let state = editor(&codec, "**abc**", Selection::cursor(at(2)));
    let out = toggled(&codec, &state, md::STRONG);
    assert_eq!(out.doc(), state.doc());
    assert_eq!(caret(&out), at(0));
    assert_saves(&codec, &typed(&out, "X"), "X**abc**");
}

/// Strictly inside a span the toggle closes it at the caret and opens it
/// again, so typing there is plain; left empty, the span is whole again.
#[test]
fn inside_a_span_the_toggle_splits_it_at_the_caret() {
    let codec = Codec::new();
    let state = editor(&codec, "**abc**", Selection::cursor(at(4)));
    let split = toggled(&codec, &state, md::STRONG);
    assert_eq!(block_source(&split, 0), "**ab****c**");
    assert_eq!(caret(&split), at(6));
    assert_saves(&codec, &typed(&split, "X"), "**ab**X**c**");
    assert_eq!(moved(&split, at(0)).doc(), state.doc());
    let rejoined = toggled(&codec, &split, md::STRONG);
    assert_eq!(rejoined.doc(), state.doc());
    assert_eq!(caret(&rejoined), at(4));
}

/// Pairs nest: a second style's pair goes inside the first, a toggle takes
/// either layer off, and leaving deletes every layer.
#[test]
fn pairs_nest_and_come_off_one_layer_at_a_time() {
    let codec = Codec::new();
    let state = editor(&codec, "ab", Selection::cursor(at(2)));
    let both = toggled(&codec, &toggled(&codec, &state, md::STRONG), md::EM);
    assert_eq!(block_source(&both, 0), "ab******");
    assert_saves(&codec, &typed(&both, "X"), "ab***X***");
    let em = toggled(&codec, &both, md::STRONG);
    assert_saves(&codec, &typed(&em, "X"), "ab*X*");
    assert_eq!(moved(&both, at(0)).doc(), state.doc());
    assert_eq!(moved(&em, at(0)).doc(), state.doc());
}

/// In `***abc***` emphasis holds strong. At the end of the text ⌘B steps out
/// of strong alone, and ⌘I out of both, since emphasis closes after strong. A
/// split in the middle has no spelling a reader takes, so it is refused and
/// the spans are left alone.
#[test]
fn nested_spans_are_left_whole() {
    let codec = Codec::new();
    let state = editor(&codec, "***abc***", Selection::cursor(at(6)));
    let strong_out = toggled(&codec, &state, md::STRONG);
    assert_eq!(caret(&strong_out), at(8));
    assert_saves(&codec, &typed(&strong_out, "X"), "***abc**X*");
    let both_out = toggled(&codec, &state, md::EM);
    assert_eq!(caret(&both_out), at(9));
    assert_saves(&codec, &typed(&both_out, "X"), "***abc***X");

    let state = editor(&codec, "***abc***", Selection::cursor(at(4)));
    for mark in [md::STRONG, md::EM] {
        let refusal = toggle(&codec, mark)(&state).expect_err("the split is refused");
        assert_eq!(
            refusal,
            CommandRefusal::NotExpressible {
                reason: Inexpressible::Delimiters { mark }
            }
        );
    }
}

/// Between two spans of the same style a new pair would run into their
/// delimiters, so the toggle goes into the span next to the caret instead.
#[test]
fn adjacent_spans_are_left_whole() {
    let codec = Codec::new();
    let state = editor(&codec, "**a** **b**", Selection::cursor(at(5)));
    let into = toggled(&codec, &state, md::STRONG);
    assert_eq!(into.doc(), state.doc());
    assert_eq!(caret(&into), at(3));
    assert_saves(&codec, &typed(&into, "X"), "**aX** **b**");

    let state = editor(&codec, "**a** **b**", Selection::cursor(at(6)));
    let into = toggled(&codec, &state, md::STRONG);
    assert_eq!(into.doc(), state.doc());
    assert_eq!(caret(&into), at(8));
    assert_saves(&codec, &typed(&into, "X"), "**a** **Xb**");
}

/// An empty pair already in the text, which a reader sees as characters, is
/// taken off by the toggle as one it wrote would be.
#[test]
fn a_literal_empty_pair_is_taken_off() {
    let codec = Codec::new();
    let state = editor(&codec, "a****b", Selection::cursor(at(3)));
    let off = toggled(&codec, &state, md::STRONG);
    assert_saves(&codec, &off, "ab");
    assert_eq!(caret(&off), at(1));
}

/// A code span's content is plain text to a reader: no other style can be
/// typed into it, so those toggles are refused. The code toggle itself steps
/// out of it or splits it like any other span's.
#[test]
fn inside_a_code_span_only_code_toggles() {
    let codec = Codec::new();
    let state = editor(&codec, "`abc`", Selection::cursor(at(2)));
    let refusal = toggle(&codec, md::STRONG)(&state).expect_err("strong is refused");
    assert_eq!(
        refusal,
        CommandRefusal::NotExpressible {
            reason: Inexpressible::Delimiters { mark: md::STRONG }
        }
    );
    let split = toggled(&codec, &state, md::CODE);
    assert_saves(&codec, &typed(&split, "X"), "`a`X`bc`");
    assert_eq!(moved(&split, at(0)).doc(), state.doc());
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

fn entered_in_list(state: &EditorState) -> EditorState {
    let item = state
        .schema()
        .node_id(md::LIST_ITEM)
        .expect("the item type");
    let enter = keeping_styles(markraft_core::commands::split_list_item(item));
    run_command(state, &enter)
        .expect("Enter runs")
        .expect("Enter applies")
        .state()
        .clone()
}

/// Enter between the runs of an empty pair leaves neither behind: the split
/// is the one Enter makes without the pair, and one undo gives the pair back
/// with the caret in it.
#[test]
fn enter_in_an_empty_pair_splits_without_it() {
    let codec = Codec::new();
    let state = editor(&codec, "ab", Selection::cursor(at(1)));
    let paired = toggled(&codec, &state, md::STRONG);
    assert_eq!(block_source(&paired, 0), "a****b");
    let split = entered(&paired);
    let plain = entered(&state);
    assert_saves(&codec, &split, "a\n\nb");
    assert_eq!(split.doc(), plain.doc());
    assert_eq!(caret(&split), caret(&plain));
    assert_saves(&codec, &type_x(&split), "a\n\nXb");

    assert_eq!(undo_depth(&split), undo_depth(&paired) + 1);
    let back = undone(&split);
    assert_eq!(back.doc(), paired.doc());
    assert_eq!(caret(&back), caret(&paired));
}

/// The same inside a list item, whose split closes the item as well.
#[test]
fn enter_in_an_empty_pair_in_a_list_item_splits_without_it() {
    let codec = Codec::new();
    // `- a|b`: the list, the item and the paragraph open before `a`.
    let state = editor(&codec, "- ab", Selection::cursor(4));
    let paired = toggled(&codec, &state, md::STRONG);
    assert_saves(&codec, &paired, "- a****b");
    let split = entered_in_list(&paired);
    let plain = entered_in_list(&state);
    assert_saves(&codec, &split, "- a\n- b");
    assert_eq!(split.doc(), plain.doc());
    assert_eq!(caret(&split), caret(&plain));

    assert_eq!(undo_depth(&split), undo_depth(&paired) + 1);
    let back = undone(&split);
    assert_eq!(back.doc(), paired.doc());
    assert_eq!(caret(&back), caret(&paired));
}

/// A pair that holds something is a span, and Enter in it keeps the style on
/// both halves as it does for any span.
#[test]
fn enter_in_a_pair_that_holds_something_keeps_the_style() {
    let codec = Codec::new();
    let state = editor(&codec, "ab", Selection::cursor(at(1)));
    let written = typed(&typed(&toggled(&codec, &state, md::STRONG), "c"), "d");
    let written = moved(&written, at(4));
    assert_eq!(block_source(&written, 0), "a**cd**b");
    let split = entered(&written);
    assert_saves(&codec, &split, "a**c**\n\n**d**b");
}

/// Typing in the pair is still left alone.
#[test]
fn typing_in_an_empty_pair_does_not_break_it() {
    let codec = Codec::new();
    let state = editor(&codec, "ab", Selection::cursor(at(1)));
    let written = typed(&toggled(&codec, &state, md::STRONG), "c");
    assert_eq!(block_source(&written, 0), "a**c**b");
}

/// At the end of a paragraph Enter opens an empty one after it, pair or not.
#[test]
fn enter_in_an_empty_pair_at_the_end_of_a_paragraph() {
    let codec = Codec::new();
    let state = two_paragraphs(&codec);
    let paired = toggled(&codec, &state, md::STRONG);
    let split = entered(&paired);
    let plain = entered(&state);
    assert_eq!(split.doc(), plain.doc());
    assert_eq!(caret(&split), caret(&plain));
    assert_eq!(block_source(&split, 1), "Para four echo.");
    assert_eq!(block_source(&split, 2), "");
}
