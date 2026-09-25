//! The conceal contract as `markraft_core::kind::conceal` reads it, over the
//! CommonMark kind: what a line shows, what a caret reveals, and how a slice
//! flattens.

use markraft_commonmark::{
    commonmark_doc_type_names, commonmark_extensions, commonmark_schema, from_markdown,
};
use markraft_core::kind::DocTypes;
use markraft_core::kind::conceal::{Reveal, concealed_steps, pieces, shown, slice_text};
use markraft_core::projection::{Line, projection_of};
use markraft_core::{EditorState, EditorStateConfig, Extension, MarkTypeId, Slice};
use std::ops::Range;

/// A state on the CommonMark schema with the editor's own extensions.
fn state_of(source: &str) -> EditorState {
    let schema = commonmark_schema();
    let doc = from_markdown(&schema, source).expect("valid Markdown");
    EditorState::create(
        EditorStateConfig::new(schema.clone())
            .doc(doc)
            .extensions(Extension::all([
                markraft_core::projection::projection(),
                markraft_core::composition::composition(),
                markraft_core::history::history(Default::default()),
                commonmark_extensions(&schema),
            ])),
    )
    .expect("a valid state")
}

fn syntax() -> Option<MarkTypeId> {
    let schema = commonmark_schema();
    DocTypes::from_schema_names(&schema, &commonmark_doc_type_names()).syntax
}

/// What the first line of `source` shows with `reveal`, as text.
fn showing(source: &str, reveal: impl Fn(&Line) -> Reveal) -> String {
    let state = state_of(source);
    let projection = projection_of(&state);
    let line = &projection.lines()[0];
    let text = projection.line_text(0).unwrap_or_default();
    let shown = shown(syntax(), line, &reveal(line));
    pieces(line, text, &shown)
        .iter()
        .map(|piece| piece.text)
        .collect()
}

/// A caret at the `offset`th character of the line's source.
fn caret(offset: usize) -> impl Fn(&Line) -> Reveal {
    move |line| {
        let pos = line.offset_to_pos(offset).expect("a position in the line");
        Reveal::at(pos..pos, None)
    }
}

#[test]
fn a_concealed_line_reads_as_prose() {
    let away = |_: &Line| Reveal::nothing();
    assert_eq!(showing("**a** *b* `c` [d](e)", away), "a b c d");
    assert_eq!(showing(r"\*f\* &amp; g", away), "*f* & g");
}

/// Two spans of one style side by side are two spans: the caret in one
/// opens only that one, though the style runs on unbroken across both.
#[test]
fn adjacent_spans_of_one_style_reveal_apart() {
    assert_eq!(showing("**a**__b__", caret(3)), "**a**b");
    assert_eq!(showing("**a**__b__", caret(8)), "a__b__");
    // `**a****b**` is one span to CommonMark — the middle run cannot close
    // the first pair (the rule of three) — so its middle is text, and the
    // caret anywhere in it opens the one pair around it.
    assert_eq!(showing("**a****b**", caret(3)), "**a****b**");
    assert_eq!(showing("**a****b** c", caret(12)), "a****b c");
}

/// The span a caret stands at the edge of is revealed, from outside as
/// well as inside.
#[test]
fn a_caret_at_a_span_edge_reveals_it() {
    assert_eq!(showing("x **a** y", caret(2)), "x **a** y");
    assert_eq!(showing("x **a** y", caret(7)), "x **a** y");
    assert_eq!(showing("x **a** y", caret(1)), "x a y");
    assert_eq!(showing("x **a** y", caret(8)), "x a y");
}

/// A caret at the start edge of a span one character wide reveals it,
/// as it does a longer one.
#[test]
fn a_caret_at_the_start_of_a_short_span_reveals_it() {
    assert_eq!(showing(r"x \*66\* y", caret(2)), r"x \*66* y");
    assert_eq!(showing("x &#38; y", caret(2)), "x &#38; y");
    assert_eq!(showing("x [44](55) y", caret(2)), "x [44](55) y");
}

/// Nested spans are revealed by where the caret is in each: inside the
/// inner one both open, inside only the outer one only it does.
#[test]
fn nested_spans_reveal_by_their_own_extent() {
    assert_eq!(showing("*a **b** c*", caret(5)), "*a **b** c*");
    assert_eq!(showing("*a **b** c*", caret(10)), "*a b c*");
}

/// An escape is a span of its own: the caret beside it opens it, and a
/// style span next to it stays as it was.
#[test]
fn an_escape_next_to_a_span_is_its_own_span() {
    assert_eq!(showing(r"\***a**", caret(1)), r"\*a");
    assert_eq!(showing(r"\***a**", caret(5)), "***a**");
}

/// An entity shows what it stands for until the caret reaches it.
#[test]
fn an_entity_shows_its_character_until_revealed() {
    assert_eq!(showing("a &amp; b", caret(0)), "a & b");
    assert_eq!(showing("a &amp; b", caret(4)), "a &amp; b");
}

/// A hard break's spelling is hidden unless the caret ends its row.
#[test]
fn a_hard_break_spelling_shows_only_at_the_caret() {
    let state = state_of("a\\\nb");
    let projection = projection_of(&state);
    let line = &projection.lines()[0];
    let text = projection.line_text(0).unwrap_or_default();
    let read = |reveal: Reveal| -> String {
        let shown = shown(syntax(), line, &reveal);
        pieces(line, text, &shown)
            .iter()
            .map(|piece| piece.text)
            .collect()
    };
    let at = |offset| {
        let pos = line.offset_to_pos(offset).expect("a position");
        Reveal::at(pos..pos, None)
    };
    assert_eq!(read(Reveal::nothing()), "a\nb");
    assert_eq!(read(at(2)), "a\\\nb", "the caret right after the backslash");
    assert_eq!(read(at(3)), "a\nb", "the caret on the next row");
}

/// Marked text reveals as a caret does, wherever the selection is.
#[test]
fn a_composition_inside_a_span_reveals_it() {
    let reveal = |line: &Line| {
        let pos = line.offset_to_pos(3).expect("a position");
        Reveal::at(line.from()..line.from(), Some(pos..pos + 1))
    };
    assert_eq!(showing("x **ab** y", reveal), "x **ab** y");
}

/// What `concealed_steps` finds on the first line of `source` for a caret
/// at `offset`, as `char` offsets into the line.
fn steps(source: &str, offset: usize) -> Vec<Range<usize>> {
    let state = state_of(source);
    let projection = projection_of(&state);
    let line = &projection.lines()[0];
    let caret = line.offset_to_pos(offset).expect("a position");
    concealed_steps(syntax(), line, caret)
        .into_iter()
        .map(|range| {
            let offset = |pos| line.pos_to_offset(pos).expect("an offset");
            offset(range.start)..offset(range.end)
        })
        .collect()
}

#[test]
fn a_caret_steps_over_what_it_leaves_concealed() {
    // Away from the span both delimiter runs are steps; beside it neither.
    assert_eq!(steps("x **a** y", 0), [2..4, 5..7]);
    assert!(steps("x **a** y", 2).is_empty());
    // Two runs that close two spans at once are one step.
    assert_eq!(steps("x ***a*** y", 0), [2..5, 6..9]);
    // An entity shows its character: a step of its own.
    assert_eq!(steps("x &amp;*a* y", 0), [2..7, 7..8, 9..10]);
}

#[test]
fn a_slice_flattens_to_what_it_displays() {
    let state = state_of("**a** &amp; \\*b");
    let slice = Slice::new(state.doc().content().clone(), 0, 0);
    assert_eq!(slice_text(state.schema(), syntax(), &slice), "a & *b");
    assert_eq!(
        slice_text(state.schema(), None, &slice),
        "**a** &amp; \\*b",
        "a kind with no conceal role flattens to its characters"
    );
}
