//! Paint helpers, one per layer: quote bars, tables and their fades, atoms,
//! pictures and markers. The element's `paint` calls them in order.

use super::*;

/// The quote bars a grid draws for the quote levels it sits in.
///
/// A cell's own height is zero except on the last of its row, so the ordinary
/// quote painter has nothing to draw against inside a grid; the grid draws one
/// bar per level over its whole height instead, at the x positions that painter
/// uses. `offset` undoes the grid's own horizontal scroll, so a bar stays at
/// the quote's indent.
pub(super) fn quote_bars(
    grid: Bounds<Pixels>,
    quotes: usize,
    offset: Pixels,
    indent: Pixels,
) -> Vec<Bounds<Pixels>> {
    (0..quotes)
        .map(|level| {
            Bounds::new(
                point(
                    grid.left() + offset - indent * (quotes - level) as f32,
                    grid.top(),
                ),
                size(QUOTE_BAR, grid.size.height),
            )
        })
        .collect()
}

/// Draw the chrome of every table in `rows`.
pub(super) fn paint_tables(
    rows: &[LayoutLine],
    style: &EditorStyle,
    caret: usize,
    scroll: &HashMap<usize, TableScroll>,
    window: &mut Window,
) {
    let mut start = 0usize;
    while start < rows.len() {
        let Some(cell) = rows[start].table else {
            start += 1;
            continue;
        };
        let mut end = start + 1;
        while end < rows.len() && rows[end].table.is_some_and(|next| next.table == cell.table) {
            end += 1;
        }
        let offset = scroll.get(&cell.table).map_or(px(0.), |grid| grid.offset);
        paint_table(&rows[start..end], style, caret, offset, window);
        start = end;
    }
}

/// The fades at the clipped edges of a scrolling grid, drawn over its cells: a
/// band of the background thinning out towards the part of the grid that is
/// still on screen, on whichever side there is more of it.
pub(super) fn paint_table_fades(
    strip: Bounds<Pixels>,
    grid: TableScroll,
    style: &EditorStyle,
    window: &mut Window,
) {
    for (leading, more) in [
        (true, grid.offset > px(0.)),
        (false, grid.offset < grid.overflow),
    ] {
        if !more {
            continue;
        }
        let opaque = linear_color_stop(style.background, if leading { 0. } else { 1. });
        let clear = linear_color_stop(style.background.opacity(0.), if leading { 1. } else { 0. });
        let (from, to) = if leading {
            (opaque, clear)
        } else {
            (clear, opaque)
        };
        let at = if leading {
            strip.left()
        } else {
            strip.right() - TABLE_FADE
        };
        window.paint_quad(fill(
            Bounds::new(point(at, strip.top()), size(TABLE_FADE, strip.size.height)),
            linear_gradient(90., from, to),
        ));
    }
}

/// Draw one table: the quote bars it sits in, the header band, the grid, and
/// the border round the cell the caret is in.
///
/// The separators are hairlines drawn along each cell's own bottom and right
/// edge rather than a border per cell, so a shared edge is one pixel wide and
/// not two, and the outer rectangle is drawn last so it sits over the band.
///
/// `offset` is how far the grid is drawn left of where it sits, which the quote
/// bars are taken back out of: the bars belong to the quote's indent and stay
/// there while the grid scrolls under them.
pub(super) fn paint_table(
    cells: &[LayoutLine],
    style: &EditorStyle,
    caret: usize,
    offset: Pixels,
    window: &mut Window,
) {
    let boxes: Vec<(TableCell, Bounds<Pixels>)> = cells
        .iter()
        .filter_map(|line| Some((line.table?, line.cell_bounds()?)))
        .collect();
    let Some((first_cell, first)) = boxes.first() else {
        return;
    };
    let outer = boxes
        .iter()
        .fold(*first, |all, (_, bounds)| all.union(bounds));
    for bar in quote_bars(outer, first_cell.quotes, offset, style.quote_indent) {
        window.paint_quad(fill(bar, style.marker));
    }
    if let Some(header) = boxes
        .iter()
        .filter(|(cell, _)| cell.row == 0)
        .map(|(_, bounds)| *bounds)
        .reduce(|all, bounds| all.union(&bounds))
    {
        window.paint_quad(fill(header, style.table_header_background));
    }
    for (cell, bounds) in &boxes {
        if cell.row + 1 < cell.rows {
            window.paint_quad(fill(
                Bounds::new(
                    point(bounds.left(), bounds.bottom() - TABLE_LINE),
                    size(bounds.size.width, TABLE_LINE),
                ),
                style.rule,
            ));
        }
        if cell.column + 1 < cell.columns {
            window.paint_quad(fill(
                Bounds::new(
                    point(bounds.right() - TABLE_LINE, bounds.top()),
                    size(TABLE_LINE, bounds.size.height),
                ),
                style.rule,
            ));
        }
    }
    window.paint_quad(quad(
        outer,
        Corners::all(px(0.)),
        gpui::transparent_black(),
        TABLE_LINE,
        style.rule,
        BorderStyle::Solid,
    ));
    // Which cell the caret is in is otherwise invisible in an empty grid.
    if let Some(bounds) = cells
        .iter()
        .find(|line| line.contains(caret))
        .and_then(|line| Some(caret_cell_frame(line.table?, line.cell_bounds()?)))
    {
        window.paint_quad(quad(
            bounds,
            Corners::all(px(0.)),
            gpui::transparent_black(),
            TABLE_LINE,
            style.marker,
            BorderStyle::Solid,
        ));
    }
}

/// The border round the caret's cell, drawn over the grid lines it borders.
///
/// A quad's border lies inside its bounds, and a cell's own separators run
/// along the inside of its right and bottom edges, so those two already line
/// up. The line on its left and top belongs to the neighbouring cell and lies
/// just outside it; the frame reaches out over it, or the two lines would sit
/// side by side and read as one twice as thick.
pub(super) fn caret_cell_frame(cell: TableCell, bounds: Bounds<Pixels>) -> Bounds<Pixels> {
    let left = if cell.column > 0 { TABLE_LINE } else { px(0.) };
    let top = if cell.row > 0 { TABLE_LINE } else { px(0.) };
    Bounds::new(
        point(bounds.left() - left, bounds.top() - top),
        size(bounds.size.width + left, bounds.size.height + top),
    )
}

/// Draw one inline atom over the fillers reserving its slot.
pub(super) fn paint_atom(
    row: &LayoutLine,
    atom: &InlineAtom,
    style: &EditorStyle,
    window: &mut Window,
    cx: &mut App,
) {
    let top = row.origin.y + row.line_height * atom.visual_row as f32;
    // An aligned row moves its atoms with its text.
    let left = row.origin.x + atom.left + row.row_shift(atom.visual_row);
    if let Some((image, drawn)) = &atom.image {
        // Centred in its row: a picture sharing the line is shorter than it.
        let top = top + ((row.line_height - drawn.height) * 0.5).max(px(0.));
        let bounds = Bounds::new(point(left, top), *drawn);
        let _ = window.paint_image(
            bounds,
            bounds,
            Corners::all(style.code_radius.min(drawn.height * 0.2)),
            image.clone(),
            0,
            false,
        );
        return;
    }
    if let Some(frame) = atom.frame {
        let bounds = Bounds::new(point(left, top), frame);
        window
            .paint_quad(fill(bounds, style.inline_code_background).corner_radii(style.code_radius));
        let icon = PILL_ICON + PILL_ICON_GAP;
        let left = bounds.origin.x + (frame.width - icon - atom.label.width).max(px(0.)) / 2.;
        let line = bounds.center().y - style.body_size * style.line_height_ratio * 0.5;
        paint_picture(
            point(left, bounds.center().y - PILL_ICON * 0.5),
            style,
            window,
        );
        let _ = atom.label.paint(
            point(left + icon, line),
            style.body_size * style.line_height_ratio,
            TextAlign::Left,
            None,
            window,
            cx,
        );
        return;
    }
    let inset = (row.line_height * 0.1).round();
    let bounds = Bounds::new(
        point(left, top + inset),
        size(atom.slot, row.line_height - inset * 2.),
    );
    window.paint_quad(fill(bounds, style.inline_code_background).corner_radii(style.code_radius));
    let icon = PILL_ICON + PILL_ICON_GAP;
    let left = bounds.origin.x + (bounds.size.width - icon - atom.label.width).max(px(0.)) / 2.;
    let glyph = if atom.note { paint_page } else { paint_picture };
    glyph(
        point(left, bounds.center().y - PILL_ICON * 0.5),
        style,
        window,
    );
    let _ = atom.label.paint(
        point(left + icon, top),
        row.line_height,
        TextAlign::Left,
        None,
        window,
        cx,
    );
}

/// A picture glyph: a frame with a sun and a hill in it, drawn at [`PILL_ICON`]
/// square from `origin`.
pub(super) fn paint_picture(origin: Point<Pixels>, style: &EditorStyle, window: &mut Window) {
    let unit = PILL_ICON / 12.;
    let at = |x: f32, y: f32| origin + point(unit * x, unit * y);
    let mut frame = PathBuilder::stroke(px(1.1));
    frame.move_to(at(0.5, 1.5));
    frame.line_to(at(11.5, 1.5));
    frame.line_to(at(11.5, 10.5));
    frame.line_to(at(0.5, 10.5));
    frame.line_to(at(0.5, 1.5));
    // The hill, drawn to the frame's lower edge so it reads as a photo.
    frame.move_to(at(1.5, 9.5));
    frame.line_to(at(4.5, 5.5));
    frame.line_to(at(7., 8.5));
    frame.line_to(at(8.5, 7.));
    frame.line_to(at(10.5, 9.5));
    // The sun, small enough that a square reads as a dot.
    frame.move_to(at(8., 3.5));
    frame.line_to(at(9.5, 3.5));
    if let Ok(path) = frame.build() {
        window.paint_path(path, style.muted_text);
    }
}

/// A page glyph: a sheet with a folded corner and two lines of writing, drawn at
/// [`PILL_ICON`] square from `origin`. It stands where a picture glyph would, for
/// an embed that names a note.
pub(super) fn paint_page(origin: Point<Pixels>, style: &EditorStyle, window: &mut Window) {
    let unit = PILL_ICON / 12.;
    let at = |x: f32, y: f32| origin + point(unit * x, unit * y);
    let mut sheet = PathBuilder::stroke(px(1.1));
    sheet.move_to(at(2.5, 1.));
    sheet.line_to(at(7.5, 1.));
    sheet.line_to(at(9.5, 3.));
    sheet.line_to(at(9.5, 11.));
    sheet.line_to(at(2.5, 11.));
    sheet.line_to(at(2.5, 1.));
    // The folded corner, which is what tells a sheet from a plain rectangle.
    sheet.move_to(at(7.5, 1.));
    sheet.line_to(at(7.5, 3.));
    sheet.line_to(at(9.5, 3.));
    // Two lines of writing, short enough to read as text at this size.
    sheet.move_to(at(4.5, 6.));
    sheet.line_to(at(7.5, 6.));
    sheet.move_to(at(4.5, 8.5));
    sheet.line_to(at(7.5, 8.5));
    if let Ok(path) = sheet.build() {
        window.paint_path(path, style.muted_text);
    }
}

pub(super) fn paint_marker(
    row: &LayoutLine,
    marker: &Marker,
    style: &EditorStyle,
    window: &mut Window,
    cx: &mut App,
) {
    let bounds = row.marker_bounds().expect("marker has bounds");
    match marker {
        Marker::Number(line) | Marker::Footnote(line) => {
            let _ = line.paint(
                bounds.origin,
                row.line_height,
                TextAlign::Left,
                None,
                window,
                cx,
            );
        }
        Marker::Bullet { depth } => {
            let radius = if depth % 3 == 2 { px(0.) } else { px(2.5) };
            if depth % 3 == 1 {
                window.paint_quad(quad(
                    bounds,
                    radius,
                    style.background,
                    px(1.),
                    style.marker,
                    BorderStyle::Solid,
                ));
            } else {
                window.paint_quad(fill(bounds, style.marker).corner_radii(radius));
            }
        }
        Marker::Task { checked, number } => {
            if let Some(number) = number {
                let _ = number.paint(
                    point(bounds.left() - NUMBER_GAP - number.width, row.origin.y),
                    row.line_height,
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                );
            }
            window.paint_quad(quad(
                bounds,
                px(3.),
                if *checked {
                    style.marker
                } else {
                    style.background
                },
                px(1.),
                style.marker,
                BorderStyle::Solid,
            ));
            if *checked {
                let origin = bounds.origin;
                let mut path = PathBuilder::stroke(px(1.5));
                path.move_to(origin + point(px(3.), px(7.)));
                path.line_to(origin + point(px(6.), px(10.)));
                path.line_to(origin + point(px(11.), px(4.)));
                if let Ok(path) = path.build() {
                    window.paint_path(path, rgb(0xffffff));
                }
            }
        }
    }
}
