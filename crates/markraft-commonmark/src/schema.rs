//! The CommonMark/GFM schema preset.
//!
//! [`commonmark_schema_spec`] returns plain [`SchemaSpec`] data so a consumer
//! can add its own node and mark types before compiling it;
//! [`commonmark_schema`] compiles the preset as it stands.
//!
//! # What the tree can hold
//!
//! Every CommonMark block has a node type, except the ones whose structure the
//! model does not model: those are kept verbatim in a [`RAW_BLOCK`], whose text
//! *is* their source, so a document never loses text it cannot interpret and
//! the source stays editable in place. Ordinary inline styling uses
//! [`markraft_core::MarkSet`]; nested or empty structures that need more than a
//! set use transparent [`INLINE_SPAN`] containers. [`RAW_INLINE`] preserves
//! CommonMark inline HTML primitives, and [`WIKI_LINK`] an Obsidian `[[…]]`
//! link, both as atoms whose source is never interpreted as text.
//!
//! # Mark ranks
//!
//! A mark set is sorted by rank, and the serialiser opens marks in that order,
//! so the ranks fix the nesting of the Markdown it writes:
//!
//! | rank | mark | written as |
//! |-----:|------|------------|
//! | 10 | [`LINK`] | `[…](href "title")` — outermost, so a link wraps its styling |
//! | 20 | [`UNDERLINE`] | `<u>…</u>` — no CommonMark syntax exists |
//! | 30 | [`STRIKETHROUGH`] | `~~…~~` or `<del>…</del>` |
//! | 40 | [`STRONG`] | `**…**` or `<strong>…</strong>` |
//! | 50 | [`EM`] | `*…*` or `<em>…</em>` |
//! | 60 | [`CODE`] | `` `…` `` — innermost, because its content is literal |
//!
//! Simultaneous strong/em openings use a tag where adjacent delimiters would
//! reverse their nesting. The tag-only underline sits outside delimiter runs.
//! A nested inline span preserves the source order independently of ranks.
//!
//! [`CODE`] excludes nothing but itself. A code span's *content* is literal —
//! no emphasis is read inside the backticks — but the span as a whole carries
//! whatever marks surround it: `` *`code`* `` is `<em><code>code</code></em>`,
//! and `` [`code`](href) `` is a link around a code span. The serialiser writes
//! the outer delimiters around the backticks, falling back to `<em>`/`<strong>`
//! tags where a delimiter run could not flank there.
//!
//! # Why `block+` for list items but `paragraph block*` for task items
//!
//! CommonMark lets a list item start with any block — `- > quote` and
//! `- - nested` are both normal — so [`LIST_ITEM`] takes `block+` rather than
//! `paragraph block*`. The stricter rule would force the importer to invent a
//! leading empty paragraph for those items, which then serialises as a `<br>`
//! line and changes what the document renders as.
//!
//! A [`TASK_ITEM`] is the exception, because GFM puts its check box *inside*
//! the item's first paragraph: an item whose first block is a nested list has
//! nowhere to write `[x]` and is not a task item at all. Its content rule says
//! `paragraph block*` so the tree cannot describe something the format cannot
//! write.
//!
//! An emptied item of either kind is repaired by the
//! [`fill_required_content`](markraft_core::fill_required_content) correction
//! that [`commonmark_extensions`](crate::commonmark_extensions) registers.
//!
//! # Why a table has no header type
//!
//! GFM gives a table exactly one header row and it is always the first, so
//! [`TABLE`] holds plain [`TABLE_ROW`]s and *the first row is the header*. A
//! header flag on the row could describe a table with two header rows, or with
//! none, and neither can be written down. The column count lives in the
//! table's `alignments` attribute for the same reason: one place to read it,
//! and no row can disagree with it.
//!
//! [`TABLE`] and [`TABLE_CELL`] are `isolating`, as they are in ProseMirror: a
//! deletion at a cell boundary must not merge two cells, and a table's
//! structure is not something the text around it may dissolve.

use markraft_core::{
    AttrKind, AttrSpec, AttrValue, MarkTypeSpec, NodeTypeSpec, Schema, SchemaSpec,
};

/// The top node type: `block+`.
pub const DOC: &str = "doc";
/// A paragraph: `inline*`. A paragraph with no content is an *empty
/// paragraph*, which the codec writes as a line holding only `<br>`.
pub const PARAGRAPH: &str = "paragraph";
/// An ATX or setext heading: `inline*`, attribute `level` (`Int`, 1..=6,
/// default 1). Always written back as ATX.
pub const HEADING: &str = "heading";
/// A block quote: `block+`.
pub const BLOCKQUOTE: &str = "blockquote";
/// A code block: `text*`, no marks, `code: true`.
///
/// Attributes:
/// * `language` (`Str`, default `""`) — the first word of the info string.
/// * `fence_char` (`Str`, default `` "`" ``) — cosmetic; `` ` `` or `~`.
/// * `fence_length` (`Int`, default 3) — cosmetic minimum fence width.
///
/// An indented code block imports as a `code_block` with the default cosmetic
/// attributes and is written back fenced; the rendered HTML is the same.
pub const CODE_BLOCK: &str = "code_block";
/// A bullet list: `item+`.
///
/// Attributes:
/// * `tight` (`Bool`, default `true`) — whether the items render without `<p>`
///   wrappers, i.e. whether the source has blank lines between them.
/// * `bullet_char` (`Str`, default `"-"`) — cosmetic; `-`, `*` or `+`.
pub const BULLET_LIST: &str = "bullet_list";
/// An ordered list: `item+`.
///
/// Attributes:
/// * `tight` (`Bool`, default `true`) — as for [`BULLET_LIST`].
/// * `start` (`Int`, default 1) — the ordinal of the first item.
/// * `delimiter` (`Str`, default `"."`) — cosmetic; `.` or `)`.
pub const ORDERED_LIST: &str = "ordered_list";
/// A plain list item: `block+`, group `item`.
pub const LIST_ITEM: &str = "list_item";
/// A GFM task list item: `paragraph block*`, group `item`, attribute `checked`
/// (`Bool`, default `false`). The check box lives in the first paragraph, so
/// unlike [`LIST_ITEM`] this one must begin with a paragraph.
pub const TASK_ITEM: &str = "task_item";
/// A thematic break. A selectable block leaf.
pub const HORIZONTAL_RULE: &str = "horizontal_rule";
/// Source text for a block construct the model does not interpret — an HTML
/// block, an HTML comment, a footnote definition: `text*`, no marks,
/// `code: true`, like [`CODE_BLOCK`] and with no attributes at all.
///
/// Its text *is* the source, line endings and all, with no trailing one, and is
/// written back unchanged. The editor shows it rather than rendering it, so it
/// is ordinary editable text: an edit that leaves something that is no longer
/// an HTML block is read as whatever it has become the next time the source is
/// parsed.
pub const RAW_BLOCK: &str = "raw_block";
/// A GFM table: `table_row+`, attribute `alignments` (`Str`, default `""`).
///
/// `alignments` is a comma-separated list with one entry per column, each
/// `left`, `center`, `right` or `none` — `"none,center,right"` for a table of
/// three columns. Its length **is** the column count, and every row holds
/// exactly that many cells; both importers normalise to that invariant, GFM
/// style: a surplus cell is dropped and a missing one arrives empty.
pub const TABLE: &str = "table";
/// One row of a [`TABLE`]: `table_cell+`.
///
/// There is no header type. GFM has exactly one header row and it is always
/// the first, so *the first row of a table is its header row* and nothing has
/// to be recorded for it.
pub const TABLE_ROW: &str = "table_row";
/// One cell of a [`TABLE_ROW`]: `inline*`, a textblock allowing the marks a
/// paragraph allows. Neither `colspan` nor `rowspan` is modelled.
pub const TABLE_CELL: &str = "table_cell";
/// The text type, group `inline`.
pub const TEXT: &str = "text";
/// An image: an inline atom with `src` (`Str`, required), `alt` (`Str`,
/// default `""`) and `title` (`Str`, default `""`). The label of an imported
/// image is flattened to plain text, which is what CommonMark's `alt`
/// attribute holds anyway.
pub const IMAGE: &str = "image";
/// A hard line break: an inline atom in the groups `inline` and
/// [`LINE_BREAK_GROUP`](markraft_core::projection::LINE_BREAK_GROUP).
pub const HARD_BREAK: &str = "hard_break";
/// A source line ending inside a paragraph, displayed as a space. Keeping the
/// primitive matters when surrounding raw HTML changes whitespace semantics.
pub const SOFT_BREAK: &str = "soft_break";

/// A transparent inline container. Its marks wrap its entire content, preserving
/// nested marks and their order where a flat mark set would lose information.
pub const INLINE_SPAN: &str = "inline_span";
/// One CommonMark inline HTML primitive, retained in the `source` attribute.
/// It is an editable/selectable atom; its source is never interpreted as text.
pub const RAW_INLINE: &str = "raw_inline";
/// An Obsidian-style wiki link: an inline atom with `target` (`Str`,
/// required), `alias` (`Str`, default `""`) and `embed` (`Bool`, default
/// `false`).
///
/// The attributes hold the bytes the source spelled, `#heading`/`^block`
/// suffixes and surrounding spaces included, and the codec writes them back as
/// `[[target]]`, `[[target|alias]]` or with a leading `!` for an embed, with no
/// normalisation of either part.
///
/// It is an *atom* rather than a mark for three reasons: the source has to
/// round-trip verbatim, `[[Note]]` has no display text apart from its target to
/// edit, and what stands inside the brackets — a heading, a block id, an image
/// size — is Obsidian's sub-syntax, which this codec never interprets.
/// [`crate::wiki`] says which spellings are read as one.
pub const WIKI_LINK: &str = "wiki_link";

/// A link, `inclusive: false`, with `href` (`Str`, required) and `title`
/// (`Str`, default `""`).
pub const LINK: &str = "link";
/// Strong emphasis.
pub const STRONG: &str = "strong";
/// Emphasis.
pub const EM: &str = "em";
/// GFM strikethrough.
pub const STRIKETHROUGH: &str = "strikethrough";
/// Underline, written as `<u>…</u>`; CommonMark has no syntax for it.
pub const UNDERLINE: &str = "underline";
/// A code span.
pub const CODE: &str = "code";

/// The group holding [`STRONG`], [`EM`], [`STRIKETHROUGH`] and [`UNDERLINE`]:
/// the marks that have a delimiter run or a tag of their own.
pub const STYLE_GROUP: &str = "style";
/// The group holding every block node type.
pub const BLOCK_GROUP: &str = "block";
/// The group holding every inline node type.
pub const INLINE_GROUP: &str = "inline";
/// The group holding [`LIST_ITEM`] and [`TASK_ITEM`], so both list types accept
/// either kind of item.
pub const ITEM_GROUP: &str = "item";

fn str_attr(name: &str, default: &str) -> AttrSpec {
    AttrSpec::new(name, AttrKind::Str, AttrValue::Str(default.to_string()))
}

/// The CommonMark/GFM schema as plain data, ready to extend.
///
/// ```
/// use markraft_core::{NodeTypeSpec, Schema};
/// use markraft_commonmark::commonmark_schema_spec;
///
/// let spec = commonmark_schema_spec().node(NodeTypeSpec::new("callout", "block+").group("block"));
/// let schema = Schema::new(spec).unwrap();
/// assert!(schema.node_id("callout").is_some());
/// ```
pub fn commonmark_schema_spec() -> SchemaSpec {
    SchemaSpec::new()
        .node(NodeTypeSpec::new(DOC, "block+"))
        .node(NodeTypeSpec::new(PARAGRAPH, "inline*").group(BLOCK_GROUP))
        .node(
            NodeTypeSpec::new(HEADING, "inline*")
                .group(BLOCK_GROUP)
                .defining(true)
                .attr(AttrSpec::new("level", AttrKind::Int, AttrValue::Int(1))),
        )
        .node(
            NodeTypeSpec::new(BLOCKQUOTE, "block+")
                .group(BLOCK_GROUP)
                .defining(true),
        )
        .node(
            NodeTypeSpec::new(CODE_BLOCK, "text*")
                .group(BLOCK_GROUP)
                .code(true)
                .defining(true)
                .marks("")
                .attr(str_attr("language", ""))
                .attr(str_attr("fence_char", "`"))
                .attr(AttrSpec::new(
                    "fence_length",
                    AttrKind::Int,
                    AttrValue::Int(3),
                )),
        )
        .node(
            NodeTypeSpec::new(BULLET_LIST, "item+")
                .group(BLOCK_GROUP)
                .attr(AttrSpec::new(
                    "tight",
                    AttrKind::Bool,
                    AttrValue::Bool(true),
                ))
                .attr(str_attr("bullet_char", "-")),
        )
        .node(
            NodeTypeSpec::new(ORDERED_LIST, "item+")
                .group(BLOCK_GROUP)
                .attr(AttrSpec::new(
                    "tight",
                    AttrKind::Bool,
                    AttrValue::Bool(true),
                ))
                .attr(AttrSpec::new("start", AttrKind::Int, AttrValue::Int(1)))
                .attr(str_attr("delimiter", ".")),
        )
        .node(
            NodeTypeSpec::new(LIST_ITEM, "block+")
                .group(ITEM_GROUP)
                .defining(true),
        )
        .node(
            NodeTypeSpec::new(TASK_ITEM, "paragraph block*")
                .group(ITEM_GROUP)
                .defining(true)
                .attr(AttrSpec::new(
                    "checked",
                    AttrKind::Bool,
                    AttrValue::Bool(false),
                )),
        )
        .node(
            NodeTypeSpec::leaf(HORIZONTAL_RULE)
                .group(BLOCK_GROUP)
                .selectable(true),
        )
        .node(
            NodeTypeSpec::new(RAW_BLOCK, "text*")
                .group(BLOCK_GROUP)
                .code(true)
                .defining(true)
                .marks(""),
        )
        .node(
            NodeTypeSpec::new(TABLE, "table_row+")
                .group(BLOCK_GROUP)
                .isolating(true)
                .attr(str_attr("alignments", "")),
        )
        .node(NodeTypeSpec::new(TABLE_ROW, "table_cell+"))
        .node(NodeTypeSpec::new(TABLE_CELL, "inline*").isolating(true))
        .node(NodeTypeSpec::text(TEXT).group(INLINE_GROUP))
        .node(
            NodeTypeSpec::new(INLINE_SPAN, "inline*")
                .inline(true)
                .group(INLINE_GROUP),
        )
        .node(
            NodeTypeSpec::leaf(RAW_INLINE)
                .inline(true)
                .group(INLINE_GROUP)
                .selectable(true)
                .atom(true)
                .attr(AttrSpec::required("source", AttrKind::Str)),
        )
        .node(
            NodeTypeSpec::leaf(WIKI_LINK)
                .inline(true)
                .group(INLINE_GROUP)
                .selectable(true)
                .atom(true)
                .attr(AttrSpec::required("target", AttrKind::Str))
                .attr(str_attr("alias", ""))
                .attr(AttrSpec::new(
                    "embed",
                    AttrKind::Bool,
                    AttrValue::Bool(false),
                )),
        )
        .node(
            NodeTypeSpec::leaf(IMAGE)
                .inline(true)
                .group(INLINE_GROUP)
                .selectable(true)
                .atom(true)
                .attr(AttrSpec::required("src", AttrKind::Str))
                .attr(str_attr("alt", ""))
                .attr(str_attr("title", "")),
        )
        .node(
            NodeTypeSpec::leaf(SOFT_BREAK)
                .inline(true)
                .group(format!("{INLINE_GROUP} soft_break")),
        )
        .node(NodeTypeSpec::leaf(HARD_BREAK).inline(true).group(format!(
            "{INLINE_GROUP} {}",
            markraft_core::projection::LINE_BREAK_GROUP
        )))
        .mark(
            MarkTypeSpec::new(LINK)
                .rank(10)
                .inclusive(false)
                .attr(AttrSpec::required("href", AttrKind::Str))
                .attr(str_attr("title", "")),
        )
        .mark(MarkTypeSpec::new(UNDERLINE).rank(20).group(STYLE_GROUP))
        .mark(MarkTypeSpec::new(STRIKETHROUGH).rank(30).group(STYLE_GROUP))
        .mark(MarkTypeSpec::new(STRONG).rank(40).group(STYLE_GROUP))
        .mark(MarkTypeSpec::new(EM).rank(50).group(STYLE_GROUP))
        .mark(MarkTypeSpec::new(CODE).rank(60))
}

/// The compiled CommonMark/GFM schema.
///
/// # Panics
///
/// Never: the preset is validated by this crate's tests.
pub fn commonmark_schema() -> Schema {
    Schema::new(commonmark_schema_spec()).expect("the CommonMark schema spec is valid")
}
