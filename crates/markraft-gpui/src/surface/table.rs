//! Tables: measuring columns, placing a grid in the column, and the strips
//! and sideways scroll a grid wider than the note is shown through. Shaping
//! calls it after the cells are shaped; paint and hit testing read its results.

use super::*;

/// Size, reshape and place the cells of one table.
pub(super) fn shape_table(
    input: &ShapeInput<'_>,
    table: usize,
    cells: &mut [LayoutLine],
    width: Pixels,
    text_system: &WindowTextSystem,
) {
    let Some(first) = cells.first() else { return };
    let left = first.origin.x;
    let grid: Vec<(usize, usize)> = cells
        .iter()
        .map(|cell| table_cell(input, cell.index).map_or((0, 0), |(_, row, column)| (row, column)))
        .collect();
    let columns = grid.iter().map(|(_, column)| column + 1).max().unwrap_or(1);
    let alignments = input.types.column_alignments(&first.source, columns);
    // The table is one block, so only its last cell carries a gap below it.
    let gap = {
        let last = cells.last().expect("a non-empty slice has a last cell");
        gap_below(input, last.index, &last.source, None, false, &None)
    };

    // How many quote levels the grid draws its own bars for; a cell carries no
    // decoration, so the ordinary quote painter never sees one.
    let quotes = input
        .types
        .quote_depth(&first.source)
        .min(visible_levels(input.style, max_indent(input.style, width)));

    let (preferred, minimum) = column_demands(cells, &grid, columns);
    let widths = column_widths(&preferred, &minimum, (width - left).max(CELL_MIN_WIDTH));
    // Taken before the reshape, which replaces each cell's width with the
    // column's: this is what an alignment offsets inside the column.
    let natural: Vec<Pixels> = cells.iter().map(|cell| cell.width).collect();

    for (index, cell) in cells.iter_mut().enumerate() {
        let content = (widths[grid[index].1] - CELL_PADDING_X * 2.).max(px(1.));
        *cell = shape_line(
            input,
            cell.index,
            width,
            Some(CellWidth::Column(content)),
            text_system,
        );
    }

    let mut heights = vec![px(0.); grid.iter().map(|(row, _)| row + 1).max().unwrap_or(1)];
    for (index, cell) in cells.iter().enumerate() {
        heights[grid[index].0] = heights[grid[index].0].max(cell.text_height());
    }
    for height in &mut heights {
        *height += CELL_PADDING_Y * 2.;
    }
    place_table(
        cells,
        table,
        &grid,
        &widths,
        &heights,
        &natural,
        &alignments,
        left,
        gap,
    );
    for line in cells.iter_mut() {
        if let Some(cell) = &mut line.table {
            cell.quotes = quotes;
        }
    }
}

/// What each column asks for, and the narrowest it may be drawn: its widest
/// cell's content plus the padding, and its widest cell's min-content plus the
/// padding, neither below [`CELL_MIN_WIDTH`].
///
/// Both come off the cells as the frame will paint them. Every one of those
/// widths was measured on the line the measuring pass shaped with that cell's
/// own runs, so a header cell — which [`text_runs`] draws bold — floors its
/// column at the width the bold text needs, and an emphasised or code cell at
/// the width of its own face.
///
/// Both are rounded up to whole pixels. A column hands its cell a content box
/// of its width less the padding again, and the wrapper breaks on strictly
/// greater — but adding and then subtracting the padding is not lossless in
/// `f32` once a width passes 48px, where the sum crosses into the next
/// exponent. A bold "Second" measuring 52.0004px came back as 52.000397px and
/// split as `Secon / d`. Whole pixels make that round trip exact.
pub(super) fn column_demands(
    cells: &[LayoutLine],
    grid: &[(usize, usize)],
    columns: usize,
) -> (Vec<Pixels>, Vec<Pixels>) {
    let mut preferred = vec![CELL_MIN_WIDTH; columns];
    let mut minimum = vec![CELL_MIN_WIDTH; columns];
    for (cell, (_, column)) in cells.iter().zip(grid) {
        preferred[*column] = preferred[*column].max(cell.width.ceil() + CELL_PADDING_X * 2.);
        minimum[*column] = minimum[*column].max(cell.min_width.ceil() + CELL_PADDING_X * 2.);
    }
    (preferred, minimum)
}

/// The width every column is drawn at.
///
/// A table is content-sized: where the preferred widths fit, they are used as
/// they are rather than stretched to the editor's width — a two-word table
/// stays two words wide rather than being blown up to the column. Only when they do not fit are the columns
/// shrunk, proportionally to what they asked for and never below their own
/// entry in `minimum`, which is the widest unbreakable unit the column holds:
/// shrinking may wrap a cell's text, never split a word. A grid that will not
/// fit even at those minimums keeps its width and scrolls sideways inside the
/// note.
pub(super) fn column_widths(
    preferred: &[Pixels],
    minimum: &[Pixels],
    available: Pixels,
) -> Vec<Pixels> {
    let mut widths = preferred.to_vec();
    if widths.iter().copied().sum::<Pixels>() <= available {
        return widths;
    }
    // Scale what is above the minimum; a column that hits its floor is taken
    // out of the budget and the rest are scaled again, until nothing can give.
    let mut floored = vec![false; widths.len()];
    loop {
        let (mut flexible, mut reserved) = (px(0.), px(0.));
        for (width, floored) in widths.iter().zip(&floored) {
            if *floored {
                reserved += *width;
            } else {
                flexible += *width;
            }
        }
        let budget = available - reserved;
        if flexible <= px(0.) || budget <= px(0.) {
            break;
        }
        let scale = f32::from(budget) / f32::from(flexible);
        if scale >= 1. {
            break;
        }
        let mut hit_the_floor = false;
        for (index, (width, floored)) in widths.iter_mut().zip(floored.iter_mut()).enumerate() {
            if *floored {
                continue;
            }
            let floor = minimum.get(index).copied().unwrap_or(CELL_MIN_WIDTH);
            if *width * scale < floor {
                *width = floor;
                *floored = true;
                hit_the_floor = true;
            } else {
                *width *= scale;
            }
        }
        if !hit_the_floor {
            break;
        }
    }
    widths
}

/// Put every cell where its column and row say it goes.
///
/// [`Lines`] stacks lines by adding each one's `top_gap` and `height` to a
/// running y, so a grid is expressed in those terms: every cell of a row keeps
/// the same y offset within it and only the row's last cell carries the row's
/// height, which makes the stack advance one row per row rather than one per
/// cell. The gap below the table rides on the very last cell.
#[allow(clippy::too_many_arguments)]
pub(super) fn place_table(
    cells: &mut [LayoutLine],
    table: usize,
    grid: &[(usize, usize)],
    widths: &[Pixels],
    heights: &[Pixels],
    natural: &[Pixels],
    alignments: &[ColumnAlignment],
    left: Pixels,
    gap: Pixels,
) {
    let (rows, columns) = (heights.len(), widths.len());
    let mut offsets = Vec::with_capacity(columns);
    let mut x = px(0.);
    for width in widths {
        offsets.push(x);
        x += *width;
    }
    for (index, cell) in cells.iter_mut().enumerate() {
        let (row, column) = grid[index];
        let content = (widths[column] - CELL_PADDING_X * 2.).max(px(0.));
        let slack = (content - natural[index]).max(px(0.));
        let shift = match alignments[column] {
            ColumnAlignment::Center => slack * 0.5,
            ColumnAlignment::Right => slack,
            ColumnAlignment::None | ColumnAlignment::Left => px(0.),
        };
        let last_of_row = grid.get(index + 1).map(|(next, _)| *next) != Some(row);
        cell.origin = point(
            left + offsets[column] + CELL_PADDING_X + shift,
            CELL_PADDING_Y,
        );
        // The text starts where the alignment put it and the cell still ends at
        // its own right edge, so End and a wrapped row's fill both stop there.
        cell.width = content - shift;
        cell.top_gap = px(0.);
        cell.height = if last_of_row {
            heights[row] + if row + 1 == rows { gap } else { px(0.) }
        } else {
            px(0.)
        };
        cell.table = Some(TableCell {
            table,
            row,
            column,
            rows,
            columns,
            alignment: alignments[column],
            // Filled in by the caller, which is the pass that knows the grid's
            // ancestors.
            quotes: 0,
            offset: point(-(CELL_PADDING_X + shift), -CELL_PADDING_Y),
            size: size(widths[column], heights[row]),
        });
    }
}

/// How far one grid is scrolled sideways, and how far it may be.
///
/// A grid whose columns will not fit even at their min-content widths keeps its
/// natural width and scrolls as a unit inside the note. The offset outlives the
/// frame — it is the reader's position in the grid — so the view holds it,
/// keyed by the position before the table node; a grid the next frame does not
/// find takes its entry with it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct TableScroll {
    /// How far left the grid is drawn, never outside `0..=overflow`.
    pub(crate) offset: Pixels,
    /// The grid's width less the strip of the note it is drawn in.
    pub(crate) overflow: Pixels,
}

/// What every grid in `lines` overruns `content` by, before any offset is
/// applied, keyed as [`TableCell::table`] is.
pub(crate) fn table_overflows(
    lines: &[LayoutLine],
    content: Bounds<Pixels>,
) -> HashMap<usize, Pixels> {
    let mut grids: HashMap<usize, Bounds<Pixels>> = HashMap::new();
    for (table, bounds) in lines
        .iter()
        .filter_map(|line| Some((line.table?.table, line.cell_bounds()?)))
    {
        grids
            .entry(table)
            .and_modify(|all| *all = all.union(&bounds))
            .or_insert(bounds);
    }
    grids
        .into_iter()
        .map(|(table, grid)| {
            let room = content.right() - grid.left();
            (table, (grid.size.width - room).max(px(0.)))
        })
        .collect()
}

/// The offset that brings `cell` fully into `strip`, starting from `offset`.
///
/// Mirrors the vertical reveal: a cell already inside the strip keeps the
/// reader's position, and one hanging off an edge is pulled in from that edge
/// only. A cell wider than the strip shows its start.
pub(crate) fn reveal_offset(
    cell: Bounds<Pixels>,
    strip: Bounds<Pixels>,
    offset: Pixels,
    overflow: Pixels,
) -> Pixels {
    let wanted = if cell.left() - offset < strip.left() {
        cell.left() - strip.left()
    } else if cell.right() - offset > strip.right() {
        cell.right() - strip.right()
    } else {
        offset
    };
    wanted.clamp(px(0.), overflow.max(px(0.)))
}

/// The part of each scrolling grid in `rows` the reader can see, for the wheel
/// and for the toolbar's anchor. Grids that fit are left out.
pub(crate) fn visible_strips(
    rows: &[LayoutLine],
    scroll: &HashMap<usize, TableScroll>,
    content: Bounds<Pixels>,
) -> Vec<(usize, Bounds<Pixels>)> {
    let mut strips: Vec<(usize, Bounds<Pixels>)> = Vec::new();
    for (table, bounds) in rows
        .iter()
        .filter_map(|row| Some((row.table?.table, row.cell_bounds()?)))
    {
        if scroll.get(&table).is_none_or(|it| it.overflow <= px(0.)) {
            continue;
        }
        match strips.iter_mut().find(|(at, _)| *at == table) {
            Some((_, all)) => *all = all.union(&bounds),
            None => strips.push((table, bounds)),
        }
    }
    for (_, bounds) in &mut strips {
        *bounds = bounds.intersect(&content);
    }
    strips
}

/// The cell of `table` that `point` falls in.
///
/// A grid's cells share one band of y, so walking the lines by y alone cannot
/// say which one a point landed in: the cell whose box holds the point wins,
/// and a point outside the grid falls to the nearest cell, vertical distance
/// first, so a click level with a row lands in that row.
pub(crate) fn cell_under(
    lines: &[LayoutLine],
    table: usize,
    point: Point<Pixels>,
) -> Option<&LayoutLine> {
    lines
        .iter()
        .filter(|line| line.table.is_some_and(|cell| cell.table == table))
        .filter_map(|line| Some((line, line.cell_bounds()?)))
        .min_by(|(_, a), (_, b)| {
            outside(a, point)
                .partial_cmp(&outside(b, point))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(line, _)| line)
}

/// How far `point` lies outside `bounds`, vertically first. Zero on both axes
/// inside it.
pub(super) fn outside(bounds: &Bounds<Pixels>, point: Point<Pixels>) -> (f32, f32) {
    let past =
        |low: Pixels, high: Pixels, at: Pixels| f32::from((low - at).max(at - high).max(px(0.)));
    (
        past(bounds.top(), bounds.bottom(), point.y),
        past(bounds.left(), bounds.right(), point.x),
    )
}

/// Collapse the centres that stand for one visual row.
///
/// Every line contributes one centre per visual row it occupies and the cells
/// of a table row all occupy the same band, so without this one press of Down
/// would step through such a row once per column.
pub(crate) fn merge_row_centers(mut centers: Vec<Pixels>) -> Vec<Pixels> {
    centers.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    centers.dedup_by(|a, b| (*a - *b).abs() < px(0.5));
    centers
}
