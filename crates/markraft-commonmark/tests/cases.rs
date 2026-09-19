//! One test per decision the codec makes.

mod common;

use common::{Codec, judge};
use markraft_commonmark::schema as md;
use markraft_commonmark::{commonmark_schema, commonmark_schema_spec};
use markraft_core::{Attrs, MarkSet, MarkTypeId, Node, attrs};

/// `parse` then `serialize`, which is the normalisation the codec promises.
fn round(source: &str) -> String {
    Codec::new().normalize(source)
}

fn shape(source: &str) -> String {
    let codec = Codec::new();
    codec.describe(&codec.parse(source))
}

// -- the schema preset ----------------------------------------------------

#[test]
fn the_preset_compiles_and_names_everything_it_documents() {
    let schema = commonmark_schema();
    for name in [
        md::DOC,
        md::PARAGRAPH,
        md::HEADING,
        md::BLOCKQUOTE,
        md::CODE_BLOCK,
        md::BULLET_LIST,
        md::ORDERED_LIST,
        md::LIST_ITEM,
        md::TASK_ITEM,
        md::HORIZONTAL_RULE,
        md::RAW_BLOCK,
        md::TEXT,
        md::IMAGE,
        md::HARD_BREAK,
    ] {
        assert!(schema.node_id(name).is_some(), "missing node type {name}");
    }
    for name in [
        md::LINK,
        md::STRONG,
        md::EM,
        md::STRIKETHROUGH,
        md::UNDERLINE,
        md::CODE,
    ] {
        assert!(schema.mark_id(name).is_some(), "missing mark type {name}");
    }
    // Both item kinds are in one group, so either list accepts either item.
    let item = schema.node_id(md::LIST_ITEM).expect("list_item");
    let task = schema.node_id(md::TASK_ITEM).expect("task_item");
    let bullet = schema.node_id(md::BULLET_LIST).expect("bullet_list");
    assert!(schema.can_contain(bullet, item) && schema.can_contain(bullet, task));
    // A hard break is tagged for the projection by its group, not a flag.
    let hard_break = schema.node_id(md::HARD_BREAK).expect("hard_break");
    assert!(markraft_core::projection::is_line_break(
        &schema, hard_break
    ));
}

#[test]
fn mark_ranks_order_the_nesting_the_serialiser_writes() {
    let schema = commonmark_schema();
    let rank = |name: &str| {
        schema
            .mark_type(schema.mark_id(name).expect("a preset mark"))
            .rank()
    };
    let order = [
        md::LINK,
        md::UNDERLINE,
        md::STRIKETHROUGH,
        md::STRONG,
        md::EM,
        md::CODE,
    ];
    for pair in order.windows(2) {
        assert!(
            rank(pair[0]) < rank(pair[1]),
            "{} must sit outside {}",
            pair[0],
            pair[1]
        );
    }
}

#[test]
fn the_spec_can_be_extended_before_it_is_compiled() {
    let spec = commonmark_schema_spec().node(
        markraft_core::NodeTypeSpec::new("callout", "block+")
            .group(md::BLOCK_GROUP)
            .defining(true),
    );
    let schema = markraft_core::Schema::new(spec).expect("the extended spec compiles");
    assert!(schema.node_id("callout").is_some());
    assert!(schema.node_id(md::PARAGRAPH).is_some());
}

// -- empty paragraphs -----------------------------------------------------

#[test]
fn an_empty_paragraph_is_a_line_holding_only_a_break_tag() {
    assert_eq!(round("a\n\n<br>\n\nb"), "a\n\n<br>\n\nb");
    assert_eq!(
        shape("a\n\n<br>\n\nb"),
        r#"doc(paragraph("a"), paragraph(), paragraph("b"))"#
    );
    // Either spelling of the tag imports.
    for tag in ["<br>", "<br/>", "<br />"] {
        assert_eq!(
            shape(&format!("a\n\n{tag}\n\nb")),
            r#"doc(paragraph("a"), paragraph(), paragraph("b"))"#,
            "{tag}"
        );
    }
}

#[test]
fn blank_lines_in_the_source_are_separators_not_empty_paragraphs() {
    assert_eq!(
        shape("a\n\n\n\nb"),
        r#"doc(paragraph("a"), paragraph("b"))"#
    );
    assert_eq!(round("a\n\n\n\nb"), "a\n\nb");
}

#[test]
fn an_empty_paragraph_that_is_all_its_parent_holds_writes_as_nothing() {
    // It *is* the empty container, which CommonMark writes by leaving it empty.
    // The space after the marker stays: it is what makes the marker complete.
    assert_eq!(round("- foo\n-\n- bar"), "- foo\n- \n- bar");
    assert_eq!(round(">"), "> ");
    assert_eq!(round(""), "");
    assert_eq!(shape(""), "doc(paragraph())");
    assert_eq!(shape(">"), "doc(blockquote(paragraph()))");
}

// -- tables ---------------------------------------------------------------

#[test]
fn a_table_is_rows_of_cells_with_the_alignments_on_the_table() {
    let source = "| a | b |\n| --- | --- |\n| 1 | 2 |";
    assert_eq!(
        shape(source),
        concat!(
            r#"doc(table[alignments=Str("none,none")]("#,
            r#"table_row(table_cell("a"), table_cell("b")), "#,
            r#"table_row(table_cell("1"), table_cell("2"))))"#
        )
    );
    // The columns are padded to one width, which is the only change a table
    // that is already square goes through.
    assert_eq!(round(source), "| a   | b   |\n| --- | --- |\n| 1   | 2   |");
    // Inside a container, where the prefix is put back on the way out.
    let quoted = "> | a | b |\n> | --- | --- |\n> | 1 | 2 |";
    assert_eq!(
        round(quoted),
        "> | a   | b   |\n> | --- | --- |\n> | 1   | 2   |"
    );
    // A header with no body is a table of one row.
    assert_eq!(
        shape("| a |\n| - |"),
        r#"doc(table[alignments=Str("none")](table_row(table_cell("a"))))"#
    );
    assert_eq!(round("| a |\n| - |"), "| a   |\n| --- |");
}

#[test]
fn every_alignment_survives_in_both_directions() {
    let source = "| l | c | r | n |\n| :-- | :-: | --: | --- |\n| 1 | 2 | 3 | 4 |";
    let codec = Codec::new();
    let doc = codec.parse(source);
    assert_eq!(
        doc.child(0)
            .attrs()
            .get("alignments")
            .and_then(|value| value.as_str()),
        Some("left,center,right,none")
    );
    assert_eq!(
        codec.write(&doc),
        "| l   | c   | r   | n   |\n| :-- | :-: | --: | --- |\n| 1   | 2   | 3   | 4   |"
    );
    assert!(judge(&codec, source).is_ok());
}

#[test]
fn a_pipe_in_a_cell_is_escaped_even_inside_a_code_span() {
    // GFM splits the row on its pipes before it reads a cell at all, so the
    // escape is resolved everywhere — code span included.
    let source = "| a\\|b | `c\\|d` |\n| - | - |\n| x | y |";
    assert_eq!(
        shape(source),
        concat!(
            r#"doc(table[alignments=Str("none,none")]("#,
            r#"table_row(table_cell("a|b"), table_cell("c|d"{code})), "#,
            r#"table_row(table_cell("x"), table_cell("y"))))"#
        )
    );
    assert_eq!(
        round(source),
        "| a\\|b | `c\\|d` |\n| ---- | ------ |\n| x    | y      |"
    );
    let codec = Codec::new();
    assert!(judge(&codec, source).is_ok());
}

#[test]
fn an_empty_cell_keeps_its_place() {
    let source = "|  | b |\n| - | - |\n| 1 |  |";
    assert_eq!(
        shape(source),
        concat!(
            r#"doc(table[alignments=Str("none,none")]("#,
            r#"table_row(table_cell(), table_cell("b")), "#,
            r#"table_row(table_cell("1"), table_cell())))"#
        )
    );
    // A cell is always a pair of spaces between pipes, never `||`, and the
    // column is still padded to the width the delimiter row needs.
    assert_eq!(round(source), "|     | b   |\n| --- | --- |\n| 1   |     |");
}

#[test]
fn a_ragged_table_is_squared_off_the_way_a_reader_squares_it() {
    // The delimiter row fixes the column count: a surplus cell is dropped and
    // a row that stops short is filled with empty cells.
    let source = "| a | b |\n| - | - |\n| 1 | 2 | 3 |\n| only |";
    assert_eq!(
        shape(source),
        concat!(
            r#"doc(table[alignments=Str("none,none")]("#,
            r#"table_row(table_cell("a"), table_cell("b")), "#,
            r#"table_row(table_cell("1"), table_cell("2")), "#,
            r#"table_row(table_cell("only"), table_cell())))"#
        )
    );
    let codec = Codec::new();
    assert!(judge(&codec, source).is_ok());
}

#[test]
fn a_cell_holds_the_marks_links_and_images_a_paragraph_holds() {
    let source = "| **b** *i* `c` [l](u) ![alt](p) |\n| - |\n| <u>u</u> |";
    assert_eq!(
        shape(source),
        concat!(
            r#"doc(table[alignments=Str("none")](table_row(table_cell("#,
            r#""b"{strong}, " ", "i"{em}, " ", "c"{code}, " ", "l"{link}, " ", "#,
            r#"image[alt=Str("alt"),src=Str("p"),title=Str("")])), "#,
            r#"table_row(table_cell("u"{underline}))))"#
        )
    );
    let codec = Codec::new();
    assert!(judge(&codec, source).is_ok());
    assert_eq!(round(source), round(&round(source)));
}

#[test]
fn a_column_is_padded_to_its_display_width_not_its_character_count() {
    // A CJK character is two columns wide in a fixed-width font, so padding by
    // character count would leave the pipes out of line.
    let written = round("| 中文 | b |\n| - | - |\n| x | 😀 |");
    assert_eq!(written, "| 中文 | b   |\n| ---- | --- |\n| x    | 😀  |");
    for line in written.lines() {
        assert_eq!(
            line.split('|').count(),
            3 + 1,
            "{line:?} has the wrong number of cells"
        );
    }
}

#[test]
fn a_list_holding_a_table_is_written_loose() {
    // A table needs a blank line before it — it cannot interrupt a paragraph —
    // and a blank line after it, or the next line is read as one more row.
    let source = "- item\n\n  | a |\n  | - |\n  | 1 |\n\n- next";
    assert_eq!(
        shape(source),
        concat!(
            r#"doc(bullet_list[bullet_char=Str("-"),tight=Bool(false)]("#,
            r#"list_item(paragraph("item"), table[alignments=Str("none")]("#,
            r#"table_row(table_cell("a")), table_row(table_cell("1")))), "#,
            r#"list_item(paragraph("next"))))"#
        )
    );
    let codec = Codec::new();
    assert!(judge(&codec, source).is_ok());
    let once = round(source);
    assert_eq!(once, round(&once));
    assert!(once.contains("  | a   |"), "{once}");
}

#[test]
fn a_list_item_that_is_only_a_table_stays_tight() {
    // Nothing follows the table inside the item, and the next item's marker
    // starts an item rather than one more row, so the list needs no blank line.
    let codec = Codec::new();
    for source in [
        "- a\n- | a |\n  | - |",
        "- | a |\n  | - |\n  | 1 |",
        "> - | a |\n>   | - |",
    ] {
        let once = round(source);
        assert_eq!(once, round(&once), "{source:?} does not settle");
        assert!(judge(&codec, source).is_ok(), "{source:?}");
        assert!(
            !once.contains("\n\n"),
            "{source:?} was written loose: {once}"
        );
    }
}

#[test]
fn a_hard_break_in_a_cell_is_a_break_tag() {
    // A row is one source line, so the break travels as the tag GFM renders.
    // It comes back as a raw inline primitive, which writes itself again.
    let codec = Codec::new();
    let schema = &codec.schema;
    let cell = schema
        .node(
            md::TABLE_CELL,
            [
                schema.text("a"),
                schema.node(md::HARD_BREAK, []).expect("a break"),
                schema.text("b"),
            ],
        )
        .expect("a cell");
    let row = schema.node(md::TABLE_ROW, [cell]).expect("a row");
    let table = schema
        .node_with(md::TABLE, attrs! {"alignments" => "none"}, [row])
        .expect("a table");
    let doc = schema.doc([table]).expect("a document");
    let written = codec.write(&doc);
    assert_eq!(written, "| a<br>b |\n| ------ |");
    assert_eq!(
        codec.describe(&codec.parse(&written)),
        concat!(
            r#"doc(table[alignments=Str("none")](table_row(table_cell("#,
            r#""a", raw_inline[source=Str("<br>")], "b"))))"#
        )
    );
    assert_eq!(codec.normalize(&written), written);
    // A `<br>` an author wrote in a cell reads the same way — a break there
    // has no spelling, so the source, not the tree, is the fixed point.
    assert_eq!(
        round("| a |\n| - |\n| x<br>y |"),
        "| a      |\n| ------ |\n| x<br>y |"
    );
}

// -- constructs with no model of their own --------------------------------

#[test]
fn an_html_block_and_a_footnote_definition_are_kept_verbatim() {
    assert_eq!(
        round("<div>\nraw <b>text</b>\n</div>"),
        "<div>\nraw <b>text</b>\n</div>"
    );
    // Footnotes are off, so a definition is the paragraph a plain CommonMark
    // reader sees — and no text is lost, which turning them on would risk.
    assert_eq!(round("[^1]: a footnote"), "\\[^1\\]: a footnote");
    assert_eq!(
        shape("[^1]: a footnote"),
        r#"doc(paragraph("[^1]: a footnote"))"#
    );
    assert_eq!(round("<!-- a comment -->"), "<!-- a comment -->");
}

#[test]
fn a_raw_block_holds_its_source_as_its_own_text() {
    // The text *is* the source, line endings and all and with no trailing one.
    // The editor shows that markup instead of rendering it, so the user edits
    // it the way they edit a code block.
    assert_eq!(
        shape("<div>\nraw <b>text</b>\n</div>"),
        "doc(raw_block(\"<div>\nraw <b>text</b>\n</div>\"))"
    );
    assert_eq!(
        shape("<!-- a comment -->"),
        "doc(raw_block(\"<!-- a comment -->\"))"
    );
    // A comment runs over as many lines as it likes and stays one block.
    assert_eq!(
        shape("<!-- one\ntwo\nthree -->"),
        "doc(raw_block(\"<!-- one\ntwo\nthree -->\"))"
    );
}

#[test]
fn a_raw_block_carries_the_prefix_of_every_container_it_sits_in() {
    // Each line of the source is written behind the list indent or the quote
    // bar, exactly as a code block's content is.
    let codec = Codec::new();
    for source in [
        "- <div>\n  x\n  </div>",
        "- a\n- <div>\n  x\n  </div>",
        "1. <div>\n   x\n   </div>",
        "> <div>\n> x\n> </div>",
        "> - <div>\n>   x\n>   </div>",
    ] {
        assert_eq!(round(source), source, "{source:?} was not written back");
        assert!(judge(&codec, source).is_ok(), "{source:?}");
    }
}

#[test]
fn a_raw_block_before_another_block_in_an_item_writes_the_list_loose() {
    // An HTML block runs on until a blank line, so a tight item would read the
    // paragraph under it as more of its own source. A loose list puts the blank
    // line there, and `tight` is cosmetic, so that is the one thing that gives.
    let codec = Codec::new();
    let schema = &codec.schema;
    let item = schema
        .node(
            md::LIST_ITEM,
            [
                schema
                    .node(md::RAW_BLOCK, [schema.text("<div>\nx\n</div>")])
                    .expect("a raw block"),
                schema
                    .node(md::PARAGRAPH, [schema.text("after")])
                    .expect("a paragraph"),
            ],
        )
        .expect("an item");
    let list = schema
        .node_with(
            md::BULLET_LIST,
            attrs! {"tight" => true, "bullet_char" => "-"},
            [item],
        )
        .expect("a list");
    let doc = schema.doc([list]).expect("a document");
    let written = codec.write(&doc);
    assert_eq!(written, "- <div>\n  x\n  </div>\n\n  after");
    assert_eq!(
        codec.describe(&codec.parse(&written)),
        concat!(
            r#"doc(bullet_list[bullet_char=Str("-"),tight=Bool(false)](list_item("#,
            "raw_block(\"<div>\nx\n</div>\"), paragraph(\"after\"))))"
        )
    );
    assert_eq!(codec.normalize(&written), written);
}

#[test]
fn an_edited_raw_block_is_read_as_whatever_its_text_has_become() {
    use markraft_core::commands::{delete_range, run_command};
    use markraft_core::{EditorState, EditorStateConfig, Selection};

    // Deleting the tags leaves text that is no longer an HTML block, and the
    // next parse reads it as the paragraph it now is. That is what making the
    // source editable means, and it is intended.
    let codec = Codec::new();
    let doc = codec.parse("<div>\nraw text\n</div>");
    let state = EditorState::create(
        EditorStateConfig::new(codec.schema.clone())
            .doc(doc)
            .selection(Selection::cursor(1)),
    )
    .expect("a valid starting state");
    let delete = |state: &EditorState, from, to| {
        run_command(state, &delete_range(from, to))
            .expect("the deletion applies")
            .expect("the transaction resolves")
            .state()
            .clone()
    };
    // `<div>\n` off the front, then `\n</div>` off the back.
    let state = delete(&state, 1, 7);
    let state = delete(&state, 9, 16);
    assert_eq!(codec.describe(state.doc()), r#"doc(raw_block("raw text"))"#);
    let written = codec.write(state.doc());
    assert_eq!(written, "raw text");
    assert_eq!(shape(&written), r#"doc(paragraph("raw text"))"#);
}

#[test]
fn an_emptied_raw_block_writes_nothing_at_all() {
    // A block whose text an edit removed has no source left to write, so it
    // writes nothing — not even the blank lines that would separate it — and
    // the next parse finds no block there.
    let codec = Codec::new();
    let schema = &codec.schema;
    let empty = schema.node(md::RAW_BLOCK, []).expect("an empty raw block");
    let paragraph = |text: &str| {
        schema
            .node(md::PARAGRAPH, [schema.text(text)])
            .expect("a paragraph")
    };
    let doc = schema
        .doc([paragraph("a"), empty.clone(), paragraph("b")])
        .expect("a document");
    assert_eq!(codec.write(&doc), "a\n\nb");
    assert_eq!(shape("a\n\nb"), r#"doc(paragraph("a"), paragraph("b"))"#);
    // On its own it leaves nothing at all, and the importer fills the empty
    // document back in.
    let doc = schema.doc([empty]).expect("a document");
    assert_eq!(codec.write(&doc), "");
    assert_eq!(shape(""), "doc(paragraph())");
}

#[test]
fn every_raw_block_shape_is_a_fixed_point_of_parse_and_write() {
    let codec = Codec::new();
    for source in [
        "<div>\nraw <b>text</b>\n</div>",
        "<!-- a comment -->",
        "<!-- one\ntwo\nthree -->",
        "<table><tr><td>a</td></tr></table>",
        "text\n\n<div>\nx\n</div>\n\nmore text",
        "- <div>\n  x\n  </div>",
        "- <div>\n  x\n  </div>\n\n  after",
        "> <div>\n> x\n> </div>",
        "> - <div>\n>   x\n>   </div>",
    ] {
        let doc = codec.parse(source);
        let written = codec.write(&doc);
        assert_eq!(codec.parse(&written), doc, "{source:?} does not settle");
        assert_eq!(codec.normalize(&written), written, "{source:?}");
        assert!(judge(&codec, source).is_ok(), "{source:?}");
    }
}

#[test]
fn an_empty_link_keeps_its_semantic_container() {
    // The empty container carries the link without inventing text.
    assert_eq!(shape("[](url)"), r#"doc(paragraph(inline_span{link}()))"#);
    assert_eq!(round("[](url)"), "[](url)");
}

// -- marks ----------------------------------------------------------------

#[test]
fn underline_uses_the_tag_convention_in_both_directions() {
    assert_eq!(round("<u>**kept**</u>"), "<u>**kept**</u>");
    assert_eq!(shape("<u>x</u>"), r#"doc(paragraph("x"{underline}))"#);
    assert_eq!(
        round("~~gone~~ and <u>**kept**</u>"),
        "~~gone~~ and <u>**kept**</u>"
    );
}

#[test]
fn html_emphasis_tags_import_as_marks_and_stray_ones_stay_raw() {
    assert_eq!(shape("<em>a</em>"), r#"doc(paragraph("a"{em}))"#);
    assert_eq!(
        shape("<strong>a</strong>"),
        r#"doc(paragraph("a"{strong}))"#
    );
    assert_eq!(
        shape("<del>a</del>"),
        r#"doc(paragraph("a"{strikethrough}))"#
    );
    // An unpaired tag is a raw HTML primitive, not escaped text.
    assert_eq!(
        shape("<em>a"),
        r#"doc(paragraph(raw_inline[source=Str("<em>")], "a"))"#
    );
    assert_eq!(round("<em>a"), "<em>a");
}

#[test]
fn an_anchor_with_nothing_but_a_destination_is_the_link_mark() {
    let codec = Codec::new();
    assert_eq!(
        shape("an <a href=\"https://example.com\">anchor</a> here"),
        r#"doc(paragraph("an ", "anchor"{link}, " here"))"#
    );
    // The mark holds a destination and a title and nothing else, so the anchor
    // is written back as the CommonMark link it is: a semantic round trip, and
    // a fixed point from there on.
    assert_eq!(
        round("an <a href=\"/u\" title=\"t\">anchor</a> here"),
        "an [anchor](/u \"t\") here"
    );
    assert_eq!(
        round("an [anchor](/u \"t\") here"),
        "an [anchor](/u \"t\") here"
    );
    // An entity in an attribute resolves on the way in and travels back out.
    assert_eq!(
        round("<a href=\"https://e.example/?a=1&amp;b=2\">x</a>"),
        "[x](https://e.example/?a=1&amp;b=2)"
    );
    // An anchor nests with the marks around it the way `<em>` does.
    assert_eq!(
        shape("<a href=\"/u\"><em>x</em> y</a>"),
        r#"doc(paragraph("x"{link,em}, " y"{link}))"#
    );
    // Anything the mark cannot hold keeps the tag as source text instead.
    for source in [
        "an <a>anchor</a> here",
        "an <a href=\"/u\" target=\"_blank\">anchor</a> here",
        "an <a class=\"x\" href=\"/u\">anchor</a> here",
        "an <a href=\"/u\">anchor here",
    ] {
        assert_eq!(round(source), source, "{source:?}");
        assert!(shape(source).contains("raw_inline"), "{source:?}");
    }
    for source in [
        "an <a href=\"/u\" title=\"t\">anchor</a> here",
        "an <a>anchor</a> here",
        "an <a href=\"/u\" target=\"_blank\">anchor</a> here",
    ] {
        judge(&codec, source).unwrap_or_else(|message| panic!("{message}"));
    }
}

#[test]
fn an_image_tag_is_the_image_atom_unless_it_says_more_than_one_holds() {
    let codec = Codec::new();
    for tag in [
        "<img src=\"x.png\" alt=\"img\">",
        "<img src=\"x.png\" alt=\"img\"/>",
        "<img src=\"x.png\" alt=\"img\" />",
    ] {
        let source = format!("see {tag} here");
        assert_eq!(
            shape(&source),
            concat!(
                r#"doc(paragraph("see ", "#,
                r#"image[alt=Str("img"),src=Str("x.png"),title=Str("")], " here"))"#
            ),
            "{tag}"
        );
        assert_eq!(round(&source), "see ![img](x.png) here", "{tag}");
        judge(&codec, &source).unwrap_or_else(|message| panic!("{message}"));
    }
    // A width, a class or a style would be lost, and an image needs a source.
    for source in [
        "see <img src=\"x.png\" width=\"20\"> here",
        "see <img class=\"icon\" src=\"x.png\"> here",
        "see <img alt=\"img\"> here",
    ] {
        assert_eq!(round(source), source, "{source:?}");
        assert!(shape(source).contains("raw_inline"), "{source:?}");
    }
}

#[test]
fn a_break_tag_is_a_hard_break_where_one_can_be_written_back() {
    let codec = Codec::new();
    for tag in ["<br>", "<br/>", "<br />"] {
        let source = format!("a{tag}b");
        assert_eq!(
            shape(&source),
            r#"doc(paragraph("a", hard_break, "b"))"#,
            "{tag}"
        );
        assert_eq!(round(&source), "a\\\nb", "{tag}");
        judge(&codec, &source).unwrap_or_else(|message| panic!("{message}"));
    }
    // A heading is one source line and a break at the end of a block is
    // dropped, so in both places the tag stays the primitive that writes
    // itself again. A tag carrying an attribute does too.
    for source in ["# a<br>b", "a<br>", "a<br class=\"x\">b"] {
        assert_eq!(round(source), source, "{source:?}");
        assert!(shape(source).contains("raw_inline"), "{source:?}");
    }
}

#[test]
fn a_code_span_carries_the_marks_around_it() {
    assert_eq!(shape("**`x`**"), r#"doc(paragraph("x"{strong,code}))"#);
    assert_eq!(shape("*`x`*"), r#"doc(paragraph("x"{em,code}))"#);
    assert_eq!(shape("[`x`](/u)"), r#"doc(paragraph("x"{link,code}))"#);
    for source in ["**`x`**", "*`x`*", "[`x`](/u)", "<u>~~`x`~~</u>"] {
        assert_eq!(round(source), source, "{source:?}");
    }
}

/// Every combination of the marks that can surround a code span, next to plain
/// text on both sides so the delimiters have to decide whether they can flank.
#[test]
fn every_mark_combination_round_trips_on_a_code_span() {
    let codec = Codec::new();
    let schema = &codec.schema;
    let outer = [
        md::LINK,
        md::UNDERLINE,
        md::STRIKETHROUGH,
        md::STRONG,
        md::EM,
    ];
    for bits in 0..(1u32 << outer.len()) {
        let mut marks = vec![schema.mark(md::CODE, Attrs::empty()).expect("code")];
        for (index, name) in outer.iter().enumerate() {
            if bits & (1 << index) == 0 {
                continue;
            }
            let attrs = if *name == md::LINK {
                attrs! {"href" => "/u", "title" => ""}
            } else {
                Attrs::empty()
            };
            marks.push(schema.mark(name, attrs).expect("a preset mark"));
        }
        let set = MarkSet::from_marks(schema, marks);
        assert_eq!(
            set.len(),
            bits.count_ones() as usize + 1,
            "a mark was dropped"
        );
        for (before, after) in [("a", "b"), ("a ", " b"), ("", ""), ("!", "!")] {
            let mut content = Vec::new();
            if !before.is_empty() {
                content.push(schema.text(before));
            }
            content.push(schema.text_marked("x", set.clone()));
            if !after.is_empty() {
                content.push(schema.text(after));
            }
            let doc = schema
                .doc([schema.node(md::PARAGRAPH, content).expect("a paragraph")])
                .expect("a document");
            let written = codec.write(&doc);
            assert_eq!(
                codec.parse(&written),
                doc,
                "{bits:05b} between {before:?} and {after:?} wrote {written:?}"
            );
        }
    }
}

#[test]
fn a_mark_whose_delimiter_cannot_flank_is_written_as_a_tag() {
    // A run that would sit next to punctuation cannot open or close emphasis.
    let codec = Codec::new();
    let schema = &codec.schema;
    let em = schema.mark(md::EM, Attrs::empty()).expect("em");
    let doc = schema
        .doc([schema
            .node(
                md::PARAGRAPH,
                [
                    schema.text("a"),
                    schema.text_marked("!", MarkSet::from_marks(schema, [em])),
                ],
            )
            .expect("a paragraph")])
        .expect("a document");
    // `a*!*` would not be emphasis: the run touches a word on one side and
    // punctuation on the other.
    assert_eq!(codec.write(&doc), "a<em>!</em>");
    assert_eq!(codec.parse(&codec.write(&doc)), doc);
}

// -- lists ----------------------------------------------------------------

#[test]
fn tight_and_loose_lists_keep_their_shape() {
    assert_eq!(
        shape("- a\n- b"),
        r#"doc(bullet_list[bullet_char=Str("-"),tight=Bool(true)](list_item(paragraph("a")), list_item(paragraph("b"))))"#
    );
    assert_eq!(round("- a\n- b"), "- a\n- b");
    assert_eq!(round("- a\n\n- b"), "- a\n\n- b");
    assert_eq!(
        shape("- a\n\n- b"),
        r#"doc(bullet_list[bullet_char=Str("-"),tight=Bool(false)](list_item(paragraph("a")), list_item(paragraph("b"))))"#
    );
}

#[test]
fn a_tight_list_that_cannot_stay_tight_is_written_loose() {
    // Two paragraphs in one item need a blank line, and a blank line is what
    // makes a list loose — so the attribute cannot be honoured.
    let codec = Codec::new();
    let schema = &codec.schema;
    let paragraph = |text: &str| {
        schema
            .node(md::PARAGRAPH, [schema.text(text)])
            .expect("a paragraph")
    };
    let item = schema
        .node(md::LIST_ITEM, [paragraph("a"), paragraph("b")])
        .expect("an item");
    let list = schema
        .node_with(
            md::BULLET_LIST,
            attrs! {"tight" => true, "bullet_char" => "-"},
            [item],
        )
        .expect("a list");
    let written = codec.write(&schema.doc([list]).expect("a document"));
    assert_eq!(written, "- a\n\n  b");
    assert_eq!(
        codec.describe(&codec.parse(&written)),
        r#"doc(bullet_list[bullet_char=Str("-"),tight=Bool(false)](list_item(paragraph("a"), paragraph("b"))))"#
    );
}

#[test]
fn ordered_lists_keep_their_start_and_delimiter_and_line_up() {
    assert_eq!(round("3. a\n4. b"), "3. a\n4. b");
    assert_eq!(round("3) a\n4) b"), "3) a\n4) b");
    assert_eq!(
        shape("3) a"),
        r#"doc(ordered_list[delimiter=Str(")"),start=Int(3),tight=Bool(true)](list_item(paragraph("a"))))"#
    );
    // Wider ordinals pad on the right, so every item's content starts in the
    // same column and no marker begins with a space.
    assert_eq!(round("9. a\n10. b"), "9.  a\n10. b");
    assert_eq!(round("- x\n- y"), "- x\n- y");
    assert_eq!(round("* x"), "* x");
    assert_eq!(round("+ x"), "+ x");
}

#[test]
fn two_lists_of_the_same_kind_are_kept_apart_by_their_markers() {
    let codec = Codec::new();
    let schema = &codec.schema;
    let list = |bullet: &str| {
        let item = schema
            .node(
                md::LIST_ITEM,
                [schema
                    .node(md::PARAGRAPH, [schema.text("x")])
                    .expect("a paragraph")],
            )
            .expect("an item");
        schema
            .node_with(
                md::BULLET_LIST,
                attrs! {"tight" => true, "bullet_char" => bullet},
                [item],
            )
            .expect("a list")
    };
    let doc = schema.doc([list("-"), list("-")]).expect("a document");
    let written = codec.write(&doc);
    assert_eq!(written, "- x\n\n\n* x");
    // Two lists, not one: the second took a different bullet.
    assert_eq!(
        codec.describe(&codec.parse(&written)),
        r#"doc(bullet_list[bullet_char=Str("-"),tight=Bool(true)](list_item(paragraph("x"))), bullet_list[bullet_char=Str("*"),tight=Bool(true)](list_item(paragraph("x"))))"#
    );
}

#[test]
fn task_items_carry_their_check_box_in_the_list_marker() {
    assert_eq!(round("- [ ] todo\n- [x] done"), "- [ ] todo\n- [x] done");
    assert_eq!(
        shape("- [x] done"),
        r#"doc(bullet_list[bullet_char=Str("-"),tight=Bool(true)](task_item[checked=Bool(true)](paragraph("done"))))"#
    );
    assert_eq!(round("1. [ ] todo"), "1. [ ] todo");
}

#[test]
fn a_list_in_a_quote_in_a_list_keeps_every_level() {
    let source = "- outer\n\n  > quoted\n  >\n  > - inner\n  >   - deeper";
    assert_eq!(round(source), source);
    assert!(shape(source).contains("blockquote(paragraph(\"quoted\"), bullet_list"));
}

#[test]
fn a_list_item_may_start_with_any_block() {
    assert_eq!(round("- > quoted"), "- > quoted");
    assert_eq!(round("- - nested"), "- - nested");
    assert!(shape("- - nested").contains("list_item(bullet_list"));
}

// -- code blocks ----------------------------------------------------------

#[test]
fn a_fence_outgrows_any_run_inside_the_block() {
    assert_eq!(round("````\n```\n````"), "````\n```\n````");
    assert_eq!(round("`````\n````\n`````"), "`````\n````\n`````");
    assert_eq!(
        shape("```\nx\n```"),
        r#"doc(code_block[fence_char=Str("`"),fence_length=Int(3),language=Str("")]("x"))"#
    );
}

#[test]
fn an_indented_code_block_is_written_back_fenced() {
    assert_eq!(round("    indented\n    more"), "```\nindented\nmore\n```");
    assert_eq!(
        shape("    indented"),
        r#"doc(code_block[fence_char=Str("`"),fence_length=Int(3),language=Str("")]("indented"))"#
    );
}

#[test]
fn a_tilde_fence_and_an_info_string_survive() {
    assert_eq!(round("~~~rust\nx\n~~~"), "~~~rust\nx\n~~~");
    assert_eq!(
        round("```rust\nlet x = 1;\n```"),
        "```rust\nlet x = 1;\n```"
    );
    assert_eq!(round("```\n```"), "```\n```");
}

// -- inline shapes --------------------------------------------------------

#[test]
fn a_soft_break_keeps_source_semantics_and_projects_as_space() {
    assert_eq!(
        shape("one\ntwo"),
        r#"doc(paragraph("one", soft_break, "two"))"#
    );
    assert_eq!(
        shape("one\\\ntwo"),
        r#"doc(paragraph("one", hard_break, "two"))"#
    );
    // Both spellings import; the backslash is what is written back, because it
    // survives an editor that strips trailing whitespace.
    assert_eq!(
        shape("one  \ntwo"),
        r#"doc(paragraph("one", hard_break, "two"))"#
    );
    assert_eq!(round("one  \ntwo"), "one\\\ntwo");
}

#[test]
fn a_break_cmark_cannot_write_is_dropped_rather_than_faked() {
    let codec = Codec::new();
    let schema = &codec.schema;
    let brk = || schema.node(md::HARD_BREAK, []).expect("a hard break");
    let trailing = schema
        .node(md::PARAGRAPH, [schema.text("a"), brk()])
        .expect("a paragraph");
    assert_eq!(
        codec.write(&schema.doc([trailing]).expect("a document")),
        "a"
    );
    let heading = schema
        .node_with(
            md::HEADING,
            attrs! {"level" => 1i64},
            [schema.text("a"), brk(), schema.text("b")],
        )
        .expect("a heading");
    assert_eq!(
        codec.write(&schema.doc([heading]).expect("a document")),
        "# a b"
    );
}

#[test]
fn images_carry_their_alt_and_title() {
    assert_eq!(
        round("![alt](src.png \"a title\")"),
        "![alt](src.png \"a title\")"
    );
    assert_eq!(
        shape("![alt](src.png \"a title\")"),
        r#"doc(paragraph(image[alt=Str("alt"),src=Str("src.png"),title=Str("a title")]))"#
    );
    // A label with markup flattens to the plain text CommonMark's `alt` holds.
    assert_eq!(
        shape("![*a*](s)"),
        r#"doc(paragraph(image[alt=Str("a"),src=Str("s"),title=Str("")]))"#
    );
}

#[test]
fn headings_import_from_both_spellings_and_leave_as_atx() {
    assert_eq!(round("Title\n====="), "# Title");
    assert_eq!(round("Title\n-----"), "## Title");
    assert_eq!(round("### Deep ###"), "### Deep");
    // A heading whose text ends in a hash keeps it.
    assert_eq!(round("# a \\#"), "# a \\#");
    assert_eq!(shape("# a \\#"), r#"doc(heading[level=Int(1)]("a #"))"#);
}

#[test]
fn a_link_reference_definition_is_resolved_into_an_inline_link() {
    assert_eq!(
        round("[foo][ref]\n\n[ref]: /url \"t\""),
        "[foo](/url \"t\")"
    );
    assert_eq!(
        shape("[foo]\n\n[foo]: /url"),
        r#"doc(paragraph("foo"{link}))"#
    );
}

// -- autolinks ------------------------------------------------------------

/// The `href` of the first link mark in the document, which is the half of an
/// autolink that `describe` does not show.
fn first_href(source: &str) -> String {
    fn find(node: &Node, ty: MarkTypeId) -> Option<String> {
        node.children().find_map(
            |child| match child.marks().iter().find(|mark| mark.ty == ty) {
                Some(mark) => mark
                    .attrs
                    .get("href")
                    .and_then(|value| value.as_str())
                    .map(str::to_string),
                None => find(child, ty),
            },
        )
    }
    let codec = Codec::new();
    let ty = codec.schema.mark_id(md::LINK).expect("the link mark");
    find(&codec.parse(source), ty).unwrap_or_else(|| panic!("no link in {source:?}"))
}

#[test]
fn a_bare_url_is_a_link_and_is_written_back_bare() {
    assert_eq!(
        round("See https://example.com now"),
        "See https://example.com now"
    );
    assert_eq!(
        shape("See https://example.com now"),
        r#"doc(paragraph("See ", "https://example.com"{link}, " now"))"#
    );
    assert_eq!(
        first_href("See https://example.com now"),
        "https://example.com"
    );
    // A `www.` address and an e-mail address carry the scheme the extension
    // gives them and still travel as the text the author typed.
    assert_eq!(
        round("Visit www.example.com today"),
        "Visit www.example.com today"
    );
    assert_eq!(
        first_href("Visit www.example.com today"),
        "http://www.example.com"
    );
    assert_eq!(
        round("Mail foo@bar.example now"),
        "Mail foo@bar.example now"
    );
    assert_eq!(
        first_href("Mail foo@bar.example now"),
        "mailto:foo@bar.example"
    );
    // An angle-bracket autolink is the same link, and reads back as the URL.
    assert_eq!(
        round("See <https://example.com> ok"),
        "See https://example.com ok"
    );
    // A URL is bare wherever a line can start, and inside a container.
    assert_eq!(round("# https://example.com"), "# https://example.com");
    assert_eq!(round("> https://example.com"), "> https://example.com");
    assert_eq!(round("- https://example.com"), "- https://example.com");
}

#[test]
fn punctuation_after_a_bare_url_stays_outside_the_link() {
    for (source, url) in [
        ("https://a.example/c. next", "https://a.example/c"),
        ("https://a.example, next", "https://a.example"),
        ("(https://a.example) next", "https://a.example"),
        ("https://a.example) next", "https://a.example"),
        // Parentheses that balance are part of the URL, so they stay in it.
        ("https://a.example/a(b)c next", "https://a.example/a(b)c"),
    ] {
        assert_eq!(round(source), source, "{source}");
        assert_eq!(first_href(source), url, "{source}");
    }
}

#[test]
fn a_url_under_another_mark_is_not_a_link_of_its_own() {
    // Emphasis around a link nests the other way round from the mark ranks, so
    // it keeps a container of its own; the URL inside it is still written bare.
    assert_eq!(round("*https://a.example*"), "<em>https://a.example</em>");
    assert_eq!(
        shape("*https://a.example*"),
        r#"doc(paragraph(inline_span{em}("https://a.example"{link})))"#
    );
    // A code span is literal, so there is no link in it at all.
    assert_eq!(round("`https://a.example`"), "`https://a.example`");
    assert_eq!(
        shape("`https://a.example`"),
        r#"doc(paragraph("https://a.example"{code}))"#
    );
    // A URL inside a label is that label, not a second link.
    let source = "[https://a.example](https://b.example)";
    assert_eq!(round(source), source);
    assert_eq!(
        shape(source),
        r#"doc(paragraph("https://a.example"{link}))"#
    );
    assert_eq!(first_href(source), "https://b.example");
}

#[test]
fn a_link_keeps_its_brackets_where_a_bare_url_would_not_read_back() {
    // A bare URL has nowhere to carry a title.
    let titled = "[https://a.example](https://a.example \"t\")";
    assert_eq!(round(titled), titled);
    // A host with no dot in it is no autolink, and neither is a label that
    // only happens to equal its destination.
    assert_eq!(
        round("<http://localhost>"),
        "[http://localhost](http://localhost)"
    );
    assert_eq!(round("[foo](foo)"), "[foo](foo)");
    // A URL the escaper has to touch would come back with the backslash in it.
    assert_eq!(
        round("https://a.example/a_(b) x"),
        "[https://a.example/a\\_(b)](https://a.example/a_(b)) x"
    );
    // A hard break writes a backslash a reader would pull into the URL.
    let codec = Codec::new();
    let schema = &codec.schema;
    let mark = schema
        .mark(
            md::LINK,
            attrs! {"href" => "https://a.example", "title" => ""},
        )
        .expect("a link mark");
    let paragraph = schema
        .node(
            md::PARAGRAPH,
            [
                schema.text_marked("https://a.example", MarkSet::from_marks(schema, [mark])),
                schema.node(md::HARD_BREAK, []).expect("a hard break"),
                schema.text("x"),
            ],
        )
        .expect("a paragraph");
    assert_eq!(
        codec.write(&schema.doc([paragraph]).expect("a document")),
        "[https://a.example](https://a.example)\\\nx"
    );
}

#[test]
fn text_that_only_looks_like_a_url_is_kept_from_becoming_one() {
    // The escape in the source keeps the address out of a link, and the text
    // it leaves has to be written so that it stays out.
    assert_eq!(
        round("<foo\\+@bar.example.com>"),
        "\\<foo+\\@bar.example.com>"
    );
    assert_eq!(
        shape("<foo\\+@bar.example.com>"),
        r#"doc(paragraph("<foo+@bar.example.com>"))"#
    );
    let codec = Codec::new();
    // Each of the three shapes the extension reads keeps its escape.
    for source in [
        "http\\://example.com",
        "www\\.example.com",
        "foo\\@bar.example",
    ] {
        assert_eq!(round(source), source, "{source}");
        assert!(!shape(source).contains("{link}"), "{source}");
        assert!(judge(&codec, source).is_ok(), "{source}");
    }
    assert!(judge(&codec, "<foo\\+@bar.example.com>").is_ok());
}

#[test]
fn every_autolink_shape_renders_the_same_and_settles() {
    let codec = Codec::new();
    for source in [
        "See https://example.com now",
        "Visit www.example.com today",
        "Mail foo@bar.example now",
        "See <https://example.com> ok",
        "https://a.example/c. next",
        "(https://a.example) next",
        "https://a.example/a(b)c next",
        "*https://a.example*",
        "`https://a.example`",
        "[https://a.example](https://b.example)",
        "[https://a.example](https://a.example \"t\")",
        "<http://localhost>",
        "https://a.example/a_(b) x",
        "# https://example.com",
        "> https://example.com",
        "- https://example.com",
    ] {
        assert!(judge(&codec, source).is_ok(), "{source}");
        let once = round(source);
        assert_eq!(once, round(&once), "{source} does not settle");
    }
}

// -- the whole thing ------------------------------------------------------

#[test]
fn a_document_with_everything_in_it_settles() {
    let source = "\
# Title

A paragraph with **bold**, *em*, ~~struck~~, <u>underlined</u>, `code` and a
[link](https://example.com \"t\").

> A quote
>
> - with a list
>   - nested

1. one
2. two

- [ ] todo
- [x] done

```rust
let x = 1;
```

| a | b |
| --- | --- |
| 1 | 2 |

***
";
    let once = round(source);
    assert_eq!(once, round(&once));
    let codec = Codec::new();
    let doc: Node = codec.parse(&once);
    doc.check(&codec.schema).expect("a valid document");
}

#[test]
fn a_thematic_break_is_dashes_unless_that_would_read_as_something_else() {
    let codec = Codec::new();
    // On its own, and after a blank line, three dashes are a thematic break.
    assert_eq!(codec.normalize("***"), "---");
    assert_eq!(codec.normalize("a\n\n***\n\nb"), "a\n\n---\n\nb");
    assert_eq!(codec.normalize("> a\n\n> ***"), "> a\n\n> ---");
    // A `-` marker and three dashes are four dashes, which is a break itself.
    // `* ***` is likewise all stars, so it is a break rather than an item.
    assert_eq!(codec.normalize("- ***"), "- ***");
    assert_eq!(codec.normalize("* ***"), "---");
    assert_eq!(codec.normalize("+ ***"), "+ ---");
    assert_eq!(codec.normalize("1. ***"), "1. ---");
    // Directly under a line of text, three dashes underline it.
    assert_eq!(codec.normalize("* a\n\n  ***"), "* a\n\n  ---");
    assert_eq!(codec.normalize("* a\n  ***"), "* a\n  ***");
    // And every one of them still reads back as a thematic break.
    for source in ["---", "- ***", "+ ***", "* a\n  ***"] {
        assert_eq!(
            codec.normalize(source),
            codec.normalize(&codec.normalize(source))
        );
        assert!(judge(&codec, source).is_ok(), "{source}");
    }
}
