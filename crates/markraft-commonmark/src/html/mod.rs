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
//! # Blocks inside a textblock
//!
//! A `<td>` or an `<h1>` takes inline content, so a block element inside one
//! has nowhere to start a block of its own: its content is flattened in place,
//! with a space where the line a browser draws would have been.
//!
//! # Blocks that are not there
//!
//! Text that is not inside any block element still has to become one, so the
//! importer keeps an open textblock and closes it whenever a block boundary
//! arrives: `a<div>b</div>c` is three paragraphs. A `<br>` alone in an
//! otherwise empty element is an editor's placeholder for an empty paragraph
//! rather than a line break, and produces nothing.
//!
//! # Inline content becomes source
//!
//! A textblock's text is its Markdown inline source, so what an element says
//! — `<strong>`, a `font-weight`, an `<a href>` — is first read into semantic
//! content, text carrying marks, and then [spelled](crate::serialize::spell)
//! as the Markdown that says the same thing. That source is what the block
//! holds, with its marks derived from it like any other.
//!
//! # Inline CSS
//!
//! Word processors put their formatting in a `style` attribute instead of a
//! tag. On top of whatever the rules say, `font-weight`, `font-style` and
//! `text-decoration` are read as the marks they stand for.

mod rules;
mod serialize;

pub use rules::{
    HtmlAttrsFn, HtmlMatchFn, HtmlRule, HtmlRules, HtmlTarget, HtmlTextFn, commonmark_html_rules,
    html_attrs_fn, html_match_fn, html_text_fn,
};
pub use serialize::{
    HtmlMarkRule, HtmlMarkRules, HtmlNodeRule, HtmlNodeRules, HtmlSerializer, HtmlState,
    commonmark_html_mark_rules, commonmark_html_node_rules, commonmark_html_serializer,
    escape_attr, escape_text,
};

use markraft_core::{Attrs, BreakKind, Fragment, Mark, MarkSet, Node, NodeTypeId, Schema, Slice};
use scraper::{ElementRef, Html, Node as HtmlNode};

use crate::fit::{fit, fit_document};
use crate::fragment::open_fragment;
use crate::inline::InlineContent;
use crate::parse::ParseError;
use crate::schema as md;
use crate::serialize::MarkdownSerializer;

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
        self.document(source)
    }

    /// Read `source` as a fragment to paste into existing content.
    ///
    /// Opened by [`crate::fragment::open_fragment`], like the Markdown fragment
    /// parser.
    pub fn parse_fragment(&self, source: &str) -> Result<Slice, ParseError> {
        let document = self.document(source)?;
        Ok(open_fragment(
            &self.schema,
            Fragment::from_nodes(document.children().cloned()),
        ))
    }

    /// The blocks of `source` as a document, with every table squared off.
    fn document(&self, source: &str) -> Result<Node, ParseError> {
        let document = fit_document(&self.schema, self.blocks(source)?)?;
        Ok(crate::table::normalize_tables(&self.schema, &document).unwrap_or(document))
    }

    fn blocks(&self, source: &str) -> Result<Vec<Node>, ParseError> {
        let html = Html::parse_fragment(source);
        let serializer = crate::commonmark_serializer(&self.schema);
        let mut build = Build {
            schema: &self.schema,
            rules: &self.rules,
            serializer: &serializer,
            blocks: Vec::new(),
            inline: InlineContent::new(&self.schema),
            leading_space: true,
            inline_only: false,
        };
        build.children(html.root_element(), &[])?;
        build.flush()?;
        Ok(build.blocks)
    }
}

struct Build<'s> {
    schema: &'s Schema,
    rules: &'s HtmlRules,
    /// What spells a textblock's semantic content as source.
    serializer: &'s MarkdownSerializer,
    blocks: Vec<Node>,
    inline: InlineContent<'s>,
    leading_space: bool,
    /// Whether the builder is filling a textblock's inline content, where a
    /// block element has nowhere to start a block of its own.
    inline_only: bool,
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
        let block = self.textblock(ty, Attrs::empty(), content)?;
        self.blocks.push(block);
        Ok(())
    }

    /// A block of `ty` holding `content`; for a textblock whose text is inline
    /// source, that content spelled as source first.
    fn textblock(
        &self,
        ty: NodeTypeId,
        attrs: Attrs,
        content: Vec<Node>,
    ) -> Result<Node, ParseError> {
        let block = fit(self.schema, ty, attrs, content)?;
        let Some(kind) = crate::textblock::block_kind(self.schema, ty) else {
            return Ok(block);
        };
        let source = crate::serialize::spell(self.serializer, &block);
        let content = crate::textblock::build(self.schema, kind, &source);
        Ok(block.copy(Fragment::from_nodes(content)))
    }

    /// Add text that is Markdown source already, which spelling leaves as it
    /// stands.
    fn spelled(&mut self, source: &str, marks: &[Mark]) {
        let syntax = crate::textblock::syntax_mark(self.schema, 0, "");
        let set = self.inline.mark_set(marks.iter().cloned().chain(syntax));
        self.inline.push_text(source, set);
    }

    /// Whether a space written now would be collapsed away.
    fn at_space(&self) -> bool {
        match self.inline.last() {
            None => self.leading_space,
            Some(node) => ends_in_space(self.schema, node),
        }
    }

    /// Separate a flattened block from its neighbours, which is what the line
    /// break a browser would draw between them amounts to inline.
    fn block_separator(&mut self) {
        self.text(" ", &[]);
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
        // Markraft's own soft break: a line ending of the source, which is
        // spelling rather than a break to spell.
        if target.tag() == "span" && target.attr("data-type") == Some("softBreak") {
            self.spelled("\n", marks);
            return Ok(());
        }
        let mut marks = marks.to_vec();
        for name in style_marks(element) {
            marks.push(self.mark(name, Attrs::empty())?);
        }
        match self.rules.rule(target).clone() {
            HtmlRule::Ignore => {}
            HtmlRule::Inline => self.children(element, &marks)?,
            HtmlRule::RawInline { node_type } => {
                let ty = self.node_id(&node_type)?;
                let atom = |source: String| {
                    self.schema.create(
                        ty,
                        markraft_core::attrs! {"source" => source},
                        MarkSet::empty(),
                        Fragment::empty(),
                    )
                };
                let mut opening = format!("<{}", target.tag());
                for (name, value) in element.value().attrs() {
                    opening.push_str(&format!(" {name}=\"{}\"", escape_attr(value)));
                }
                opening.push('>');
                let set = self.inline.mark_set(marks.iter().cloned());
                self.inline.push_node(atom(opening)?, set.clone());
                self.children(element, &marks)?;
                if !matches!(
                    target.tag(),
                    "area"
                        | "base"
                        | "br"
                        | "col"
                        | "embed"
                        | "hr"
                        | "img"
                        | "input"
                        | "link"
                        | "meta"
                        | "param"
                        | "source"
                        | "track"
                        | "wbr"
                ) {
                    self.inline
                        .push_node(atom(format!("</{}>", target.tag()))?, set);
                }
            }
            // A block met where only inline content can go — inside a table
            // cell, inside a heading — has nowhere to start a block of its
            // own, so its content lands in place instead of being lost.
            HtmlRule::Block { .. } | HtmlRule::Boundary if self.inline_only => {
                self.block_separator();
                self.children(element, &marks)?;
                self.block_separator();
            }
            HtmlRule::TextBlock { text, .. } if self.inline_only => {
                self.block_separator();
                let body = text(target);
                self.text(&body, &marks);
                self.block_separator();
            }
            HtmlRule::Boundary => {
                self.flush()?;
                self.children(element, &marks)?;
                self.flush()?;
            }
            HtmlRule::Mark { mark_type, attrs } => {
                let leading_space = self.at_space();
                let previous_leading = self.leading_space;
                let before = self.inline.take();
                self.leading_space = leading_space;
                self.children(element, &[])?;
                let children = self.inline.take();
                self.leading_space = previous_leading;
                self.inline.restore(before);
                let mark = self.mark(&mark_type, attrs(target))?;
                // A link with nothing in it has no text to carry the mark, so
                // it arrives as the spelling it has.
                if children.is_empty() && self.schema.mark_type(mark.ty).name() == md::LINK {
                    let attr = |name: &str| {
                        mark.attrs
                            .get(name)
                            .and_then(|value| value.as_str())
                            .unwrap_or_default()
                            .to_string()
                    };
                    let spelling = format!(
                        "[{}",
                        crate::inline::link_closing(&attr("href"), &attr("title"))
                    );
                    self.spelled(&spelling, &marks);
                    return Ok(());
                }
                for node in crate::inline::wrap_mark(self.schema, mark, children) {
                    let set = marks.iter().fold(node.marks().clone(), |set, mark| {
                        set.add(self.schema, mark.clone())
                    });
                    if let Some(text) = node.text() {
                        self.inline.push_text(text, set);
                    } else {
                        self.inline.push_node(node, set);
                    }
                }
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
            HtmlRule::TextBlock {
                node_type,
                attrs,
                text,
            } => {
                self.flush()?;
                let ty = self.node_id(&node_type)?;
                let body = text(target);
                let body = body.strip_suffix('\n').unwrap_or(&body);
                let content = (!body.is_empty())
                    .then(|| self.schema.text(body))
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
                let block = self.textblock(ty, attrs(target), content)?;
                self.blocks.push(block);
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
            serializer: self.serializer,
            blocks: Vec::new(),
            inline: InlineContent::new(self.schema),
            leading_space: true,
            inline_only: false,
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
            serializer: self.serializer,
            blocks: Vec::new(),
            inline: InlineContent::new(self.schema),
            leading_space: true,
            inline_only: true,
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

fn ends_in_space(schema: &Schema, node: &Node) -> bool {
    if let Some(text) = node.text() {
        return text.ends_with(' ');
    }
    if let Some(child) = node.last_child() {
        return ends_in_space(schema, child);
    }
    schema.node_type(node.type_id()).break_kind() == Some(BreakKind::Hard)
}
