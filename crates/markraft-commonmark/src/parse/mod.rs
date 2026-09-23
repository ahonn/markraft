//! Reading Markdown into a document tree.
//!
//! comrak does the CommonMark parsing; this module maps its AST onto the
//! schema with a [`ParseRules`] table and repairs whatever the two disagree
//! about, so every document it returns passes [`Node::check`].
//!
//! # What the mapping decides
//!
//! * A paragraph's, a heading's and a table cell's content is its **inline
//!   source**: the text a reader sees once the block's own syntax is stripped,
//!   one `line_break` atom per line ending, and an atom for each image, wiki
//!   link and raw HTML tag. Every style mark is derived from that text; see
//!   [`crate::textblock`] and [`crate::derive`]. comrak's inline tree is not
//!   consulted for it, so the inline rules of a [`ParseRules`] table only
//!   apply where an inline construct turns up in block position.
//! * An **HTML block holding only `<br>`** becomes an empty paragraph, so older
//!   Markraft files that used that spelling still open. Empty paragraphs have
//!   no CommonMark write-back; they become blank separators. Runs of blank
//!   lines in the source are separators, as CommonMark says, and produce
//!   nothing.
//! * **Inline HTML** is a `raw_inline` atom holding the tag as written, except
//!   what [`derive`](crate::derive) reads as something else: a paired `<u>`,
//!   `<em>`, `<strong>`, `<del>` or `<a href>` is that style spelled in the
//!   text, a `<br>` ending a line spells that line's hard break, an `<img>` is
//!   an `image` atom that writes its tag back, and a `<u>` or `</u>` without
//!   its partner stays text.
//! * An **Obsidian wiki link** — `[[target]]`, `[[target|alias]]` or the embed
//!   `![[target]]` — becomes a `wiki_link` atom holding the bytes the source
//!   spelled. A spelling [`crate::wiki`] refuses stays the text a reader sees.
//! * An **indented code block** becomes an ordinary `code_block` and is written
//!   back fenced. The two render identically.
//! * **Link reference definitions** are kept as they were written, in a
//!   `raw_block` where they stood — between blocks, or at the start of the
//!   paragraph they opened — so the reference links in the text still resolve
//!   once the file is written back. The tree does not resolve them yet: a
//!   reference link is text until it does.
//! * A **GFM table** becomes a `table` of `table_row`s of `table_cell`s, with
//!   the delimiter row's alignments on the table. The first row is the header
//!   row, and every row is squared off to the column count the alignments
//!   declare — see [`crate::table`].
//! * Anything else — footnote definitions, HTML blocks, whatever a comrak
//!   extension produces — is kept as source text: a `raw_block` whose text is
//!   that source where a block is expected. Inline HTML has its own raw
//!   primitive.
//!
//! # Repair
//!
//! comrak's tree and the schema need not agree: a list item may hold blocks the
//! schema's content rule forbids, and a consumer's rule set may be looser still.
//! Each container's children are matched against its content automaton; a child
//! that does not fit is wrapped in whatever the schema says would make it fit,
//! failing that preceded by the children the rule requires, and only dropped
//! when neither works.

mod inline;

use std::cell::Ref;

use comrak::nodes::{AstNode, NodeValue};
use comrak::{Arena, Options, parse_document};
use markraft_core::{Attrs, Fragment, Node, NodeError, NodeTypeId, Schema, Slice};

use crate::rules::{ParseCx, ParseRule, ParseRules, ParseTarget, commonmark_rules};

/// Why a document could not be built.
///
/// Parsing Markdown itself never fails — CommonMark has no invalid input — so
/// every variant is a disagreement between the rule set and the schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// A rule named a node or mark type the schema does not declare.
    UnknownType {
        /// `"node"` or `"mark"`.
        kind: &'static str,
        /// The name the rule asked for.
        name: String,
    },
    /// A node could not be built: wrong attributes, or content the schema
    /// rejects even after repair.
    Schema(String),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::UnknownType { kind, name } => {
                write!(f, "the schema declares no {kind} type `{name}`")
            }
            ParseError::Schema(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for ParseError {}

impl From<NodeError> for ParseError {
    fn from(error: NodeError) -> ParseError {
        ParseError::Schema(error.to_string())
    }
}

/// The comrak options this codec parses with.
///
/// `strikethrough`, `tasklist` and `table` back schema features; with `table`
/// off a table would arrive as a paragraph full of pipes and be reflowed into
/// one line. `autolink` is on so the URL an author typed plain becomes a link mark
/// rather than text that only *looks* like one; the serialiser writes such a
/// link back as the bare URL. `relaxed_autolinks` stays off: it reads a URL
/// inside brackets as a link too, which is not what GFM does.
///
/// `footnotes` is deliberately **off**: comrak drops a footnote definition that
/// nothing refers to, and losing text is worse than reading `[^1]: note` as the
/// paragraph a plain CommonMark reader sees. A consumer that wants footnotes as
/// nodes turns the extension on with
/// [`MarkdownParser::with_options`] — the rule set already sends both footnote
/// kinds to [`ParseRule::Raw`].
///
/// `wikilinks_title_after_pipe` is on for Obsidian's `[[target|alias]]` order,
/// which is what the files this editor shares are written in. comrak only
/// *finds* the construct: what it reads is normalised — the destination is
/// trimmed, unescaped and entity-resolved — so the [`WIKI_LINK`] atom takes its
/// parts from the source instead, and [`crate::wiki`] decides which spellings
/// count. The embed form `![[…]]` is not comrak's at all and is recognised in
/// the conversion layer.
///
/// Front matter is off: the host strips it before the codec sees the text.
/// Setext headings are *not* ignored, so `Title\n=====` imports as a heading.
///
/// comrak records source positions in the AST unconditionally; its `sourcepos`
/// option only adds attributes to rendered HTML, so it stays off.
///
/// [`WIKI_LINK`]: crate::schema::WIKI_LINK
pub fn commonmark_options() -> Options<'static> {
    let mut options = Options::default();
    options.extension.strikethrough = true;
    options.extension.tasklist = true;
    options.extension.table = true;
    options.extension.autolink = true;
    options.extension.wikilinks_title_after_pipe = true;
    options
}

/// Reads Markdown into a [`Node`] tree.
#[derive(Clone)]
pub struct MarkdownParser {
    schema: Schema,
    rules: ParseRules,
    options: Options<'static>,
}

impl MarkdownParser {
    /// A parser driving `rules` against `schema`.
    pub fn new(schema: Schema, rules: ParseRules) -> MarkdownParser {
        MarkdownParser {
            schema,
            rules,
            options: commonmark_options(),
        }
    }

    /// A parser with the CommonMark/GFM rule set.
    pub fn commonmark(schema: Schema) -> MarkdownParser {
        MarkdownParser::new(schema, commonmark_rules())
    }

    /// Replace the comrak options. Enabling an extension is how a consumer gets
    /// its construct into the AST; a rule for that kind then decides what the
    /// construct becomes.
    pub fn with_options(mut self, options: Options<'static>) -> MarkdownParser {
        self.options = options;
        self
    }

    /// The comrak options in force.
    pub fn options(&self) -> &Options<'static> {
        &self.options
    }

    /// The schema documents are built against.
    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    /// Read `source` as a fragment to paste into existing content.
    ///
    /// The result is a [`Slice`] opened by
    /// [`open_fragment`](crate::fragment::open_fragment), so a source that is
    /// one paragraph merges into the textblock the caret sits in, while a run
    /// of blocks lands as blocks.
    ///
    /// Unlike [`MarkdownParser::parse`], the spaces and tabs at the end of the
    /// source are kept: a reader strips a paragraph's trailing whitespace, but
    /// a pasted `hello ` has to stay apart from the `tail` after the caret.
    pub fn parse_fragment(&self, source: &str) -> Result<Slice, ParseError> {
        let normalized = source.replace("\r\n", "\n").replace('\r', "\n");
        let kept = normalized[normalized.trim_end_matches([' ', '\t']).len()..].to_string();
        let doc = self.parse(&normalized)?;
        let doc = crate::fragment::append_trailing(&self.schema, &doc, &kept);
        Ok(crate::fragment::open_fragment(
            &self.schema,
            doc.content().clone(),
        ))
    }

    /// Read `source` into a document.
    pub fn parse(&self, source: &str) -> Result<Node, ParseError> {
        let normalized = source.replace("\r\n", "\n").replace('\r', "\n");
        let cx = ParseCx::new(&self.schema, &normalized);
        let arena = Arena::new();
        let root = parse_document(&arena, &normalized, &self.options);
        let walk = Walk {
            schema: &self.schema,
            rules: &self.rules,
            cx: &cx,
            options: &self.options,
        };
        let blocks = walk.blocks(root)?;
        let doc = walk.fit(self.schema.top_type(), Attrs::empty(), blocks)?;
        let doc = crate::table::normalize_tables(&self.schema, &doc).unwrap_or(doc);
        doc.check(&self.schema)?;
        Ok(doc)
    }
}

pub(crate) struct Walk<'a> {
    pub(crate) schema: &'a Schema,
    pub(crate) rules: &'a ParseRules,
    pub(crate) cx: &'a ParseCx<'a>,
    pub(crate) options: &'a Options<'static>,
}

impl<'a> Walk<'a> {
    pub(crate) fn node_id(&self, name: &str) -> Result<NodeTypeId, ParseError> {
        self.schema
            .node_id(name)
            .ok_or_else(|| ParseError::UnknownType {
                kind: "node",
                name: name.to_string(),
            })
    }

    pub(crate) fn target(&self, node: &'a AstNode<'a>) -> ParseTarget<'a> {
        ParseTarget { node, cx: self.cx }
    }

    pub(crate) fn value(&self, node: &'a AstNode<'a>) -> Ref<'a, NodeValue> {
        self.target(node).value()
    }

    // -- blocks ------------------------------------------------------------

    fn blocks(&self, parent: &'a AstNode<'a>) -> Result<Vec<Node>, ParseError> {
        let mut out = Vec::new();
        let pos = self.target(parent).sourcepos();
        let mut next = pos.start.line.max(1);
        for child in parent.children() {
            let child_pos = self.target(child).sourcepos();
            self.push_definitions(parent, next, child_pos.start.line, &mut out)?;
            self.block(child, &mut out)?;
            next = next.max(child_pos.end.line + 1);
        }
        self.push_definitions(parent, next, pos.end.line + 1, &mut out)?;
        Ok(out)
    }

    /// The link reference definitions between `from` and `to`, each run as a
    /// raw block.
    fn push_definitions(
        &self,
        parent: &'a AstNode<'a>,
        from: usize,
        to: usize,
        out: &mut Vec<Node>,
    ) -> Result<(), ParseError> {
        if from >= to {
            return Ok(());
        }
        let in_list = matches!(&*self.value(parent), NodeValue::List(_));
        for definitions in self.definitions_between(parent, from, to) {
            let block = self.raw_block(crate::schema::RAW_BLOCK, &definitions)?;
            // Between two items, a definition indented under the first is its
            // content: comrak ends an item at its last block, not at the
            // definitions and blank lines after it.
            match out.last_mut() {
                Some(item) if in_list && item.is_container() => {
                    let mut children: Vec<Node> = item.children().cloned().collect();
                    children.push(block);
                    *item = item.copy(Fragment::from_nodes(children));
                }
                _ => out.push(block),
            }
        }
        Ok(())
    }

    fn block(&self, node: &'a AstNode<'a>, out: &mut Vec<Node>) -> Result<(), ParseError> {
        let target = self.target(node);
        if self.is_empty_paragraph_html(node)
            && let Some(name) = self.paragraph_type(target)
        {
            let ty = self.node_id(&name)?;
            out.push(self.fit(ty, Attrs::empty(), Vec::new())?);
            return Ok(());
        }
        let rule = self.rules.rule(&self.value(node)).clone();
        match rule {
            ParseRule::Ignore => {}
            ParseRule::Block { node_type, attrs } => {
                let ty = self.node_id(&node_type(target))?;
                let kind = self.schema.node_type(ty);
                let children = if kind.is_leaf() {
                    Vec::new()
                } else if kind.has_inline_content() {
                    let (children, definitions) = self.textblock(node)?;
                    if let Some(definitions) = definitions {
                        out.push(self.raw_block(crate::schema::RAW_BLOCK, &definitions)?);
                    }
                    children
                } else {
                    self.blocks(node)?
                };
                let (attrs, children) = self.callout(node, attrs(target), children);
                out.push(self.fit(ty, attrs, children)?);
            }
            ParseRule::TextBlock {
                node_type,
                attrs,
                text,
            } => {
                let ty = self.node_id(&node_type(target))?;
                let body = text(target);
                let children = if body.is_empty() {
                    Vec::new()
                } else {
                    vec![self.schema.text(&body)]
                };
                out.push(self.fit(ty, attrs(target), children)?);
            }
            ParseRule::Raw { node_type } => {
                out.push(self.raw_block(&node_type(target), &self.block_source(node))?);
            }
            // An inline rule met in block position: keep the source rather than
            // lose the text.
            ParseRule::Atom { .. } | ParseRule::Mark { .. } | ParseRule::Text { .. } => {
                if let Some(name) = self.rules.raw_block_type(target) {
                    out.push(self.raw_block(&name, &self.block_source(node))?);
                }
            }
        }
        Ok(())
    }

    /// Take a callout's marker line out of the body it opened.
    ///
    /// The marker is in the attributes now, and it is not text, so the
    /// paragraph it shares with the first body line has to lose its first
    /// line. A marker that is the whole paragraph takes the paragraph with it;
    /// [`crate::fit::fit`] puts the empty one back that the content rule asks
    /// for.
    ///
    /// The cut is the paragraph's first line break, and what follows it is
    /// read again as a paragraph of its own: `> [!note] **bold` with the
    /// emphasis closing on the line below leaves a body that is not bold, which
    /// is how the body reads once the marker is taken off it.
    fn callout(
        &self,
        node: &'a AstNode<'a>,
        attrs: Attrs,
        mut children: Vec<Node>,
    ) -> (Attrs, Vec<Node>) {
        let Some(kind) = attrs.get("callout").and_then(|value| value.as_str()) else {
            return (attrs, children);
        };
        if kind.is_empty() {
            // An ordinary quote whose first line only looks like a marker —
            // indented, say — keeps it text with a backslash, as it is
            // written back.
            if let Some(first) = children.first_mut() {
                *first = crate::textblock::escape_callout_lookalike(self.schema, first);
            }
            return (attrs, children);
        }
        if self.take_marker_line(node, &mut children) {
            (attrs, children)
        } else {
            let plain = attrs.with("callout", "").with("fold", "").with("title", "");
            (plain, children)
        }
    }

    /// Take the marker line out of the body, answering whether it could be
    /// taken out at all.
    fn take_marker_line(&self, node: &'a AstNode<'a>, children: &mut Vec<Node>) -> bool {
        if node.first_child().is_none() {
            return false;
        }
        let Some(paragraph) = children.first().cloned() else {
            return false;
        };
        let Some(kind) = crate::textblock::block_kind(self.schema, paragraph.type_id()) else {
            return false;
        };
        let items = crate::textblock::Items::from_nodes(self.schema, paragraph.children());
        let cut = items
            .0
            .iter()
            .position(|item| *item == crate::textblock::Item::Break);
        match cut {
            Some(cut) => {
                let body = crate::textblock::Items(items.0[cut + 1..].to_vec());
                let derived =
                    crate::derive::derive(kind, &body.text(), &crate::derive::DeriveContext::new());
                let body = body.nodes(self.schema, &derived);
                children[0] = paragraph.copy(Fragment::from_nodes(body));
                true
            }
            // No break, so the marker is the whole paragraph.
            None => {
                children.remove(0);
                true
            }
        }
    }

    /// The node type the rule set uses for paragraphs, which is also what an
    /// empty paragraph is built from.
    fn paragraph_type(&self, target: ParseTarget<'a>) -> Option<String> {
        match self.rules.rule(&NodeValue::Paragraph) {
            ParseRule::Block { node_type, .. } => Some(node_type(target)),
            _ => None,
        }
    }

    /// Whether the node is an HTML block holding nothing but a `<br>` tag.
    /// Older Markraft files used that spelling for empty paragraphs; we still
    /// read it, but no longer write it.
    fn is_empty_paragraph_html(&self, node: &'a AstNode<'a>) -> bool {
        matches!(&*self.value(node), NodeValue::HtmlBlock(html) if inline::is_break_tag(&html.literal))
    }

    /// The source of a block that is kept verbatim. comrak hands an HTML block
    /// its literal text already free of container indentation; everything else
    /// is recovered from the source lines.
    fn block_source(&self, node: &'a AstNode<'a>) -> String {
        if let NodeValue::HtmlBlock(html) = &*self.value(node) {
            return html
                .literal
                .strip_suffix('\n')
                .unwrap_or(&html.literal)
                .to_string();
        }
        // References live outside the raw block and are consumed by comrak.
        // Its formatter writes resolved link/image destinations inline, making
        // the preserved block self-contained when those definitions disappear.
        if node.descendants().any(|child| {
            matches!(
                &*self.value(child),
                NodeValue::Link(_) | NodeValue::Image(_)
            )
        }) {
            let mut source = String::new();
            comrak::format_commonmark(node, &commonmark_options(), &mut source)
                .expect("formatting into a String cannot fail");
            return source.trim_end_matches('\n').to_string();
        }
        self.cx.block_source(self.target(node).sourcepos())
    }

    /// A raw block holding `source` as its text. An empty source leaves the
    /// block empty, because a textblock has no child standing for no text.
    fn raw_block(&self, name: &str, source: &str) -> Result<Node, ParseError> {
        let ty = self.node_id(name)?;
        let children = if source.is_empty() {
            Vec::new()
        } else {
            vec![self.schema.text(source)]
        };
        self.fit(ty, Attrs::empty(), children)
    }

    /// Build a node of `ty` holding `children`, making the content fit.
    pub(crate) fn fit(
        &self,
        ty: NodeTypeId,
        attrs: Attrs,
        children: Vec<Node>,
    ) -> Result<Node, ParseError> {
        Ok(crate::fit::fit(self.schema, ty, attrs, children)?)
    }
}
