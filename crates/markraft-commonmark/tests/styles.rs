//! Styles are spelled by the text: a style mark covers its delimiters, which
//! are characters of the text carrying `syntax` as well, and every edit
//! re-derives the marks from what the text now says.
mod common;

use common::Codec;
use markraft_commonmark::schema as md;
use markraft_commonmark::serialize::spell_document;
use markraft_commonmark::{Formatter, commonmark_extensions, from_markdown, to_markdown};
use markraft_core::commands::{Direction, delete_by_grapheme, insert_text, run_command};
use markraft_core::{Attrs, EditorState, EditorStateConfig, Selection};

#[test]
fn strong_keeps_delimiters_in_the_tree() {
    let codec = Codec::new();
    let doc = codec.parse("**bold**");
    assert_eq!(
        codec.describe(&doc),
        r#"doc(paragraph("**"{strong,syntax}, "bold"{strong}, "**"{strong,syntax}))"#
    );
    assert_eq!(codec.write(&doc), "**bold**");
}

#[test]
fn em_code_and_strike_keep_delimiters() {
    let codec = Codec::new();
    assert_eq!(
        codec.describe(&codec.parse("*em*")),
        r#"doc(paragraph("*"{em,syntax}, "em"{em}, "*"{em,syntax}))"#
    );
    assert_eq!(
        codec.describe(&codec.parse("`code`")),
        r#"doc(paragraph("`"{code,syntax}, "code"{code}, "`"{code,syntax}))"#
    );
    assert_eq!(
        codec.describe(&codec.parse("~~x~~")),
        r#"doc(paragraph("~~"{strikethrough,syntax}, "x"{strikethrough}, "~~"{strikethrough,syntax}))"#
    );
    assert_eq!(
        codec.describe(&codec.parse("[label](https://a.example)")),
        r#"doc(paragraph("["{link,syntax}, "label"{link}, "](https://a.example)"{link,syntax}))"#
    );
    assert_eq!(
        codec.describe(&codec.parse("https://a.example")),
        r#"doc(paragraph("https://a.example"{link}))"#
    );
}

#[test]
fn nested_emphasis_keeps_each_delimiter_pair() {
    let codec = Codec::new();
    let doc = codec.parse("*a **b** c*");
    assert_eq!(codec.write(&doc), "*a **b** c*");
    assert_eq!(
        codec.describe(&doc),
        r#"doc(paragraph("*"{em,syntax}, "a "{em}, "**"{strong,em,syntax}, "b"{strong,em}, "**"{strong,em,syntax}, " c"{em}, "*"{em,syntax}))"#
    );
}

#[test]
fn em_around_autolink_writes_bare() {
    let codec = Codec::new();
    assert_eq!(
        codec.normalize("*https://a.example*"),
        "*https://a.example*"
    );
    let described = codec.describe(&codec.parse("*https://a.example*"));
    assert!(described.contains("syntax"), "{described}");
    assert!(
        described.contains(r#""https://a.example"{link,em}"#),
        "{described}"
    );
}

#[test]
fn spelled_styles_round_trip_without_doubling() {
    let codec = Codec::new();
    for source in [
        "**bold**",
        "*em*",
        "`code`",
        "~~x~~",
        "**a*b*c**",
        "a **b** c",
    ] {
        let written = codec.normalize(source);
        assert_eq!(codec.normalize(&written), written, "source={source:?}");
    }
}

/// A bare style mark — what a host or an HTML paste describes — has no
/// spelling until it is given one; spelling it writes its delimiters.
#[test]
fn a_bare_style_mark_is_spelled_with_its_delimiters() {
    let codec = Codec::new();
    let strong = codec
        .schema
        .mark(md::STRONG, markraft_core::Attrs::empty())
        .unwrap();
    let doc = codec
        .schema
        .doc([codec
            .schema
            .node(
                md::PARAGRAPH,
                [codec.schema.text_marked(
                    "bold",
                    markraft_core::MarkSet::from_marks(&codec.schema, [strong]),
                )],
            )
            .unwrap()])
        .unwrap();
    // Unspelled, the text is all there is to write.
    assert_eq!(codec.write(&doc), "bold");
    let spelled = spell_document(&codec.serializer, &doc);
    assert_eq!(codec.write(&spelled), "**bold**");
    assert_eq!(spelled, codec.parse("**bold**"));
}

/// Content spelled from its marks — a paste — follows the house style, and
/// uses asterisks where a letter borders the run and `_` would not be read.
#[test]
fn spelling_marks_in_the_underscore_house_style() {
    use markraft_commonmark::HouseStyle;
    use markraft_core::MarkSet;

    let codec = Codec::in_house(HouseStyle {
        emphasis: '_',
        ..HouseStyle::default()
    });
    let serializer = &codec.serializer;

    let schema = &codec.schema;
    let spelled = |mark: &str, parts: [&str; 3]| {
        let marks = MarkSet::from_marks(schema, [schema.mark(mark, Attrs::empty()).unwrap()]);
        let doc = schema
            .doc([schema
                .node(
                    md::PARAGRAPH,
                    [
                        schema.text(parts[0]),
                        schema.text_marked(parts[1], marks),
                        schema.text(parts[2]),
                    ],
                )
                .unwrap()])
            .unwrap();
        codec.write(&spell_document(serializer, &doc))
    };
    assert_eq!(spelled(md::EM, ["a ", "b", " c"]), "a _b_ c");
    assert_eq!(spelled(md::STRONG, ["a ", "b", ", c"]), "a __b__, c");
    assert_eq!(spelled(md::EM, ["foo", "bar", "baz"]), "foo*bar*baz");
    assert_eq!(spelled(md::STRONG, ["中", "文", "字"]), "中**文**字");
}

#[test]
fn deleting_a_delimiter_takes_the_style_with_it() {
    let codec = Codec::new();
    let doc = codec.parse("*em*");
    // Caret after the opening `*` (doc pos 2 is after the one-char syntax leaf).
    let state = EditorState::create(
        EditorStateConfig::new(codec.schema.clone())
            .doc(doc)
            .selection(Selection::cursor(2))
            .extensions(commonmark_extensions(&codec.schema)),
    )
    .unwrap();
    let after = run_command(&state, &delete_by_grapheme(Direction::Backward))
        .unwrap()
        .unwrap()
        .state()
        .clone();
    let written = to_markdown(&codec.schema, after.doc());
    // Opening delimiter gone: no longer emphasis.
    assert_eq!(common::html(&written), common::html("em*"), "{written}");
    let described = codec.schema.describe(after.doc());
    assert!(
        !described.contains("{em}") || described.contains(r#""em*""#),
        "expected literal leftover marker, got {described}"
    );
}

#[test]
fn toggle_style_mark_inserts_delimiters() {
    let codec = Codec::new();
    let doc = from_markdown(&codec.schema, "hello").unwrap();
    let state = EditorState::create(
        EditorStateConfig::new(codec.schema.clone())
            .doc(doc)
            .selection(Selection::text(1, 6))
            .extensions(commonmark_extensions(&codec.schema)),
    )
    .unwrap();
    let strong = codec.schema.mark_id(md::STRONG).unwrap();
    let after = run_command(
        &state,
        &Formatter::new(Default::default()).toggle_style_mark(strong, Attrs::empty()),
    )
    .unwrap()
    .unwrap()
    .state()
    .clone();
    assert_eq!(to_markdown(&codec.schema, after.doc()).trim(), "**hello**");
    let described = codec.schema.describe(after.doc());
    assert!(described.contains("syntax"), "{described}");
}

// -- typing ---------------------------------------------------------------

fn edited(codec: &Codec, source: &str, selection: Selection) -> EditorState {
    EditorState::create(
        EditorStateConfig::new(codec.schema.clone())
            .doc(codec.parse(source))
            .selection(selection)
            .extensions(commonmark_extensions(&codec.schema)),
    )
    .unwrap()
}

fn typed(codec: &Codec, text: &str) -> EditorState {
    let mut state = edited(codec, "", Selection::cursor(1));
    for character in text.chars() {
        state = run_command(&state, &insert_text(&character.to_string()))
            .expect("the command runs")
            .expect("the command applies")
            .state()
            .clone();
    }
    state
}

/// The delimiters a writer types make the style as soon as the text spells it.
#[test]
fn typing_a_delimiter_pair_applies_the_style() {
    let codec = Codec::new();
    for (input, written) in [
        ("**bold**", "**bold**"),
        ("*em*", "*em*"),
        ("~~x~~", "~~x~~"),
        ("`code`", "`code`"),
        ("a **b** c", "a **b** c"),
        // Flanking whitespace is not emphasis, so the text stays text — and
        // is written as it is, since a reader reads it as text too.
        ("** not bold**", "** not bold**"),
    ] {
        let state = typed(&codec, input);
        assert_eq!(
            to_markdown(&codec.schema, state.doc()).trim(),
            written,
            "typed {input:?} — {}",
            codec.describe(state.doc())
        );
    }
    assert_eq!(
        codec.describe(typed(&codec, "**bold**").doc()),
        r#"doc(paragraph("**"{strong,syntax}, "bold"{strong}, "**"{strong,syntax}))"#
    );
}

/// The caret lands past the closing delimiter, where the style is over.
#[test]
fn typing_past_a_closed_style_is_not_styled() {
    let codec = Codec::new();
    let state = typed(&codec, "**bold**tail");
    assert_eq!(
        codec.describe(state.doc()),
        r#"doc(paragraph("**"{strong,syntax}, "bold"{strong}, "**"{strong,syntax}, "tail"))"#
    );
}

/// Characters that only *look* like delimiters were written `\*` in the source,
/// and the backslashes are in the text, so they stay literal however much the
/// rest of the block is edited.
#[test]
fn an_escaped_literal_survives_an_edit_to_its_paragraph() {
    let codec = Codec::new();
    let source = r"a \*not em\* b";
    let state = edited(&codec, source, Selection::cursor(1));
    let after = run_command(&state, &insert_text("X"))
        .unwrap()
        .unwrap()
        .state()
        .clone();
    assert_eq!(
        to_markdown(&codec.schema, after.doc()).trim(),
        r"Xa \*not em\* b"
    );
}

// -- toggling -------------------------------------------------------------

fn toggled(codec: &Codec, state: &EditorState, mark: &str) -> EditorState {
    let ty = codec.schema.mark_id(mark).expect("the mark type");
    run_command(
        state,
        &Formatter::new(Default::default()).toggle_style_mark(ty, Attrs::empty()),
    )
    .expect("the command runs")
    .expect("the command applies")
    .state()
    .clone()
}

/// A cursor toggle where no word is at the caret leaves an empty pair to type
/// into, and what is typed there becomes the style — the Typora-like flow.
/// In a word the toggle styles the word; see `formatting.rs`.
#[test]
fn toggling_at_a_cursor_leaves_a_pair_to_type_into() {
    let codec = Codec::new();
    let state = toggled(
        &codec,
        &edited(&codec, "a  b", Selection::cursor(3)),
        md::STRONG,
    );
    let state = run_command(&state, &insert_text("X"))
        .unwrap()
        .unwrap()
        .state()
        .clone();
    assert_eq!(to_markdown(&codec.schema, state.doc()).trim(), "a **X** b");
}

/// Taking a style off keeps every other mark that was inside it.
#[test]
fn toggling_a_style_off_keeps_the_marks_inside_it() {
    let codec = Codec::new();
    let state = edited(&codec, "**a *b* c**", Selection::text(3, 12));
    let after = toggled(&codec, &state, md::STRONG);
    assert_eq!(to_markdown(&codec.schema, after.doc()).trim(), "a *b* c");
}

/// A code span's fence is as long as its content needs, and the toggle has to
/// recognise the one that is actually there.
#[test]
fn toggling_off_a_code_span_with_a_long_fence() {
    let codec = Codec::new();
    let state = edited(&codec, "`` a`b ``", Selection::text(3, 8));
    let after = toggled(&codec, &state, md::CODE);
    assert!(
        !codec.describe(after.doc()).contains("code"),
        "{}",
        codec.describe(after.doc())
    );
}
