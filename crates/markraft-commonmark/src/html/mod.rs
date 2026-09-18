//! HTML — a clipboard's rich flavour — read into and written from a document
//! tree.
//!
//! [`HtmlParser`] reads; [`HtmlSerializer`] writes, and its module documents
//! what the two agree on.
//!
//! The shape mirrors [`crate::parse`]: a rule table maps elements onto schema
//! types, and whatever the two disagree about is repaired through the schema
//! the same way, so every document this returns passes
//! [`Node::check`](markraft_core::Node::check).
//!
//! # Whitespace
//!
//! HTML is written with indentation that means nothing, so text is normalised
//! the way a browser lays it out: every run of spaces, tabs and newlines
//! becomes one space, a run at the start of a block or straight after a line
//! break disappears, and the whitespace at the end of a block is dropped when
//! the block closes. A non-breaking space is not whitespace and survives, and
//! the text inside `<pre>` is taken exactly as written.
//!
//! # Blocks that are not there
//!
//! Text that is not inside any block element still has to become one, so the
//! importer keeps an open textblock and closes it whenever a block boundary
//! arrives: `a<div>b</div>c` is three paragraphs. A `<br>` alone in an
//! otherwise empty element is an editor's placeholder for an empty paragraph
//! rather than a line break, and produces nothing.
//!
//! # Inline CSS
//!
//! Word processors put their formatting in a `style` attribute instead of a
//! tag. On top of whatever the rules say, `font-weight`, `font-style` and
//! `text-decoration` are read as the marks they stand for.

mod rules;
mod serialize;

pub use rules::{
    HtmlAttrsFn, HtmlMatchFn, HtmlRule, HtmlRules, HtmlTarget, commonmark_html_rules,
    html_attrs_fn, html_match_fn,
};
pub use serialize::{
    HtmlMarkRule, HtmlMarkRules, HtmlNodeRule, HtmlNodeRules, HtmlSerializer, HtmlState,
    commonmark_html_mark_rules, commonmark_html_node_rules, commonmark_html_serializer,
    escape_attr, escape_text,
};

use markraft_core::{Attrs, Fragment, Mark, MarkSet, Node, NodeTypeId, Schema, Slice};
use scraper::{ElementRef, Html, Node as HtmlNode};

use crate::fit::{fit, fit_document};
use crate::fragment::open_fragment;
use crate::inline::InlineContent;
use crate::parse::ParseError;
use crate::schema as md;

/// Reads HTML into a [`Node`] tree.
#[derive(Clone)]
pub struct HtmlParser {
    schema: Schema,
    rules: HtmlRules,
}

impl HtmlParser {
    /// A parser driving `rules` against `schema`.
    pub fn new(schema: Schema, rules: HtmlRules) -> HtmlParser {
        HtmlParser { schema, rules }
    }

    /// A parser with the CommonMark/GFM rule table.
    pub fn commonmark(schema: Schema) -> HtmlParser {
        HtmlParser::new(schema, commonmark_html_rules())
    }

    /// The schema documents are built against.
    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    /// Read `source` into a document. No resources are loaded and no script is
    /// run: this walks the parsed tree and nothing else.
    pub fn parse(&self, source: &str) -> Result<Node, ParseError> {
        Ok(fit_document(&self.schema, self.blocks(source)?)?)
    }

    /// Read `source` as a fragment to paste into existing content.
    ///
    /// Opened by [`crate::fragment::open_fragment`], like the Markdown fragment
    /// parser.
    pub fn parse_fragment(&self, source: &str) -> Result<Slice, ParseError> {
        let blocks = self.blocks(source)?;
        Ok(open_fragment(
            &self.schema,
            Fragment::from_nodes(fit_document(&self.schema, blocks)?.children().cloned()),
        ))
    }

    fn blocks(&self, source: &str) -> Result<Vec<Node>, ParseError> {
        let html = Html::parse_fragment(source);
        let mut build = Build {
            schema: &self.schema,
            rules: &self.rules,
            blocks: Vec::new(),
            inline: InlineContent::new(&self.schema),
        };
        build.children(html.root_element(), &[])?;
        build.flush()?;
        Ok(build.blocks)
    }
}

struct Build<'s> {
    schema: &'s Schema,
    rules: &'s HtmlRules,
    blocks: Vec<Node>,
    inline: InlineContent<'s>,
}

impl<'s> Build<'s> {
    fn node_id(&self, name: &str) -> Result<NodeTypeId, ParseError> {
        self.schema
            .node_id(name)
            .ok_or_else(|| ParseError::UnknownType {
                kind: "node",
                name: name.to_string(),
            })
    }

    fn mark(&self, name: &str, attrs: Attrs) -> Result<Mark, ParseError> {
        let ty = self
            .schema
            .mark_id(name)
            .ok_or_else(|| ParseError::UnknownType {
                kind: "mark",
                name: name.to_string(),
            })?;
        Ok(Mark::with_attrs(
            ty,
            self.schema.build_mark_attrs(ty, &attrs)?,
        ))
    }

    /// Close the open textblock, if anything was written into it.
    fn flush(&mut self) -> Result<(), ParseError> {
        self.inline.trim_end();
        if self.inline.is_empty() {
            return Ok(());
        }
        let content = self.inline.take();
        let ty = self
            .schema
            .default_type(self.schema.content_match(self.schema.top_type()))
            .ok_or_else(|| ParseError::UnknownType {
                kind: "node",
                name: md::PARAGRAPH.to_string(),
            })?;
        self.blocks
            .push(fit(self.schema, ty, Attrs::empty(), content)?);
        Ok(())
    }

    /// Whether a space written now would be collapsed away.
    fn at_space(&self) -> bool {
        match self.inline.last() {
            None => true,
            Some(node) => match node.text() {
                Some(text) => text.ends_with(' '),
                None => markraft_core::projection::is_line_break(self.schema, node.type_id()),
            },
        }
    }

    /// Add text, collapsing whitespace the way a browser lays it out.
    fn text(&mut self, raw: &str, marks: &[Mark]) {
        let mut normalized = String::with_capacity(raw.len());
        let mut space = self.at_space();
        for character in raw.chars() {
            if matches!(character, ' ' | '\t' | '\n' | '\r' | '\u{c}') {
                if !space {
                    normalized.push(' ');
                }
                space = true;
            } else {
                normalized.push(character);
                space = false;
            }
        }
        let set = self.inline.mark_set(marks.iter().cloned());
        self.inline.push_text(&normalized, set);
    }

    fn children(&mut self, element: ElementRef<'s>, marks: &[Mark]) -> Result<(), ParseError> {
        for child in element.children() {
            match child.value() {
                HtmlNode::Text(text) => self.text(text, marks),
                HtmlNode::Element(_) => {
                    let Some(element) = ElementRef::wrap(child) else {
                        continue;
                    };
                    self.element(element, marks)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn element(&mut self, element: ElementRef<'s>, marks: &[Mark]) -> Result<(), ParseError> {
        let target = HtmlTarget {
            element,
            schema: self.schema,
        };
        let mut marks = marks.to_vec();
        for name in style_marks(element) {
            marks.push(self.mark(name, Attrs::empty())?);
        }
        match self.rules.rule(target).clone() {
            HtmlRule::Ignore => {}
            HtmlRule::Inline => self.children(element, &marks)?,
            HtmlRule::Boundary => {
                self.flush()?;
                self.children(element, &marks)?;
                self.flush()?;
            }
            HtmlRule::Mark { mark_type, attrs } => {
                marks.push(self.mark(&mark_type, attrs(target))?);
                self.children(element, &marks)?;
            }
            HtmlRule::Atom { node_type, attrs } => {
                let ty = self.node_id(&node_type)?;
                let node =
                    self.schema
                        .create(ty, attrs(target), MarkSet::empty(), Fragment::empty())?;
                let set = self.inline.mark_set(marks.iter().cloned());
                self.inline.push_node(node, set);
            }
            HtmlRule::LineBreak { node_type } => {
                // A break alone in an empty element is how an editor writes an
                // empty paragraph, not a line break inside one.
                if self.inline.is_empty() && only_child(element) {
                    return Ok(());
                }
                let ty = self.node_id(&node_type)?;
                let node =
                    self.schema
                        .create(ty, Attrs::empty(), MarkSet::empty(), Fragment::empty())?;
                self.inline.push_node(node, MarkSet::empty());
            }
            HtmlRule::TextBlock { node_type, attrs } => {
                self.flush()?;
                let ty = self.node_id(&node_type)?;
                let text = target.text();
                let text = text.strip_suffix('\n').unwrap_or(&text);
                let content = (!text.is_empty())
                    .then(|| self.schema.text(text))
                    .into_iter()
                    .collect();
                self.blocks
                    .push(fit(self.schema, ty, attrs(target), content)?);
            }
            HtmlRule::Block { node_type, attrs } => {
                self.flush()?;
                let ty = self.node_id(&node_type)?;
                let kind = self.schema.node_type(ty);
                let content = if kind.is_leaf() {
                    Vec::new()
                } else if kind.has_inline_content() {
                    self.collect_inline(element, &marks)?
                } else {
                    self.collect_blocks(element, &marks)?
                };
                self.blocks
                    .push(fit(self.schema, ty, attrs(target), content)?);
            }
        }
        Ok(())
    }

    /// The children of `element` as blocks, in a builder of their own.
    fn collect_blocks(
        &mut self,
        element: ElementRef<'s>,
        marks: &[Mark],
    ) -> Result<Vec<Node>, ParseError> {
        let mut inner = Build {
            schema: self.schema,
            rules: self.rules,
            blocks: Vec::new(),
            inline: InlineContent::new(self.schema),
        };
        inner.children(element, marks)?;
        inner.flush()?;
        Ok(inner.blocks)
    }

    /// The children of `element` as inline content. A block element met in here
    /// contributes its text in place rather than starting a block of its own,
    /// because the schema type has nowhere to put one.
    fn collect_inline(
        &mut self,
        element: ElementRef<'s>,
        marks: &[Mark],
    ) -> Result<Vec<Node>, ParseError> {
        let mut inner = Build {
            schema: self.schema,
            rules: self.rules,
            blocks: Vec::new(),
            inline: InlineContent::new(self.schema),
        };
        inner.children(element, marks)?;
        inner.inline.trim_end();
        Ok(inner.inline.take())
    }
}

/// Whether the element is the only thing in its parent that carries meaning.
fn only_child(element: ElementRef<'_>) -> bool {
    element.parent().is_some_and(|parent| {
        parent.children().all(|sibling| {
            sibling.id() == element.id()
                || match sibling.value() {
                    HtmlNode::Text(text) => text.trim().is_empty(),
                    HtmlNode::Comment(_) => true,
                    _ => false,
                }
        })
    })
}

/// The marks an element's inline CSS stands for.
fn style_marks(element: ElementRef<'_>) -> Vec<&'static str> {
    let Some(style) = element.value().attr("style") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for declaration in style.split(';') {
        let Some((property, value)) = declaration.split_once(':') else {
            continue;
        };
        let value = value.trim().to_ascii_lowercase();
        match property.trim().to_ascii_lowercase().as_str() {
            "font-weight" if value == "bold" || value.parse::<u16>().is_ok_and(|w| w >= 600) => {
                out.push(md::STRONG)
            }
            "font-style" if value == "italic" => out.push(md::EM),
            "text-decoration" | "text-decoration-line" => {
                if value.contains("underline") {
                    out.push(md::UNDERLINE);
                }
                if value.contains("line-through") {
                    out.push(md::STRIKETHROUGH);
                }
            }
            _ => {}
        }
    }
    out
}
