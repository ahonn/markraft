//! Parse rules: the data that maps comrak's AST onto schema types.
//!
//! A [`ParseRules`] set is a table keyed by comrak node *kind* — the enum
//! variant of [`NodeValue`], not its payload — plus a fallback for kinds the
//! table does not mention. A consumer that adds node types to the schema adds
//! rules for them here, including rules for comrak kinds this crate never
//! enables.
//!
//! Rules are looked up by name at parse time rather than resolved to ids up
//! front, so a rule set is plain data that outlives any one [`Schema`] and
//! [`MarkdownParser::new`](crate::MarkdownParser::new) cannot fail. A rule
//! naming a type the schema lacks is reported by
//! [`parse`](crate::MarkdownParser::parse) as
//! [`ParseError::UnknownType`](crate::ParseError::UnknownType).

use std::collections::HashMap;
use std::mem::Discriminant;
use std::sync::Arc;

use comrak::nodes::{AstNode, NodeValue, Sourcepos};
use markraft_core::{Attrs, Schema};

mod commonmark;

pub use commonmark::commonmark_rules;

/// The identity of a comrak node kind, ignoring its payload.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct NodeKind(Discriminant<NodeValue>);

impl NodeKind {
    /// The kind of `value`.
    ///
    /// The payload is ignored, so any value of the variant will do:
    /// `NodeKind::of(&NodeValue::Paragraph)`.
    pub fn of(value: &NodeValue) -> NodeKind {
        NodeKind(std::mem::discriminant(value))
    }
}

/// What a rule is given about the comrak node it is building from.
#[derive(Clone, Copy)]
pub struct ParseTarget<'a> {
    /// The comrak node.
    pub node: &'a AstNode<'a>,
    /// The document being parsed.
    pub cx: &'a ParseCx<'a>,
}

impl<'a> ParseTarget<'a> {
    /// The comrak node's value.
    pub fn value(&self) -> std::cell::Ref<'a, NodeValue> {
        std::cell::Ref::map(self.node.data.borrow(), |ast| &ast.value)
    }

    /// The comrak node's source position.
    pub fn sourcepos(&self) -> Sourcepos {
        self.node.data.borrow().sourcepos
    }

    /// The source text the node covers, with the indentation and quote markers
    /// of the containers around it removed.
    pub fn source(&self) -> String {
        self.cx.source(self.sourcepos())
    }
}

/// The document a rule is running against.
pub struct ParseCx<'a> {
    schema: &'a Schema,
    lines: Vec<&'a str>,
}

impl<'a> ParseCx<'a> {
    pub(crate) fn new(schema: &'a Schema, source: &'a str) -> ParseCx<'a> {
        ParseCx {
            schema,
            lines: source.split('\n').collect(),
        }
    }

    /// The schema the document is being built against.
    pub fn schema(&self) -> &'a Schema {
        self.schema
    }

    /// A source line by its one-based number.
    pub fn line(&self, number: usize) -> &'a str {
        self.lines
            .get(number.wrapping_sub(1))
            .copied()
            .unwrap_or_default()
    }

    /// The whole source lines a *block* covers, from its starting column.
    ///
    /// A block owns its lines to their ends, and some of comrak's block
    /// positions stop at the last piece of content rather than the last
    /// character — a table row's ending column leaves off the closing pipe — so
    /// asking for whole lines is both simpler and more faithful than trusting
    /// the ending column.
    pub fn block_source(&self, pos: Sourcepos) -> String {
        self.slice_lines(pos, false)
    }

    /// The source text a node covers.
    ///
    /// The first line is taken from the node's starting column and the last
    /// stops at its ending column; lines in between lose the same number of
    /// leading spaces, tabs and `>` markers, which is what the containers
    /// around the node contribute. The serialiser puts the prefixes of whatever
    /// containers the node ends up in back.
    pub fn source(&self, pos: Sourcepos) -> String {
        self.slice_lines(pos, true)
    }

    fn slice_lines(&self, pos: Sourcepos, stop_at_end_column: bool) -> String {
        let indent = pos.start.column.saturating_sub(1);
        let mut out = String::new();
        for number in pos.start.line..=pos.end.line {
            if number > pos.start.line {
                out.push('\n');
            }
            let line = self.line(number);
            let from = if number == pos.start.line {
                pos.start.column
            } else {
                container_prefix(line, indent) + 1
            };
            let to = if stop_at_end_column && number == pos.end.line {
                pos.end.column + 1
            } else {
                usize::MAX
            };
            out.push_str(slice(line, from, to));
        }
        out
    }
}

/// How many leading bytes of `line` are container indentation: spaces, tabs and
/// quote markers, never more than `max`.
fn container_prefix(line: &str, max: usize) -> usize {
    line.bytes()
        .take(max)
        .take_while(|b| matches!(b, b' ' | b'\t' | b'>'))
        .count()
}

/// `line` between two one-based, inclusive-exclusive byte columns.
fn slice(line: &str, from: usize, to: usize) -> &str {
    let start = from.saturating_sub(1).min(line.len());
    let end = to.saturating_sub(1).clamp(start, line.len());
    line.get(start..end).unwrap_or_default()
}

/// Picks the schema type a rule builds.
pub type TypeFn = Arc<dyn for<'a> Fn(ParseTarget<'a>) -> String + Send + Sync>;
/// Builds the attributes of the node or mark a rule creates.
pub type AttrsFn = Arc<dyn for<'a> Fn(ParseTarget<'a>) -> Attrs + Send + Sync>;
/// Produces the literal text a rule contributes.
pub type TextFn = Arc<dyn for<'a> Fn(ParseTarget<'a>) -> String + Send + Sync>;

/// Wrap a closure as a [`TypeFn`].
pub fn type_fn<F>(f: F) -> TypeFn
where
    F: for<'a> Fn(ParseTarget<'a>) -> String + Send + Sync + 'static,
{
    Arc::new(f)
}

/// Wrap a closure as an [`AttrsFn`].
pub fn attrs_fn<F>(f: F) -> AttrsFn
where
    F: for<'a> Fn(ParseTarget<'a>) -> Attrs + Send + Sync + 'static,
{
    Arc::new(f)
}

/// Wrap a closure as a [`TextFn`].
pub fn text_fn<F>(f: F) -> TextFn
where
    F: for<'a> Fn(ParseTarget<'a>) -> String + Send + Sync + 'static,
{
    Arc::new(f)
}

pub(crate) fn fixed(name: &'static str) -> TypeFn {
    type_fn(move |_| name.to_string())
}

pub(crate) fn no_attrs() -> AttrsFn {
    attrs_fn(|_| Attrs::empty())
}

/// What to do with one comrak node kind.
#[derive(Clone)]
pub enum ParseRule {
    /// Build a node of a block type and parse the comrak node's children into
    /// its content — as inline content when the schema type takes inline
    /// content, as blocks otherwise, and not at all for a leaf.
    Block {
        /// The schema node type to build.
        node_type: TypeFn,
        /// Its attributes.
        attrs: AttrsFn,
    },
    /// Build an inline leaf. The comrak node's children are not visited; an
    /// [`AttrsFn`] that needs them, such as an image's `alt`, walks them
    /// itself.
    Atom {
        /// The schema node type to build.
        node_type: TypeFn,
        /// Its attributes.
        attrs: AttrsFn,
    },
    /// Build a node of a block type whose content is literal text taken from
    /// the comrak node rather than from its children — a code block, whose
    /// text comrak keeps in the node itself.
    TextBlock {
        /// The schema node type to build.
        node_type: TypeFn,
        /// Its attributes.
        attrs: AttrsFn,
        /// The text to put inside it.
        text: TextFn,
    },
    /// Add a mark to everything the comrak node's children produce.
    Mark {
        /// The schema mark type to add.
        mark_type: TypeFn,
        /// Its attributes.
        attrs: AttrsFn,
    },
    /// Contribute literal text, carrying `marks` on top of the marks already in
    /// force.
    Text {
        /// The text to contribute.
        text: TextFn,
        /// Mark type names to add to it.
        marks: Vec<String>,
    },
    /// Drop the comrak node and everything under it.
    Ignore,
    /// Keep the node's source text: a textblock of `node_type` whose text is
    /// that source when a block is expected, plain text when an inline is.
    Raw {
        /// The block node type that holds the source.
        node_type: TypeFn,
    },
}

impl std::fmt::Debug for ParseRule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            ParseRule::Block { .. } => "Block",
            ParseRule::TextBlock { .. } => "TextBlock",
            ParseRule::Atom { .. } => "Atom",
            ParseRule::Mark { .. } => "Mark",
            ParseRule::Text { .. } => "Text",
            ParseRule::Ignore => "Ignore",
            ParseRule::Raw { .. } => "Raw",
        };
        f.write_str(name)
    }
}

impl ParseRule {
    /// A block node of a fixed type with default attributes.
    pub fn block(node_type: &'static str) -> ParseRule {
        ParseRule::Block {
            node_type: fixed(node_type),
            attrs: no_attrs(),
        }
    }

    /// A block node of a fixed type with computed attributes.
    pub fn block_with(node_type: &'static str, attrs: AttrsFn) -> ParseRule {
        ParseRule::Block {
            node_type: fixed(node_type),
            attrs,
        }
    }

    /// A block node of a fixed type whose content is literal text.
    pub fn text_block(node_type: &'static str, attrs: AttrsFn, text: TextFn) -> ParseRule {
        ParseRule::TextBlock {
            node_type: fixed(node_type),
            attrs,
            text,
        }
    }

    /// An inline leaf of a fixed type with computed attributes.
    pub fn atom_with(node_type: &'static str, attrs: AttrsFn) -> ParseRule {
        ParseRule::Atom {
            node_type: fixed(node_type),
            attrs,
        }
    }

    /// A mark of a fixed type with default attributes.
    pub fn mark(mark_type: &'static str) -> ParseRule {
        ParseRule::Mark {
            mark_type: fixed(mark_type),
            attrs: no_attrs(),
        }
    }

    /// A mark of a fixed type with computed attributes.
    pub fn mark_with(mark_type: &'static str, attrs: AttrsFn) -> ParseRule {
        ParseRule::Mark {
            mark_type: fixed(mark_type),
            attrs,
        }
    }

    /// Literal text with no marks of its own.
    pub fn text(text: TextFn) -> ParseRule {
        ParseRule::Text {
            text,
            marks: Vec::new(),
        }
    }

    /// A fixed string, whatever the node holds.
    pub fn literal(value: &'static str) -> ParseRule {
        ParseRule::text(text_fn(move |_| value.to_string()))
    }
}

/// A table of [`ParseRule`]s keyed by comrak node kind.
#[derive(Clone, Debug)]
pub struct ParseRules {
    rules: HashMap<NodeKind, ParseRule>,
    fallback: ParseRule,
}

impl ParseRules {
    /// A table whose fallback keeps unknown constructs as source text.
    pub fn new(raw_block_type: &'static str) -> ParseRules {
        ParseRules {
            rules: HashMap::new(),
            fallback: ParseRule::Raw {
                node_type: fixed(raw_block_type),
            },
        }
    }

    /// Register `rule` for the kind of `sample`.
    pub fn with(mut self, sample: &NodeValue, rule: ParseRule) -> ParseRules {
        self.rules.insert(NodeKind::of(sample), rule);
        self
    }

    /// Replace the rule used for kinds the table does not mention.
    pub fn fallback(mut self, rule: ParseRule) -> ParseRules {
        self.fallback = rule;
        self
    }

    /// The rule for `value`'s kind, or the fallback.
    pub fn rule(&self, value: &NodeValue) -> &ParseRule {
        self.rules
            .get(&NodeKind::of(value))
            .unwrap_or(&self.fallback)
    }

    /// The textblock type the fallback keeps unknown blocks in, when it keeps
    /// them at all.
    pub fn raw_block_type(&self, target: ParseTarget<'_>) -> Option<String> {
        match &self.fallback {
            ParseRule::Raw { node_type } => Some(node_type(target)),
            _ => None,
        }
    }
}

/// The plain text of an inline subtree, which is what CommonMark's `alt`
/// attribute holds for an image.
pub fn inline_text<'a>(node: &'a AstNode<'a>) -> String {
    let mut out = String::new();
    collect_text(node, &mut out);
    out
}

fn collect_text<'a>(node: &'a AstNode<'a>, out: &mut String) {
    for child in node.children() {
        match &child.data.borrow().value {
            NodeValue::Text(text) => out.push_str(text),
            NodeValue::Code(code) => out.push_str(&code.literal),
            NodeValue::SoftBreak | NodeValue::LineBreak => out.push(' '),
            NodeValue::HtmlInline(_) | NodeValue::FootnoteReference(_) => {}
            _ => collect_text(child, out),
        }
    }
}
