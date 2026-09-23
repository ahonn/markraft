//! The CommonMark/GFM document kind for [`markraft_core`]: a schema preset and
//! every codec for it — Markdown, HTML and plain text.
//!
//! ```
//! use markraft_commonmark::{commonmark_schema, commonmark_serializer, MarkdownParser};
//!
//! let schema = commonmark_schema();
//! let parser = MarkdownParser::commonmark(schema.clone());
//! let serializer = commonmark_serializer(&schema);
//!
//! let doc = parser.parse("# Title\n\n- one\n- two").unwrap();
//! assert_eq!(serializer.serialize(&doc), "# Title\n\n- one\n- two");
//! ```
//!
//! # What this crate promises
//!
//! **Inline content is written as it was read; blocks are written
//! canonically.** A paragraph's, a heading's or a table cell's text *is* its
//! Markdown inline source — delimiters, escapes, entities and all — so it goes
//! back out byte for byte, with only the backslashes that keep it the block it
//! is. Every style mark is derived from that text; see [`schema`] and
//! [`derive`]. The blocks around the text are written in one spelling each:
//! code blocks become fenced, list markers and quote prefixes are re-spelled,
//! and a document that survives a round trip renders the same HTML.
//!
//! [`SourceDocument`] is the separate persistence codec for editing existing
//! files. It retains original bytes, front matter and parser trivia, applies
//! validated local patches, and refuses edits it cannot safely map to source.
//! The normalization rules below describe the canonical codecs, not that
//! source-preserving save path.
//!
//! **Nothing is silently lost.** Every construct either has a node type, or is
//! kept in a `raw_block` or `raw_inline` primitive. A `raw_block` is a
//! textblock whose text *is* that source: the editor shows the markup rather
//! than rendering it, so the user edits it in place, and it is written back
//! verbatim. Link reference definitions are kept in one, and an HTML table
//! whose structure the model cannot describe stays a `raw_block` holding its
//! markup.
//!
//! # The three pieces
//!
//! * [`commonmark_schema_spec`] / [`commonmark_schema`] — the document kind.
//!   See [`schema`] for every type, attribute and mark rank.
//! * [`MarkdownParser`] with [`commonmark_rules`] — comrak's AST mapped onto
//!   the schema by a table a consumer can extend. See [`parse`].
//! * [`MarkdownSerializer`] with [`commonmark_serializer`] — a rule per node
//!   type and a rule per mark type over a state that handles block separation,
//!   line prefixes and escaping. See [`serialize`].
//! * [`commonmark_extensions`] — the input rules and corrections an editor on
//!   this schema wants.
//!
//! # The clipboard surface
//!
//! * [`to_plain_text`] and [`slice_to_plain_text`] — the `text/plain` flavour.
//! * [`MarkdownParser::parse_fragment`] and
//!   [`MarkdownSerializer::serialize_fragment`] — pasting into and copying out
//!   of existing content, over a [`Slice`](markraft_core::Slice) rather than a
//!   whole document. See [`fragment`] for what an open slice means here.
//! * [`html::HtmlParser`] with [`html::commonmark_html_rules`] — the rich
//!   flavour, over a rule table of the same shape as the Markdown one.
//! * [`CommonMarkCodecs`] gathers all three behind [`markraft_core::kind::Codecs`],
//!   which is what an editing surface takes, and
//!   [`commonmark_doc_type_names`] names this schema's roles for it.
//!
//! # Extending it
//!
//! A consumer adds its node types to [`commonmark_schema_spec`] before
//! compiling, registers [`ParseRule`]s for the comrak kinds that produce them
//! (enabling the comrak extension that emits those kinds through
//! [`MarkdownParser::with_options`]) and adds [`NodeRule`]s and [`MarkRule`]s
//! to the serialiser's tables. Nothing in the preset is privileged.
//!
//! # Known losses
//!
//! Editor-created shapes and cosmetic normalizations follow these rules.
//! Each has a test of its own in `tests/cases.rs`.
//!
//! * A line break at either end of a paragraph is dropped, and so is
//!   whitespace starting a line: no reader can give either back. A heading of
//!   level 3 or more cannot hold a line break, which becomes a space.
//! * Cosmetic attributes are advisory: `fence_char` and `fence_length` grow to
//!   clear the content. Adjacent lists that share a marker may merge on the
//!   next read — CommonMark has no portable way to keep them apart.
//! * `tight` is honoured where the shape allows it. A list whose items hold
//!   blocks that need a blank line between them is written loose, and a list
//!   with nowhere to put a blank line — one item holding one block — always
//!   reads back tight. A table and a raw block have to be the last block of
//!   their item, or the list is written loose: each runs on until a blank line,
//!   the table reading the line under it as one more of its rows and the HTML
//!   block reading it as more of its source.
//! * A table's columns are re-padded to a uniform display width, and its
//!   delimiter row is rewritten from the `alignments` attribute. A `|` inside
//!   a cell travels as `\|`, which is the only spelling GFM reads back.
//! * An HTML table with a nested table, a `colspan`/`rowspan` over more than
//!   one cell, or a `<caption>` is kept whole as a `raw_block`. One with a
//!   `<th>` in a body row imports as an ordinary cell, because GFM has no row
//!   header either. Block content inside a cell is flattened to inline.
//! * A `raw_block` is source, not a sealed node. An edit that leaves text which
//!   is no longer an HTML block is read as whatever it has become — a paragraph
//!   once the `<div>` is gone — and one emptied of its text writes nothing, so
//!   the next parse finds no block there at all.

#![forbid(unsafe_code)]

mod autolink;
pub mod callout;
mod commands;
pub mod derive;
pub mod escape;
mod extensions;
mod fit;
pub mod fragment;
mod guard;
pub mod html;
mod inline;
mod kind;
pub mod parse;
mod pending;
mod preset;
pub mod rules;
pub mod schema;
pub mod serialize;
pub mod source;
pub mod table;
mod text;
mod textblock;
pub mod wiki;

/// The comrak version this codec parses with, re-exported so a consumer
/// registering rules for its own node kinds uses the same types.
pub use comrak;

pub use callout::{Callout, read_callout};
pub use extensions::{commonmark_corrections, commonmark_extensions, commonmark_input_rules};
pub use fragment::open_fragment;
pub use html::{
    HtmlParser, HtmlRule, HtmlRules, HtmlSerializer, commonmark_html_rules,
    commonmark_html_serializer,
};
pub use kind::{
    CommandRefusal, CommonMarkCodecs, CommonMarkSpelling, FormatCommand, Formatted, Inexpressible,
    clear_formatting, commonmark_doc_type_names, keeping_styles, set_link,
    split_block_keeping_styles, toggle_style, toggle_style_mark, unlink,
};
pub use parse::{MarkdownParser, ParseError, commonmark_options};
pub use preset::{
    commonmark_mark_rules, commonmark_node_rules, commonmark_serializer, inline_link_mark_rule,
};
pub use rules::{
    NodeKind, ParseCx, ParseRule, ParseRules, ParseTarget, commonmark_rules, inline_text,
};
pub use schema::{commonmark_schema, commonmark_schema_spec};
pub use serialize::{
    MarkRule, MarkRules, MarkTarget, MarkdownSerializer, NodeRule, NodeRules, SerializerState,
};
pub use source::{SourceDocument, SourceError};
pub use text::{slice_to_plain_text, to_plain_text};
pub use textblock::holds_definitions;
pub use wiki::{WikiLink, read_wiki_link, whole_wiki_link};

/// Parse `source` into a document on the CommonMark schema.
///
/// A convenience for hosts that do not keep a parser around; building one is
/// cheap, so the cost is the schema clone.
pub fn from_markdown(
    schema: &markraft_core::Schema,
    source: &str,
) -> Result<markraft_core::Node, ParseError> {
    MarkdownParser::commonmark(schema.clone()).parse(source)
}

/// Write `doc` back out as Markdown.
pub fn to_markdown(schema: &markraft_core::Schema, doc: &markraft_core::Node) -> String {
    commonmark_serializer(schema).serialize(doc)
}

/// Read a pasted fragment into a [`Slice`](markraft_core::Slice).
///
/// See [`MarkdownParser::parse_fragment`] for what makes a fragment different
/// from a document.
pub fn from_markdown_fragment(
    schema: &markraft_core::Schema,
    source: &str,
) -> Result<markraft_core::Slice, ParseError> {
    MarkdownParser::commonmark(schema.clone()).parse_fragment(source)
}

/// Write a copied [`Slice`](markraft_core::Slice) as Markdown.
pub fn to_markdown_fragment(
    schema: &markraft_core::Schema,
    slice: &markraft_core::Slice,
) -> String {
    commonmark_serializer(schema).serialize_fragment(slice)
}
