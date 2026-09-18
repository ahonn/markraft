//! CommonMark/GFM for [`markraft_doc`]: a schema preset and a Markdown codec.
//!
//! ```
//! use markraft_markdown::{commonmark_schema, commonmark_serializer, MarkdownParser};
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
//! **Semantic fidelity, not byte fidelity.** A document that survives a round
//! trip renders the same HTML; it does not come back as the same source text.
//! Headings become ATX, code blocks become fenced, reference links become
//! inline links, and where the author wrapped a paragraph is not recorded.
//!
//! **Nothing is silently lost.** Every construct either has a node type, or is
//! kept verbatim in a `raw_block`, or — inline — degrades to the source text a
//! reader sees anyway. The exceptions are listed under *Known losses* below and
//! are each a thing CommonMark itself cannot express.
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
//!   of existing content, over a [`Slice`](markraft_doc::Slice) rather than a
//!   whole document. See [`fragment`] for what an open slice means here.
//! * [`html::HtmlParser`] with [`html::commonmark_html_rules`] — the rich
//!   flavour, over a rule table of the same shape as the Markdown one.
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
//! Every one of these is a shape CommonMark cannot write down, not a gap in the
//! codec. Each has a test of its own in `tests/cases.rs`.
//!
//! * A `hard_break` with nothing after it in its block is dropped, and one
//!   inside a heading becomes a space; whitespace directly after a break is
//!   dropped too, because a reader strips the indentation of the line a break
//!   starts.
//! * Leading and trailing spaces *inside* `em`, `strong` or `strikethrough`
//!   move outside the mark, because `* a *` is not emphasis at all.
//! * A line ending inside a **code span** becomes a space: CommonMark says so.
//!   Everywhere else in inline content it travels as `&#10;` and comes back.
//! * Emphasis cannot nest inside emphasis of the same kind, and a code span
//!   takes no styling marks — a mark set holds one mark per type.
//! * **Inline** HTML is not a modelled construct: a tag that pairs up as `<u>`,
//!   `<em>`, `<strong>` or `<del>` becomes that mark, and anything else becomes
//!   the text it reads as. HTML *blocks* are kept verbatim.
//! * Cosmetic attributes are advisory: `fence_char` and `fence_length` grow to
//!   clear the content, and `bullet_char`/`delimiter` change when the list
//!   before would otherwise merge with this one.
//! * `tight` is honoured where the shape allows it. A list whose items hold
//!   blocks that need a blank line between them is written loose, and a list
//!   with nowhere to put a blank line — one item holding one block — always
//!   reads back tight.

#![forbid(unsafe_code)]

pub mod escape;
mod extensions;
mod fit;
pub mod fragment;
pub mod html;
mod inline;
pub mod parse;
mod preset;
pub mod rules;
pub mod schema;
pub mod serialize;
mod text;

/// The comrak version this codec parses with, re-exported so a consumer
/// registering rules for its own node kinds uses the same types.
pub use comrak;

pub use extensions::{commonmark_corrections, commonmark_extensions, commonmark_input_rules};
pub use fragment::open_fragment;
pub use html::{
    HtmlParser, HtmlRule, HtmlRules, HtmlSerializer, commonmark_html_rules,
    commonmark_html_serializer,
};
pub use parse::{MarkdownParser, ParseError, commonmark_options};
pub use preset::{commonmark_mark_rules, commonmark_node_rules, commonmark_serializer};
pub use rules::{
    NodeKind, ParseCx, ParseRule, ParseRules, ParseTarget, commonmark_rules, inline_text,
};
pub use schema::{commonmark_schema, commonmark_schema_spec};
pub use serialize::{
    MarkRule, MarkRules, MarkTarget, MarkdownSerializer, NodeRule, NodeRules, SerializerState,
};
pub use text::{slice_to_plain_text, to_plain_text};

/// Parse `source` into a document on the CommonMark schema.
///
/// A convenience for hosts that do not keep a parser around; building one is
/// cheap, so the cost is the schema clone.
pub fn from_markdown(
    schema: &markraft_doc::Schema,
    source: &str,
) -> Result<markraft_doc::Node, ParseError> {
    MarkdownParser::commonmark(schema.clone()).parse(source)
}

/// Write `doc` back out as Markdown.
pub fn to_markdown(schema: &markraft_doc::Schema, doc: &markraft_doc::Node) -> String {
    commonmark_serializer(schema).serialize(doc)
}

/// Read a pasted fragment into a [`Slice`](markraft_doc::Slice).
///
/// See [`MarkdownParser::parse_fragment`] for what makes a fragment different
/// from a document.
pub fn from_markdown_fragment(
    schema: &markraft_doc::Schema,
    source: &str,
) -> Result<markraft_doc::Slice, ParseError> {
    MarkdownParser::commonmark(schema.clone()).parse_fragment(source)
}

/// Write a copied [`Slice`](markraft_doc::Slice) as Markdown.
pub fn to_markdown_fragment(schema: &markraft_doc::Schema, slice: &markraft_doc::Slice) -> String {
    commonmark_serializer(schema).serialize_fragment(slice)
}
