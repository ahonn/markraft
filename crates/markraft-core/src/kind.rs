//! A document kind as an editor sees it: the names a schema gives the roles an
//! editing surface knows about, and the codecs that read and write its content.
//!
//! Neither item names a concrete document kind, a platform or a schema type, so
//! a view can be built against them and a host plugs its own kind in. The
//! CommonMark implementations live in `markraft-commonmark`.

use crate::slice::Slice;

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
    /// A heading, carrying a `level` attribute.
    pub heading: Option<&'static str>,
    /// A block quote.
    pub blockquote: Option<&'static str>,
    /// A code block, carrying a `language` attribute.
    pub code_block: Option<&'static str>,
    /// A bullet list.
    pub bullet_list: Option<&'static str>,
    /// An ordered list.
    pub ordered_list: Option<&'static str>,
    /// A plain list item.
    pub list_item: Option<&'static str>,
    /// A list item with a check box, carrying a `checked` attribute.
    pub task_item: Option<&'static str>,
    /// A thematic break: a block-level leaf.
    pub horizontal_rule: Option<&'static str>,
    /// Source text kept verbatim, in a `source` attribute.
    pub raw_block: Option<&'static str>,
    /// A table, carrying an `alignments` attribute: one entry per column,
    /// comma-separated, each `left`, `center`, `right` or `none`.
    pub table: Option<&'static str>,
    /// One row of a table. The first row of a table is its header row.
    pub table_row: Option<&'static str>,
    /// One cell of a table row: a textblock.
    pub table_cell: Option<&'static str>,
    /// A hard line break: an inline atom.
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
    /// A link, carrying an `href` attribute.
    pub link: Option<&'static str>,
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
    /// than characters. A caller that wants the characters themselves inserts
    /// the text literally instead of going through this.
    fn from_text(&self, text: &str) -> Slice;
}
