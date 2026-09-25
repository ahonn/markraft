//! Where vim's idea of a line meets a table.
//!
//! The projection gives every cell a line of its own, in row-major order, so
//! left alone `dd` would take one cell out of its row and `j` would walk along
//! one. Vim's line inside a table is the *row*: this module answers which lines
//! a row holds, which line a column's cell is, and whether two positions sit in
//! the same cell. That is everything the commands need to keep every row as wide
//! as the header — the one rule a table has that no content rule can express.
//!
//! Nothing here edits. The row and table commands that do live in
//! [`markraft_core::commands`], and the commands in `command.rs` run them.

use crate::motion;
use markraft_core::Node;
use markraft_core::Slice;
use markraft_core::commands::{TableTypes, spans_cells};
use markraft_core::kind::TABLE_ALIGNMENTS_ATTR;
use markraft_core::projection::{Line, Projection};
use markraft_gpui::DocTypes;
use std::ops::Range;

/// One cell of a table, as the projection sees it.
pub(crate) struct Cell {
    /// The position before the table node, which identifies the grid.
    pub table: usize,
    /// The row's index in the table. Row 0 is the header.
    pub row: usize,
    /// The cell's index in its row, which is its column.
    pub column: usize,
    /// The line the cell is.
    pub line: usize,
    /// The lines of the cell's whole row, which is vim's line here.
    pub row_lines: Range<usize>,
}

/// The three types the table commands take, or `None` unless the schema
/// declares all three: every one of them maintains the shape all three
/// describe, so a partial set cannot keep it.
pub(crate) fn types(types: &DocTypes) -> Option<TableTypes> {
    Some(TableTypes::new(
        types.table?,
        types.table_row?,
        types.table_cell?,
        TABLE_ALIGNMENTS_ATTR,
    ))
}

/// Where a line sits in a table: the position before the table, and the line's
/// row and column within it. `None` for a line that is not a cell.
fn cell_of(types: &DocTypes, line: &Line) -> Option<(usize, usize, usize)> {
    let cell = line.ancestors().last()?;
    if Some(cell.node_type) != types.table_cell {
        return None;
    }
    let row = line.ancestors().iter().nth_back(1)?;
    let table_index = line.depth().checked_sub(3)?;
    let table = &line.ancestors()[table_index];
    (Some(row.node_type) == types.table_row && Some(table.node_type) == types.table).then_some((
        line.ancestor_before(table_index),
        row.index,
        cell.index,
    ))
}

/// The cell the line at `index` is.
pub(crate) fn cell(types: &DocTypes, projection: &Projection, index: usize) -> Option<Cell> {
    let (table, row, column) = cell_of(types, projection.line(index)?)?;
    let same = |line: usize| cell_of(types, &projection.lines()[line]);
    let mut start = index;
    while start > 0 && same(start - 1).is_some_and(|(t, r, _)| (t, r) == (table, row)) {
        start -= 1;
    }
    let mut end = index + 1;
    while end < projection.line_count() && same(end).is_some_and(|(t, r, _)| (t, r) == (table, row))
    {
        end += 1;
    }
    Some(Cell {
        table,
        row,
        column,
        line: index,
        row_lines: start..end,
    })
}

/// The cell `pos` falls in.
pub(crate) fn cell_at(types: &DocTypes, projection: &Projection, pos: usize) -> Option<Cell> {
    cell(types, projection, motion::line_of(projection, pos))
}

/// A whole table as lines: where its cells start and end, and how wide it is.
///
/// Every row holds one cell per column, so the rows are a fixed stride of lines
/// and every cell's line follows from its row and column without a search.
struct Grid {
    lines: Range<usize>,
    width: usize,
}

impl Grid {
    fn rows(&self) -> usize {
        self.lines.len() / self.width
    }

    fn line(&self, row: usize, column: usize) -> Option<usize> {
        (row < self.rows() && column < self.width)
            .then(|| self.lines.start + row * self.width + column)
    }
}

/// The grid `cell` belongs to. The scan walks out from the cell rather than over
/// the projection, so it costs the table's size and not the document's.
fn grid(types: &DocTypes, projection: &Projection, cell: &Cell) -> Grid {
    let same = |line: usize| {
        cell_of(types, &projection.lines()[line]).is_some_and(|(table, _, _)| table == cell.table)
    };
    let mut start = cell.row_lines.start;
    while start > 0 && same(start - 1) {
        start -= 1;
    }
    let mut end = cell.row_lines.end;
    while end < projection.line_count() && same(end) {
        end += 1;
    }
    Grid {
        lines: start..end,
        width: cell.row_lines.len(),
    }
}

/// `lines` widened to whole rows at both ends.
///
/// Every linewise operator passes its range through this, so none of them can
/// take part of a row: a row with fewer cells than the header is a table no
/// command can put right again.
pub(crate) fn whole_rows(
    types: &DocTypes,
    projection: &Projection,
    lines: Range<usize>,
) -> Range<usize> {
    let mut range = lines.clone();
    if let Some(cell) = cell(types, projection, lines.start) {
        range.start = cell.row_lines.start;
    }
    if let Some(cell) = lines
        .end
        .checked_sub(1)
        .and_then(|last| cell(types, projection, last))
    {
        range.end = range.end.max(cell.row_lines.end);
    }
    range
}

/// The lines a count of vim lines covers from `line`, when `line` is a cell.
///
/// A count counts rows here, and stops at the table's last row: `3dd` on the
/// last row of a two-row table takes what is left of the grid rather than
/// reaching out of it and taking the block below with it.
pub(crate) fn count_rows(
    types: &DocTypes,
    projection: &Projection,
    line: usize,
    count: usize,
) -> Option<Range<usize>> {
    let cell = cell(types, projection, line)?;
    let grid = grid(types, projection, &cell);
    let start = cell.row_lines.start;
    let end = start
        .saturating_add(count.saturating_mul(grid.width))
        .min(grid.lines.end);
    Some(start..end)
}

/// Where `j` and `k` go inside a table: the same column of the row `delta`
/// rows away, or out of the table at its edges, which is where vim's next line
/// is. `None` when `pos` is not in a table.
///
/// No row is ever appended. Core's `goto_cell_below` grows the table at its
/// last row, which is what Enter should do and what a motion must not.
pub(crate) fn row_step(
    types: &DocTypes,
    projection: &Projection,
    pos: usize,
    delta: isize,
) -> Option<usize> {
    let cell = cell_at(types, projection, pos)?;
    let grid = grid(types, projection, &cell);
    let row = (cell.row as isize).saturating_add(delta);
    let line = if row < 0 {
        // Out of the top of the table, or nowhere to go when it opens the
        // document.
        grid.lines.start.checked_sub(1)
    } else {
        grid.line(row as usize, cell.column)
            .or_else(|| (grid.lines.end < projection.line_count()).then_some(grid.lines.end))
    };
    Some(line.map_or(pos, |line| motion::first_non_blank(projection, line)))
}

/// Where `h` and `l` go at a cell's edge: into the cell before or after it, in
/// row-major order, which is where the previous or next line is.
///
/// `None` anywhere else, including at the two ends of the table, where the
/// motion stops rather than appending a row as core's `goto_next_cell` would.
pub(crate) fn cross_cell(
    types: &DocTypes,
    projection: &Projection,
    pos: usize,
    forward: bool,
) -> Option<usize> {
    let cell = cell_at(types, projection, pos)?;
    let grid = grid(types, projection, &cell);
    if forward {
        // The cursor rests on a cell's last grapheme, or on an empty cell.
        if pos < motion::last_grapheme_of(projection, cell.line) {
            return None;
        }
        let next = cell.line + 1;
        (next < grid.lines.end).then(|| motion::line_start(projection, next))
    } else {
        if pos > motion::line_start(projection, cell.line) {
            return None;
        }
        (cell.line > grid.lines.start).then(|| motion::last_grapheme_of(projection, cell.line - 1))
    }
}

/// The line of the cell at (`row`, `column`) of the table at `table`, with both
/// clamped into the grid, or `None` when there is no such table any more.
///
/// This one searches the projection, because it is for after an edit, when there
/// is no cell left in hand to walk out from.
pub(crate) fn locate(
    types: &DocTypes,
    projection: &Projection,
    table: usize,
    row: usize,
    column: usize,
) -> Option<usize> {
    let index = projection
        .lines()
        .iter()
        .position(|line| cell_of(types, line).is_some_and(|(found, _, _)| found == table))?;
    let cell = cell(types, projection, index)?;
    line_of_cell(types, projection, &cell, row, column)
}

/// The line the cell at (`row`, `column`) of `cell`'s table is, with both clamped
/// into the grid.
pub(crate) fn line_of_cell(
    types: &DocTypes,
    projection: &Projection,
    cell: &Cell,
    row: usize,
    column: usize,
) -> Option<usize> {
    let grid = grid(types, projection, cell);
    grid.line(
        row.min(grid.rows().saturating_sub(1)),
        column.min(grid.width.saturating_sub(1)),
    )
}

/// Whether a charwise range reaches from one cell into another, or between a
/// cell and the text outside its table.
///
/// Replacing such a range would merge the cells it spans and leave those rows
/// short, which core's table invariant refuses; the operators ask first and
/// refuse the whole operator, register and all, so that it can be narrowed
/// and tried again.
pub(crate) fn crosses_cells(types: &DocTypes, doc: &Node, range: &Range<usize>) -> bool {
    self::types(types).is_some_and(|types| spans_cells(types, doc, range.start, range.end))
}

/// Which ancestor of the cursor's line a linewise paste of `slice` belongs
/// beside, or `None` when no table is involved and the register's own depth
/// stands.
///
/// Only a whole row may go into a grid. A row pasted where there is no grid
/// goes at the top level, where the slice machinery's `Fit` wraps it into a
/// table of its own; anything else pasted inside a table goes beside the table,
/// because a node that is not a row cannot be one of its children.
pub(crate) fn paste_level(
    types: &DocTypes,
    projection: &Projection,
    pos: usize,
    slice: &Slice,
) -> Option<usize> {
    let pasted = slice.content().first_child()?.type_id();
    let row = Some(pasted) == types.table_row;
    let cell = Some(pasted) == types.table_cell;
    match cell_at(types, projection, pos) {
        // The cursor's own ancestors are `[.., table, row, cell]`.
        Some(cursor) => {
            let depth = projection.line(cursor.line)?.ancestors().len();
            depth.checked_sub(if row { 2 } else { 3 })
        }
        None => (row || cell).then_some(0),
    }
}
