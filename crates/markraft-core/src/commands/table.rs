//! Table editing: moving between cells, adding and removing rows and columns.
//!
//! Nothing here knows what a table *means*. A caller names the three node types
//! a table is built from ([`TableTypes`]) and the commands maintain the shape
//! those names describe: a table holds rows, a row holds cells, the first row
//! is the header, and the table's [`ALIGNMENTS_ATTR`] attribute carries one
//! entry per column.
//!
//! # The invariant
//!
//! Every row has exactly as many cells as the table has alignment entries. A
//! command that cannot keep that true does not apply. This is why a cell is
//! never split, joined or lifted on its own: doing so would give one row a
//! different width than its siblings, and no content rule can express that.
//! [`guard_cell_boundary`] and [`guard_cell_split`] are what stop the general
//! commands from trying.
//!
//! # Relation to other editors
//!
//! [`goto_next_cell`] grows the table when it runs out of cells, which is what
//! Tab does in Typora and Bear; [`goto_cell_below`] does the same for Enter, as
//! Obsidian's tables do. [`goto_prev_cell`] and [`goto_cell_above`] never
//! create anything, so a key chain can fall through them at the table's edge.

use crate::attr::{AttrValue, Attrs};
use crate::change::{Change, ChangeSet};
use crate::fragment::Fragment;
use crate::mark::MarkSet;
use crate::node::Node;
use crate::pos::ResolvedPos;
use crate::schema::{NodeTypeId, Schema};
use crate::selection::Selection;
use crate::slice::{Slice, Token};
use crate::state::{EditorState, TransactionSpec};

use super::structure::{can_replace, default_block_type, markup_of};
use super::{Command, changes_spec, command, resolve_changes};

/// The attribute a table carries its per-column alignments in.
///
/// Its value is a comma-separated list of [`ColumnAlignment`] names, one per
/// column. A table type that does not declare the attribute still works: the
/// commands then keep the structure and leave alignment alone.
pub const ALIGNMENTS_ATTR: &str = "alignments";

/// The node types a table is built from.
///
/// Passed to every command in this module, so a host that calls its types
/// something else — or has several table-like shapes — needs no configuration
/// beyond these three ids.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TableTypes {
    /// The table itself: a block holding rows, carrying [`ALIGNMENTS_ATTR`].
    pub table: NodeTypeId,
    /// A row: holds one cell per column.
    pub row: NodeTypeId,
    /// A cell: a textblock.
    pub cell: NodeTypeId,
}

impl TableTypes {
    /// The three types, in the order a table nests them.
    pub fn new(table: NodeTypeId, row: NodeTypeId, cell: NodeTypeId) -> TableTypes {
        TableTypes { table, row, cell }
    }
}

/// How one column's cells are aligned.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColumnAlignment {
    /// No alignment of its own; the renderer decides.
    #[default]
    None,
    /// Aligned to the start of the column.
    Left,
    /// Centred in the column.
    Center,
    /// Aligned to the end of the column.
    Right,
}

impl ColumnAlignment {
    /// The name this alignment is written under in [`ALIGNMENTS_ATTR`].
    pub fn name(self) -> &'static str {
        match self {
            ColumnAlignment::None => "none",
            ColumnAlignment::Left => "left",
            ColumnAlignment::Center => "center",
            ColumnAlignment::Right => "right",
        }
    }

    /// The alignment a name stands for.
    ///
    /// Anything the attribute does not name reads as [`ColumnAlignment::None`],
    /// so a malformed attribute degrades into an unaligned table rather than
    /// making the document unusable.
    pub fn from_name(name: &str) -> ColumnAlignment {
        match name.trim() {
            "left" => ColumnAlignment::Left,
            "center" => ColumnAlignment::Center,
            "right" => ColumnAlignment::Right,
            _ => ColumnAlignment::None,
        }
    }
}

/// Where in a table the selection sits.
///
/// Returned by [`cell_at`], which is also what a view uses to decide whether
/// its table key bindings apply at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellPos {
    /// The position before the table node.
    pub table: usize,
    /// The row's index in the table. Row 0 is the header.
    pub row: usize,
    /// The cell's index in its row, which is its column.
    pub column: usize,
    /// The position before the cell node.
    pub cell: usize,
}

/// The cell the selection's head sits in, or `None` outside a table.
///
/// The head, not the anchor: a range selection that reaches out of one cell
/// into another is allowed to exist, and every command here acts on the end the
/// user is moving.
pub fn cell_at(types: TableTypes, state: &EditorState) -> Option<CellPos> {
    let doc = state.doc();
    cell_at_pos(types, doc, state.selection().head(doc))
}

/// The cell `pos` sits in.
fn cell_at_pos(types: TableTypes, doc: &Node, pos: usize) -> Option<CellPos> {
    let resolved = doc.resolve(pos).ok()?;
    let depth = cell_depth(types, &resolved)?;
    Some(CellPos {
        table: resolved.before(depth - 2),
        row: resolved.index(depth - 2),
        column: resolved.index(depth - 1),
        cell: resolved.before(depth),
    })
}

/// The alignments `table` declares, one per column.
///
/// A missing, short or over-long attribute is padded and trimmed to `columns`,
/// so callers never have to bounds-check the result.
pub fn column_alignments(table: &Node, columns: usize) -> Vec<ColumnAlignment> {
    let mut out: Vec<ColumnAlignment> = table
        .attrs()
        .get(ALIGNMENTS_ATTR)
        .and_then(AttrValue::as_str)
        .filter(|text| !text.is_empty())
        .map(|text| text.split(',').map(ColumnAlignment::from_name).collect())
        .unwrap_or_default();
    out.resize(columns, ColumnAlignment::None);
    out
}

/// The depth at which `resolved` sits in a cell of a table of these types.
///
/// All three levels have to match: a lone cell type somewhere else in the
/// document is not a table.
fn cell_depth(types: TableTypes, resolved: &ResolvedPos) -> Option<usize> {
    (3..=resolved.depth()).rev().find(|&depth| {
        resolved.node(depth).type_id() == types.cell
            && resolved.node(depth - 1).type_id() == types.row
            && resolved.node(depth - 2).type_id() == types.table
    })
}

/// A table resolved around the selection: everything the edits below need,
/// computed once.
struct TableCtx {
    /// Where the selection sits.
    pos: CellPos,
    /// The table node itself.
    table: Node,
    /// The table's width, taken from its first row.
    columns: usize,
    /// The declared alignments, one per column.
    alignments: Vec<ColumnAlignment>,
}

impl TableCtx {
    /// The number of rows.
    fn rows(&self) -> usize {
        self.table.child_count()
    }

    /// The position after the table.
    fn end(&self) -> usize {
        self.pos.table + self.table.node_size()
    }

    /// The position before row `index`; `index == rows()` gives the position
    /// after the last row, where an appended row goes.
    fn row_start(&self, index: usize) -> usize {
        self.pos.table
            + 1
            + self.table.content().as_slice()[..index.min(self.rows())]
                .iter()
                .map(Node::node_size)
                .sum::<usize>()
    }

    /// The position before cell `column` of row `row`; a `column` equal to the
    /// row's cell count gives the position after its last cell, where an
    /// appended cell goes.
    fn cell_start(&self, row: usize, column: usize) -> Option<usize> {
        cell_offset(&self.table, self.pos.table, row, column)
    }
}

/// Resolve the table around the selection, or `None` when there is none.
fn context(types: TableTypes, state: &EditorState) -> Option<TableCtx> {
    let pos = cell_at(types, state)?;
    let table = state.doc().node_at(pos.table)?;
    let columns = table.first_child()?.child_count();
    let alignments = column_alignments(&table, columns);
    Some(TableCtx {
        pos,
        table,
        columns,
        alignments,
    })
}

/// The position before cell `column` of row `row` of the table at `table_pos`.
///
/// Accepts a `column` one past the last cell, which is the position an appended
/// cell is inserted at.
fn cell_offset(table: &Node, table_pos: usize, row: usize, column: usize) -> Option<usize> {
    let row_node = table.maybe_child(row)?;
    if column > row_node.child_count() {
        return None;
    }
    let mut pos = table_pos + 1;
    for index in 0..row {
        pos += table.child(index).node_size();
    }
    pos += 1;
    for index in 0..column {
        pos += row_node.child(index).node_size();
    }
    Some(pos)
}

/// A cursor at the start of cell (`row`, `column`) of the table at `table_pos`
/// in `doc`, with the column clamped to the row's width.
fn cursor_in_cell(
    schema: &Schema,
    doc: &Node,
    table_pos: usize,
    row: usize,
    column: usize,
) -> Option<Selection> {
    let table = doc.node_at(table_pos)?;
    let last = table.maybe_child(row)?.child_count().checked_sub(1)?;
    let pos = cell_offset(&table, table_pos, row, column.min(last))?;
    // A cell need not be a textblock itself; find the first place a cursor may
    // go inside it.
    Selection::find_from(schema, doc, pos + 1, 1, true)
}

/// An empty cell, with whatever children the cell's content rule requires.
fn empty_cell(schema: &Schema, types: TableTypes) -> Option<Node> {
    schema.create_and_fill(
        types.cell,
        schema.node_type(types.cell).default_attrs().clone(),
        MarkSet::empty(),
        Fragment::empty(),
    )
}

/// A row of `columns` empty cells.
fn empty_row(schema: &Schema, types: TableTypes, columns: usize) -> Option<Node> {
    if columns == 0 {
        return None;
    }
    let cell = empty_cell(schema, types)?;
    schema
        .create(
            types.row,
            schema.node_type(types.row).default_attrs().clone(),
            MarkSet::empty(),
            Fragment::from_nodes(vec![cell; columns]),
        )
        .ok()
}

/// A `rows` by `columns` table of empty cells, every column unaligned.
fn empty_table(schema: &Schema, types: TableTypes, rows: usize, columns: usize) -> Option<Node> {
    if rows == 0 {
        return None;
    }
    let row = empty_row(schema, types, columns)?;
    let attrs = aligned_attrs(
        schema,
        types.table,
        schema.node_type(types.table).default_attrs(),
        &vec![ColumnAlignment::None; columns],
    );
    Some(Node::container(
        markup_of(schema, types.table, &attrs),
        Fragment::from_nodes(vec![row; rows]),
    ))
}

/// Whether the table type declares [`ALIGNMENTS_ATTR`] at all.
fn declares_alignments(schema: &Schema, ty: NodeTypeId) -> bool {
    schema
        .node_type(ty)
        .attrs()
        .iter()
        .any(|spec| spec.name == ALIGNMENTS_ATTR)
}

/// `attrs` with [`ALIGNMENTS_ATTR`] set, or unchanged when the type does not
/// declare it.
fn aligned_attrs(
    schema: &Schema,
    ty: NodeTypeId,
    attrs: &Attrs,
    alignments: &[ColumnAlignment],
) -> Attrs {
    if !declares_alignments(schema, ty) {
        return attrs.clone();
    }
    let text = alignments
        .iter()
        .map(|alignment| alignment.name())
        .collect::<Vec<_>>()
        .join(",");
    attrs.with(ALIGNMENTS_ATTR, text)
}

/// The changes that rewrite the table's alignments.
///
/// A node's attributes live on its markup, which both its open and its close
/// token carry, so both are replaced — the same shape
/// [`set_block_type`](super::set_block_type) uses to re-type a block. Empty
/// when the table type declares no alignment attribute.
fn realign_changes(schema: &Schema, ctx: &TableCtx, alignments: &[ColumnAlignment]) -> Vec<Change> {
    if !declares_alignments(schema, ctx.table.type_id()) {
        return Vec::new();
    }
    let attrs = aligned_attrs(schema, ctx.table.type_id(), ctx.table.attrs(), alignments);
    let markup = markup_of(schema, ctx.table.type_id(), &attrs).marked(ctx.table.marks().clone());
    vec![
        Change::replace(
            ctx.pos.table,
            ctx.pos.table + 1,
            Slice::from_tokens(&[Token::Open(markup.clone())]),
        ),
        Change::replace(
            ctx.end() - 1,
            ctx.end(),
            Slice::from_tokens(&[Token::Close(markup)]),
        ),
    ]
}

/// A slice holding one whole node.
fn node_slice(node: Node) -> Slice {
    Slice::from_fragment(Fragment::from_node(node))
}

/// Move the cursor to the start of cell (`row`, `column`) of the current table.
fn move_to_cell(
    state: &EditorState,
    ctx: &TableCtx,
    row: usize,
    column: usize,
) -> Option<TransactionSpec> {
    let selection = cursor_in_cell(state.schema(), state.doc(), ctx.pos.table, row, column)?;
    if selection == *state.selection() {
        return None;
    }
    Some(
        TransactionSpec::new()
            .selection(selection)
            .user_event("move")
            .scroll_into_view(),
    )
}

/// Append an empty row to the table and put the cursor in its `column`-th cell.
fn append_row(
    state: &EditorState,
    types: TableTypes,
    ctx: &TableCtx,
    column: usize,
) -> Option<TransactionSpec> {
    let schema = state.schema();
    let row = empty_row(schema, types, ctx.columns)?;
    let at = ctx.row_start(ctx.rows());
    let (set, new_doc) = resolve_changes(state, vec![Change::insert(at, node_slice(row))])?;
    let selection = cursor_in_cell(schema, &new_doc, ctx.pos.table, ctx.rows(), column)?;
    selection.check(&new_doc, schema).ok()?;
    Some(
        TransactionSpec::new()
            .change_set(set)
            .selection(selection)
            .user_event("insert")
            .scroll_into_view(),
    )
}

/// Move the cursor to the start of the next cell, in row-major order.
///
/// In the last cell an empty row is appended and the cursor moves into its
/// first cell, so Tab keeps filling a table in rather than falling out of it.
pub fn goto_next_cell(types: TableTypes) -> Command {
    command(move |state| {
        let ctx = context(types, state)?;
        if ctx.pos.column + 1 < ctx.columns {
            return move_to_cell(state, &ctx, ctx.pos.row, ctx.pos.column + 1);
        }
        if ctx.pos.row + 1 < ctx.rows() {
            return move_to_cell(state, &ctx, ctx.pos.row + 1, 0);
        }
        append_row(state, types, &ctx, 0)
    })
}

/// Move the cursor to the start of the previous cell, in row-major order.
///
/// Does not apply in the first cell, so ⇧Tab at the top left of a table falls
/// through to whatever a chain lists after it.
pub fn goto_prev_cell(types: TableTypes) -> Command {
    command(move |state| {
        let ctx = context(types, state)?;
        if ctx.pos.column > 0 {
            return move_to_cell(state, &ctx, ctx.pos.row, ctx.pos.column - 1);
        }
        let row = ctx.pos.row.checked_sub(1)?;
        move_to_cell(state, &ctx, row, ctx.columns.saturating_sub(1))
    })
}

/// Move the cursor to the same column of the next row.
///
/// On the last row an empty row is appended and the cursor moves into it, which
/// is the Enter behaviour a view binds inside a table.
pub fn goto_cell_below(types: TableTypes) -> Command {
    command(move |state| {
        let ctx = context(types, state)?;
        if ctx.pos.row + 1 < ctx.rows() {
            return move_to_cell(state, &ctx, ctx.pos.row + 1, ctx.pos.column);
        }
        append_row(state, types, &ctx, ctx.pos.column)
    })
}

/// Move the cursor to the same column of the previous row.
///
/// Does not apply in the header, and never creates a row.
pub fn goto_cell_above(types: TableTypes) -> Command {
    command(move |state| {
        let ctx = context(types, state)?;
        let row = ctx.pos.row.checked_sub(1)?;
        move_to_cell(state, &ctx, row, ctx.pos.column)
    })
}

/// Insert an empty row above the cursor's row.
///
/// A row inserted above the header becomes the new header. That is deliberate:
/// the header is a position, not a property of the row, and a user who wants a
/// different header says so by putting a row in front of it.
pub fn add_row_before(types: TableTypes) -> Command {
    command(move |state| add_row(state, types, false))
}

/// Insert an empty row below the cursor's row.
pub fn add_row_after(types: TableTypes) -> Command {
    command(move |state| add_row(state, types, true))
}

fn add_row(state: &EditorState, types: TableTypes, after: bool) -> Option<TransactionSpec> {
    let ctx = context(types, state)?;
    let row = empty_row(state.schema(), types, ctx.columns)?;
    let at = ctx.row_start(ctx.pos.row + usize::from(after));
    changes_spec(state, vec![Change::insert(at, node_slice(row))], "insert")
}

/// Insert an empty column to the left of the cursor's column.
pub fn add_column_before(types: TableTypes) -> Command {
    command(move |state| add_column(state, types, false))
}

/// Insert an empty column to the right of the cursor's column.
pub fn add_column_after(types: TableTypes) -> Command {
    command(move |state| add_column(state, types, true))
}

/// Give every row one more cell and the alignment list one more entry, so the
/// table stays rectangular.
fn add_column(state: &EditorState, types: TableTypes, after: bool) -> Option<TransactionSpec> {
    let ctx = context(types, state)?;
    let at = ctx.pos.column + usize::from(after);
    let cell = empty_cell(state.schema(), types)?;
    let mut alignments = ctx.alignments.clone();
    alignments.insert(at.min(alignments.len()), ColumnAlignment::None);
    let mut changes = realign_changes(state.schema(), &ctx, &alignments);
    for row in 0..ctx.rows() {
        let width = ctx.table.child(row).child_count();
        let pos = ctx.cell_start(row, at.min(width))?;
        changes.push(Change::insert(pos, node_slice(cell.clone())));
    }
    changes_spec(state, changes, "insert")
}

/// Delete the cursor's row.
///
/// Deleting the only row deletes the table: a table with no rows is not a
/// document the schema accepts, and an empty table is not something a user
/// asked to keep.
pub fn delete_row(types: TableTypes) -> Command {
    command(move |state| {
        let ctx = context(types, state)?;
        if ctx.rows() <= 1 {
            return remove_table(state, &ctx, false, "delete");
        }
        let from = ctx.row_start(ctx.pos.row);
        let to = ctx.row_start(ctx.pos.row + 1);
        let (set, new_doc) = resolve_changes(state, vec![Change::delete(from, to)])?;
        let row = ctx.pos.row.min(ctx.rows() - 2);
        cell_edit_spec(
            state,
            set,
            new_doc,
            ctx.pos.table,
            row,
            ctx.pos.column,
            "delete",
        )
    })
}

/// Delete the cursor's column from every row.
///
/// Deleting the only column deletes the table, for the same reason
/// [`delete_row`] does.
pub fn delete_column(types: TableTypes) -> Command {
    command(move |state| {
        let ctx = context(types, state)?;
        if ctx.columns <= 1 {
            return remove_table(state, &ctx, false, "delete");
        }
        let mut alignments = ctx.alignments.clone();
        if ctx.pos.column < alignments.len() {
            alignments.remove(ctx.pos.column);
        }
        let mut changes = realign_changes(state.schema(), &ctx, &alignments);
        for row in 0..ctx.rows() {
            let row_node = ctx.table.child(row);
            let Some(cell) = row_node.maybe_child(ctx.pos.column) else {
                continue;
            };
            let from = ctx.cell_start(row, ctx.pos.column)?;
            changes.push(Change::delete(from, from + cell.node_size()));
        }
        let (set, new_doc) = resolve_changes(state, changes)?;
        let column = ctx.pos.column.min(ctx.columns - 2);
        cell_edit_spec(
            state,
            set,
            new_doc,
            ctx.pos.table,
            ctx.pos.row,
            column,
            "delete",
        )
    })
}

/// Delete the whole table the cursor is in.
pub fn delete_table(types: TableTypes) -> Command {
    command(move |state| {
        let ctx = context(types, state)?;
        remove_table(state, &ctx, false, "delete")
    })
}

/// Backspace at the start of an empty table: take the table.
///
/// Applies only when the cursor is at the start of the first cell and no cell
/// holds anything — the one case where a user pressing Backspace means the
/// table rather than a character. The table becomes an empty block, so the next
/// Backspace joins that block with whatever comes before it.
pub fn delete_empty_table(types: TableTypes) -> Command {
    command(move |state| {
        let doc = state.doc();
        if !state.selection().is_cursor() {
            return None;
        }
        let ctx = context(types, state)?;
        if ctx.pos.row != 0 || ctx.pos.column != 0 {
            return None;
        }
        let resolved = doc.resolve(state.selection().head(doc)).ok()?;
        let depth = cell_depth(types, &resolved)?;
        if resolved.pos().checked_sub(resolved.depth() - depth) != Some(resolved.start(depth)) {
            return None;
        }
        let empty = ctx
            .table
            .children()
            .all(|row| row.children().all(|cell| cell.content_size() == 0));
        if !empty {
            return None;
        }
        remove_table(state, &ctx, true, "delete.backward")
    })
}

/// Replace the table with nothing, or with an empty block.
///
/// `leave_block` forces the empty block; without it one appears only where the
/// parent's content rule would otherwise be broken, so deleting a table between
/// two paragraphs does not leave a blank one behind.
fn remove_table(
    state: &EditorState,
    ctx: &TableCtx,
    leave_block: bool,
    event: &str,
) -> Option<TransactionSpec> {
    let doc = state.doc();
    let schema = state.schema();
    let resolved = doc.resolve(ctx.pos.table).ok()?;
    let depth = resolved.depth();
    let index = resolved.index(depth);
    let replacement = (leave_block || !can_replace(schema, resolved.node(depth), index, index + 1))
        .then(|| default_block_type(schema, resolved.node(depth), index))
        .flatten()
        .and_then(|ty| {
            schema.create_and_fill(
                ty,
                schema.node_type(ty).default_attrs().clone(),
                MarkSet::empty(),
                Fragment::empty(),
            )
        });
    let at = ctx.pos.table;
    let slice = match replacement.clone() {
        Some(node) => node_slice(node),
        None => Slice::empty(),
    };
    let (set, new_doc) = resolve_changes(state, vec![Change::replace(at, ctx.end(), slice)])?;
    let selection = match replacement {
        Some(_) => Selection::find_from(schema, &new_doc, at + 1, 1, true)?,
        None => Selection::near(schema, &new_doc, at, -1),
    };
    selection.check(&new_doc, schema).ok()?;
    Some(
        TransactionSpec::new()
            .change_set(set)
            .selection(selection)
            .user_event(event)
            .scroll_into_view(),
    )
}

/// Set the alignment of the cursor's column.
///
/// Does not apply when the column already has that alignment, or when the table
/// type declares no alignment attribute.
pub fn set_column_alignment(types: TableTypes, alignment: ColumnAlignment) -> Command {
    command(move |state| {
        let ctx = context(types, state)?;
        let mut alignments = ctx.alignments.clone();
        let slot = alignments.get_mut(ctx.pos.column)?;
        if *slot == alignment {
            return None;
        }
        *slot = alignment;
        let changes = realign_changes(state.schema(), &ctx, &alignments);
        if changes.is_empty() {
            return None;
        }
        changes_spec(state, changes, "settype")
    })
}

/// Insert a `rows` by `columns` table of empty cells, cursor in the first one.
///
/// A non-empty selection is deleted first. What is left is a cursor, and where
/// the table goes follows from the block it sits in: an empty block is replaced
/// by the table, and a block with content keeps its content and gets the table
/// after it — pressing "insert table" in the middle of a sentence should not
/// cut the sentence in two.
///
/// Does not apply inside a table: a cell holds inline content, so a table
/// cannot nest in one.
pub fn insert_table(types: TableTypes, rows: usize, columns: usize) -> Command {
    command(move |state| {
        if rows == 0 || columns == 0 {
            return None;
        }
        if !state.selection().is_empty(state.doc()) {
            // Deleting first reduces every case to the cursor case below, and
            // keeps the whole thing one change set and one undo step.
            let deletion = super::delete_selection()(state)?;
            let deleted = state.update([deletion]).ok()?;
            let insertion = insert_table_at_cursor(deleted.state(), types, rows, columns)?;
            let inserted = deleted.state().update([insertion]).ok()?;
            return Some(
                TransactionSpec::new()
                    .change_set(deleted.changes().compose(inserted.changes()).ok()?)
                    .selection(inserted.state().selection().clone())
                    .user_event("insert")
                    .scroll_into_view(),
            );
        }
        insert_table_at_cursor(state, types, rows, columns)
    })
}

fn insert_table_at_cursor(
    state: &EditorState,
    types: TableTypes,
    rows: usize,
    columns: usize,
) -> Option<TransactionSpec> {
    let doc = state.doc();
    let schema = state.schema();
    let table = empty_table(schema, types, rows, columns)?;
    let at = state.selection().head(doc);
    let resolved = doc.resolve(at).ok()?;
    let block = (1..=resolved.depth()).rev().find(|&depth| {
        resolved.node(depth).is_textblock(schema) && resolved.node(depth).type_id() != types.cell
    });
    let (change, table_pos) = match block {
        Some(depth) if resolved.node(depth).content_size() == 0 => {
            let before = resolved.before(depth);
            (
                Change::replace(before, resolved.after(depth), node_slice(table)),
                before,
            )
        }
        Some(depth) => {
            let after = resolved.after(depth);
            (Change::insert(after, node_slice(table)), after)
        }
        None => (Change::insert(at, node_slice(table)), at),
    };
    let (set, new_doc) = resolve_changes(state, vec![change])?;
    let selection = cursor_in_cell(schema, &new_doc, table_pos, 0, 0)?;
    selection.check(&new_doc, schema).ok()?;
    Some(
        TransactionSpec::new()
            .change_set(set)
            .selection(selection)
            .user_event("insert")
            .scroll_into_view(),
    )
}

/// Stop a Backspace or Delete chain at a cell's edge.
///
/// At the start or end of a cell the general commands would gladly join the two
/// cells around the boundary, delete an empty neighbour or lift the cell out of
/// its row — each of which leaves one row narrower than the rest. This command
/// applies exactly there and does nothing, so a chain stops at it.
///
/// Place it after the commands that should still run inside a cell —
/// [`delete_selection`](super::delete_selection),
/// [`delete_by_grapheme`](super::delete_by_grapheme) and [`delete_empty_table`]
/// — and before [`join_backward`](super::join_backward),
/// [`join_forward`](super::join_forward) and the `select_node_*` pair.
pub fn guard_cell_boundary(types: TableTypes) -> Command {
    command(move |state| {
        let doc = state.doc();
        if !state.selection().is_cursor() {
            return None;
        }
        let resolved = doc.resolve(state.selection().head(doc)).ok()?;
        let depth = cell_depth(types, &resolved)?;
        let hidden = resolved.depth() - depth;
        let at_edge = resolved.pos().checked_sub(hidden) == Some(resolved.start(depth))
            || resolved.pos() + hidden == resolved.end(depth);
        at_edge.then(no_op)
    })
}

/// Stop an Enter chain from splitting a cell in two.
///
/// A row's content rule usually allows another cell, so
/// [`split_block`](super::split_block) inside a cell would happily make one —
/// and that row would then be one cell wider than its siblings. This command
/// applies anywhere inside a cell and does nothing.
///
/// A chain that lists [`goto_cell_below`] first never reaches it, because that
/// command always applies inside a table; it is here for a host that binds
/// Enter differently.
pub fn guard_cell_split(types: TableTypes) -> Command {
    command(move |state| cell_at(types, state).map(|_| no_op()))
}

/// Stop an edit that reaches from one cell into another.
///
/// A selection whose two ends sit in different cells is allowed to exist — a
/// drag across a table produces one — but *replacing* it merges the cells it
/// spans and leaves those rows short. This command applies to exactly such a
/// selection and does nothing.
///
/// It belongs at the very front of an editing chain, before
/// [`delete_selection`](super::delete_selection) and the typing commands, which
/// is the only place that can refuse the edit; the other two guards sit further
/// down, where the cursor commands have already had their turn.
pub fn guard_cell_range(types: TableTypes) -> Command {
    command(move |state| {
        let doc = state.doc();
        let selection = state.selection();
        if selection.is_empty(doc) {
            return None;
        }
        let from = cell_at_pos(types, doc, selection.from(doc));
        let to = cell_at_pos(types, doc, selection.to(doc));
        if from.is_none() && to.is_none() {
            return None;
        }
        (from != to).then(no_op)
    })
}

/// A transaction that changes nothing, which is how a guard stops a chain
/// without touching the document or the undo history.
fn no_op() -> TransactionSpec {
    TransactionSpec::new()
        .user_event("guard")
        .add_to_history(false)
}

/// The spec for a structural edit that leaves the cursor in cell (`row`,
/// `column`) of the table that is still there.
fn cell_edit_spec(
    state: &EditorState,
    set: ChangeSet,
    new_doc: Node,
    table_pos: usize,
    row: usize,
    column: usize,
    event: &str,
) -> Option<TransactionSpec> {
    let schema = state.schema();
    let selection = cursor_in_cell(schema, &new_doc, table_pos, row, column)?;
    selection.check(&new_doc, schema).ok()?;
    Some(
        TransactionSpec::new()
            .change_set(set)
            .selection(selection)
            .user_event(event)
            .scroll_into_view(),
    )
}
