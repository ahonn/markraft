//! Plain text, for the `text/plain` flavour of a clipboard and for search.
//!
//! One line per block, and one line per line break inside a block. A
//! textblock's text is Markdown source, so what it spells rather than says —
//! a delimiter, an escape's backslash — is left out, and an entity reads as
//! the character it displays. An atom contributes the text a reader would see
//! in its place: an image's `alt`, a wiki link's label. A thematic break contributes nothing, because it has no
//! text; a raw block contributes its source, which is its text already.
//!
//! A table is the one block that is not one line: its cells are separated by
//! tabs and its rows by line endings, which is what a spreadsheet reads and
//! what every other application writes.

use markraft_core::kind::SYNTAX_DISPLAY_ATTR;
use markraft_core::{Fragment, MarkSet, Node, Schema, Slice};

use crate::schema as md;

/// The plain text of a whole document.
///
/// Concealed runs read as what they display, so the result reads as prose,
/// not as Markdown source.
///
/// ```
/// use markraft_commonmark::{commonmark_schema, from_markdown, to_plain_text};
///
/// let schema = commonmark_schema();
/// let doc = from_markdown(&schema, "# Title\n\nsome *text*").unwrap();
/// assert_eq!(to_plain_text(&schema, &doc), "Title\nsome text");
/// ```
pub fn to_plain_text(schema: &Schema, doc: &Node) -> String {
    let cell = |cell: &Node| prose_between(schema, cell, 0, cell.content_size(), None);
    let flat = flatten_tables(schema, doc, &cell);
    let doc = flat.as_ref().unwrap_or(doc);
    prose_between(schema, doc, 0, doc.content_size(), Some("\n"))
}

/// Like [`Node::text_between`], but reads concealed runs as what they display.
fn prose_between(
    schema: &Schema,
    node: &Node,
    from: usize,
    to: usize,
    block_separator: Option<&str>,
) -> String {
    let mut text = String::new();
    let mut first = true;
    node.nodes_between(from, to, &mut |child, pos, _, _| {
        if let Some(display) = concealed(schema, child) {
            text.push_str(display);
            return true;
        }
        let piece = if let Some(node_text) = child.text() {
            let start = from.max(pos) - pos;
            let end = (to - pos).min(child.text_len());
            let start_b = node_text
                .char_indices()
                .nth(start)
                .map(|(i, _)| i)
                .unwrap_or(node_text.len());
            let end_b = node_text
                .char_indices()
                .nth(end)
                .map(|(i, _)| i)
                .unwrap_or(node_text.len());
            node_text[start_b..end_b].to_string()
        } else if child.is_leaf() {
            leaf_text(schema, child)
        } else {
            String::new()
        };
        let ty = schema.node_type(child.type_id());
        if ty.is_block()
            && (ty.is_textblock() || (child.is_leaf() && !piece.is_empty()))
            && let Some(sep) = block_separator
        {
            if first {
                first = false;
            } else {
                text.push_str(sep);
            }
        }
        text.push_str(&piece);
        true
    });
    text
}

/// The plain text of a slice — what a copied selection puts on the clipboard
/// beside its Markdown.
///
/// Concealed runs read as what they display, so the clipboard reads as prose.
/// Tables are laid out as rows of tab-separated cells first.
pub fn slice_to_plain_text(schema: &Schema, slice: &Slice) -> String {
    let cell = |cell: &Node| {
        let stripped = strip_syntax(schema, cell.content());
        markraft_core::projection::slice_to_plain_text(schema, &Slice::new(stripped, 0, 0))
    };
    let flat = flatten_fragment(schema, slice.content(), &cell);
    let content = flat.unwrap_or_else(|| strip_syntax(schema, slice.content()));
    let flattened = Slice::new(content, slice.open_start(), slice.open_end());
    markraft_core::projection::slice_to_plain_text(schema, &flattened)
}

/// What a concealed run displays, or `None` for anything that is not one.
fn concealed<'n>(schema: &Schema, node: &'n Node) -> Option<&'n str> {
    let syntax = schema.mark_id(md::SYNTAX)?;
    let mark = node.marks().get(syntax)?;
    Some(
        mark.attrs
            .get(SYNTAX_DISPLAY_ATTR)
            .and_then(|value| value.as_str())
            .unwrap_or_default(),
    )
}

/// A fragment with each concealed run replaced by what it displays
/// (recursively through containers), so plain-text views read as prose.
fn strip_syntax(schema: &Schema, content: &Fragment) -> Fragment {
    let children: Vec<Node> = content
        .iter()
        .filter_map(|node| {
            if let Some(display) = concealed(schema, node) {
                return (!display.is_empty()).then(|| schema.text(display));
            }
            if node.is_container() {
                Some(node.copy(strip_syntax(schema, node.content())))
            } else {
                Some(node.clone())
            }
        })
        .collect();
    Fragment::from_nodes(children)
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
        md::LINE_BREAK => "\n".to_string(),
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
