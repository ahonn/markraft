//! One test per decision the codec makes.

mod common;

use common::{Codec, judge};
use markraft_commonmark::schema as md;
use markraft_commonmark::{commonmark_schema, commonmark_schema_spec};
use markraft_core::{Attrs, MarkSet, Node, attrs};

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

// -- constructs with no model of their own --------------------------------

#[test]
fn a_table_is_kept_verbatim() {
    let source = "| a | b |\n| --- | --- |\n| 1 | 2 |";
    assert_eq!(
        shape(source),
        r#"doc(raw_block[source=Str("| a | b |\n| --- | --- |\n| 1 | 2 |")])"#
    );
    assert_eq!(round(source), source);
    // Including inside a container, where the prefix is put back on the way out.
    let quoted = "> | a | b |\n> | --- | --- |\n> | 1 | 2 |";
    assert_eq!(round(quoted), quoted);
}

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
