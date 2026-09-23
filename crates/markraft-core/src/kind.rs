//! A document kind as an editor sees it: the names a schema gives the roles an
//! editing surface knows about, and the codecs that read and write its content.
//!
//! Neither item names a concrete document kind, a platform or a schema type, so
//! a view can be built against them and a host plugs its own kind in. The
//! CommonMark implementations live in `markraft-commonmark`.
//!
//! This module is a contract between a view and a document kind, and nothing
//! else in this crate reads it: the model, the change system and every command
//! take a [`NodeTypeId`](crate::NodeTypeId) or a
//! [`MarkTypeId`](crate::MarkTypeId) and never ask what role it plays. The
//! roles below are therefore not the model's idea of what a document is — they
//! are the vocabulary editing surfaces have converged on, which is the one
//! CommonMark and GFM gave them. A kind that has no heading leaves
//! [`DocTypeNames::heading`] `None` and the bindings that would need it do
//! nothing; a kind whose roles are not on this list resolves its own ids and
//! hands the view whatever it needs beside this table.

use std::ops::Range;

use crate::node::Node;
use crate::projection::Line;
use crate::slice::Slice;

/// The integer attribute of the [`DocTypeNames::syntax`] mark naming the span
/// a run belongs to. Runs that open and close one span share it.
pub const SYNTAX_SPAN_ATTR: &str = "span";

/// The string attribute of the [`DocTypeNames::syntax`] mark holding what a
/// reader sees in the run's place while it is concealed.
pub const SYNTAX_DISPLAY_ATTR: &str = "display";

/// The integer attribute of a [`DocTypeNames::heading`] holding its level.
pub const HEADING_LEVEL_ATTR: &str = "level";

/// The string attribute of a [`DocTypeNames::code_block`] holding its
/// language, empty when none is given.
pub const CODE_BLOCK_LANGUAGE_ATTR: &str = "language";

/// The boolean attribute of a [`DocTypeNames::task_item`] saying whether its
/// box is checked.
pub const TASK_CHECKED_ATTR: &str = "checked";

/// The string attribute of a [`DocTypeNames::link`] holding its destination.
pub const LINK_HREF_ATTR: &str = "href";

/// The string attribute of a [`DocTypeNames::table`] holding its column
/// alignments: one entry per column, comma-separated, each `left`, `center`,
/// `right` or `none`. This is the name to hand
/// [`TableTypes`](crate::commands::TableTypes) for a kind that follows this
/// table.
pub const TABLE_ALIGNMENTS_ATTR: &str = "alignments";

/// The string attribute of the [`DocTypeNames::footnote_definition`] node and
/// the [`DocTypeNames::footnote_reference`] mark holding the footnote's label.
pub const FOOTNOTE_LABEL_ATTR: &str = "label";

/// The name a schema gives each role an editing surface and its key bindings
/// need.
///
/// A role the schema does not declare is left `None`; what that disables is
/// described where the resolved ids live. Names are `'static` because a schema
/// preset declares them as constants; a host whose names are computed at
/// runtime resolves the ids itself instead of going through this table.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DocTypeNames {
    /// The default textblock, which a block toggle returns to.
    pub paragraph: Option<&'static str>,
    /// A heading, carrying a [`HEADING_LEVEL_ATTR`] attribute.
    pub heading: Option<&'static str>,
    /// A block quote.
    pub blockquote: Option<&'static str>,
    /// A footnote definition, carrying a [`FOOTNOTE_LABEL_ATTR`] attribute.
    pub footnote_definition: Option<&'static str>,
    /// A code block, carrying a [`CODE_BLOCK_LANGUAGE_ATTR`] attribute.
    pub code_block: Option<&'static str>,
    /// A bullet list.
    pub bullet_list: Option<&'static str>,
    /// An ordered list.
    pub ordered_list: Option<&'static str>,
    /// A plain list item.
    pub list_item: Option<&'static str>,
    /// A list item with a check box, carrying a [`TASK_CHECKED_ATTR`]
    /// attribute.
    pub task_item: Option<&'static str>,
    /// A thematic break: a block-level leaf.
    pub horizontal_rule: Option<&'static str>,
    /// Source text kept verbatim as the block's own text, edited in place.
    pub raw_block: Option<&'static str>,
    /// A table, carrying a [`TABLE_ALIGNMENTS_ATTR`] attribute: one entry per
    /// column, comma-separated, each `left`, `center`, `right` or `none`.
    pub table: Option<&'static str>,
    /// One row of a table. The first row of a table is its header row.
    pub table_row: Option<&'static str>,
    /// One cell of a table row: a textblock.
    pub table_cell: Option<&'static str>,
    /// A line break inside a textblock: an inline atom. Where a kind keeps
    /// its source in the text, whether a break is a hard one is the text's to
    /// say, and this is the atom every line ending of it is.
    pub hard_break: Option<&'static str>,
    /// An image: an inline atom.
    pub image: Option<&'static str>,
    /// Strong emphasis.
    pub strong: Option<&'static str>,
    /// Emphasis.
    pub em: Option<&'static str>,
    /// A code span.
    pub code: Option<&'static str>,
    /// Strikethrough.
    pub strikethrough: Option<&'static str>,
    /// Underline.
    pub underline: Option<&'static str>,
    /// Highlighted text.
    pub highlight: Option<&'static str>,
    /// Superscript.
    pub superscript: Option<&'static str>,
    /// Subscript.
    pub subscript: Option<&'static str>,
    /// A formula, whose content is TeX source rather than prose.
    pub math: Option<&'static str>,
    /// A link, carrying a [`LINK_HREF_ATTR`] attribute.
    pub link: Option<&'static str>,
    /// The mark a kind puts on the characters that *spell* rather than say —
    /// a run that marks up the text around it, or stands for something else,
    /// where the kind keeps that spelling in the document's text. A view
    /// conceals such a run and reveals it while the selection, the caret or a
    /// composition touches the span it belongs to — anywhere from the start of
    /// the span's first run to the end of its last one in the textblock, a
    /// caret at either edge included.
    ///
    /// The mark carries two attributes:
    ///
    /// * [`SYNTAX_SPAN_ATTR`] (an integer) — which span the run belongs to.
    ///   The runs that open and close one span share it, so a view reveals
    ///   them together; a run that stands alone has one of its own. Ids are
    ///   unique within a textblock, not across the document.
    /// * [`SYNTAX_DISPLAY_ATTR`] (a string) — what a reader sees in the run's
    ///   place while it is concealed: empty for a run that shows nothing,
    ///   otherwise the text it stands for.
    ///
    /// A plain-text rendering of the content — a clipboard's, an accessibility
    /// tree's — reads each such run as its [`SYNTAX_DISPLAY_ATTR`]. A kind that
    /// writes its marks some other way leaves this `None` and nothing is
    /// concealed.
    pub syntax: Option<&'static str>,
    /// A reference to a footnote, carrying a [`FOOTNOTE_LABEL_ATTR`]
    /// attribute that names its definition.
    pub footnote_reference: Option<&'static str>,
}

/// How a document kind turns a [`Slice`] into the flavours a clipboard carries,
/// and reads each of them back.
///
/// The three flavours are independent: a kind that has no markup syntax of its
/// own answers `None` from [`Codecs::to_markup`] and the caller falls back to
/// [`Codecs::to_text`]. Every reader answers `None` for input it cannot make a
/// non-empty slice of, so a caller can try them in order.
///
/// Implementations hold their schema and are shared between the threads a
/// platform's pasteboard callbacks run on, hence `Send + Sync`.
// The `from_*` readers take `&self` because a codec holds the schema it reads
// into; they convert a string, not a `Self`.
#[allow(clippy::wrong_self_convention)]
pub trait Codecs: Send + Sync {
    /// The `text/plain` flavour of `slice`.
    fn to_text(&self, slice: &Slice) -> String;

    /// The kind's own markup flavour of `slice` — for CommonMark, Markdown.
    ///
    /// `None` when the kind has no markup syntax, which makes
    /// [`Codecs::to_text`] the flavour written instead.
    fn to_markup(&self, slice: &Slice) -> Option<String>;

    /// The HTML flavour of `slice`, which is what a word processor takes.
    fn to_html(&self, slice: &Slice) -> Option<String>;

    /// Read HTML another application wrote. `None` when nothing came of it.
    fn from_html(&self, html: &str) -> Option<Slice>;

    /// Read the kind's own markup. `None` when nothing came of it.
    fn from_markup(&self, markup: &str) -> Option<Slice>;

    /// Read plain text as structure: line endings become block breaks rather
    /// than characters, and every other character stays the character it is —
    /// a kind whose text is markup escapes what would read as markup. A caller
    /// that wants the characters inserted as typed goes around this.
    fn from_text(&self, text: &str) -> Slice;

    /// What a copy of `slice` carries to a paste in the same kind, in place of
    /// the slice itself.
    ///
    /// A kind whose marks are what its text spells cannot paste a mark without
    /// its spelling — a copy of a styled span's inside would arrive unstyled —
    /// so it completes the spelling here. The default is `slice` unchanged.
    fn copied(&self, slice: &Slice) -> Slice {
        slice.clone()
    }
}

/// How a document kind spells the parts of itself that a view may want to show
/// as source.
///
/// An editing surface that reveals the characters behind what it draws — an
/// image's source, a wiki link's brackets — needs to know what those characters are, and only the kind
/// does. Nothing here is required: a kind whose blocks have no written prefix
/// answers `None` and the view draws only what it drew before.
///
/// This is the counterpart of [`Codecs`] for the *view* rather than for the
/// clipboard, and a host that has no such spelling simply does not supply one.
pub trait SourceSpelling: Send + Sync {
    /// An inline atom as the source it was read from.
    fn atom_source(&self, node: &Node) -> Option<String>;

    /// The atoms the text of `line` spells but holds as text — a picture's
    /// `![alt](src)` the caret was let into — as `char` ranges of the line's
    /// text, each with the atom it spells. A view uses them to show what the
    /// source stands for beside it, as it shows the atom itself once folded.
    fn spelled_atoms(&self, line: &Line) -> Vec<(Range<usize>, Node)>;

    /// What the parts of a verbatim line's source are, where the kind reads
    /// more in it than markup to keep — a link definition's label and
    /// destination — as `char` ranges of the line's text. A view draws such a
    /// line as the prose it describes rather than as quiet source. The
    /// default reads nothing, and the line stays source.
    fn source_highlights(&self, line: &Line) -> Vec<(Range<usize>, SourceHighlight)> {
        let _ = line;
        Vec::new()
    }
}

/// A part of a verbatim line's source; see
/// [`SourceSpelling::source_highlights`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceHighlight {
    /// Characters that only delimit: brackets, a colon.
    Punctuation,
    /// What names the rest, such as the label a link definition defines.
    Label,
    /// Where it leads: a URL or a path.
    Destination,
    /// A title, quotes included.
    Title,
}
