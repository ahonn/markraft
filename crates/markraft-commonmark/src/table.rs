//! What every codec has to agree on about a [`TABLE`](crate::schema::TABLE):
//! how its `alignments` attribute is spelled, and the shape a table has to be
//! in before anything writes it.
//!
//! # The invariant
//!
//! [`TABLE_ALIGNMENTS_ATTR`] is a comma-separated list with one entry per
//! column — `left`, `center`, `right` or `none`, as in `"none,center,right"` —
//! so its length *is* the column count, and every row holds exactly that many
//! cells. It is the attribute this kind hands the table commands in
//! [`markraft_core::commands`] through
//! [`TableTypes`](markraft_core::commands::TableTypes), read through the same
//! [`Alignment`] type, so neither side can spell it differently from the other.
//!
//! GFM normalises a ragged table the way a reader does: a cell past the last
//! column is dropped, and a row that stops short is filled with empty cells.
//! `normalize_tables` puts an imported tree in that shape, so no serialiser,
//! view or command has to cope with a ragged one.

use markraft_core::commands::ColumnAlignment;
use markraft_core::kind::TABLE_ALIGNMENTS_ATTR;
use markraft_core::{Attrs, Fragment, MarkSet, Node, NodeTypeId, Schema, attrs};
use unicode_width::UnicodeWidthStr;

use crate::schema as md;

/// How one column of a table is aligned.
///
/// The codec and the table commands read and write the same attribute, so they
/// share the type that spells it: `none` is a plain `---` delimiter cell,
/// `left` is `:---`, `center` is `:---:` and `right` is `---:`.
pub use markraft_core::commands::ColumnAlignment as Alignment;

/// Read an `alignments` attribute value.
pub fn parse_alignments(value: &str) -> Vec<Alignment> {
    if value.is_empty() {
        return Vec::new();
    }
    value.split(',').map(ColumnAlignment::from_name).collect()
}

/// Write an `alignments` attribute value.
pub fn format_alignments(alignments: &[Alignment]) -> String {
    alignments
        .iter()
        .map(|alignment| alignment.name())
        .collect::<Vec<_>>()
        .join(",")
}

/// The alignment of every column of `table`, one entry per column.
///
/// The attribute is trusted for its content but not for its length: a table
/// built by hand — the correction that refills an emptied container builds one
/// — carries the default `""`, so the widest row decides how many columns
/// there are and the missing entries are [`Alignment::None`].
pub fn alignments_of(table: &Node) -> Vec<Alignment> {
    let declared = table
        .attrs()
        .get(TABLE_ALIGNMENTS_ATTR)
        .and_then(|value| value.as_str())
        .map(parse_alignments)
        .unwrap_or_default();
    let columns = declared.len().max(widest_row(table));
    let mut out = declared;
    out.resize(columns, Alignment::None);
    out
}

fn widest_row(table: &Node) -> usize {
    table
        .children()
        .map(|row| row.child_count())
        .max()
        .unwrap_or_default()
}

/// The display width of a cell's text, in terminal columns: what the
/// serialiser pads with so a CJK or emoji cell still lines up in a fixed-width
/// editor.
pub fn cell_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

/// Bring every table in `node` into the shape [the module documentation]
/// describes, or answer `None` when it is already in it.
///
/// Both importers run this over the whole tree, because a table is an ordinary
/// block and may sit inside a list item or a quote.
///
/// [the module documentation]: self
pub(crate) fn normalize_tables(schema: &Schema, node: &Node) -> Option<Node> {
    let table = schema.node_id(md::TABLE)?;
    let cell = schema.node_id(md::TABLE_CELL)?;
    normalize(schema, node, table, cell)
}

fn normalize(schema: &Schema, node: &Node, table: NodeTypeId, cell: NodeTypeId) -> Option<Node> {
    if !node.is_container() {
        return None;
    }
    let mut children: Vec<Node> = Vec::new();
    let mut changed = false;
    for child in node.children() {
        match normalize(schema, child, table, cell) {
            Some(fixed) => {
                changed = true;
                children.push(fixed);
            }
            None => children.push(child.clone()),
        }
    }
    let node = if changed {
        node.copy(Fragment::from_nodes(children))
    } else {
        node.clone()
    };
    if node.type_id() != table {
        return changed.then_some(node);
    }
    square(schema, &node, cell).or_else(|| changed.then_some(node))
}

/// Give every row of `table` the column count its `alignments` declares, and
/// write that count back into the attribute.
fn square(schema: &Schema, table: &Node, cell: NodeTypeId) -> Option<Node> {
    let alignments = alignments_of(table);
    let columns = alignments.len();
    let spelled = format_alignments(&alignments);
    let attrs_match = table
        .attrs()
        .get(TABLE_ALIGNMENTS_ATTR)
        .and_then(|value| value.as_str())
        == Some(spelled.as_str());
    if attrs_match && table.children().all(|row| row.child_count() == columns) {
        return None;
    }
    let empty = || {
        schema
            .create(cell, Attrs::empty(), MarkSet::empty(), Fragment::empty())
            .ok()
    };
    let mut rows: Vec<Node> = Vec::with_capacity(table.child_count());
    for row in table.children() {
        if row.child_count() == columns {
            rows.push(row.clone());
            continue;
        }
        let mut cells: Vec<Node> = row.children().take(columns).cloned().collect();
        while cells.len() < columns {
            cells.push(empty()?);
        }
        rows.push(row.copy(Fragment::from_nodes(cells)));
    }
    let attrs = schema
        .build_node_attrs(table.type_id(), &attrs! {TABLE_ALIGNMENTS_ATTR => spelled})
        .ok()?;
    Some(table.with_attrs(attrs).copy(Fragment::from_nodes(rows)))
}
