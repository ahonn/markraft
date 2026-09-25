//! The inline source model, from the editor's side.
//!
//! A textblock's text *is* its Markdown inline source — delimiters, backslash
//! escapes, entities and `<u>` tags included — and every style mark is derived
//! from that text. Editing is therefore editing source: splitting `**abcd**`
//! leaves `**ab` and `cd**`, typing the `*` that opens a span makes the span,
//! and `\*a\*` stays exactly those five characters however the block around it
//! is edited. Nothing is ever escaped behind the writer's back, so what the
//! editor holds is what the file holds.
//!
//! Positions in these tests count source characters: [`at`] is the document
//! position of the `n`th character of the first paragraph's source, which in
//! this model is the `n`th character of its text.
//!
//! The formatting commands have a suite of their own in `tests/formatting.rs`;
//! the two toggles here are the cases that motivated them.

mod common;

use common::{Codec, byte_range, html, leading_definition_lines, options, with_atoms};
use comrak::nodes::{AstNode, NodeValue, Sourcepos};
use comrak::{Arena, parse_document};
use markraft_commonmark::derive::{BlockKind, guard};
use markraft_commonmark::schema as md;
use markraft_commonmark::{Formatter, commonmark_extensions, to_markdown};
use markraft_core::commands::{
    Command, Direction, delete_by_grapheme, delete_selection, insert_text, run_command, split_block,
};
use markraft_core::{
    Attrs, Change, EditorState, EditorStateConfig, Extension, Selection, Slice, TransactionSpec,
    composition::{
        CompositionRange, composition, finish_composition, start_composition, update_composition,
    },
    history::{HistoryConfig, history, undo},
    protocol::corrections_diverged,
};
use serde::Deserialize;

// -- harness ----------------------------------------------------------------

/// The document position of source character `offset` in the first
/// paragraph: one past the paragraph's opening.
fn at(offset: usize) -> usize {
    1 + offset
}

/// An editor on `source` with the Markdown extensions, undo history and
/// composition.
fn editor(codec: &Codec, source: &str, selection: Selection) -> EditorState {
    EditorState::create(
        EditorStateConfig::new(codec.schema.clone())
            .doc(codec.parse(source))
            .selection(selection)
            .extensions(Extension::all([
                commonmark_extensions(&codec.schema),
                history(HistoryConfig::default()),
                composition(),
            ])),
    )
    .expect("a valid editor state")
}

fn run(state: &EditorState, command: &Command) -> EditorState {
    run_command(state, command)
        .expect("the command runs")
        .expect("the command applies")
        .state()
        .clone()
}

fn type_text(state: &EditorState, text: &str) -> EditorState {
    text.chars().fold(state.clone(), |state, character| {
        run(&state, &insert_text(&character.to_string()))
    })
}

fn written(codec: &Codec, state: &EditorState) -> String {
    to_markdown(&codec.schema, state.doc())
}

/// The file `state` saves as is exactly `expected`, and so reads as it does.
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

// -- splitting ----------------------------------------------------------------

/// Enter inside a span splits its source; each half is what it spells, and
/// nothing is escaped to keep a style the source no longer has.
#[test]

fn splitting_inside_strong_splits_its_source() {
    let codec = Codec::new();
    let state = editor(&codec, "**abcd**", Selection::cursor(at(4)));
    assert_saves(&codec, &run(&state, &split_block()), "**ab\n\ncd**");
}

#[test]

fn splitting_inside_em_splits_its_source() {
    let codec = Codec::new();
    let state = editor(&codec, "*abcd*", Selection::cursor(at(3)));
    assert_saves(&codec, &run(&state, &split_block()), "*ab\n\ncd*");
}

/// Splitting between an escape's backslash and its character leaves a literal
/// backslash and a literal character, as the source says.
#[test]

fn splitting_an_escape_leaves_both_halves_literal() {
    let codec = Codec::new();
    let state = editor(&codec, r"a\*b", Selection::cursor(at(2)));
    assert_saves(&codec, &run(&state, &split_block()), "a\\\n\n*b");
}

// -- typing ---------------------------------------------------------------------

/// The opening delimiter typed after the closing one already exists makes
/// the span: the text is `*b*` whichever `*` came first.
#[test]

fn typing_the_opening_delimiter_last_makes_the_span() {
    let codec = Codec::new();
    let state = editor(&codec, "b*", Selection::cursor(at(0)));
    assert_saves(&codec, &type_text(&state, "*"), "*b*");
}

#[test]

fn typing_the_opening_strong_delimiter_last_makes_the_span() {
    let codec = Codec::new();
    let state = editor(&codec, "bold**", Selection::cursor(at(0)));
    assert_saves(&codec, &type_text(&state, "**"), "**bold**");
}

/// Typing a whole span character by character: nothing is a style until the
/// source says so, and then it is.
#[test]
fn typing_a_span_in_order_makes_it() {
    let codec = Codec::new();
    let state = editor(&codec, "", Selection::cursor(1));
    assert_saves(&codec, &type_text(&state, "a *b* c"), "a *b* c");
}

// -- settling the caret's line -------------------------------------------------

/// The text the `index`th block of `state` holds, a line break as `\n`.
fn block_text(codec: &Codec, state: &EditorState, index: usize) -> String {
    let line_break = codec.schema.node_id(md::LINE_BREAK);
    state
        .doc()
        .child(index)
        .children()
        .map(|child| match child.text() {
            Some(text) => text.to_string(),
            None if Some(child.type_id()) == line_break => "\n".to_string(),
            None => "\u{fffc}".to_string(),
        })
        .collect()
}

/// The marker a writer is still typing is not escaped under the caret: `- `
/// at a line start becomes a list, not `\-` followed by a space.
#[test]
fn typing_a_bullet_marker_at_a_line_start_makes_a_list() {
    let codec = Codec::new();
    let state = editor(&codec, "", Selection::cursor(1));
    let state = type_text(&state, "- a");
    assert_saves(&codec, &state, "- a");
}

/// Nor is a `*` that is about to open emphasis.
#[test]
fn typing_emphasis_at_a_line_start_makes_it() {
    let codec = Codec::new();
    let state = editor(&codec, "", Selection::cursor(1));
    let state = type_text(&state, "*em*");
    assert_eq!(block_text(&codec, &state, 0), "*em*");
    assert!(
        codec.describe(state.doc()).contains("em"),
        "the tree is {}",
        codec.describe(state.doc())
    );
    assert_saves(&codec, &state, "*em*");
}

/// Once the caret leaves a line that would open a block, the transaction
/// that moved it puts the backslash in the tree — not only the writer in the
/// file — and one undo takes the typing and the backslash back together.
#[test]
fn leaving_a_line_that_opens_a_block_escapes_it_in_the_tree() {
    let codec = Codec::new();
    let state = editor(&codec, "a\nc\n\nb", Selection::cursor(at(2)));
    let state = type_text(&state, "# x ");
    assert_eq!(block_text(&codec, &state, 0), "a\n# x c");
    // Down into the next paragraph, and nothing else.
    let next = state.doc().child(0).node_size() + 1;
    let tr = state
        .update([TransactionSpec::new()
            .selection(Selection::cursor(next))
            .user_event("select")])
        .expect("the move applies");
    let moved = tr.state().clone();
    assert_eq!(block_text(&codec, &moved, 0), "a\n\\# x c");
    assert_eq!(moved.selection(), &Selection::cursor(next + 1));
    assert_saves(&codec, &moved, "a\n\\# x c\n\nb");
    let back = undone(&moved);
    assert_eq!(block_text(&codec, &back, 0), "a\nc");
}

/// Whitespace starting a line is kept while the caret is on it and dropped
/// once it leaves, within the same block.
#[test]
fn leaving_a_line_drops_its_leading_whitespace() {
    let codec = Codec::new();
    let state = editor(&codec, "a\nc", Selection::cursor(at(2)));
    let state = type_text(&state, "  ");
    assert_eq!(block_text(&codec, &state, 0), "a\n  c");
    let tr = state
        .update([TransactionSpec::new().selection(Selection::cursor(at(1)))])
        .expect("the move applies");
    assert_eq!(block_text(&codec, tr.state(), 0), "a\nc");
    // A move that stays on the line settles nothing.
    let tr = state
        .update([TransactionSpec::new().selection(Selection::cursor(at(3)))])
        .expect("the move applies");
    assert!(!tr.doc_changed());
}

/// Whitespace ending the block is kept while the caret is on its line — the
/// next word is about to follow it — and dropped once the caret leaves.
#[test]
fn leaving_a_block_drops_its_trailing_whitespace() {
    let codec = Codec::new();
    let state = editor(&codec, "a\n\nb", Selection::cursor(at(1)));
    let state = type_text(&state, " ");
    assert_eq!(block_text(&codec, &state, 0), "a ");
    let next = state.doc().child(0).node_size() + 1;
    let tr = state
        .update([TransactionSpec::new().selection(Selection::cursor(next))])
        .expect("the move applies");
    assert_eq!(block_text(&codec, tr.state(), 0), "a");
    assert_eq!(codec.parse("a\n\nb"), *tr.state().doc());
}

/// Every atom spelling one edit brings in folds in the same round.
#[test]
fn one_edit_folds_every_atom_it_spells() {
    let codec = Codec::new();
    let state = editor(&codec, "x", Selection::cursor(at(1)));
    let spelled = " ![a](b) [[c]] <b> ![d](e)";
    let tr = state
        .update([TransactionSpec::new()
            .changes([Change::insert(
                at(1),
                Slice::from_fragment(markraft_core::Fragment::from_node(
                    codec.schema.text(spelled),
                )),
            )])
            .user_event("input.paste")])
        .expect("the insertion applies");
    assert_eq!(tr.annotation(corrections_diverged()), None);
    assert_eq!(
        block_text(&codec, tr.state(), 0),
        "x \u{fffc} \u{fffc} \u{fffc} \u{fffc}"
    );
    assert_saves(&codec, tr.state(), &format!("x{spelled}"));
}

/// However many marks one keystroke changes, and however many blocks one
/// insertion splits into, the correction settles within its round bound.
#[test]
fn one_edit_settles_every_mark_and_block_it_changes() {
    let codec = Codec::new();
    let source = "*a **b** `c` [d](e) ~~f~~ <u>g</u> h";
    let state = editor(
        &codec,
        source,
        Selection::cursor(at(source.chars().count())),
    );
    let tr = state
        .update([TransactionSpec::new()
            .changes([Change::insert(
                at(source.chars().count()),
                Slice::from_fragment(markraft_core::Fragment::from_node(codec.schema.text("*"))),
            )])
            .user_event("input.type")])
        .expect("typing applies");
    assert_eq!(tr.annotation(corrections_diverged()), None);
    let expected = format!("{source}*");
    assert_saves(&codec, tr.state(), &expected);
    assert_eq!(
        codec.parse(&expected),
        *tr.state().doc(),
        "the tree is what reading the file gives"
    );

    let line_break = codec
        .schema
        .node_id(md::LINE_BREAK)
        .expect("the break type");
    let mut nodes = Vec::new();
    for index in 0..12 {
        nodes.push(codec.schema.text(&format!("p{index}")));
        for _ in 0..2 {
            nodes.push(
                codec
                    .schema
                    .create(
                        line_break,
                        Attrs::empty(),
                        markraft_core::MarkSet::empty(),
                        markraft_core::Fragment::empty(),
                    )
                    .expect("a break"),
            );
        }
    }
    nodes.push(codec.schema.text("end"));
    let state = editor(&codec, "x", Selection::cursor(at(1)));
    let tr = state
        .update([TransactionSpec::new().changes([Change::insert(
            at(1),
            Slice::from_fragment(markraft_core::Fragment::from_nodes(nodes)),
        )])])
        .expect("the insertion applies");
    assert_eq!(tr.annotation(corrections_diverged()), None);
    let expected = (0..12)
        .map(|index| {
            if index == 0 {
                "xp0".to_string()
            } else {
                format!("p{index}")
            }
        })
        .chain(["end".to_string()])
        .collect::<Vec<_>>()
        .join("\n\n");
    assert_saves(&codec, tr.state(), &expected);
}

/// A heading of level three or more holds no line break: one put there
/// becomes a space, and the backslash that spelled it goes with it — but not a
/// backslash that only escapes the one before it.
#[test]
fn a_break_a_heading_cannot_hold_takes_only_its_own_backslash() {
    let codec = Codec::new();
    let line_break = codec
        .schema
        .node_id(md::LINE_BREAK)
        .expect("the break type");
    for (before, expected) in [("a\\", "### xa b"), ("a\\\\", "### xa\\\\ b")] {
        let state = editor(&codec, "### x", Selection::cursor(at(1)));
        let nodes = [
            codec.schema.text(before),
            codec
                .schema
                .create(
                    line_break,
                    Attrs::empty(),
                    markraft_core::MarkSet::empty(),
                    markraft_core::Fragment::empty(),
                )
                .expect("a break"),
            codec.schema.text("b"),
        ];
        let tr = state
            .update([TransactionSpec::new().changes([Change::insert(
                at(1),
                Slice::from_fragment(markraft_core::Fragment::from_nodes(nodes)),
            )])])
            .expect("the insertion applies");
        assert_saves(&codec, tr.state(), expected);
    }
}

// -- escapes ------------------------------------------------------------------

/// An escaped delimiter is two characters of text, and a file keeps them —
/// including the escapes a writer would not have needed, like the `]` here.
#[test]

fn an_escaped_delimiter_round_trips() {
    let codec = Codec::new();
    for source in [
        r"\*a\*",
        r"a \* b",
        r"\# not a heading",
        r"\[x](y)",
        r"1\. a",
    ] {
        assert_eq!(codec.normalize(source), source);
    }
}

/// Typing inside an escaped span edits its source: the escapes are
/// characters with positions of their own, so the caret can sit between the
/// backslash-escaped `*` and the `a`.
#[test]

fn typing_inside_escaped_delimiters_keeps_them_escaped() {
    let codec = Codec::new();
    let state = editor(&codec, r"\*a\*", Selection::cursor(at(2)));
    assert_saves(&codec, &type_text(&state, "X"), r"\*Xa\*");
}

/// Deleting an escape's backslash leaves the delimiter it escaped, which then
/// means what it says.
#[test]

fn deleting_an_escape_backslash_makes_the_style() {
    let codec = Codec::new();
    let state = editor(&codec, r"\*a*", Selection::cursor(at(1)));
    let state = run(&state, &delete_by_grapheme(Direction::Backward));
    assert_saves(&codec, &state, "*a*");
}

/// An edit elsewhere in the block leaves an escaped span alone.
#[test]
fn an_escaped_span_survives_an_edit_beside_it() {
    let codec = Codec::new();
    let state = editor(&codec, r"a \*b\* c", Selection::cursor(at(0)));
    assert_saves(&codec, &type_text(&state, "X"), r"Xa \*b\* c");
}

// -- deleting -------------------------------------------------------------------

/// Cutting across a span's closing delimiter leaves the opening one as text,
/// written as it stands.
#[test]

fn cutting_half_a_span_leaves_its_source() {
    let codec = Codec::new();
    let state = editor(&codec, "**ab**cd", Selection::text(at(3), at(7)));
    assert_saves(&codec, &run(&state, &delete_selection()), "**ad");
}

#[test]

fn deleting_a_closing_delimiter_leaves_the_opening_one_as_text() {
    let codec = Codec::new();
    let state = editor(&codec, "*em* x", Selection::cursor(at(4)));
    let state = run(&state, &delete_by_grapheme(Direction::Backward));
    assert_saves(&codec, &state, "*em x");
}

// -- undo -------------------------------------------------------------------

fn undone(state: &EditorState) -> EditorState {
    let spec = undo(state).expect("something to undo");
    state.update([spec]).expect("undo applies").state().clone()
}

#[test]
fn undoing_a_split_restores_the_span() {
    let codec = Codec::new();
    let state = editor(&codec, "**abcd**", Selection::cursor(at(4)));
    let state = undone(&run(&state, &split_block()));
    assert_saves(&codec, &state, "**abcd**");
}

#[test]

fn undoing_the_opening_delimiter_restores_the_text() {
    let codec = Codec::new();
    let state = editor(&codec, "b*", Selection::cursor(at(0)));
    let state = undone(&type_text(&state, "*"));
    assert_saves(&codec, &state, "b*");
}

// -- composition --------------------------------------------------------------

/// An input method composing inside a span writes into its source; the span
/// holds whatever it commits.
#[test]
fn composing_inside_a_span_writes_into_it() {
    let codec = Codec::new();
    let state = editor(&codec, "**ab**", Selection::cursor(at(3)));
    let apply = |state: &EditorState, spec| state.update([spec]).unwrap().state().clone();
    let state = apply(
        &state,
        start_composition(CompositionRange::new(at(3), at(3))),
    );
    let state = apply(&state, update_composition(&state, "n", 1).unwrap());
    let state = apply(&state, update_composition(&state, "ni", 2).unwrap());
    let state = apply(&state, update_composition(&state, "\u{4f60}", 1).unwrap());
    let state = apply(&state, finish_composition());
    assert_saves(&codec, &state, "**a\u{4f60}b**");
}

// -- links ------------------------------------------------------------------------

/// A link's destination is text like any other: typing in it changes the
/// href the link derives.
#[test]
fn typing_in_a_link_destination_changes_the_href() {
    let codec = Codec::new();
    let state = editor(&codec, "[a](https://x.example)", Selection::cursor(at(13)));
    assert_saves(&codec, &type_text(&state, "y"), "[a](https://xy.example)");
}

#[test]

fn deleting_a_link_bracket_leaves_the_rest_as_text() {
    let codec = Codec::new();
    let state = editor(&codec, "[a](u) b", Selection::cursor(at(1)));
    let state = run(&state, &delete_by_grapheme(Direction::Backward));
    assert_saves(&codec, &state, "a](u) b");
}

// -- toggling -------------------------------------------------------------------

fn toggle(codec: &Codec, state: &EditorState, mark: &str) -> EditorState {
    let ty = codec.schema.mark_id(mark).expect("the mark type");
    run(
        state,
        &Formatter::new(Default::default()).toggle_style_mark(ty, Attrs::empty()),
    )
}

/// Strong over a selection that is half strong already: one span covering
/// both, not a span nested in or beside the old one.
#[test]
fn toggling_strong_across_a_span_and_plain_text_joins_them() {
    let codec = Codec::new();
    let state = editor(&codec, "**a** b", Selection::text(at(2), at(7)));
    assert_saves(&codec, &toggle(&codec, &state, md::STRONG), "**a b**");
}

#[test]
fn toggling_strong_off_across_two_spans_removes_both() {
    let codec = Codec::new();
    let state = editor(&codec, "**a** and **b**", Selection::text(at(0), at(15)));
    assert_saves(&codec, &toggle(&codec, &state, md::STRONG), "a and b");
}

// -- parse → write ------------------------------------------------------------

#[derive(Deserialize)]
struct Example {
    markdown: String,
    example: usize,
}

/// The inline source of every paragraph, heading and table cell in
/// `markdown`, in document order, as comrak reads it: container prefixes and
/// the indentation of continuation lines removed, a block's trailing
/// whitespace dropped, line endings as `\n` — the three things a textblock's
/// text is allowed to differ from its file in. Atoms are one U+FFFC each, as
/// they are in the tree: how the writer spells an image is not the block's
/// text.
fn inline_sources(markdown: &str) -> Vec<(BlockKind, String)> {
    let markdown = with_atoms(&markdown.replace("\r\n", "\n").replace('\r', "\n"));
    let arena = Arena::new();
    let root = parse_document(&arena, &markdown, &options());
    let lines: Vec<&str> = markdown.split('\n').collect();
    let mut out = Vec::new();
    for node in root.descendants() {
        let (value, pos) = {
            let data = node.data.borrow();
            (data.value.clone(), data.sourcepos)
        };
        match value {
            NodeValue::Paragraph => out.push((
                BlockKind::Paragraph,
                paragraph_source(&lines, pos, pos.end.line),
            )),
            NodeValue::Heading(heading) if heading.setext => out.push((
                BlockKind::Heading,
                paragraph_source(&lines, pos, pos.end.line - 1),
            )),
            NodeValue::Heading(_) => out.push((BlockKind::Heading, atx_source(node, &markdown))),
            NodeValue::TableCell => {
                let text = byte_range(&common::line_starts(&markdown), pos, &markdown)
                    .map_or("", |range| markdown[range].trim());
                // A cell the row was short of has the position of the row's
                // closing pipe; it is empty.
                let text = if text == "|" { "" } else { text };
                out.push((BlockKind::TableCell, text.to_string()));
            }
            _ => {}
        }
    }
    out
}

/// A paragraph's lines up to `last`, without the link reference definitions
/// it starts with.
fn paragraph_source(lines: &[&str], pos: Sourcepos, last: usize) -> String {
    let mut source: Vec<&str> = Vec::new();
    for number in pos.start.line..=last {
        let line = lines[number - 1];
        source.push(if number == pos.start.line {
            &line[pos.start.column - 1..]
        } else {
            // A continuation line's container prefix is block quote markers
            // and indentation, and its content cannot start with a `>`: that
            // would open a block quote.
            line.trim_start_matches([' ', '\t', '>'])
        });
    }
    let definitions = leading_definition_lines(&source);
    source[definitions..]
        .join("\n")
        .trim_end_matches([' ', '\t'])
        .to_string()
}

/// An ATX heading's content: its line without the marker, the whitespace
/// around the content and the closing sequence. Read from the line rather
/// than from the inline nodes, whose positions start after an escape's
/// backslash.
fn atx_source<'a>(node: &'a AstNode<'a>, markdown: &str) -> String {
    let pos = node.data.borrow().sourcepos;
    let line = markdown
        .split('\n')
        .nth(pos.start.line - 1)
        .unwrap_or_default();
    let rest = line[pos.start.column - 1..]
        .trim_start_matches(' ')
        .trim_start_matches('#')
        .trim_matches([' ', '\t']);
    if rest.chars().all(|c| c == '#') {
        return String::new();
    }
    let hashes = rest.len() - rest.trim_end_matches('#').len();
    let before = &rest[..rest.len() - hashes];
    if hashes > 0 && before.ends_with([' ', '\t']) {
        before.trim_end_matches([' ', '\t']).to_string()
    } else {
        rest.to_string()
    }
}

/// Parsing a file and writing it again gives back every textblock's inline
/// source byte for byte, whatever it does to the blocks around them.
///
/// Byte for byte *once guarded*: a continuation line indented four columns,
/// `Foo\n    ***`, is protected by its indentation in the file, and its text
/// has none. Written back as `Foo\n***` it would be a thematic break, so the
/// guard's `\` is the one difference allowed.
#[test]

fn parse_then_write_keeps_every_textblock_source() {
    let codec = Codec::new();
    let spec: Vec<Example> =
        serde_json::from_str(include_str!("data/commonmark-spec-0.31.2.json")).unwrap();
    let tables: Vec<Example> =
        serde_json::from_str(include_str!("data/gfm-tables-0.29.json")).unwrap();
    let corpus = common::CORPUS
        .iter()
        .enumerate()
        .map(|(index, markdown)| Example {
            markdown: markdown.to_string(),
            example: 10_000 + index,
        });
    let mut failures = Vec::new();
    let mut checked = 0;
    for example in spec.into_iter().chain(tables).chain(corpus) {
        checked += 1;
        let written = codec.normalize(&example.markdown);
        let expected: Vec<String> = inline_sources(&example.markdown)
            .into_iter()
            .map(|(kind, text)| guard(kind, &text).text)
            .collect();
        let actual: Vec<String> = inline_sources(&written)
            .into_iter()
            .map(|(_, text)| text)
            .collect();
        if expected != actual {
            failures.push(format!(
                "example {}: {:?}\n  written:  {written:?}\n  expected: {expected:?}\n  actual:   {actual:?}",
                example.example, example.markdown
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {checked} documents changed a textblock's source:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// Every example as a document, the way the property tests take them.
fn all_examples() -> Vec<Example> {
    let spec: Vec<Example> =
        serde_json::from_str(include_str!("data/commonmark-spec-0.31.2.json")).unwrap();
    let tables: Vec<Example> =
        serde_json::from_str(include_str!("data/gfm-tables-0.29.json")).unwrap();
    let corpus = common::CORPUS
        .iter()
        .enumerate()
        .map(|(index, markdown)| Example {
            markdown: markdown.to_string(),
            example: 10_000 + index,
        });
    spec.into_iter().chain(tables).chain(corpus).collect()
}

/// A document the parser builds is already what the canonicalising correction
/// would make of it: inserting the whole of it into an empty editor corrects
/// nothing — no mark, no backslash, no atom.
#[test]
fn a_parsed_document_is_already_canonical() {
    let codec = Codec::new();
    let mut failures = Vec::new();
    for example in all_examples() {
        let doc = codec.parse(&example.markdown);
        let state = editor(&codec, "", Selection::cursor(1));
        let insert = Change::replace(
            0,
            state.doc().content_size(),
            Slice::from_fragment(doc.content().clone()),
        );
        let result = state
            .update([TransactionSpec::new().changes([insert])])
            .expect("the document replaces the empty one");
        if result.state().doc() != &doc {
            failures.push(format!(
                "example {}: {:?}\n  parsed:    {}\n  corrected: {}",
                example.example,
                example.markdown,
                codec.describe(&doc),
                codec.describe(result.state().doc())
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} documents were corrected:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// The extraction the property above relies on, on shapes whose inline
/// source is known.
#[test]
fn inline_sources_strip_exactly_the_three_exceptions() {
    let texts = |markdown: &str| -> Vec<String> {
        inline_sources(markdown)
            .into_iter()
            .map(|(_, text)| text)
            .collect()
    };
    assert_eq!(texts("a *b*\n  c  \n"), ["a *b*\nc"]);
    assert_eq!(texts("> a\n> b\nlazy\n\n- x\n  y"), ["a\nb\nlazy", "x\ny"]);
    assert_eq!(texts("# a \\# #\n\nb\n---"), ["a \\#", "b"]);
    assert_eq!(
        texts("| a \\| b | c |\n|-|-|\n| d |"),
        ["a \\| b", "c", "d", ""]
    );
    assert_eq!(
        texts("[r]: /u\nx ![i](s) <b>y</b>"),
        ["x \u{fffc} \u{fffc}y\u{fffc}"]
    );
    assert_eq!(texts("a\r\nb"), ["a\nb"]);
    assert_eq!(texts("Foo\n    ***"), ["Foo\n***"]);
}
