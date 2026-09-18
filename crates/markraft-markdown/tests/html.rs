//! HTML import: the clipboard's rich flavour.
//!
//! The cases are the ones `markraft-core`'s importer was hardened against,
//! carried over wherever the tree model keeps the expectation.

mod common;

use common::Codec;
use markraft_markdown::html::{HtmlParser, HtmlRule, HtmlRules, commonmark_html_rules};
use markraft_markdown::{commonmark_schema, to_plain_text};

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
fn an_unknown_element_passes_its_children_through() {
    assert_eq!(
        shape("<p>a <custom-tag>b <b>c</b></custom-tag></p>"),
        r#"doc(paragraph("a b ", "c"{strong}))"#
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
    assert_eq!(markdown("<hr>"), "***");
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
