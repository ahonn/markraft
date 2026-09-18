//! Plain text, for the `text/plain` flavour of a clipboard and for search.
//!
//! One line per block, and one line per hard break inside a block. An atom
//! contributes the text a reader would see in its place: an image's `alt`, a
//! raw block's source. A thematic break contributes nothing, because it has no
//! text.

use markraft_doc::{Node, Schema, Slice};

use crate::schema as md;

/// The plain text of a whole document.
///
/// ```
/// use markraft_markdown::{commonmark_schema, from_markdown, to_plain_text};
///
/// let schema = commonmark_schema();
/// let doc = from_markdown(&schema, "# Title\n\nsome *text*").unwrap();
/// assert_eq!(to_plain_text(&schema, &doc), "Title\nsome text");
/// ```
pub fn to_plain_text(schema: &Schema, doc: &Node) -> String {
    doc.text_between(
        schema,
        0,
        doc.content_size(),
        Some("\n"),
        Some(&|node| leaf_text(schema, node)),
    )
}

/// The plain text of a slice — what a copied selection puts on the clipboard
/// beside its Markdown.
///
/// This is [`markraft_doc::projection::slice_to_plain_text`], wrapped so a host
/// finds both halves of the plain-text surface in one place.
pub fn slice_to_plain_text(schema: &Schema, slice: &Slice) -> String {
    markraft_doc::projection::slice_to_plain_text(schema, slice)
}

/// What a leaf reads as when its structure is thrown away.
fn leaf_text(schema: &Schema, node: &Node) -> String {
    let name = schema.node_type(node.type_id()).name();
    let attr = |key: &str| {
        node.attrs()
            .get(key)
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string()
    };
    match name {
        md::HARD_BREAK => "\n".to_string(),
        md::IMAGE => attr("alt"),
        md::RAW_BLOCK => attr("source"),
        _ => String::new(),
    }
}
