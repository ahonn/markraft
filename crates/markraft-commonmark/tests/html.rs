//! HTML import: the clipboard's rich flavour.
//!
//! The cases are the ones `markraft-core`'s importer was hardened against,
//! carried over wherever the tree model keeps the expectation.

mod common;

use common::Codec;
use markraft_commonmark::html::{HtmlParser, HtmlRule, HtmlRules, commonmark_html_rules};
use markraft_commonmark::{commonmark_schema, slice_to_plain_text, to_plain_text};

fn parser() -> HtmlParser {
    HtmlParser::commonmark(commonmark_schema())
}

/// The document an HTML source imports as, described.
fn shape(html: &str) -> String {
    let codec = Codec::new();
    let doc = parser().parse(html).expect("HTML parses");
    doc.check(&codec.schema).expect("a valid document");
    codec.describe(&doc)
}

/// The Markdown an HTML source imports as.
fn markdown(html: &str) -> String {
    let codec = Codec::new();
    codec.write(&parser().parse(html).expect("HTML parses"))
}

#[test]
fn semantic_styles_nested_lists_and_code_come_across() {
    let html = "<h2>Title</h2>\n<p>Hello <strong>bold</strong> \
        <a href='https://example.com?a=1&amp;b=2'><em>link</em></a></p>\
        <ul><li>Parent<ul><li>Child</li></ul></li><li><input type='checkbox' checked>Done</li></ul>\
        <pre><code class='language-rust'>let x = 1;\n  x</code></pre>";
    assert_eq!(
        markdown(html),
        "## Title\n\n\
         Hello **bold** [*link*](https://example.com?a=1&amp;b=2)\n\n\
         - Parent\n  - Child\n- [x] Done\n\n\
         ```rust\nlet x = 1;\n  x\n```"
    );
}

#[test]
fn a_quote_in_a_quote_keeps_both_levels() {
    assert_eq!(
        shape("<blockquote><p>Outer</p><blockquote>Inner</blockquote></blockquote>"),
        r#"doc(blockquote(paragraph("Outer"), blockquote(paragraph("Inner"))))"#
    );
}

#[test]
fn scripts_and_styles_are_dropped_and_images_are_kept() {
    let html = "<p>A</p><script>secret()</script><style>p{}</style>\
        <p><img alt='diagram' src='image.png'></p>";
    let doc = parser().parse(html).expect("parses");
    let schema = commonmark_schema();
    assert!(!to_plain_text(&schema, &doc).contains("secret"));
    assert_eq!(markdown(html), "A\n\n![diagram](image.png)");
}

#[test]
fn inline_css_is_read_as_the_marks_it_stands_for() {
    assert_eq!(
        shape("<p><span style='font-weight:700;text-decoration:underline'>style</span></p>"),
        r#"doc(paragraph("style"{underline,strong}))"#
    );
    assert_eq!(
        shape("<p><span style='font-style:italic;text-decoration:line-through'>x</span></p>"),
        r#"doc(paragraph("x"{strikethrough,em}))"#
    );
}

#[test]
fn a_wrapped_task_item_imports_once_with_its_children() {
    let html = "<ul><li data-type='taskItem' data-checked='true'>\
        <label><input type='checkbox' checked></label>\
        <div><p>Done</p><ul><li data-type='taskItem' data-checked='false'>\
        <label><input type='checkbox'></label><div><p>Child</p></div></li></ul></div></li></ul>";
    assert_eq!(markdown(html), "- [x] Done\n  - [ ] Child");
    // A plain `<li>` that owns a check box is a task item too.
    assert_eq!(
        markdown(
            "<ul><li><label><input type='checkbox' checked></label><div><p>Done</p></div></li></ul>"
        ),
        "- [x] Done"
    );
    // A check box inside a nested list belongs to that list's item.
    assert_eq!(
        markdown("<ul><li>Parent<ul><li><input type='checkbox'>Child</li></ul></li></ul>"),
        "- Parent\n  - [ ] Child"
    );
}

#[test]
fn a_break_is_a_line_break_unless_it_stands_alone() {
    assert_eq!(
        shape("<p>A<br>B</p>"),
        r#"doc(paragraph("A", hard_break, "B"))"#
    );
    // An editor's placeholder for an empty paragraph.
    assert_eq!(shape("<p><br></p>"), "doc(paragraph())");
    assert_eq!(
        shape("<p>A</p><p><br></p><p>B</p>"),
        r#"doc(paragraph("A"), paragraph(), paragraph("B"))"#
    );
}

#[test]
fn whitespace_is_collapsed_the_way_a_browser_lays_it_out() {
    assert_eq!(
        shape("<p>  a\n\t b  </p>"),
        r#"doc(paragraph("a b"))"#,
        "runs collapse, and the edges go"
    );
    assert_eq!(
        shape("<p>a <strong> b </strong> c</p>"),
        r#"doc(paragraph("a ", "b "{strong}, "c"))"#,
        "a space between styled runs still separates the words"
    );
    // A non-breaking space is not whitespace.
    assert_eq!(shape("<p>a\u{a0}b</p>"), "doc(paragraph(\"a\u{a0}b\"))");
    // Text inside `<pre>` is taken exactly as written.
    assert_eq!(
        shape("<pre>  two\n    four</pre>"),
        r#"doc(code_block[fence_char=Str("`"),fence_length=Int(3),language=Str("")]("  two
    four"))"#
    );
}

#[test]
fn loose_text_becomes_blocks_at_the_boundaries_around_it() {
    assert_eq!(
        shape("a<div>b</div>c"),
        r#"doc(paragraph("a"), paragraph("b"), paragraph("c"))"#
    );
    assert_eq!(shape("bare text"), r#"doc(paragraph("bare text"))"#);
}

#[test]
fn an_unknown_element_preserves_boundaries_and_editable_children() {
    assert_eq!(
        shape("<p>a <custom-tag>b <b>c</b></custom-tag></p>"),
        r#"doc(paragraph("a ", raw_inline[source=Str("<custom-tag>")], "b ", "c"{strong}, raw_inline[source=Str("</custom-tag>")]))"#
    );
}

#[test]
fn every_styling_tag_has_a_mark() {
    for (tag, mark) in [
        ("strong", "strong"),
        ("b", "strong"),
        ("em", "em"),
        ("i", "em"),
        ("s", "strikethrough"),
        ("del", "strikethrough"),
        ("strike", "strikethrough"),
        ("u", "underline"),
        ("ins", "underline"),
        ("code", "code"),
    ] {
        assert_eq!(
            shape(&format!("<p><{tag}>x</{tag}></p>")),
            format!("doc(paragraph(\"x\"{{{mark}}}))"),
            "<{tag}>"
        );
    }
}

#[test]
fn an_ordered_list_keeps_the_ordinal_it_starts_at() {
    assert_eq!(
        markdown("<ol start='3'><li>a</li><li>b</li></ol>"),
        "3. a\n4. b"
    );
    assert_eq!(markdown("<ol><li>a</li></ol>"), "1. a");
    assert_eq!(markdown("<hr>"), "---");
}

#[test]
fn html_and_markdown_agree_on_the_same_document() {
    let codec = Codec::new();
    for (html, markdown) in [
        ("<h1>T</h1><p>body</p>", "# T\n\nbody"),
        ("<ul><li>a</li><li>b</li></ul>", "- a\n- b"),
        ("<blockquote><p>q</p></blockquote>", "> q"),
        ("<p>a<br>b</p>", "a\\\nb"),
        ("<p><a href='/u' title='t'>x</a></p>", "[x](/u \"t\")"),
    ] {
        assert_eq!(
            codec.describe(&parser().parse(html).expect("HTML parses")),
            codec.describe(&codec.parse(markdown)),
            "{html}"
        );
    }
}

#[test]
fn a_fragment_opens_the_same_way_markdown_does() {
    let parser = parser();
    let inline = parser.parse_fragment("<p>hello</p>").expect("parses");
    assert_eq!((inline.open_start(), inline.open_end()), (1, 1));
    let blocks = parser.parse_fragment("<h1>a</h1><p>b</p>").expect("parses");
    assert_eq!((blocks.open_start(), blocks.open_end()), (0, 0));
}

#[test]
fn the_rule_table_can_be_replaced() {
    // A consumer that wants `<mark>` to mean something registers it, and one
    // that wants an element dropped says so.
    let rules = commonmark_html_rules()
        .with("mark", HtmlRule::mark("strong"))
        .with("aside", HtmlRule::Ignore);
    let parser = HtmlParser::new(commonmark_schema(), rules);
    let codec = Codec::new();
    let doc = parser
        .parse("<p><mark>hit</mark></p><aside>gone</aside>")
        .expect("parses");
    assert_eq!(codec.describe(&doc), r#"doc(paragraph("hit"{strong}))"#);
    // An empty table keeps the text and nothing else.
    let bare = HtmlParser::new(commonmark_schema(), HtmlRules::new());
    assert_eq!(
        codec.describe(&bare.parse("<p>a</p><p>b</p>").expect("parses")),
        r#"doc(paragraph("ab"))"#
    );
}

// -- writing ----------------------------------------------------------------

fn serializer() -> markraft_commonmark::HtmlSerializer {
    markraft_commonmark::HtmlSerializer::commonmark(&commonmark_schema())
}

/// The HTML a Markdown source writes as.
fn html_of(source: &str) -> String {
    serializer().serialize(&Codec::new().parse(source))
}

#[test]
fn a_document_writes_as_the_html_another_application_expects() {
    assert_eq!(
        html_of("## Title\n\nHello **bold** [*link*](https://x.example \"t\")"),
        "<h2>Title</h2>\n<p>Hello <strong>bold</strong> \
         <a href=\"https://x.example\" title=\"t\"><em>link</em></a></p>"
    );
    assert_eq!(
        html_of("- one\n  - two\n\n1. a\n1. b"),
        "<ul>\n<li><p>one</p>\n<ul>\n<li><p>two</p></li>\n</ul></li>\n</ul>\n\
         <ol>\n<li><p>a</p></li>\n<li><p>b</p></li>\n</ol>"
    );
    assert_eq!(
        html_of("> quoted\n\n***\n\n![a](b.png)"),
        "<blockquote>\n<p>quoted</p>\n</blockquote>\n<hr>\n<p><img src=\"b.png\" alt=\"a\"></p>"
    );
    assert_eq!(
        html_of("```rust\nlet x = 1;\n```"),
        "<pre><code class=\"language-rust\">let x = 1;\n</code></pre>"
    );
    // An empty paragraph is a line holding only a break, in both directions.
    assert_eq!(html_of("a\n\n<br>\n\nb"), "<p>a</p>\n<p><br></p>\n<p>b</p>");
    assert_eq!(
        shape("<p>a</p><p><br></p><p>b</p>"),
        shape("<p>a</p><p></p><p>b</p>")
    );
}

#[test]
fn a_task_item_carries_its_box_and_its_state_both_ways() {
    let written = html_of("- [x] done\n- [ ] todo");
    assert!(
        written.contains(
            "<li data-type=\"taskItem\" data-checked=\"true\">\
             <input type=\"checkbox\" checked disabled><p>done</p></li>"
        ),
        "{written}"
    );
    assert_eq!(markdown(&written), "- [x] done\n- [ ] todo");
    // A box written by another editor is read the same way.
    assert_eq!(
        markdown("<ul><li><input type=checkbox checked>done</li></ul>"),
        "- [x] done"
    );
}

#[test]
fn text_and_attributes_are_escaped() {
    assert_eq!(html_of("a < b & c > d"), "<p>a &lt; b &amp; c &gt; d</p>");
    assert_eq!(
        html_of("[x](https://e.example/?a=1&b=<2>)"),
        "<p><a href=\"https://e.example/?a=1&amp;b=&lt;2&gt;\">x</a></p>"
    );
    assert_eq!(
        html_of("`<script>alert(1)</script>`"),
        "<p><code>&lt;script&gt;alert(1)&lt;/script&gt;</code></p>"
    );
    // A quote in an attribute cannot end it.
    let schema = commonmark_schema();
    let link = schema
        .mark(
            markraft_commonmark::schema::LINK,
            markraft_core::attrs! {"href" => "a\"b", "title" => "c\"d"},
        )
        .expect("a link");
    let marks = markraft_core::MarkSet::from_marks(&schema, [link]);
    let paragraph = schema
        .node(
            markraft_commonmark::schema::PARAGRAPH,
            [schema.text_marked("x", marks)],
        )
        .expect("a paragraph");
    let doc = schema.doc([paragraph]).expect("a document");
    let written = serializer().serialize(&doc);
    assert_eq!(
        written,
        "<p><a href=\"a&quot;b\" title=\"c&quot;d\">x</a></p>"
    );
    assert_eq!(parser().parse(&written).expect("parses"), doc);
}

#[test]
fn the_cosmetic_attributes_travel_in_data_attributes_and_default_without_them() {
    let codec = Codec::new();
    // A bullet character, an ordered delimiter, looseness and a fence spelling.
    for source in [
        "* one\n* two",
        "1) a\n1) b",
        "- one\n\n- two",
        "~~~~js\nx\n~~~~",
        "5. a\n6. b",
    ] {
        let doc = codec.parse(source);
        let written = serializer().serialize(&doc);
        assert_eq!(parser().parse(&written).expect("parses"), doc, "{source}");
    }
    // Plain HTML from another application takes the defaults.
    assert_eq!(markdown("<ul><li>one</li></ul>"), "- one");
    assert_eq!(markdown("<ol><li>a</li></ol>"), "1. a");
}

#[test]
fn a_raw_block_survives_a_round_trip_and_reads_as_a_pre_elsewhere() {
    let codec = Codec::new();
    let doc = codec.parse("| a | b |\n| --- | --- |");
    let written = serializer().serialize(&doc);
    assert!(
        written.starts_with("<pre data-type=\"rawBlock\">"),
        "{written}"
    );
    assert!(written.contains("| a | b |"), "{written}");
    assert_eq!(parser().parse(&written).expect("parses"), doc);
}

#[test]
fn a_leading_line_ending_inside_a_pre_survives() {
    let codec = Codec::new();
    for source in ["```\n\nx\n```", "```\nx\n\n```", "```\n\n```"] {
        let doc = codec.parse(source);
        let written = serializer().serialize(&doc);
        assert_eq!(parser().parse(&written).expect("parses"), doc, "{source}");
    }
}

#[test]
fn a_copied_slice_writes_as_html_and_reads_back_as_the_same_fragment() {
    let codec = Codec::new();
    let html = serializer();
    for source in [
        "a **b** c",
        "# Title\n\nbody",
        "- one\n- two",
        "> quoted\n\n```rust\nx\n```",
        "| a |\n| - |",
    ] {
        let once = codec.parser.parse_fragment(source).expect("parses");
        let written = html.serialize_fragment(&once);
        let twice = parser()
            .parse_fragment(&written)
            .expect("the fragment reparses");
        assert_eq!(twice, once, "{source:?} wrote {written:?}");
    }
    // Whitespace at a fragment's edge is the one thing HTML cannot carry: a
    // reader strips it, as a browser would. The clipboard's own JSON flavour is
    // what keeps a copy inside Markraft exact.
    let spaced = codec.parser.parse_fragment("hello ").expect("parses");
    let written = html.serialize_fragment(&spaced);
    assert_eq!(written, "<p>hello </p>");
    let back = parser().parse_fragment(&written).expect("reparses");
    assert_eq!(
        slice_to_plain_text(&codec.schema, &back),
        "hello",
        "the trailing space is gone"
    );
}

#[test]
fn a_cut_inside_one_paragraph_writes_as_bare_inline_html() {
    let codec = Codec::new();
    let doc = codec.parse("hello **world**");
    let slice = doc.slice(1, 11).expect("a slice");
    assert_eq!(
        serializer().serialize_fragment(&slice),
        "<p>hello <strong>worl</strong></p>"
    );
    // An empty selection has nothing to write.
    let empty = markraft_core::Selection::cursor(2).content(&doc);
    assert!(empty.is_empty());
    assert_eq!(serializer().serialize_fragment(&empty), "");
}

#[test]
fn a_foreign_html_table_keeps_cells_attributes_links_and_images() {
    let codec = Codec::new();
    let source = "<table><tr><th colspan='2'>Title</th></tr><tr><td><a href='https://example.com'>link</a></td><td><img src='photo.png' alt='photo'></td></tr></table>";
    let doc = parser().parse(source).unwrap();
    assert_eq!(
        codec.schema.node_type(doc.child(0).type_id()).name(),
        "raw_block"
    );
    let written = codec.write(&doc);
    assert!(written.contains("<table>"));
    assert!(written.contains("colspan=\"2\""));
    assert!(written.contains("href=\"https://example.com\""));
    assert!(written.contains("src=\"photo.png\""));
    assert_eq!(codec.parse(&written), doc);
    let rich = markraft_commonmark::HtmlSerializer::commonmark(&codec.schema).serialize(&doc);
    assert_eq!(parser().parse(&rich).unwrap(), doc);
}

#[test]
fn a_foreign_span_keeps_unknown_attributes_around_editable_text() {
    use markraft_core::commands::{insert_text, run_command};
    use markraft_core::{EditorState, EditorStateConfig, Selection};
    let codec = Codec::new();
    let doc = parser()
        .parse("<p><span class='red'>hello</span></p>")
        .unwrap();
    assert_eq!(doc.child(0).child(1).text(), Some("hello"));
    assert_eq!(codec.write(&doc), "<span class=\"red\">hello</span>");
    let state = EditorState::create(
        EditorStateConfig::new(codec.schema.clone())
            .doc(doc)
            .selection(Selection::cursor(3)),
    )
    .unwrap();
    let edited = run_command(&state, &insert_text("X"))
        .unwrap()
        .unwrap()
        .state()
        .clone();
    assert_eq!(
        codec.write(edited.doc()),
        "<span class=\"red\">hXello</span>"
    );
    let rich =
        markraft_commonmark::HtmlSerializer::commonmark(&codec.schema).serialize(edited.doc());
    assert_eq!(parser().parse(&rich).unwrap(), *edited.doc());
}

#[test]
fn unmodelled_css_and_void_inline_elements_survive_html_paste() {
    let source = "<p>a<span style='color:red'>b</span><wbr>c</p>";
    let written = markdown(source);
    assert_eq!(written, "a<span style=\"color:red\">b</span><wbr>c");
    assert!(!written.contains("</wbr>"));
}
