//! Plain text, and the slice codecs a clipboard needs.

mod common;

use common::Codec;
use markraft_commonmark::schema as md;
use markraft_commonmark::{slice_to_plain_text, to_plain_text};
use markraft_core::commands::replace_selection_changes;
use markraft_core::{ChangeSet, Node, Selection, Slice, attrs};

fn paste(codec: &Codec, doc: &Node, from: usize, to: usize, slice: &Slice) -> Node {
    // What a host's paste command does: the model knows how to open a textblock
    // for closed block content and how to let an open slice merge.
    let changes = replace_selection_changes(&codec.schema, doc, from, to, slice);
    let set = ChangeSet::create(&codec.schema, doc, changes).expect("the paste is expressible");
    let pasted = set.apply(doc).expect("the paste applies");
    pasted.check(&codec.schema).expect("the paste is valid");
    pasted
}

// -- plain text -----------------------------------------------------------

#[test]
fn plain_text_is_one_line_per_block_and_per_break() {
    let codec = Codec::new();
    let doc = codec.parse("# Title\n\nsome *text*\n\n- one\n- two");
    assert_eq!(
        to_plain_text(&codec.schema, &doc),
        "Title\nsome text\none\ntwo"
    );
    let broken = codec.parse("a\\\nb");
    assert_eq!(to_plain_text(&codec.schema, &broken), "a\nb");
}

#[test]
fn plain_text_reads_an_atom_as_the_text_it_stands_for() {
    let codec = Codec::new();
    let doc = codec.parse("![a diagram](x.png)\n\n<div>\nraw\n</div>");
    assert_eq!(
        to_plain_text(&codec.schema, &doc),
        "a diagram\n<div>\nraw\n</div>"
    );
}

#[test]
fn plain_text_reads_a_wiki_link_as_its_label() {
    let codec = Codec::new();
    // A link with no label of its own reads as the target it names, `#heading` and
    // all, and one with an alias reads as the alias — what the editor draws for it.
    // The spacing a source keeps inside the brackets is not part of either.
    let doc = codec.parse("See [[ Note#H ]] and ![[x.png|Cover]]");
    assert_eq!(to_plain_text(&codec.schema, &doc), "See Note#H and Cover");
    // A line built around a link still reads as a whole sentence. Notes are named
    // after their first such line, so a link contributing nothing would file this
    // one under "kickoff" alone.
    let opening = codec.parse("[[Project Alpha]] kickoff");
    assert_eq!(
        to_plain_text(&codec.schema, &opening),
        "Project Alpha kickoff"
    );
}

#[test]
fn plain_text_reads_a_table_as_tab_separated_rows() {
    let codec = Codec::new();
    let doc = codec.parse("| a | b |\n| - | - |\n| 1 |  |");
    assert_eq!(to_plain_text(&codec.schema, &doc), "a\tb\n1\t");
    // And so does a copied one, which is what a spreadsheet pastes.
    let slice = doc.slice(0, doc.content_size()).expect("a slice");
    assert_eq!(slice_to_plain_text(&codec.schema, &slice), "a\tb\n1\t");
}

#[test]
fn a_slice_has_plain_text_too() {
    let codec = Codec::new();
    let doc = codec.parse("one\n\ntwo");
    let slice = doc.slice(1, 9).expect("a slice");
    assert_eq!(slice_to_plain_text(&codec.schema, &slice), "one\ntwo");
}

// -- pasting --------------------------------------------------------------

#[test]
fn a_pasted_word_keeps_the_space_that_holds_it_apart() {
    let codec = Codec::new();
    let doc = codec.parse("tail");
    let fragment = codec
        .parser
        .parse_fragment("hello ")
        .expect("a fragment parses");
    let pasted = paste(&codec, &doc, 1, 1, &fragment);
    assert_eq!(to_plain_text(&codec.schema, &pasted), "hello tail");
    assert_eq!(codec.describe(&pasted), r#"doc(paragraph("hello tail"))"#);
}

#[test]
fn trailing_whitespace_survives_in_every_shape_a_fragment_takes() {
    let codec = Codec::new();
    for (source, expected) in [
        ("hello ", "hello tail"),
        ("**bold** ", "bold tail"),
        ("hello\t", "hello\ttail"),
        (" \t", " \ttail"),
        ("", "tail"),
    ] {
        let doc = codec.parse("tail");
        let fragment = codec
            .parser
            .parse_fragment(source)
            .expect("a fragment parses");
        let pasted = paste(&codec, &doc, 1, 1, &fragment);
        assert_eq!(
            to_plain_text(&codec.schema, &pasted),
            expected,
            "{source:?}"
        );
    }
}

#[test]
fn a_single_paragraph_merges_into_the_block_the_caret_is_in() {
    let codec = Codec::new();
    let fragment = codec.parser.parse_fragment("a **b**").expect("parses");
    assert_eq!(fragment.open_start(), 1);
    assert_eq!(fragment.open_end(), 1);
    let doc = codec.parse("xy");
    let pasted = paste(&codec, &doc, 2, 2, &fragment);
    assert_eq!(
        codec.describe(&pasted),
        r#"doc(paragraph("xa ", "**"{strong,syntax}, "b"{strong}, "**"{strong,syntax}, "y"))"#
    );
}

#[test]
fn a_multi_block_fragment_keeps_its_blocks() {
    let codec = Codec::new();
    let fragment = codec
        .parser
        .parse_fragment("# Title\n\n- one\n- two")
        .expect("parses");
    let doc = codec.parse("beforeafter");
    let pasted = paste(&codec, &doc, 7, 7, &fragment);
    let written = codec.write(&pasted);
    assert_eq!(written, "before\n\n# Title\n\n- one\n- two\n\nafter");
    assert_eq!(codec.parse(&written), pasted);
}

// -- copying --------------------------------------------------------------

#[test]
fn a_cut_inside_one_paragraph_copies_as_bare_text() {
    let codec = Codec::new();
    let doc = codec.parse("hello **world**");
    // Method-B: cutting mid-span keeps the opening delimiter characters that
    // fall inside the slice; the mark rule does not invent a closing pair.
    let slice = doc.slice(1, 13).expect("a slice");
    assert_eq!(codec.serializer.serialize_fragment(&slice), "hello **worl");
}

#[test]
fn a_cut_spanning_two_list_items_copies_as_a_list() {
    let codec = Codec::new();
    let doc = codec.parse("- one\n- two");
    // From inside the first item's text to inside the second's.
    let selection = Selection::text(4, 12);
    let slice = selection.content(&doc);
    assert_eq!(slice.open_start(), 2);
    assert_eq!(slice.open_end(), 2);
    let written = codec.serializer.serialize_fragment(&slice);
    assert_eq!(written, "- ne\n- tw");

    // Markdown cannot say "a list item without its list", so what comes back is
    // the list itself, closed — which pastes as the same two items.
    let back = codec.parser.parse_fragment(&written).expect("parses");
    assert_eq!((back.open_start(), back.open_end()), (0, 0));
    let empty = codec.parse("");
    assert_eq!(
        codec.describe(&paste(&codec, &empty, 0, 0, &back)),
        r#"doc(bullet_list[bullet_char=Str("-"),tight=Bool(true)](list_item(paragraph("ne")), list_item(paragraph("tw"))), paragraph())"#
    );
}

#[test]
fn a_parsed_fragment_is_a_fixed_point_of_the_fragment_codec() {
    let codec = Codec::new();
    for source in [
        "hello ",
        "a **b** c",
        "# Title\n\nbody",
        "- one\n- two",
        "> quoted\n\n```rust\nx\n```",
        "| a |\n| - |",
    ] {
        let once = codec.parser.parse_fragment(source).expect("parses");
        let written = codec.serializer.serialize_fragment(&once);
        let twice = codec
            .parser
            .parse_fragment(&written)
            .expect("the fragment reparses");
        assert_eq!(twice, once, "{source:?} wrote {written:?}");
    }
}

#[test]
fn an_empty_selection_copies_as_nothing() {
    let codec = Codec::new();
    let doc = codec.parse("text");
    let slice = Selection::cursor(2).content(&doc);
    assert!(slice.is_empty());
    assert_eq!(codec.serializer.serialize_fragment(&slice), "");
}

#[test]
fn a_node_selection_copies_the_whole_node() {
    let codec = Codec::new();
    let schema = &codec.schema;
    let rule = schema
        .node(md::HORIZONTAL_RULE, [])
        .expect("a thematic break");
    let paragraph = schema
        .node_with(md::PARAGRAPH, attrs! {}, [schema.text("x")])
        .expect("a paragraph");
    let doc = schema.doc([paragraph, rule]).expect("a document");
    let slice = Selection::node(3).content(&doc);
    assert_eq!(codec.serializer.serialize_fragment(&slice), "---");
}

// -- pasting each flavour ---------------------------------------------------------

/// A plain-text paste is the characters it is: `*`, `_` and `#` that would
/// read as Markdown arrive escaped, in the tree as in the file.
#[test]
fn pasted_plain_text_stays_literal() {
    use markraft_commonmark::CommonMarkCodecs;
    use markraft_core::Codecs;
    let codec = Codec::new();
    let codecs = CommonMarkCodecs::new(codec.schema.clone());
    let doc = codec.parse("x");
    let pasted = paste(&codec, &doc, 2, 2, &codecs.from_text("*a* _b_\n# c"));
    let file = codec.write(&pasted);
    assert_eq!(file, "x\\*a\\* \\_b\\_\n\n\\# c");
    assert_eq!(to_plain_text(&codec.schema, &pasted), "x*a* _b_\n# c");
}

/// Markdown pastes as the source it is, and HTML as the source that spells it.
#[test]
fn pasted_markdown_and_html_arrive_as_source() {
    use markraft_commonmark::CommonMarkCodecs;
    use markraft_core::Codecs;
    let codec = Codec::new();
    let codecs = CommonMarkCodecs::new(codec.schema.clone());
    let doc = codec.parse("x");
    let markdown = codecs.from_markup("__a__ *b*").expect("a fragment");
    assert_eq!(
        codec.write(&paste(&codec, &doc, 2, 2, &markdown)),
        "x__a__ *b*"
    );
    let html = codecs
        .from_html("<b>a</b> <a href=\"u)\">c</a>")
        .expect("a fragment");
    let pasted = paste(&codec, &doc, 2, 2, &html);
    assert_eq!(
        common::html(&codec.write(&pasted)),
        common::html("x**a** [c](<u)>)")
    );
}

/// What a copy carries is the selection with the spelling its styles need;
/// its Markdown flavour stays the characters selected.
#[test]
fn a_copy_of_a_spans_inside_carries_its_delimiters() {
    use markraft_commonmark::CommonMarkCodecs;
    use markraft_core::Codecs;
    let codec = Codec::new();
    let codecs = CommonMarkCodecs::new(codec.schema.clone());
    let doc = codec.parse("a *b **c** d* e\n\nf");
    // `b **c** d`, inside the emphasis.
    let slice = doc.slice(4, 13).expect("a slice");
    assert_eq!(codecs.to_markup(&slice).as_deref(), Some("b **c** d"));
    let carried = codecs.copied(&slice);
    let pasted = paste(&codec, &doc, 19, 19, &carried);
    assert_eq!(codec.write(&pasted), "a *b **c** d* e\n\nf*b **c** d*");
    // A copy that already spells its styles travels unchanged.
    let whole = doc.slice(3, 14).expect("a slice");
    assert_eq!(codecs.copied(&whole), whole);
}
