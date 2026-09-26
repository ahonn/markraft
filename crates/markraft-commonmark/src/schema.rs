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
//! the source stays editable in place.
//!
//! # Inline content is source
//!
//! A [`PARAGRAPH`]'s, a [`HEADING`]'s and a [`TABLE_CELL`]'s text *is* its
//! Markdown inline source (see the [crate docs](crate)), as a reader sees it
//! once the block's own prefixes are stripped. Each line ending is one [`LINE_BREAK`] atom; a hard break's
//! spelling (`\`, trailing spaces or `<br>`) is ordinary text before it.
//! What the tree holds as an atom rather than as text is what a reader never
//! shows as its characters: an [`IMAGE`], a [`WIKI_LINK`], a [`RAW_INLINE`]
//! HTML tag, an [`EMOJI`] shortcode.
//!
//! Every style mark is *derived* from that text by
//! [`derive`](crate::derive::derive) and kept in step with it by the
//! canonicalising correction in [`commonmark_extensions`](crate::commonmark_extensions):
//! a style covers its delimiters as well as its content, and the characters
//! that spell rather than say — delimiters, a backslash, an entity, a hard
//! break's spelling — carry [`SYNTAX`] too. Nothing else authors a style mark,
//! and the serialiser writes the text as it stands.
//!
//! # Mark ranks
//!
//! A mark set is sorted by rank; the ranks decide the order marks are opened
//! in when [`spell`](crate::serialize::spell) turns semantic inline content —
//! pasted HTML — into source:
//!
//! | rank | mark | spelled as |
//! |-----:|------|------------|
//! | 10 | [`LINK`] | `[…](href "title")` — outermost, so a link wraps its styling |
//! | 12 | [`FOOTNOTE_REFERENCE`] | `[^label]` |
//! | 20 | [`UNDERLINE`] | `<u>…</u>` |
//! | 25 | [`HIGHLIGHT`] | `==…==` |
//! | 30 | [`STRIKETHROUGH`] | `~~…~~` |
//! | 40 | [`STRONG`] | `**…**` |
//! | 50 | [`EM`] | `*…*` |
//! | 55 | [`SUPERSCRIPT`] | `^…^` |
//! | 56 | [`SUBSCRIPT`] | `~…~` |
//! | 58 | [`KEYBOARD`] | `<kbd>…</kbd>` |
//! | 60 | [`CODE`] | `` `…` `` — innermost, because its content is literal |
//! | 65 | [`MATH`] | `$…$` or `$$…$$` — literal, as code is |
//! | 70 | [`SYNTAX`] | never spelled: it marks spelling |
//!
//! [`CODE`] and [`MATH`] exclude nothing but themselves: a code span or a
//! formula carries whatever styles surround it, `` *`code`* `` being
//! `<em><code>code</code></em>`.
//!
//! # Why `block+` for list items but not for task items
//!
//! CommonMark lets a list item start with any block — `- > quote` and
//! `- - nested` are both normal — so [`LIST_ITEM`] takes `block+` rather than
//! `paragraph block*`. The stricter rule would force the importer to invent a
//! leading empty paragraph for those items, which would then be an empty
//! paragraph with no CommonMark spelling.
//!
//! A [`TASK_ITEM`] is the exception, because its check box is written at the
//! start of its first block's line: an item whose first block is a nested
//! list or a fence has nowhere to write `[x]` and is not a task item at all.
//! GFM only reads a box before a paragraph; this editor also reads one before
//! a heading or a quote, `- [ ] # title` and `- [ ] > quote`, and writes those
//! when a task's line is made one — `parse_ast`
//! does the reading. Its
//! content rule says `(paragraph | heading | blockquote) block*` so the tree
//! cannot describe something the format cannot write.
//!
//! An emptied item of either kind is repaired by the
//! [`fill_required_content`](markraft_core::corrections::fill_required_content) correction
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
//! [`TABLE`] and [`TABLE_CELL`] are `isolating`: a deletion at a cell boundary
//! must not merge two cells, and a table's structure is not something the text
//! around it may dissolve.

use markraft_core::kind::{
    CODE_BLOCK_LANGUAGE_ATTR, HEADING_LEVEL_ATTR, LINK_HREF_ATTR, SYNTAX_DISPLAY_ATTR,
    SYNTAX_SPAN_ATTR, TABLE_ALIGNMENTS_ATTR, TASK_CHECKED_ATTR,
};
use markraft_core::{
    AttrKind, AttrSpec, AttrValue, BreakKind, MarkTypeSpec, NodeTypeSpec, Schema, SchemaSpec,
};

/// The top node type: `block+`.
pub const DOC: &str = "doc";
/// A paragraph: `(inline | line_break)*`. A paragraph with no content is an *empty
/// paragraph*. CommonMark has no spelling for those; they write as blank
/// separators (and may collapse on re-read). A lone `<br>` HTML block reads as
/// one.
pub const PARAGRAPH: &str = "paragraph";
/// An ATX or setext heading: `(inline | line_break)*`, attribute `level`
/// (`Int`, 1..=6, default 1). Written ATX, or setext when a level 1 or 2
/// heading holds a line break.
pub const HEADING: &str = "heading";
/// A block quote: `block+`.
///
/// Attributes, all empty on an ordinary quote:
/// * `callout` (`Str`, default `""`) — a callout's type, exactly as
///   written. A non-empty value is what makes the quote a callout.
/// * `fold` (`Str`, default `""`) — `"-"` or `"+"`, the fold marker after the
///   type. Kept as a byte; this codec always shows the content.
/// * `title` (`Str`, default `""`) — the raw title after the marker, not read
///   as inline content.
///
/// A callout is this node rather than one of its own so that every structural
/// command, key binding and correction that works on a block quote keeps
/// working on it. [`crate::callout`] says which first lines are markers.
pub const BLOCKQUOTE: &str = "blockquote";
/// A footnote definition: `block+`, attribute `label` (`Str`, required) —
/// the label as written between `[^` and `]`. Written `[^label]: ` before its
/// first line, with its other lines indented four columns, where the source
/// had it: comrak moves every definition to the end of the document, and
/// `parse_ast` puts each back.
///
/// A reference is text, `[^label]`, read as [`FOOTNOTE_REFERENCE`] against
/// the definitions the document holds.
pub const FOOTNOTE_DEFINITION: &str = "footnote_definition";
/// The attribute holding a footnote's label, on [`FOOTNOTE_DEFINITION`] and
/// [`FOOTNOTE_REFERENCE`].
pub use markraft_core::kind::FOOTNOTE_LABEL_ATTR;
/// A reference to a footnote, `[^label]`, derived over the whole spelling
/// wherever the document defines that label: `inclusive: false`, attribute
/// `label` (`Str`, required). `[^label]` with no definition is text.
pub const FOOTNOTE_REFERENCE: &str = "footnote_reference";
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
/// * `same_ordinal` (`Bool`, default `false`) — cosmetic; every item is
///   written with the first one's number, `1.` `1.` `1.`, as the source had
///   it. A reader counts them either way.
pub const ORDERED_LIST: &str = "ordered_list";
/// A plain list item: `block+`, group `item`.
pub const LIST_ITEM: &str = "list_item";
/// A GFM task list item: `(paragraph | heading | blockquote) block*`, group
/// `item`, attribute `checked` (`Bool`, default `false`). The check box is
/// written before the first block, so unlike [`LIST_ITEM`] this one must begin
/// with a block that can follow it on its line.
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
/// A line ending inside a paragraph or a heading: an inline atom declared a
/// [`BreakKind::Hard`] break and not in `inline`, so a table cell, which is
/// one source line, cannot hold one.
///
/// Whether it is a hard break is the text's to say: `\` or two spaces before
/// it make it one, as they do in the source, and
/// [`derive`](crate::derive::derive) reports which.
pub const LINE_BREAK: &str = "line_break";
/// One CommonMark inline HTML primitive, retained in the `source` attribute.
/// It is an editable/selectable atom; its source is never interpreted as text.
pub const RAW_INLINE: &str = "raw_inline";
/// A wiki link: an inline atom with `target` (`Str`,
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
/// size — is the link's own sub-syntax, which this codec never interprets.
/// [`crate::wiki`] says which spellings are read as one.
pub const WIKI_LINK: &str = "wiki_link";
/// An emoji shortcode, `:smile:`: an inline atom with `code` (`Str`,
/// required), the name between the colons, written back as `:code:`.
/// [`crate::shortcode`] says which spellings are read as one.
pub const EMOJI: &str = "emoji";

/// A link, `inclusive: false`, with `href` (`Str`, required) and `title`
/// (`Str`, default `""`).
pub const LINK: &str = "link";
/// Strong emphasis.
pub const STRONG: &str = "strong";
/// Emphasis.
pub const EM: &str = "em";
/// GFM strikethrough.
pub const STRIKETHROUGH: &str = "strikethrough";
/// Underline: a paired `<u>`…`</u>` in the text.
pub const UNDERLINE: &str = "underline";
/// Highlighted text: `==…==`, or a paired `<mark>`…`</mark>`.
pub const HIGHLIGHT: &str = "highlight";
/// Superscript: `^…^`, or a paired `<sup>`…`</sup>`.
pub const SUPERSCRIPT: &str = "superscript";
/// Subscript: `~…~`, or a paired `<sub>`…`</sub>`. A single tilde is
/// subscript and a double one strikethrough.
pub const SUBSCRIPT: &str = "subscript";
/// A key or key combination: a paired `<kbd>`…`</kbd>` in the text.
pub const KEYBOARD: &str = "keyboard";
/// A code span.
pub const CODE: &str = "code";
/// A formula: `$…$`, `$$…$$` or `` $`…`$ ``, with [`MATH_DISPLAY_ATTR`]
/// (`Bool`, default `false`) set for the `$$` spelling. Its content is TeX
/// source, which nothing here interprets: no style is read inside it.
pub const MATH: &str = "math";
/// Whether a [`MATH`] span is display math, spelled `$$…$$`.
pub const MATH_DISPLAY_ATTR: &str = "display";
/// The characters that spell rather than say — a style's delimiters, an
/// escape's backslash, an entity, a hard break's spelling — which a view
/// conceals while the caret is away. `inclusive: false`, with:
///
/// * `span` (`Int`, default 0) — shared by the opening and closing runs of one
///   span, so a view reveals them together; an escape, an entity and a hard
///   break each have their own. Numbered within the block in the order their
///   first runs appear.
/// * `display` (`Str`, default `""`) — what a reader sees in the run's place:
///   the decoded character of an entity, nothing for every other run.
///
/// The attributes also keep two neighbouring runs apart: `**` next to a
/// `` ` `` are two spans, not one run.
pub const SYNTAX: &str = "syntax";

/// The group holding [`STRONG`], [`EM`], [`STRIKETHROUGH`], [`UNDERLINE`],
/// [`HIGHLIGHT`], [`SUPERSCRIPT`], [`SUBSCRIPT`] and [`KEYBOARD`]: the marks that have a
/// delimiter run or a tag of their own.
pub const STYLE_GROUP: &str = "style";
/// The group holding every block node type.
pub const BLOCK_GROUP: &str = "block";
/// The group holding every inline node type.
pub const INLINE_GROUP: &str = "inline";
/// The group holding [`LIST_ITEM`] and [`TASK_ITEM`], so both list types accept
/// either kind of item.
pub const ITEM_GROUP: &str = "item";

/// What a paragraph and a heading hold: inline content and line breaks.
const TEXTBLOCK_CONTENT: &str = "(inline | line_break)*";

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
        .node(NodeTypeSpec::new(PARAGRAPH, TEXTBLOCK_CONTENT).group(BLOCK_GROUP))
        .node(
            NodeTypeSpec::new(HEADING, TEXTBLOCK_CONTENT)
                .group(BLOCK_GROUP)
                .defining(true)
                .attr(AttrSpec::new(
                    HEADING_LEVEL_ATTR,
                    AttrKind::Int,
                    AttrValue::Int(1),
                )),
        )
        .node(
            NodeTypeSpec::new(FOOTNOTE_DEFINITION, "block+")
                .group(BLOCK_GROUP)
                .defining(true)
                .attr(AttrSpec::required(FOOTNOTE_LABEL_ATTR, AttrKind::Str)),
        )
        .node(
            NodeTypeSpec::new(BLOCKQUOTE, "block+")
                .group(BLOCK_GROUP)
                .defining(true)
                .attr(str_attr("callout", ""))
                .attr(str_attr("fold", ""))
                .attr(str_attr("title", "")),
        )
        .node(
            NodeTypeSpec::new(CODE_BLOCK, "text*")
                .group(BLOCK_GROUP)
                .code(true)
                .defining(true)
                .marks("")
                .attr(str_attr(CODE_BLOCK_LANGUAGE_ATTR, ""))
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
                .attr(str_attr("delimiter", "."))
                .attr(AttrSpec::new(
                    "same_ordinal",
                    AttrKind::Bool,
                    AttrValue::Bool(false),
                )),
        )
        .node(
            NodeTypeSpec::new(LIST_ITEM, "block+")
                .group(ITEM_GROUP)
                .defining(true),
        )
        .node(
            NodeTypeSpec::new(TASK_ITEM, "(paragraph | heading | blockquote) block*")
                .group(ITEM_GROUP)
                .defining(true)
                .attr(AttrSpec::new(
                    TASK_CHECKED_ATTR,
                    AttrKind::Bool,
                    AttrValue::Bool(false),
                )),
        )
        .node(
            NodeTypeSpec::leaf(HORIZONTAL_RULE)
                .group(BLOCK_GROUP)
                .selectable(true)
                // The character the break is written with: `-`, `*` or `_`.
                .attr(str_attr("mark", "-")),
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
                .attr(str_attr(TABLE_ALIGNMENTS_ATTR, "")),
        )
        .node(NodeTypeSpec::new(TABLE_ROW, "table_cell+"))
        .node(NodeTypeSpec::new(TABLE_CELL, "inline*").isolating(true))
        .node(NodeTypeSpec::text(TEXT).group(INLINE_GROUP))
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
            NodeTypeSpec::leaf(EMOJI)
                .inline(true)
                .group(INLINE_GROUP)
                .selectable(true)
                .atom(true)
                .attr(AttrSpec::required("code", AttrKind::Str)),
        )
        .node(
            NodeTypeSpec::leaf(IMAGE)
                .inline(true)
                .group(INLINE_GROUP)
                .selectable(true)
                .atom(true)
                .attr(AttrSpec::required("src", AttrKind::Str))
                .attr(str_attr("alt", ""))
                .attr(str_attr("title", ""))
                // The `<img>` tag an image was read from, written back as it
                // was; empty for a Markdown image.
                .attr(str_attr("source", ""))
                // The size that tag asks for, as written; empty when it asks for
                // none, and always for a Markdown image.
                .attr(str_attr("width", ""))
                .attr(str_attr("height", "")),
        )
        .node(
            NodeTypeSpec::leaf(LINE_BREAK)
                .inline(true)
                .break_kind(BreakKind::Hard),
        )
        .mark(
            MarkTypeSpec::new(LINK)
                .rank(10)
                .inclusive(false)
                .attr(AttrSpec::required(LINK_HREF_ATTR, AttrKind::Str))
                .attr(str_attr("title", "")),
        )
        .mark(
            MarkTypeSpec::new(FOOTNOTE_REFERENCE)
                .rank(12)
                .inclusive(false)
                .attr(AttrSpec::required(FOOTNOTE_LABEL_ATTR, AttrKind::Str)),
        )
        .mark(MarkTypeSpec::new(UNDERLINE).rank(20).group(STYLE_GROUP))
        .mark(MarkTypeSpec::new(HIGHLIGHT).rank(25).group(STYLE_GROUP))
        .mark(MarkTypeSpec::new(STRIKETHROUGH).rank(30).group(STYLE_GROUP))
        .mark(MarkTypeSpec::new(STRONG).rank(40).group(STYLE_GROUP))
        .mark(MarkTypeSpec::new(EM).rank(50).group(STYLE_GROUP))
        .mark(MarkTypeSpec::new(SUPERSCRIPT).rank(55).group(STYLE_GROUP))
        .mark(MarkTypeSpec::new(SUBSCRIPT).rank(56).group(STYLE_GROUP))
        .mark(MarkTypeSpec::new(KEYBOARD).rank(58).group(STYLE_GROUP))
        .mark(MarkTypeSpec::new(CODE).rank(60))
        .mark(
            MarkTypeSpec::new(MATH)
                .rank(65)
                .shared(true)
                .attr(AttrSpec::new(
                    MATH_DISPLAY_ATTR,
                    AttrKind::Bool,
                    AttrValue::Bool(false),
                )),
        )
        .mark(
            // Every delimiter carries one, and its values repeat from block to
            // block: span ids restart at 0 in each, and a display is nearly
            // always empty.
            MarkTypeSpec::new(SYNTAX)
                .rank(70)
                .inclusive(false)
                .shared(true)
                .attr(str_attr(SYNTAX_DISPLAY_ATTR, ""))
                .attr(AttrSpec::new(
                    SYNTAX_SPAN_ATTR,
                    AttrKind::Int,
                    AttrValue::Int(0),
                )),
        )
}

/// The compiled CommonMark/GFM schema.
///
/// # Panics
///
/// Never: the preset is validated by this crate's tests.
pub fn commonmark_schema() -> Schema {
    Schema::new(commonmark_schema_spec()).expect("the CommonMark schema spec is valid")
}
