//! Plain text, for the `text/plain` flavour of a clipboard and for search.
//!
//! One line per block, and one line per hard break inside a block. An atom
//! contributes the text a reader would see in its place: an image's `alt`, a
//! wiki link's label. A thematic break contributes nothing, because it has no
//! text; a raw block contributes its source, which is its text already.
//!
//! A table is the one block that is not one line: its cells are separated by
//! tabs and its rows by line endings, which is what a spreadsheet reads and
//! what every other application writes.

use markraft_core::{Fragment, MarkSet, Node, Schema, Slice};

use crate::schema as md;

/// The plain text of a whole document.
///
/// ```
/// use markraft_commonmark::{commonmark_schema, from_markdown, to_plain_text};
///
/// let schema = commonmark_schema();
/// let doc = from_markdown(&schema, "# Title\n\nsome *text*").unwrap();
/// assert_eq!(to_plain_text(&schema, &doc), "Title\nsome text");
/// ```
pub fn to_plain_text(schema: &Schema, doc: &Node) -> String {
    let cell = |cell: &Node| {
        cell.text_between(
            schema,
            0,
            cell.content_size(),
            None,
            Some(&|node| leaf_text(schema, node)),
        )
    };
    let flat = flatten_tables(schema, doc, &cell);
    let doc = flat.as_ref().unwrap_or(doc);
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
/// This is [`markraft_core::projection::slice_to_plain_text`] over a slice
/// whose tables have been laid out as rows of tab-separated cells, so a host
/// finds both halves of the plain-text surface in one place.
pub fn slice_to_plain_text(schema: &Schema, slice: &Slice) -> String {
    let cell = |cell: &Node| {
        markraft_core::projection::slice_to_plain_text(
            schema,
            &Slice::new(cell.content().clone(), 0, 0),
        )
    };
    let flat = flatten_fragment(schema, slice.content(), &cell);
    let Some(content) = flat else {
        return markraft_core::projection::slice_to_plain_text(schema, slice);
    };
    let flattened = Slice::new(content, slice.open_start(), slice.open_end());
    markraft_core::projection::slice_to_plain_text(schema, &flattened)
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
        md::SOFT_BREAK => " ".to_string(),
        md::HARD_BREAK => "\n".to_string(),
        md::IMAGE => attr("alt"),
        // An atom keeps the source's own spacing in its attributes, so `[[ a ]]`
        // reads as the label without it, the way the editor draws one. An embed
        // reads as its target too, where the editor has room to shorten it to the
        // file it names.
        md::WIKI_LINK => {
            let (target, alias) = (attr("target"), attr("alias"));
            crate::wiki::label(target.trim(), alias.trim()).to_string()
        }
        _ => String::new(),
    }
}

/// Replace every table in `node` with a paragraph holding its rows, or answer
/// `None` when there is no table to replace.
///
/// Turning the table into an ordinary textblock is what lets both projections
/// stay as they are: a block that already holds its own line endings needs no
/// rule of its own in either of them.
fn flatten_tables(
    schema: &Schema,
    node: &Node,
    cell_text: &dyn Fn(&Node) -> String,
) -> Option<Node> {
    if !node.is_container() {
        return None;
    }
    if schema.node_id(md::TABLE) == Some(node.type_id()) {
        return as_paragraph(schema, &rows_text(node, cell_text));
    }
    let content = flatten_fragment(schema, node.content(), cell_text)?;
    Some(node.copy(content))
}

fn flatten_fragment(
    schema: &Schema,
    content: &Fragment,
    cell_text: &dyn Fn(&Node) -> String,
) -> Option<Fragment> {
    let mut children: Vec<Node> = Vec::with_capacity(content.child_count());
    let mut changed = false;
    for child in content.iter() {
        match flatten_tables(schema, child, cell_text) {
            Some(flat) => {
                changed = true;
                children.push(flat);
            }
            None => children.push(child.clone()),
        }
    }
    changed.then(|| Fragment::from_nodes(children))
}

/// A table's cells, tabs between the cells of a row and a line ending between
/// the rows.
fn rows_text(table: &Node, cell_text: &dyn Fn(&Node) -> String) -> String {
    table
        .children()
        .map(|row| row.children().map(cell_text).collect::<Vec<_>>().join("\t"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn as_paragraph(schema: &Schema, text: &str) -> Option<Node> {
    let ty = schema.node_id(md::PARAGRAPH)?;
    let content = if text.is_empty() {
        Fragment::empty()
    } else {
        Fragment::from_node(schema.text(text))
    };
    schema
        .create(
            ty,
            schema.node_type(ty).default_attrs().clone(),
            MarkSet::empty(),
            content,
        )
        .ok()
}
