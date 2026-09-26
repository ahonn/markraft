//! An HTML block rendered for a view, while the caret is away from it.
//!
//! A raw block keeps its HTML as its text, and that text is all the document
//! holds; this reads it into a document of the same schema for the view to
//! draw in its place. Only what a README's HTML uses is rendered — the
//! elements GitHub keeps when it cleans a README, bar tables — and anything
//! else leaves the whole block as source, since half a page is worse than its
//! source. Nothing is loaded or run: the page is read, not opened.
//!
//! Each top-level block keeps the alignment its `align` attribute, or that of
//! the `<div>` it sits in, asks for; the importer that reads the rest has no
//! use for it, so the blocks are cut here and read one at a time.

use super::HtmlParser;
use crate::schema as md;
use markraft_core::Schema;
use markraft_core::kind::{Align, Rendered};
use scraper::{ElementRef, Html, Node};

/// The elements a rendered block may hold. Tables are left out: a grid drawn
/// from a raw block has nowhere in the document to keep its scroll.
const ALLOWED: &[&str] = &[
    "html",
    "body",
    "p",
    "div",
    "center",
    "span",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "br",
    "hr",
    "img",
    "picture",
    "source",
    "a",
    "b",
    "strong",
    "i",
    "em",
    "u",
    "ins",
    "s",
    "del",
    "strike",
    "code",
    "kbd",
    "sub",
    "sup",
    "mark",
    "small",
    "q",
    "samp",
    "var",
    "abbr",
    "cite",
    "dfn",
    "tt",
    "blockquote",
    "pre",
    "ul",
    "ol",
    "li",
    "dl",
    "dt",
    "dd",
    "details",
    "summary",
];

/// The elements that start blocks of their own; everything else is inline
/// content, gathered into a paragraph with its neighbours.
const BLOCKS: &[&str] = &[
    "p",
    "div",
    "center",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "hr",
    "blockquote",
    "pre",
    "ul",
    "ol",
    "dl",
    "details",
];

/// The elements that only group blocks, and pass their alignment on.
const CONTAINERS: &[&str] = &["div", "center", "details"];

/// `source`, an HTML block, as the document it renders to. `None` when it holds
/// an element outside [`ALLOWED`], or nothing that would show.
pub(crate) fn render(parser: &HtmlParser, source: &str) -> Option<Rendered> {
    let page = Html::parse_fragment(source);
    let root = page.root_element();
    if root
        .descendants()
        .filter_map(ElementRef::wrap)
        .any(|element| !ALLOWED.contains(&element.value().name()))
    {
        return None;
    }
    let mut blocks = Vec::new();
    cut(root, Align::Start, &mut blocks);
    let mut children = Vec::new();
    let mut aligns = Vec::new();
    for (html, align) in blocks {
        let doc = parser.parse(&html).ok()?;
        for child in doc.children() {
            children.push(child.clone());
            aligns.push(align);
        }
    }
    let schema: &Schema = parser.schema();
    if !children.iter().any(|block| shows(schema, block)) {
        return None;
    }
    // The importer reads a picture as Markdown spells one, which has no size,
    // so each takes the size its own tag asks for back here.
    let mut sizes: Vec<(String, Option<String>, Option<String>)> = root
        .descendants()
        .filter_map(ElementRef::wrap)
        .filter(|element| element.value().name() == "img")
        .map(|element| {
            let attr = |name| element.value().attr(name).map(str::to_owned);
            (
                attr("src").unwrap_or_default(),
                attr("width"),
                attr("height"),
            )
        })
        .collect();
    let image = schema.node_id(md::IMAGE);
    let children = children
        .iter()
        .map(|block| sized(block, image, &mut sizes))
        .collect::<Vec<_>>();
    let doc = schema.node(md::DOC, children).ok()?;
    let projection = markraft_core::projection::Projection::of(&doc, schema);
    Some(Rendered {
        doc,
        projection,
        aligns,
    })
}

/// `node` with each picture in it given the size its tag asked for: the first
/// of `sizes` not yet taken whose source is the picture's.
fn sized(
    node: &markraft_core::Node,
    image: Option<markraft_core::NodeTypeId>,
    sizes: &mut Vec<(String, Option<String>, Option<String>)>,
) -> markraft_core::Node {
    if Some(node.type_id()) == image {
        let src = node
            .attrs()
            .get("src")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let Some(at) = sizes.iter().position(|(from, ..)| from == src) else {
            return node.clone();
        };
        let (_, width, height) = sizes.remove(at);
        let mut attrs = node.attrs().clone();
        for (side, value) in [("width", width), ("height", height)] {
            if let Some(value) = value {
                attrs = attrs.with(side, value);
            }
        }
        return node.with_attrs(attrs);
    }
    if node.child_count() == 0 {
        return node.clone();
    }
    node.copy(markraft_core::Fragment::from_nodes(
        node.children().map(|child| sized(child, image, sizes)),
    ))
}

/// Whether `block` draws anything: text that is not blank, a picture or a rule.
/// A block of comments or whitespace renders to nothing, and stays source.
fn shows(schema: &Schema, block: &markraft_core::Node) -> bool {
    let drawn = [md::IMAGE, md::HORIZONTAL_RULE].map(|name| schema.node_id(name));
    let mut found = false;
    block.descendants(&mut |node, _, _, _| {
        found |= node.text().is_some_and(|text| !text.trim().is_empty())
            || drawn.contains(&Some(node.type_id()));
        !found
    });
    found || drawn.contains(&Some(block.type_id()))
}

/// Cut `parent`'s children into blocks, each as the HTML to read it from and
/// the alignment it is drawn with. Inline content between blocks becomes a
/// paragraph of its own, aligned as `parent` is.
fn cut(parent: ElementRef<'_>, align: Align, out: &mut Vec<(String, Align)>) {
    let mut inline = String::new();
    let flush = |inline: &mut String, out: &mut Vec<(String, Align)>| {
        if !inline.trim().is_empty() {
            out.push((format!("<p>{inline}</p>"), align));
        }
        inline.clear();
    };
    for child in parent.children() {
        match child.value() {
            Node::Text(text) => inline.push_str(&escape(text)),
            Node::Element(element) => {
                let Some(child) = ElementRef::wrap(child) else {
                    continue;
                };
                let name = element.name();
                if !BLOCKS.contains(&name) {
                    inline.push_str(&child.html());
                    continue;
                }
                flush(&mut inline, out);
                let own = aligned(element.attr("align"), name).unwrap_or(align);
                if CONTAINERS.contains(&name) {
                    cut(child, own, out);
                } else {
                    out.push((child.html(), own));
                }
            }
            _ => {}
        }
    }
    flush(&mut inline, out);
}

/// The alignment an element asks for: its `align` attribute, or `<center>`.
fn aligned(attr: Option<&str>, name: &str) -> Option<Align> {
    if name == "center" {
        return Some(Align::Center);
    }
    match attr?.trim().to_ascii_lowercase().as_str() {
        "center" | "middle" => Some(Align::Center),
        "right" => Some(Align::End),
        "left" | "justify" => Some(Align::Start),
        _ => None,
    }
}

/// Text put back into HTML, where the parser had resolved its references.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::{HouseStyleHandle, commonmark_schema};
    use markraft_core::Node;

    fn render_html(source: &str) -> Option<Rendered> {
        let parser = HtmlParser::commonmark(commonmark_schema(), &HouseStyleHandle::default());
        render(&parser, source)
    }

    /// Each top-level block as its type's name and its text, pictures as `[img]`.
    fn blocks(rendered: &Rendered) -> Vec<(String, String)> {
        let schema = commonmark_schema();
        let image = schema.node_id(md::IMAGE);
        rendered
            .doc
            .children()
            .map(|block| {
                let mut text = String::new();
                block.descendants(&mut |node: &Node, _, _, _| {
                    if let Some(own) = node.text() {
                        text.push_str(own);
                    } else if Some(node.type_id()) == image {
                        text.push_str("[img]");
                    }
                    true
                });
                (schema.node_type(block.type_id()).name().to_owned(), text)
            })
            .collect()
    }

    fn image_attr(rendered: &Rendered, name: &str) -> Vec<String> {
        let image = commonmark_schema().node_id(md::IMAGE);
        let mut out = Vec::new();
        rendered.doc.descendants(&mut |node: &Node, _, _, _| {
            if Some(node.type_id()) == image {
                out.push(
                    node.attrs()
                        .get(name)
                        .and_then(|value| value.as_str())
                        .unwrap_or_default()
                        .to_owned(),
                );
            }
            true
        });
        out
    }

    #[test]
    fn a_readme_header_renders_centred_with_its_sizes() {
        let icon = render_html(
            "<p align=\"center\">\n  <img src=\"assets/icon.png\" alt=\"icon\" width=\"96\">\n</p>",
        )
        .expect("renders");
        assert_eq!(
            blocks(&icon),
            [("paragraph".to_owned(), "[img]".to_owned())]
        );
        assert_eq!(icon.aligns, [Align::Center]);
        assert_eq!(image_attr(&icon, "width"), ["96"]);

        let title = render_html("<h1 align=\"center\">Markraft</h1>").expect("renders");
        assert_eq!(
            blocks(&title),
            [("heading".to_owned(), "Markraft".to_owned())]
        );
        assert_eq!(title.aligns, [Align::Center]);

        let badges = render_html(concat!(
            "<p align=\"center\">\n",
            "  <img src=\"https://a/1.svg\" alt=\"one\">\n",
            "  <a href=\"LICENSE\"><img src=\"https://a/2.svg\" alt=\"two\"></a>\n",
            "</p>"
        ))
        .expect("renders");
        assert_eq!(blocks(&badges).len(), 1);
        assert_eq!(
            image_attr(&badges, "src"),
            ["https://a/1.svg", "https://a/2.svg"]
        );
    }

    #[test]
    fn a_div_passes_its_alignment_to_the_blocks_it_holds() {
        let rendered = render_html("<div align=\"center\"><p>a</p><p align=\"right\">b</p>c</div>")
            .expect("renders");
        assert_eq!(
            blocks(&rendered),
            [
                ("paragraph".to_owned(), "a".to_owned()),
                ("paragraph".to_owned(), "b".to_owned()),
                ("paragraph".to_owned(), "c".to_owned()),
            ]
        );
        assert_eq!(rendered.aligns, [Align::Center, Align::End, Align::Center]);
    }

    #[test]
    fn inline_styles_render_as_the_marks_they_are() {
        let rendered = render_html("<p><b>Download</b> · press <kbd>K</kbd></p>").expect("renders");
        let schema = commonmark_schema();
        let marks: Vec<String> = {
            let mut out = Vec::new();
            rendered.doc.descendants(&mut |node: &Node, _, _, _| {
                for mark in node.marks().iter() {
                    let name = schema.mark_type(mark.ty).name().to_owned();
                    if !out.contains(&name) {
                        out.push(name);
                    }
                }
                true
            });
            out
        };
        assert!(marks.contains(&"strong".to_owned()), "{marks:?}");
        assert!(marks.contains(&"keyboard".to_owned()), "{marks:?}");
    }

    #[test]
    fn what_cannot_be_rendered_whole_stays_source() {
        assert_eq!(render_html("<table><tr><td>a</td></tr></table>"), None);
        assert_eq!(render_html("<p>a</p><script>b()</script>"), None);
        assert_eq!(render_html("<custom-tag>a</custom-tag>"), None);
        assert_eq!(render_html("<!-- a note to the writer -->"), None);
        assert_eq!(render_html("<p>   </p>"), None);
    }
}
