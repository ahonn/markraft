//! Layout and painting, driven by the document's [`Projection`].
//!
//! One [`LayoutLine`] is laid out per projection [`Line`]: a textblock or a
//! block-level leaf. A line's text may hold newlines — a code block is one
//! textblock — so a line is shaped into one [`LayoutRow`] per newline-separated
//! row, each of which may wrap further.
//!
//! Every geometry query is in visible `char` offsets. The projection retained
//! by each layout maps document positions past hidden inline boundaries.

use crate::style::EditorStyle;
use crate::types::DocTypes;
use crate::{CaretShape, EditorEvent, EditorView};
use gpui::{prelude::*, *};
use markraft_core::commands::ColumnAlignment;
use markraft_core::projection::{Line, LineKind, Projection, Run, RunContent};
use markraft_core::{MarkSet, Node, NodeTypeId};
use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use unicode_segmentation::UnicodeSegmentation;

/// A block caret over text is translucent rather than inverted: the run under it keeps
/// its own colour, weight and syntax highlighting, which repainting one grapheme would
/// have to reproduce.
const BLOCK_CARET_ALPHA: f32 = 0.42;
/// How wide a block caret is where there is no grapheme to cover, past the last one of a
/// line, as a share of the row's line height.
const EMPTY_BLOCK_CARET_RATIO: f32 = 0.4;
const CARET_THICKNESS: Pixels = px(2.);

const INLINE_CODE_SCALE: f32 = 0.86;
const INLINE_CODE_PADDING: Pixels = px(5.);
const NUMBER_GAP: Pixels = px(6.);

/// An inline atom the view draws itself — an image, a retained HTML primitive —
/// is a pill like inline code. One atom is one character of the projection and
/// a pill needs more room than that character advances, so the display text
/// holds a run of [`PILL_FILLER`] in its place; see [`Widening`].
const PILL_SCALE: f32 = 0.82;
const PILL_PADDING: Pixels = px(7.);
const PILL_ICON: Pixels = px(13.);
const PILL_ICON_GAP: Pixels = px(5.);
/// No-break, so a pill never wraps in the middle of its own placeholder.
const PILL_FILLER: char = '\u{00a0}';
/// The widest a pill may grow, as a share of the column.
const PILL_MAX_RATIO: f32 = 0.9;

/// An image the note can read off the disk is drawn for real, at the column's
/// width, on the line it has to itself. Everything else — a remote source, a
/// format no decoder handles, an image sharing its line with text — keeps the
/// placeholder pill.
const IMAGE_MAX_HEIGHT: Pixels = px(320.);
/// A file this large is not worth blocking a layout pass on.
const IMAGE_MAX_BYTES: u64 = 16 * 1024 * 1024;
/// How many decoded images are kept, keyed by source.
const IMAGE_CACHE_LIMIT: usize = 24;
/// How far apart the dots of an inline-span hint sit, and how big each one is.
const HINT_PITCH: Pixels = px(3.);
const HINT_DOT: Pixels = px(1.);
// SF Mono; Menlo is wider and heavier at this size.
const CODE_FONT: &str = ".AppleSystemUIFontMonospaced";
const CODE_PADDING: Pixels = px(12.);
const CODE_HEADER_HEIGHT: Pixels = px(24.);
// The header controls sit in the block's top padding rather than below it.
const CODE_HEADER_LIFT: Pixels = px(8.);
const CODE_CHEVRON_WIDTH: Pixels = px(14.);
const CODE_COPY_WIDTH: Pixels = px(28.);
const CODE_RADIUS: Pixels = px(12.);

/// A table is a grid: the projection gives every cell a line of its own, and
/// the table pass puts the cells of one row on one band of y. A column is as
/// wide as its widest cell wants to be, never narrower than
/// [`CELL_MIN_WIDTH`], and the grid as a whole is content-sized rather than
/// stretched to the editor's width.
const CELL_PADDING_X: Pixels = px(8.);
const CELL_PADDING_Y: Pixels = px(6.);
const CELL_MIN_WIDTH: Pixels = px(56.);
const TABLE_LINE: Pixels = px(1.);

/// The byte index of the `n`th `char` of `text`, clamped to its length.
fn char_to_byte(text: &str, n: usize) -> usize {
    text.char_indices().nth(n).map_or(text.len(), |(i, _)| i)
}

/// The number of `char`s before byte index `byte`.
fn byte_to_char(text: &str, byte: usize) -> usize {
    text[..byte.min(text.len())].chars().count()
}

/// Inline code on one visual row. Text runs share one font size, so the main line only
/// reserves the space and the code is painted again, smaller, centred in that slot.
/// What is left of the slot on either side becomes the pill's padding.
#[derive(Clone)]
struct InlineCode {
    /// Byte range within the row's own text.
    range: Range<usize>,
    /// Visual row within the whole block.
    visual_row: usize,
    /// Relative to the block's origin.
    left: Pixels,
    slot: Pixels,
    line: Rc<ShapedLine>,
}

impl InlineCode {
    fn text_left(&self) -> Pixels {
        self.left + (self.slot - self.line.width) / 2.
    }
}

/// One inline atom drawn as a pill, once shaping says which slot it landed in.
#[derive(Clone)]
struct InlinePill {
    /// Relative to the line's origin.
    left: Pixels,
    slot: Pixels,
    /// Visual row within the whole block.
    visual_row: usize,
    /// Whether the picture glyph is drawn before the label.
    picture: bool,
    label: Rc<ShapedLine>,
    /// A decoded image drawn in place of the pill, at the size it was measured
    /// for.
    image: Option<(Arc<RenderImage>, Size<Pixels>)>,
}

/// Where a line's display text holds more characters than the projection does.
///
/// Every geometry query the view answers is in projection offsets, and the rows
/// are shaped from the display text, so the two spaces have to be mapped onto
/// each other wherever a pill widened an atom.
#[derive(Clone, Copy)]
struct Widening {
    /// `char` offset of the atom within the projection line.
    source: usize,
    /// `char` offset of its placeholder within the display text.
    display: usize,
    /// How many `char`s the placeholder takes. Always at least one.
    len: usize,
}

#[derive(Clone)]
struct CodeHeader {
    language: Rc<ShapedLine>,
    /// A code block's header carries the language picker and the copy button;
    /// a raw block's is a plain caption with nothing to click.
    interactive: bool,
}

#[derive(Clone, Copy)]
enum Decoration {
    /// One bar per block quote the line sits in; a level joins with the line below
    /// when that line sits in the same quote.
    Quote {
        levels: usize,
        joined: usize,
    },
    Divider,
    /// A code or raw block's rounded background, behind the whole line.
    Panel,
}

#[derive(Clone)]
enum Marker {
    /// An ordered-list number, right-aligned against the text.
    Number(Rc<ShapedLine>),
    Bullet {
        depth: usize,
    },
    Task {
        checked: bool,
    },
}

/// Where a table cell sits in its grid, once the table pass has placed it.
///
/// The box is kept as an offset from the line's own origin, which `prepaint`
/// translates into the window, so a cell's chrome follows its text wherever
/// the frame puts it.
#[derive(Clone, Copy)]
pub(crate) struct TableCell {
    /// The position before the table node, which is what tells two adjacent
    /// grids apart.
    pub(crate) table: usize,
    /// The cell's row; row 0 is the header.
    pub(crate) row: usize,
    pub(crate) column: usize,
    pub(crate) rows: usize,
    pub(crate) columns: usize,
    /// The alignment of the cell's own column.
    pub(crate) alignment: ColumnAlignment,
    /// The cell box's top-left corner, relative to the line's origin.
    offset: Point<Pixels>,
    /// The cell box, padding included.
    size: Size<Pixels>,
}

/// One newline-separated row of a line, shaped on its own.
#[derive(Clone)]
pub(crate) struct LayoutRow {
    line: Rc<WrappedLine>,
    /// `char` offset of the row's start within the projection line.
    char_start: usize,
    /// The first visual row this row occupies within the block.
    visual_start: usize,
    inline_code: Vec<InlineCode>,
}

impl LayoutRow {
    fn text(&self) -> &str {
        &self.line.text
    }
    fn char_len(&self) -> usize {
        self.text().chars().count()
    }
    fn visual_rows(&self) -> usize {
        self.line.wrap_boundaries().len() + 1
    }
    /// Byte offsets at which each of this row's visual rows starts.
    fn wrap_starts(&self) -> Vec<usize> {
        let mut starts = vec![0];
        starts.extend(
            self.line
                .wrap_boundaries()
                .iter()
                .map(|b| self.line.runs()[b.run_ix].glyphs[b.glyph_ix].index),
        );
        starts
    }
}

/// One projection line, laid out.
#[derive(Clone)]
pub(crate) struct LayoutLine {
    /// The projection used to shape this frame, including inline position maps.
    source: Line,
    /// The projection line this was built from.
    pub index: usize,
    /// Document position of the line's first content token.
    pub from: usize,
    /// How many visible `char`s the line holds.
    pub char_len: usize,
    pub rows: Vec<LayoutRow>,
    pub origin: Point<Pixels>,
    pub line_height: Pixels,
    /// Text height plus the gap below the line.
    pub height: Pixels,
    pub width: Pixels,
    pub(crate) top_gap: Pixels,
    /// Position directly before a code block's node, for the host's language picker.
    pub(crate) code_pos: Option<usize>,
    marker: Option<Marker>,
    decoration: Option<Decoration>,
    code_header: Option<CodeHeader>,
    code_hitboxes: Option<(Hitbox, Hitbox)>,
    /// Sorted by `source`; see [`Widening`].
    widenings: Vec<Widening>,
    pills: Vec<InlinePill>,
    /// `char` ranges whose text sits inside an inline span the line shows
    /// nothing else for.
    hints: Vec<Range<usize>>,
    /// Where the line sits in a table, when it is a cell of one.
    pub(crate) table: Option<TableCell>,
}

impl LayoutLine {
    /// The document position just past the line's own content.
    pub(crate) fn to(&self) -> usize {
        self.source.to
    }

    pub(crate) fn pos_to_offset(&self, pos: usize) -> usize {
        self.source
            .pos_to_offset(pos.clamp(self.from, self.to()))
            .unwrap_or(0)
    }

    pub(crate) fn offset_to_pos(&self, offset: usize) -> usize {
        self.source
            .offset_to_pos(offset.min(self.char_len))
            .unwrap_or(self.from)
    }

    pub(crate) fn hit_position(&self, offset: usize, projection: &Projection) -> usize {
        let pos = self.offset_to_pos(offset);
        // A leaf block has no grapheme to snap to. Searching backwards would
        // move a hit on a divider into the preceding paragraph.
        if self.char_len == 0 {
            pos
        } else {
            projection.floor_grapheme(pos)
        }
    }

    /// Whether `pos` falls inside this line.
    ///
    /// This is what decides which row draws the caret. Asking the row rather
    /// than asking the projection for a line index and comparing it to
    /// [`LayoutLine::index`] is deliberate: a layout is shaped from one
    /// projection and painted against whatever the view holds when the frame
    /// comes, and if those two ever disagree — a row list left over from a
    /// document with fewer lines — an index comparison attributes the caret to
    /// a row that stands for a different line and draws it there. A row that
    /// only ever answers for its own token range cannot.
    pub(crate) fn contains(&self, pos: usize) -> bool {
        pos >= self.from && pos <= self.to()
    }

    /// How many visual rows the line occupies.
    pub(crate) fn visual_rows(&self) -> usize {
        self.rows.iter().map(LayoutRow::visual_rows).sum()
    }

    fn text_height(&self) -> Pixels {
        self.line_height * self.visual_rows() as f32
    }

    /// The display-text `char` offset a projection offset stands at.
    fn to_display(&self, offset: usize) -> usize {
        let mut shift = 0;
        for widening in &self.widenings {
            if offset <= widening.source {
                break;
            }
            shift += widening.len - 1;
        }
        offset + shift
    }

    /// The projection `char` offset a display offset stands at. Inside a
    /// placeholder the nearer of the atom's two edges wins, so a click on the
    /// right half of a pill puts the caret after it.
    fn to_source(&self, display: usize) -> usize {
        let mut shift = 0;
        for widening in &self.widenings {
            if display >= widening.display + widening.len {
                shift += widening.len - 1;
            } else if display > widening.display {
                let past = display - widening.display >= widening.len.div_ceil(2);
                return widening.source + usize::from(past);
            } else {
                break;
            }
        }
        display - shift
    }

    /// The row a `char` offset falls in, and the byte offset within it.
    fn locate(&self, offset: usize) -> (usize, usize) {
        let offset = self.to_display(offset);
        let index = self
            .rows
            .iter()
            .rposition(|row| row.char_start <= offset)
            .unwrap_or(0);
        let row = &self.rows[index];
        let local = offset.saturating_sub(row.char_start).min(row.char_len());
        (index, char_to_byte(row.text(), local))
    }

    /// Window-space bounds of the header's label, on a code or raw block. Only
    /// a code block's is a button; a raw block's caption reserves neither the
    /// chevron nor the copy slot.
    fn header_bounds(&self) -> Option<Bounds<Pixels>> {
        let header = self.code_header.as_ref()?;
        let (chevron, copy) = if header.interactive {
            (CODE_CHEVRON_WIDTH, CODE_COPY_WIDTH + px(2.))
        } else {
            (px(0.), px(0.))
        };
        let label_width = header.language.width + chevron + px(12.);
        Some(Bounds::new(
            point(
                self.origin.x + self.width - copy - label_width,
                self.origin.y - CODE_HEADER_HEIGHT - CODE_HEADER_LIFT,
            ),
            size(label_width, CODE_HEADER_HEIGHT),
        ))
    }

    /// Window-space button bounds, available only on a code line.
    pub(crate) fn code_language_bounds(&self) -> Option<Bounds<Pixels>> {
        self.code_header.as_ref().filter(|it| it.interactive)?;
        self.header_bounds()
    }

    fn code_copy_bounds(&self) -> Option<Bounds<Pixels>> {
        self.code_header.as_ref().filter(|it| it.interactive)?;
        Some(Bounds::new(
            point(
                self.origin.x + self.width - CODE_COPY_WIDTH,
                self.origin.y - CODE_HEADER_HEIGHT - CODE_HEADER_LIFT,
            ),
            size(CODE_COPY_WIDTH, CODE_HEADER_HEIGHT),
        ))
    }

    /// Window-space bounds of the whole cell box, padding included, available
    /// only on a table cell.
    pub(crate) fn cell_bounds(&self) -> Option<Bounds<Pixels>> {
        let cell = self.table.as_ref()?;
        Some(Bounds::new(self.origin + cell.offset, cell.size))
    }

    pub(crate) fn marker_bounds(&self) -> Option<Bounds<Pixels>> {
        let (offset, width, height) = match self.marker.as_ref()? {
            Marker::Number(line) => (line.width + NUMBER_GAP, line.width, self.line_height),
            // Drawn markers share one center, 15px left of the text.
            Marker::Bullet { .. } => (px(17.5), px(5.), px(5.)),
            Marker::Task { .. } => (px(22.), px(14.), px(14.)),
        };
        Some(Bounds::new(
            self.origin + point(-offset, (self.line_height - height) * 0.5),
            size(width, height),
        ))
    }

    /// The top-left of the caret at `offset`, a `char` offset into the line.
    pub(crate) fn caret(&self, offset: usize, upstream: bool) -> Point<Pixels> {
        if self.rows.is_empty() {
            return self.origin;
        }
        let (index, byte) = self.locate(offset);
        let row = &self.rows[index];
        if !upstream {
            for (visual, boundary) in row.line.wrap_boundaries().iter().enumerate() {
                if row.line.runs()[boundary.run_ix].glyphs[boundary.glyph_ix].index == byte {
                    return self.origin
                        + point(
                            px(0.),
                            self.line_height * (row.visual_start + visual + 1) as f32,
                        );
                }
            }
        }
        let mut position = row
            .line
            .position_for_index(byte, self.line_height)
            .unwrap_or_default();
        let visual = row.visual_start + (position.y / self.line_height).round() as usize;
        if let Some(x) = self.inline_code_x(index, byte, visual) {
            position.x = x;
        }
        self.origin + point(position.x, self.line_height * visual as f32)
    }

    /// Where a position strictly inside inline code is drawn. Its edges keep the
    /// slot's own bounds, so the caret rests outside the pill there.
    fn inline_code_x(&self, index: usize, byte: usize, visual: usize) -> Option<Pixels> {
        let row = &self.rows[index];
        let code = row.inline_code.iter().find(|code| {
            code.visual_row == visual && code.range.start < byte && byte < code.range.end
        })?;
        Some(code.text_left() + code.line.x_for_index(byte - code.range.start))
    }

    /// The `char` offset under `local`, a point relative to the line's origin.
    pub(crate) fn char_at(&self, local: Point<Pixels>) -> usize {
        if self.rows.is_empty() {
            return 0;
        }
        let visual = ((local.y / self.line_height).floor() as isize)
            .clamp(0, self.visual_rows() as isize - 1) as usize;
        let index = self
            .rows
            .iter()
            .rposition(|row| row.visual_start <= visual)
            .unwrap_or(0);
        let row = &self.rows[index];
        let inner = point(
            local.x.max(px(0.)),
            self.line_height * (visual - row.visual_start) as f32 + self.line_height * 0.5,
        );
        let byte = row.inline_code_index(visual, local.x).unwrap_or_else(|| {
            match row.line.closest_index_for_position(inner, self.line_height) {
                Ok(index) | Err(index) => index,
            }
        });
        self.to_source(row.char_start + byte_to_char(row.text(), byte))
    }

    /// The `char` range of each visual row, clamped to the line's own content so
    /// a placeholder or a raw block's source reports nothing selectable.
    pub(crate) fn accessible_rows(&self) -> Vec<Range<usize>> {
        let mut ranges = Vec::new();
        for row in &self.rows {
            let text = row.text();
            let starts = row.wrap_starts();
            for (visual, &start) in starts.iter().enumerate() {
                let end = starts.get(visual + 1).copied().unwrap_or(text.len());
                let from = self
                    .to_source(row.char_start + byte_to_char(text, start))
                    .min(self.char_len);
                let to = self
                    .to_source(row.char_start + byte_to_char(text, end))
                    .min(self.char_len);
                ranges.push(from..to);
            }
        }
        ranges
    }

    /// The quads covering `chars`, one per visual row it touches. `newline` adds a
    /// stub past the end of the line, for a selection that spans into the next.
    pub(crate) fn rectangles(&self, chars: Range<usize>, newline: bool) -> Vec<Bounds<Pixels>> {
        let stub = newline && chars.end >= self.char_len;
        self.display_rectangles(
            self.to_display(chars.start)..self.to_display(chars.end),
            stub,
        )
    }

    /// The quads covering a range of the *display* text, which is what the rows
    /// were shaped from. `stub` marks the end of the last visual row.
    fn display_rectangles(&self, chars: Range<usize>, stub: bool) -> Vec<Bounds<Pixels>> {
        let mut rectangles = vec![];
        let last_visual = self.visual_rows().saturating_sub(1);
        for (index, row) in self.rows.iter().enumerate() {
            let text = row.text();
            let starts = row.wrap_starts();
            let from = char_to_byte(text, chars.start.saturating_sub(row.char_start));
            let to = if chars.end < row.char_start {
                0
            } else {
                char_to_byte(text, chars.end - row.char_start)
            };
            for (visual, &start) in starts.iter().enumerate() {
                let end = starts.get(visual + 1).copied().unwrap_or(text.len());
                let a = from.max(start);
                let b = to.min(end);
                let absolute = row.visual_start + visual;
                let trailing = stub && absolute == last_visual;
                if a >= b && !trailing {
                    continue;
                }
                let y = self.origin.y + self.line_height * absolute as f32;
                let x_for = |byte: usize| {
                    if byte == start {
                        return px(0.);
                    }
                    let p = row
                        .line
                        .position_for_index(byte, self.line_height)
                        .unwrap_or_default();
                    if p.y < self.line_height * visual as f32 {
                        px(0.)
                    } else if p.y > self.line_height * visual as f32 {
                        self.width
                    } else {
                        self.inline_code_x(index, byte, absolute).unwrap_or(p.x)
                    }
                };
                let left = x_for(a);
                let right = if b == end && visual + 1 < starts.len() {
                    self.width
                } else {
                    x_for(b)
                };
                rectangles.push(Bounds::new(
                    point(self.origin.x + left, y),
                    size(
                        (right - left + if trailing { px(7.) } else { px(0.) }).max(px(2.)),
                        self.line_height,
                    ),
                ));
            }
        }
        rectangles
    }
}

impl LayoutRow {
    /// The byte offset under `x` when it falls on inline code of `visual` row.
    fn inline_code_index(&self, visual: usize, x: Pixels) -> Option<usize> {
        let code = self.inline_code.iter().find(|code| {
            code.visual_row == visual && code.left <= x && x <= code.left + code.slot
        })?;
        Some(code.range.start + code.line.closest_index_for_x(x - code.text_left()))
    }
}

/// The caret quad for `shape` at `offset`. `next` is the grapheme boundary after
/// the caret within the same line, when there is one.
fn caret_quad(
    row: &LayoutLine,
    offset: usize,
    next: Option<usize>,
    upstream: bool,
    shape: CaretShape,
) -> Bounds<Pixels> {
    let origin = row.caret(offset, upstream);
    let bar = Bounds::new(origin, size(CARET_THICKNESS, row.line_height));
    match shape {
        CaretShape::Bar => bar,
        CaretShape::Block | CaretShape::Underline => {
            // A grapheme that wraps onto the next visual row leaves no width here, so
            // the caret falls back to its nominal one rather than spanning the row.
            let width = next
                .map(|next| row.caret(next, false))
                .filter(|end| end.y == origin.y && end.x > origin.x)
                .map(|end| end.x - origin.x)
                .unwrap_or(row.line_height * EMPTY_BLOCK_CARET_RATIO);
            if shape == CaretShape::Block {
                Bounds::new(origin, size(width, row.line_height))
            } else {
                Bounds::new(
                    origin + point(px(0.), row.line_height - CARET_THICKNESS),
                    size(width, CARET_THICKNESS),
                )
            }
        }
    }
}

/// Everything shaping needs that is not the window.
pub(crate) struct ShapeInput<'a> {
    pub doc: &'a Node,
    pub types: &'a DocTypes,
    pub projection: &'a Projection,
    pub style: &'a EditorStyle,
    pub single_line: bool,
}

/// How wide a table cell is shaped.
#[derive(Clone, Copy, PartialEq)]
enum CellWidth {
    /// The measuring pass: nothing constrains the text, so the line's width
    /// comes out as the width its content wants and the grid can size the
    /// column from it.
    Natural,
    /// The laying-out pass: the content box the cell's column gave it, so text
    /// too long for its column wraps inside the cell.
    Column(Pixels),
}

pub(crate) fn shape(input: &ShapeInput<'_>, width: Pixels, window: &Window) -> Vec<LayoutLine> {
    let mut lines: Vec<LayoutLine> = (0..input.projection.line_count())
        .map(|index| {
            let cell = table_cell(input, index).map(|_| CellWidth::Natural);
            shape_line(input, index, width, cell, window)
        })
        .collect();
    shape_tables(input, &mut lines, width, window);
    lines
}

/// Which table, row and column a projection line is a cell of.
fn table_cell(input: &ShapeInput<'_>, index: usize) -> Option<(usize, usize, usize)> {
    input.types.table_cell_of(input.projection.line(index)?)
}

fn shape_line(
    input: &ShapeInput<'_>,
    index: usize,
    width: Pixels,
    cell: Option<CellWidth>,
    window: &Window,
) -> LayoutLine {
    let ShapeInput {
        doc,
        types,
        projection,
        style,
        single_line,
    } = *input;
    let line = &projection.lines()[index];
    let heading = types.heading_level(line);
    let code = types.is_code_block(line);
    // A raw block is drawn in the same panel as a code block: its source is text
    // the document could not interpret, not prose.
    let raw = types.is_raw_block(line);
    let font_size = style.font_size(heading, code);
    // Keep deeply nested imported content editable in a narrow note. Only its
    // visual indentation is capped; document depth is preserved.
    let max_indent = (width - px(80.)).max(style.quote_indent);
    let quote_levels = types.quote_depth(line);
    let number = ordered_marker(doc, types, line, style, font_size, window);
    let indent = indent_of(types, line, style, number.as_ref().map(|(_, w)| *w)).min(max_indent);
    let marker = marker_of(types, line, style, number.map(|(shaped, _)| shaped));
    // A cell carries no block decoration of its own: the grid is the table's,
    // and a cell's own height is zero except on the last of its row, which a
    // quote bar or a panel has no way to draw against.
    let decoration = if cell.is_some() {
        None
    } else if code || raw {
        Some(Decoration::Panel)
    } else if types.horizontal_rule.is_some() && line.node_type() == types.horizontal_rule {
        Some(Decoration::Divider)
    } else if quote_levels > 0 {
        Some(Decoration::Quote {
            levels: quote_levels.min(visible_levels(style, max_indent)),
            joined: joined_quote_levels(projection, index, types).min(quote_levels),
        })
    } else {
        None
    };

    let wrap_width = match cell {
        Some(CellWidth::Column(content)) => content.max(px(16.)),
        // The measuring pass is unconstrained, but a pill still needs a nominal
        // column to size itself against.
        Some(CellWidth::Natural) => (width - indent).max(px(40.)),
        None => (width - indent - if code || raw { CODE_PADDING } else { px(0.) }).max(px(40.)),
    };
    let unwrapped = single_line || cell == Some(CellWidth::Natural);
    let text = display_text(input, line, index, font_size, wrap_width, window);
    // A drawn image needs the whole row it was measured for.
    let line_height = text
        .line_height
        .unwrap_or(font_size * style.line_height_ratio);
    let runs = text_runs(input, line, &text, heading, code, font_size, style);
    let shaped = window
        .text_system()
        .shape_text(
            text.text.clone().into(),
            font_size,
            &runs.runs,
            (!unwrapped).then_some(wrap_width),
            None,
        )
        .expect("valid UTF-8 text can be shaped");

    let mut rows = Vec::with_capacity(shaped.len());
    let mut char_start = 0usize;
    let mut visual_start = 0usize;
    for wrapped in shaped {
        let row = LayoutRow {
            char_start,
            visual_start,
            inline_code: Vec::new(),
            line: Rc::new(wrapped),
        };
        char_start += row.char_len() + 1;
        visual_start += row.visual_rows();
        rows.push(row);
    }

    let gap = gap_below(input, index, line, heading, code || raw, &marker);
    // A panel's own top padding holds its header, at the top of the document as
    // anywhere else. Everything else starts flush and only a heading claims space.
    let top_gap = if code || raw {
        CODE_PADDING + CODE_HEADER_HEIGHT
    } else if index == 0 {
        px(0.)
    } else if let Some(level) = heading {
        style.heading_top_gap(level)
    } else {
        px(0.)
    };
    let code_header = if code {
        code_header(
            crate::syntax::language_label(types.code_language(line).unwrap_or("")),
            true,
            width,
            style,
            window,
        )
    } else if raw {
        code_header(raw_block_label(&text.text), false, width, style, window)
    } else {
        None
    };

    let mut layout = LayoutLine {
        source: line.clone(),
        index,
        from: line.from,
        char_len: line.len(),
        rows,
        origin: point(indent, px(0.)),
        line_height,
        height: px(0.),
        width: if single_line {
            wrap_width.max(px(0.))
        } else {
            wrap_width
        },
        top_gap,
        code_pos: code
            .then(|| line.ancestors.last().map(|a| a.before))
            .flatten(),
        marker,
        decoration,
        code_header,
        code_hitboxes: None,
        widenings: text.widenings,
        pills: Vec::new(),
        hints: hints_of(input, line),
        table: None,
    };
    if single_line && let Some(row) = layout.rows.first() {
        layout.width = row.line.size(line_height).width.max(wrap_width);
    }
    if cell == Some(CellWidth::Natural) {
        // What the content wants, not what it was given: the grid reads each
        // column's preferred width off this.
        layout.width = layout
            .rows
            .iter()
            .map(|row| row.line.size(line_height).width)
            .fold(px(0.), |widest, width| widest.max(width));
    }
    layout.height = layout.text_height() + gap;
    shape_inline_code(&mut layout, &text.text, &runs.code, font_size, window);
    place_pills(&mut layout, text.pills);
    layout
}

/// Lay every table out as a grid.
///
/// Each cell has already been shaped unwrapped, so its `width` is what its
/// content wants. Consecutive lines naming the same table form one grid: its
/// columns are sized from those widths, each cell is reshaped inside the column
/// it was given, and the cells of one row are placed on one band of y.
fn shape_tables(input: &ShapeInput<'_>, lines: &mut [LayoutLine], width: Pixels, window: &Window) {
    let mut start = 0usize;
    while start < lines.len() {
        let Some((table, _, _)) = table_cell(input, lines[start].index) else {
            start += 1;
            continue;
        };
        let mut end = start + 1;
        while end < lines.len()
            && table_cell(input, lines[end].index).map(|(at, _, _)| at) == Some(table)
        {
            end += 1;
        }
        shape_table(input, table, &mut lines[start..end], width, window);
        start = end;
    }
}

/// Size, reshape and place the cells of one table.
fn shape_table(
    input: &ShapeInput<'_>,
    table: usize,
    cells: &mut [LayoutLine],
    width: Pixels,
    window: &Window,
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

    let mut preferred = vec![CELL_MIN_WIDTH; columns];
    for (cell, (_, column)) in cells.iter().zip(&grid) {
        preferred[*column] = preferred[*column].max(cell.width + CELL_PADDING_X * 2.);
    }
    let widths = column_widths(&preferred, (width - left).max(CELL_MIN_WIDTH));
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
            window,
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
}

/// The width every column is drawn at.
///
/// A table is content-sized: where the preferred widths fit, they are used as
/// they are rather than stretched to the editor's width — a two-word table
/// stays two words wide, as it does in Bear, rather than being blown up to the
/// column the way Typora does it. Only when they do not fit are the columns
/// shrunk, proportionally to what they asked for and never below
/// [`CELL_MIN_WIDTH`]; a table that will not fit even at that minimum overflows
/// to the right, where the editor's own bounds clip it, because this view has
/// no horizontal scrolling to offer instead.
fn column_widths(preferred: &[Pixels], available: Pixels) -> Vec<Pixels> {
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
        for (width, floored) in widths.iter_mut().zip(floored.iter_mut()) {
            if *floored {
                continue;
            }
            if *width * scale < CELL_MIN_WIDTH {
                *width = CELL_MIN_WIDTH;
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
/// `prepaint` stacks lines by adding each one's `top_gap` and `height` to a
/// running y, so a grid is expressed in those terms: every cell of a row keeps
/// the same y offset within it and only the row's last cell carries the row's
/// height, which makes the stack advance one row per row rather than one per
/// cell. The gap below the table rides on the very last cell.
#[allow(clippy::too_many_arguments)]
fn place_table(
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
            offset: point(-(CELL_PADDING_X + shift), -CELL_PADDING_Y),
            size: size(widths[column], heights[row]),
        });
    }
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
fn outside(bounds: &Bounds<Pixels>, point: Point<Pixels>) -> (f32, f32) {
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

/// Give every placeholder the slot the shaped rows put it in. A placeholder is
/// non-breaking, so it always lands on exactly one visual row.
fn place_pills(layout: &mut LayoutLine, pending: Vec<PendingPill>) {
    for pill in pending {
        let Some(slot) = layout
            .display_rectangles(pill.chars.clone(), false)
            .into_iter()
            .next()
        else {
            continue;
        };
        layout.pills.push(InlinePill {
            left: slot.origin.x - layout.origin.x,
            slot: slot.size.width,
            visual_row: ((slot.origin.y - layout.origin.y) / layout.line_height).round() as usize,
            picture: pill.picture,
            label: pill.label,
            image: pill.image,
        });
    }
}

/// What a raw block's header calls it. Only a GFM table and an HTML block ever
/// reach one, and the first line of the source says which.
fn raw_block_label(source: &str) -> &'static str {
    let first = source
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default();
    if first.trim_start().starts_with('|') {
        "Table"
    } else {
        "HTML"
    }
}

/// A line's display text, and whether it stands in for content the caret cannot
/// enter.
struct DisplayText {
    text: String,
    /// True when the text is a stand-in — a placeholder space for an empty line,
    /// or the source of a raw block — so no run may be derived from the content.
    synthetic: bool,
    /// The display byte length of each of the line's projection runs, which is
    /// the run's own length except where a pill widened an atom.
    run_bytes: Vec<usize>,
    widenings: Vec<Widening>,
    pills: Vec<PendingPill>,
    /// The height a drawn image needs, where it is taller than a text row.
    line_height: Option<Pixels>,
}

impl DisplayText {
    /// A stand-in for content no run may be derived from.
    fn stand_in(text: String) -> DisplayText {
        DisplayText {
            text,
            synthetic: true,
            run_bytes: Vec::new(),
            widenings: Vec::new(),
            pills: Vec::new(),
            line_height: None,
        }
    }
}

/// A pill the display text has reserved room for, before shaping says where on
/// the block it landed.
struct PendingPill {
    /// `char` range of the placeholder within the display text.
    chars: Range<usize>,
    picture: bool,
    label: Rc<ShapedLine>,
    image: Option<(Arc<RenderImage>, Size<Pixels>)>,
}

fn display_text(
    input: &ShapeInput<'_>,
    line: &Line,
    index: usize,
    font_size: Pixels,
    column: Pixels,
    window: &Window,
) -> DisplayText {
    let ShapeInput {
        types,
        projection,
        style,
        ..
    } = *input;
    if line.kind == LineKind::LeafBlock {
        let source = line
            .ancestors
            .last()
            .filter(|own| Some(own.node_type) == types.raw_block)
            .and_then(|own| own.attrs.get("source"))
            .and_then(|value| value.as_str())
            .unwrap_or(" ");
        return DisplayText::stand_in(source.to_owned());
    }
    let source = projection.line_text(index).unwrap_or_default();
    if source.is_empty() {
        return DisplayText::stand_in(" ".to_owned());
    }
    // An image that has its line to itself is drawn at full size; one sharing
    // its line with text has to stay within a row.
    let alone = line.runs.len() == 1 && line.len() == 1;
    let mut text = String::with_capacity(source.len());
    let mut run_bytes = Vec::with_capacity(line.runs.len());
    let mut widenings = Vec::new();
    let mut pills = Vec::new();
    let mut byte = 0usize;
    let mut display = 0usize;
    let mut filler: Option<Pixels> = None;
    let mut line_height = None;
    for run in &line.runs {
        let chars = run.char_to - run.char_from;
        let len: usize = source[byte..].chars().take(chars).map(char::len_utf8).sum();
        let slice = &source[byte..byte + len];
        byte += len;
        match pill_of(types, run, style, font_size, column, alone, window) {
            Some(pill) => {
                // One atom is one character, and it advances nowhere near far
                // enough for a pill, so the row reserves the width in fillers.
                let unit = *filler.get_or_insert_with(|| filler_width(font_size, window));
                let count = (pill.width / unit).ceil().max(1.) as usize;
                text.extend(std::iter::repeat_n(PILL_FILLER, count));
                run_bytes.push(count * PILL_FILLER.len_utf8());
                widenings.push(Widening {
                    source: run.char_from,
                    display,
                    len: count,
                });
                if let Some((_, size)) = &pill.image {
                    line_height = Some(size.height);
                }
                pills.push(PendingPill {
                    chars: display..display + count,
                    picture: pill.picture,
                    label: pill.label,
                    image: pill.image,
                });
                display += count;
            }
            None => {
                text.push_str(slice);
                run_bytes.push(len);
                display += chars;
            }
        }
    }
    DisplayText {
        text,
        synthetic: false,
        run_bytes,
        widenings,
        pills,
        line_height,
    }
}

/// The advance of one [`PILL_FILLER`], which is the unit a placeholder reserves
/// its width in.
fn filler_width(font_size: Pixels, window: &Window) -> Pixels {
    const SAMPLE: usize = 8;
    let text: String = std::iter::repeat_n(PILL_FILLER, SAMPLE).collect();
    let shaped = window.text_system().shape_line(
        text.clone().into(),
        font_size,
        &[TextRun {
            len: text.len(),
            font: font(".SystemUIFont"),
            color: gpui::transparent_black(),
            background_color: None,
            underline: None,
            strikethrough: None,
        }],
        None,
    );
    (shaped.width / SAMPLE as f32).max(px(1.))
}

/// A pill's content and the width it needs.
struct Pill {
    picture: bool,
    label: Rc<ShapedLine>,
    width: Pixels,
    image: Option<(Arc<RenderImage>, Size<Pixels>)>,
}

/// The pill an inline atom is drawn as, for the atoms the view draws itself.
/// Every other atom keeps the object-replacement character the projection gave
/// it.
fn pill_of(
    types: &DocTypes,
    run: &Run,
    style: &EditorStyle,
    font_size: Pixels,
    column: Pixels,
    alone: bool,
    window: &Window,
) -> Option<Pill> {
    let RunContent::Atom(node) = &run.content else {
        return None;
    };
    let ty = node.type_id();
    let attr = |name: &str| {
        node.attrs()
            .get(name)
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .trim()
    };
    // A local file the note can read is drawn for real where it has the line to
    // itself; the placeholder is still built, and stands in wherever it is not.
    let drawn = (alone && Some(ty) == types.image)
        .then(|| drawn_image(attr("src"), column))
        .flatten();
    let (picture, face, text) = if Some(ty) == types.image {
        let label = match (attr("alt"), file_name(attr("src"))) {
            ("", Some(name)) => name,
            ("", None) => "Image",
            (alt, _) => alt,
        };
        (true, font(".SystemUIFont"), label)
    } else if Some(ty) == types.raw_inline {
        // A closing tag only ends what its opener named, so it collapses to `</>`
        // and the text between the two pills stays readable.
        match attr("source") {
            "" => (false, font(CODE_FONT), "HTML"),
            source if source.starts_with("</") => (false, font(CODE_FONT), "</>"),
            source => (false, font(CODE_FONT), source),
        }
    } else {
        return None;
    };
    let icon = if picture {
        PILL_ICON + PILL_ICON_GAP
    } else {
        px(0.)
    };
    let room = (column * PILL_MAX_RATIO - PILL_PADDING * 2. - icon).max(px(16.));
    let shape = |label: String| {
        Rc::new(window.text_system().shape_line(
            label.clone().into(),
            font_size * PILL_SCALE,
            &[TextRun {
                len: label.len(),
                font: face.clone(),
                color: style.muted_text,
                background_color: None,
                underline: None,
                strikethrough: None,
            }],
            None,
        ))
    };
    let mut graphemes = text.graphemes(true).collect::<Vec<_>>();
    let mut label = shape(graphemes.concat());
    while label.width > room && graphemes.len() > 1 {
        graphemes.pop();
        label = shape(format!("{}…", graphemes.concat()));
    }
    Some(Pill {
        width: match &drawn {
            Some((_, size)) => size.width,
            None => PILL_PADDING * 2. + icon + label.width,
        },
        picture,
        label,
        image: drawn,
    })
}

/// A decoded local image and the size it is drawn at: the column's width, or
/// the image's own where that is narrower, capped at [`IMAGE_MAX_HEIGHT`].
fn drawn_image(src: &str, column: Pixels) -> Option<(Arc<RenderImage>, Size<Pixels>)> {
    let image = local_image(src)?;
    let intrinsic = image.size(0);
    let (native_width, native_height) = (intrinsic.width.0 as f32, intrinsic.height.0 as f32);
    if native_width <= 0. || native_height <= 0. {
        return None;
    }
    let mut width = column.min(px(native_width)).max(px(1.));
    let mut height = width * (native_height / native_width);
    if height > IMAGE_MAX_HEIGHT {
        height = IMAGE_MAX_HEIGHT;
        width = height * (native_width / native_height);
    }
    Some((image, size(width, height)))
}

thread_local! {
    /// Decoding happens on the layout pass, so every source — including one
    /// that cannot be read — is decided exactly once.
    static IMAGES: RefCell<HashMap<String, Option<Arc<RenderImage>>>> =
        RefCell::new(HashMap::new());
}

fn local_image(src: &str) -> Option<Arc<RenderImage>> {
    IMAGES.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(decoded) = cache.get(src) {
            return decoded.clone();
        }
        let decoded = decode_image(src);
        if cache.len() >= IMAGE_CACHE_LIMIT {
            cache.clear();
        }
        cache.insert(src.to_owned(), decoded.clone());
        decoded
    })
}

/// Read and decode a local file. A remote source has no path and never reaches
/// a decoder here.
fn decode_image(src: &str) -> Option<Arc<RenderImage>> {
    let path = std::path::Path::new(src);
    if !path.is_file() || std::fs::metadata(path).ok()?.len() > IMAGE_MAX_BYTES {
        return None;
    }
    let format = match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => ImageFormat::Png,
        "jpg" | "jpeg" => ImageFormat::Jpeg,
        "webp" => ImageFormat::Webp,
        "gif" => ImageFormat::Gif,
        "svg" => ImageFormat::Svg,
        "bmp" => ImageFormat::Bmp,
        "tif" | "tiff" => ImageFormat::Tiff,
        "ico" => ImageFormat::Ico,
        _ => return None,
    };
    let bytes = std::fs::read(path).ok()?;
    // The renderer is only consulted for SVG, and nothing here loads an
    // embedded asset, so an empty asset source is all it needs.
    Image::from_bytes(format, bytes)
        .to_image_data(SvgRenderer::new(Arc::new(())))
        .ok()
}

/// The file name an image source ends in, for a placeholder with no alt text.
fn file_name(src: &str) -> Option<&str> {
    let path = src
        .split(['?', '#'])
        .next()
        .unwrap_or(src)
        .trim_end_matches('/');
    let name = path.rsplit(['/', '\\']).next()?;
    (!name.is_empty()).then_some(name)
}

/// The `char` ranges of the line that sit inside an inline span carrying no
/// mark the view draws.
///
/// Such a run is the only part of a document whose structure nothing on screen
/// stands for, so it gets a hint. A span that became a mark — `<b>` is strong —
/// already shows what it is and is left alone.
fn hints_of(input: &ShapeInput<'_>, line: &Line) -> Vec<Range<usize>> {
    let types = input.types;
    let (Some(span), Some(own)) = (types.inline_span, line.ancestors.last()) else {
        return Vec::new();
    };
    // An inline container is the only thing that makes a line's token span
    // longer than its visible text, so an ordinary line needs no walk at all.
    if line.to - line.from == line.len() {
        return Vec::new();
    }
    let Some(block) = input.doc.node_at(own.before) else {
        return Vec::new();
    };
    let mut positions = Vec::new();
    silent_spans(&block, own.before + 1, span, types, &mut positions);
    positions
        .into_iter()
        .filter_map(|range| {
            let from = line.pos_to_offset(range.start)?;
            let to = line.pos_to_offset(range.end)?;
            (to > from).then_some(from..to)
        })
        .collect()
}

/// Collect the content ranges of every inline span under `parent` whose own
/// marks the view draws nothing for.
fn silent_spans(
    parent: &Node,
    from: usize,
    span: NodeTypeId,
    types: &DocTypes,
    out: &mut Vec<Range<usize>>,
) {
    let mut pos = from;
    for child in parent.children() {
        let size = child.node_size();
        if child.is_container() {
            if child.type_id() == span && !drawn_marks(types, child.marks()) {
                out.push(pos + 1..pos + size - 1);
            }
            silent_spans(child, pos + 1, span, types, out);
        }
        pos += size;
    }
}

/// Whether the view paints anything for `marks`, which is what tells a reader
/// that the span carrying them is there.
fn drawn_marks(types: &DocTypes, marks: &MarkSet) -> bool {
    [
        types.strong,
        types.em,
        types.code,
        types.strikethrough,
        types.underline,
        types.link,
    ]
    .into_iter()
    .any(|ty| has(ty, marks))
}

struct Runs {
    runs: Vec<TextRun>,
    /// Byte ranges of inline code within the whole line text, with their face.
    code: Vec<(Range<usize>, Font, Hsla)>,
}

fn text_runs(
    input: &ShapeInput<'_>,
    line: &Line,
    text: &DisplayText,
    heading: Option<u8>,
    code_block: bool,
    font_size: Pixels,
    style: &EditorStyle,
) -> Runs {
    let types = input.types;
    // A header cell is bold as a heading is: the weight is what says the row
    // names the columns rather than holding data.
    let header = types.is_table_header(line);
    let checked_item = types.in_checked_item(line);
    let text_color = if checked_item {
        style.muted_text
    } else {
        style.text
    };
    // A ticked item is greyed and struck through as a whole, but a pill and a
    // link keep the colours that say what they are.
    let done = checked_item.then_some(StrikethroughStyle {
        thickness: px(1.),
        color: Some(style.muted_text),
    });
    if text.synthetic {
        let mut face = font(".SystemUIFont");
        if heading.is_some() {
            face.weight = FontWeight::BOLD;
        }
        // A raw block's source sits on its own panel, so it reads as code here.
        let raw = line.kind == LineKind::LeafBlock && text.text != " ";
        if raw {
            face = font(CODE_FONT);
        }
        return Runs {
            runs: vec![TextRun {
                len: text.text.len(),
                font: face,
                color: if raw { style.text } else { text_color },
                background_color: None,
                underline: None,
                strikethrough: None,
            }],
            code: Vec::new(),
        };
    }
    let mut runs = Vec::with_capacity(line.runs.len());
    let mut code = Vec::new();
    let mut byte = 0usize;
    for (index, run) in line.runs.iter().enumerate() {
        let len = text.run_bytes.get(index).copied().unwrap_or(0);
        let range = byte..byte + len;
        byte += len;
        if len == 0 {
            continue;
        }
        let marks = &run.marks;
        let is_code = code_block || has(types.code, marks);
        let is_link = has(types.link, marks);
        let mut face = font(if is_code { CODE_FONT } else { ".SystemUIFont" });
        if has(types.strong, marks) || heading.is_some() || header {
            face.weight = FontWeight::BOLD;
        }
        if has(types.em, marks) {
            face.style = FontStyle::Italic;
        }
        let atom = matches!(run.content, RunContent::Atom(_));
        // A widened atom is drawn as a pill over the fillers standing in for it.
        let pill = text
            .widenings
            .iter()
            .any(|widening| widening.source == run.char_from);
        let ink = if is_link || (atom && !code_block) {
            style.link
        } else if has(types.code, marks) {
            style.inline_code_text
        } else {
            text_color
        };
        // Inline code only reserves its space here; see `InlineCode`.
        let color = if pill {
            gpui::transparent_black()
        } else if has(types.code, marks) && !code_block {
            code.push((range, face.clone(), ink));
            gpui::transparent_black()
        } else {
            ink
        };
        runs.push(TextRun {
            len,
            font: face,
            color,
            background_color: None,
            underline: (!pill && (has(types.underline, marks) || is_link)).then_some(
                UnderlineStyle {
                    thickness: px(1.),
                    color: Some(ink),
                    wavy: false,
                },
            ),
            strikethrough: has(types.strikethrough, marks)
                .then_some(StrikethroughStyle {
                    thickness: px(1.),
                    color: Some(ink),
                })
                .or(done),
        });
    }
    if code_block {
        let language = types.code_language(line).unwrap_or("");
        let highlighted = crate::syntax::highlight(&text.text, language, style.background.l < 0.5);
        let total: usize = highlighted
            .iter()
            .map(|row| row.iter().map(|(len, _)| len).sum::<usize>())
            .sum::<usize>()
            + highlighted.len().saturating_sub(1);
        if total == text.text.len() {
            runs = highlight_runs(&highlighted, font_size);
        }
    }
    Runs { runs, code }
}

fn highlight_runs(
    highlighted: &crate::syntax::HighlightedLines,
    _font_size: Pixels,
) -> Vec<TextRun> {
    let mut runs = Vec::new();
    for (index, row) in highlighted.iter().enumerate() {
        if index > 0 {
            runs.push(TextRun {
                len: 1,
                font: font(CODE_FONT),
                color: gpui::transparent_black(),
                background_color: None,
                underline: None,
                strikethrough: None,
            });
        }
        for (len, syntax) in row {
            let mut face = font(CODE_FONT);
            // Themes embolden keywords; at note size colour alone reads calmer.
            if syntax
                .font_style
                .contains(syntect::highlighting::FontStyle::ITALIC)
            {
                face.style = FontStyle::Italic;
            }
            let color = syntax.foreground;
            runs.push(TextRun {
                len: *len,
                font: face,
                color: rgb(((color.r as u32) << 16) | ((color.g as u32) << 8) | color.b as u32)
                    .into(),
                background_color: None,
                underline: None,
                strikethrough: None,
            });
        }
    }
    runs
}

fn has(ty: Option<markraft_core::MarkTypeId>, marks: &MarkSet) -> bool {
    ty.is_some_and(|ty| marks.contains_type(ty))
}

/// How many indentation levels fit before the text would be squeezed away.
fn visible_levels(style: &EditorStyle, max_indent: Pixels) -> usize {
    ((max_indent / style.quote_indent.max(px(1.))) as usize).max(1)
}

fn indent_of(
    types: &DocTypes,
    line: &Line,
    style: &EditorStyle,
    number_width: Option<Pixels>,
) -> Pixels {
    let mut indent = px(0.);
    for ancestor in &line.ancestors {
        let ty = ancestor.node_type;
        if Some(ty) == types.blockquote {
            indent += style.quote_indent;
        } else if types.is_list(ty) {
            indent += style.list_indent;
        } else if Some(ty) == types.code_block || Some(ty) == types.raw_block {
            indent += CODE_PADDING;
        }
    }
    if let Some(width) = number_width {
        // An ordered list's items share the indent its widest number needs.
        indent += (width + NUMBER_GAP - style.list_indent).max(px(0.));
    }
    indent
}

/// The shaped ordinal for a line that starts an ordered-list item, and the width
/// every item of that list reserves.
fn ordered_marker(
    doc: &Node,
    types: &DocTypes,
    line: &Line,
    style: &EditorStyle,
    font_size: Pixels,
    window: &Window,
) -> Option<(Rc<ShapedLine>, Pixels)> {
    let (item, list) = types.item_of(line)?;
    if Some(list.node_type) != types.ordered_list || !starts_item(line) {
        return None;
    }
    let start = list
        .attrs
        .get("start")
        .and_then(|value| value.as_int())
        .unwrap_or(1);
    let count = doc
        .node_at(list.before)
        .map_or(item.index + 1, |node| node.child_count());
    let shape_number = |ordinal: i64| {
        let text = format!("{ordinal}.");
        Rc::new(window.text_system().shape_line(
            text.clone().into(),
            font_size,
            &[TextRun {
                len: text.len(),
                font: font(".SystemUIFont"),
                color: style.marker,
                background_color: None,
                underline: None,
                strikethrough: None,
            }],
            None,
        ))
    };
    let widest = shape_number(start + count.saturating_sub(1) as i64).width;
    Some((shape_number(start + item.index as i64), widest))
}

/// Whether a line is the first block of its list item, which is where the
/// marker is drawn.
fn starts_item(line: &Line) -> bool {
    line.ancestors
        .last()
        .is_some_and(|own| own.index == 0 && line.ancestors.len() >= 2)
}

fn marker_of(
    types: &DocTypes,
    line: &Line,
    style: &EditorStyle,
    number: Option<Rc<ShapedLine>>,
) -> Option<Marker> {
    if let Some(number) = number {
        return Some(Marker::Number(number));
    }
    let (item, list) = types.item_of(line)?;
    if !starts_item(line) || Some(list.node_type) != types.bullet_list {
        return None;
    }
    let _ = style;
    if Some(item.node_type) == types.task_item {
        return Some(Marker::Task {
            checked: DocTypes::task_checked(&item.attrs),
        });
    }
    Some(Marker::Bullet {
        depth: types.list_depth(line),
    })
}

/// How many of a line's quote levels continue into the line below.
fn joined_quote_levels(projection: &Projection, index: usize, types: &DocTypes) -> usize {
    let Some(next) = projection.line(index + 1) else {
        return 0;
    };
    let line = &projection.lines()[index];
    line.ancestors
        .iter()
        .zip(next.ancestors.iter())
        .take_while(|(a, b)| a.before == b.before && a.node_type == b.node_type)
        .filter(|(a, _)| Some(a.node_type) == types.blockquote)
        .count()
}

fn gap_below(
    input: &ShapeInput<'_>,
    index: usize,
    line: &Line,
    heading: Option<u8>,
    code: bool,
    marker: &Option<Marker>,
) -> Pixels {
    let style = input.style;
    if input.single_line {
        return px(0.);
    }
    let next = input.projection.line(index + 1);
    // The code fill reaches CODE_PADDING past the last row, so a code block keeps
    // its own bottom padding whatever block follows it.
    if code {
        return CODE_PADDING + style.paragraph_gap;
    }
    // A table is one block: its cells sit tight against each other, and only
    // the cell that closes the grid is spaced off what follows it.
    if let Some((table, _, _)) = input.types.table_cell_of(line) {
        let continues = next
            .and_then(|next| input.types.table_cell_of(next))
            .is_some_and(|(below, _, _)| below == table);
        return if continues {
            px(0.)
        } else {
            style.paragraph_gap
        };
    }
    // The tighter list gap holds between the lines of a list. The block that
    // closes one is spaced off it like any other pair of blocks.
    if marker.is_some() || input.types.item_of(line).is_some() {
        let continues = next.is_some_and(|next| input.types.item_of(next).is_some());
        return if continues {
            style.list_gap
        } else {
            style.paragraph_gap
        };
    }
    if heading.is_some() {
        return style.heading_bottom_gap;
    }
    // A quote is a block like any other: it is spaced off what surrounds it and
    // only its own lines sit close together. A quote opening inside a quote is
    // the exception — the bar already says where it starts, and a gap there
    // would break the bar in two.
    let depth = input.types.quote_depth(line);
    let below = next.map_or(0, |next| input.types.quote_depth(next));
    if depth > 0 || below > 0 {
        return if below > depth {
            if depth == 0 {
                style.paragraph_gap
            } else {
                px(0.)
            }
        } else if below == depth {
            style.list_gap
        } else {
            style.paragraph_gap
        };
    }
    style.paragraph_gap
}

fn code_header(
    label: &str,
    interactive: bool,
    width: Pixels,
    style: &EditorStyle,
    window: &Window,
) -> Option<CodeHeader> {
    let shape_label = |label: String| {
        Rc::new(window.text_system().shape_line(
            label.clone().into(),
            px(13.),
            &[TextRun {
                len: label.len(),
                font: font(".SystemUIFont"),
                color: style.text,
                background_color: None,
                underline: None,
                strikethrough: None,
            }],
            None,
        ))
    };
    let chrome = if interactive {
        CODE_COPY_WIDTH + CODE_CHEVRON_WIDTH
    } else {
        px(0.)
    };
    let language_width = (width - chrome - px(30.)).max(px(20.));
    let mut label = label.graphemes(true).take(64).collect::<Vec<_>>();
    let mut shaped = shape_label(label.concat());
    while shaped.width > language_width && !label.is_empty() {
        label.pop();
        shaped = shape_label(format!("{}…", label.concat()));
    }
    Some(CodeHeader {
        language: shaped,
        interactive,
    })
}

/// Shape the smaller text drawn inside each inline-code pill, and record the
/// slots it sits in.
fn shape_inline_code(
    layout: &mut LayoutLine,
    text: &str,
    code_ranges: &[(Range<usize>, Font, Hsla)],
    font_size: Pixels,
    window: &Window,
) {
    if code_ranges.is_empty() {
        return;
    }
    for (range, face, ink) in code_ranges {
        // The slot is as wide as full-size text. Shrinking by a fixed ratio would
        // leave long spans mostly padding, so the size is chosen to leave about
        // `INLINE_CODE_PADDING` on either side of each visual row instead.
        let chars = byte_to_char(text, range.start)..byte_to_char(text, range.end);
        let slots = layout.display_rectangles(chars.clone(), false);
        let reserved: Pixels = slots.iter().map(|slot| slot.size.width).sum();
        let padding = INLINE_CODE_PADDING * 2. * slots.len() as f32;
        let scale = ((reserved - padding) / reserved.max(px(1.))).clamp(INLINE_CODE_SCALE, 1.);
        let mut pieces: Vec<(usize, Range<usize>, Bounds<Pixels>)> = Vec::new();
        for (index, row) in layout.rows.iter().enumerate() {
            let row_text = row.text();
            let from = range.start.saturating_sub(row_byte_start(layout, index));
            let to = range.end.saturating_sub(row_byte_start(layout, index));
            if to == 0 || from >= row_text.len() {
                continue;
            }
            let (from, to) = (from.min(row_text.len()), to.min(row_text.len()));
            let starts = row.wrap_starts();
            for (visual, &start) in starts.iter().enumerate() {
                let end = starts.get(visual + 1).copied().unwrap_or(row_text.len());
                let part = from.max(start)..to.min(end);
                if part.is_empty() {
                    continue;
                }
                let chars = row.char_start + byte_to_char(row_text, part.start)
                    ..row.char_start + byte_to_char(row_text, part.end);
                let Some(slot) = layout.display_rectangles(chars, false).into_iter().next() else {
                    continue;
                };
                pieces.push((index, part, slot));
            }
        }
        for (index, part, slot) in pieces {
            let row_text = layout.rows[index].text().to_owned();
            let line = window.text_system().shape_line(
                row_text[part.clone()].to_owned().into(),
                font_size * scale,
                &[TextRun {
                    len: part.len(),
                    font: face.clone(),
                    color: *ink,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                }],
                None,
            );
            let visual = layout.rows[index].visual_start
                + ((slot.origin.y - layout.origin.y) / layout.line_height).round() as usize;
            layout.rows[index].inline_code.push(InlineCode {
                range: part,
                visual_row: visual,
                left: slot.origin.x - layout.origin.x,
                slot: slot.size.width,
                line: Rc::new(line),
            });
        }
    }
}

/// The byte offset at which a row's text starts within the line's text.
fn row_byte_start(layout: &LayoutLine, index: usize) -> usize {
    layout.rows[..index]
        .iter()
        .map(|row| row.text().len() + 1)
        .sum()
}

pub(crate) struct EditorSurface {
    pub editor: Entity<EditorView>,
}
impl IntoElement for EditorSurface {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}
impl Element for EditorSurface {
    type RequestLayoutState = ();
    type PrepaintState = Vec<LayoutLine>;
    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }
    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        _: &mut App,
    ) -> (LayoutId, ()) {
        let editor = self.editor.clone();
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.flex_shrink = 0.;
        (
            window.request_measured_layout(style, move |known, available, window, cx| {
                let view = editor.read(cx);
                let width = known.width.unwrap_or(match available.width {
                    AvailableSpace::Definite(w) => w,
                    _ if view.single_line => px(0.),
                    _ => px(600.),
                });
                let rows = shape(&view.shape_input(), width, window);
                let height = rows
                    .iter()
                    .fold(if view.single_line { px(0.) } else { px(40.) }, |h, row| {
                        h + row.top_gap + row.height
                    });
                size(width, height)
            }),
            (),
        )
    }
    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Vec<LayoutLine> {
        let view = self.editor.read(cx);
        let mut rows = shape(&view.shape_input(), bounds.size.width, window);
        let mut y = bounds.top();
        for row in &mut rows {
            y += row.top_gap;
            row.origin += point(bounds.left(), y);
            y += row.height;
        }
        let head = view.head();
        let single_line_scroll_x = if view.single_line {
            rows.first()
                .map(|row| {
                    let offset = row.pos_to_offset(head);
                    crate::single_line::scroll_offset(
                        view.single_line_scroll_x,
                        row.caret(offset, view.upstream).x - bounds.left(),
                        row.rows
                            .first()
                            .map(|inner| inner.line.size(row.line_height).width)
                            .unwrap_or(px(0.)),
                        bounds.size.width,
                    )
                })
                .unwrap_or(px(0.))
        } else {
            px(0.)
        };
        // Every consumer uses these translated rows: paint, hit testing,
        // selection, input-method rectangles, and accessible text bounds.
        for row in &mut rows {
            row.origin.x -= single_line_scroll_x;
            if let (Some(language), Some(copy)) =
                (row.code_language_bounds(), row.code_copy_bounds())
            {
                row.code_hitboxes = Some((
                    window.insert_hitbox(language, HitboxBehavior::Normal),
                    window.insert_hitbox(copy, HitboxBehavior::Normal),
                ));
            }
        }
        if window.is_a11y_active() {
            view.accessible_text.borrow_mut().update(
                &view.projection(),
                view.state(),
                &rows,
                window.scale_factor(),
            );
        }
        self.editor.update(cx, |editor, cx| {
            editor.single_line_scroll_x = single_line_scroll_x;
            editor.layout = rows.clone();
            if editor.reveal {
                editor.reveal = false;
                if editor.single_line {
                    return;
                }
                let head = editor.head();
                if let Some((row, offset)) = rows.iter().find(|row| row.contains(head)).map(|row| {
                    let offset = row.pos_to_offset(head);
                    (row, offset)
                }) {
                    let caret = row.caret(offset, editor.upstream);
                    let viewport = editor.scroll.bounds();
                    let margin = px(12.);
                    let top = viewport.top() + editor.style.top_overlay;
                    let bottom = viewport.bottom() - editor.style.bottom_overlay;
                    let correction = if caret.y < top + margin {
                        top + margin - caret.y
                    } else if caret.y + row.line_height > bottom - margin {
                        bottom - margin - caret.y - row.line_height
                    } else {
                        px(0.)
                    };
                    if correction != px(0.) {
                        let offset = editor.scroll.offset();
                        editor
                            .scroll
                            .set_offset(point(offset.x, offset.y + correction));
                        cx.notify();
                    }
                }
            }
        });
        rows
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        rows: &mut Vec<LayoutLine>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let editor = self.editor.read(cx);
        let projection = editor.projection();
        let state = editor.state();
        let doc = state.doc();
        let selection = state.selection();
        let (a, b) = (selection.from(doc), selection.to(doc));
        let caret_pos = selection.head(doc);
        let marked = markraft_core::composition_range(state).map(|range| (range.from, range.to));
        let focused = editor.focus.is_focused(window);
        let caret_visible = editor.caret_blink.visible && window.is_window_active();
        let caret_shape = editor.extension_caret();
        // The grapheme the caret rests on, for the shapes that cover one.
        // The row that draws the caret keeps this only while it stays inside it.
        let caret_next = (caret_shape != CaretShape::Bar && a == b)
            .then(|| projection.next_grapheme_boundary(caret_pos))
            .flatten();
        let focus = editor.focus.clone();
        let upstream = editor.upstream;
        let style = editor.style.clone();
        let placeholder = (projection.line_count() == 1 && projection.plain_text().is_empty())
            .then(|| editor.placeholder.clone());
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.editor.clone()),
            cx,
        );
        // The grids first: their bands and lines sit under everything a cell
        // draws, including the selection.
        paint_tables(rows, &style, caret_pos, window);
        for row in rows.iter() {
            for inner in &row.rows {
                for code in &inner.inline_code {
                    // A pill shorter than the line.
                    let inset = (row.line_height * 0.1).round();
                    let pill = Bounds::new(
                        row.origin
                            + point(code.left, row.line_height * code.visual_row as f32 + inset),
                        size(code.slot, row.line_height - inset * 2.),
                    );
                    window.paint_quad(
                        fill(pill, style.inline_code_background).corner_radii(style.code_radius),
                    );
                }
            }
            for pill in &row.pills {
                paint_pill(row, pill, &style, window, cx);
            }
            match row.decoration {
                Some(Decoration::Quote { levels, joined }) => {
                    for level in 0..levels {
                        let height = if level < joined {
                            row.height
                        } else {
                            row.text_height()
                        };
                        window.paint_quad(fill(
                            Bounds::new(
                                point(
                                    row.origin.x - style.quote_indent * (levels - level) as f32,
                                    row.origin.y,
                                ),
                                size(px(2.), height),
                            ),
                            style.marker,
                        ));
                    }
                }
                Some(Decoration::Divider) => window.paint_quad(fill(
                    Bounds::new(
                        point(row.origin.x, row.origin.y + row.line_height * 0.5),
                        size(row.width, px(1.5)),
                    ),
                    style.rule,
                )),
                Some(Decoration::Panel) => {
                    window.paint_quad(
                        fill(
                            Bounds::new(
                                point(
                                    row.origin.x - CODE_PADDING,
                                    row.origin.y - CODE_PADDING - CODE_HEADER_HEIGHT,
                                ),
                                size(
                                    row.width + CODE_PADDING * 2.,
                                    row.text_height() + CODE_PADDING * 2. + CODE_HEADER_HEIGHT,
                                ),
                            ),
                            style.code_background,
                        )
                        .corner_radii(Corners::all(CODE_RADIUS)),
                    );
                }
                None => {}
            }
            if let Some(header) = &row.code_header {
                paint_code_header(self, row, header, &style, window, cx);
            }
            if a != b {
                let from = row.pos_to_offset(a);
                let to = row.pos_to_offset(b);
                if b > row.from && a <= row.to() {
                    let spans_next = b > row.to();
                    let selection = if focused {
                        style.selection
                    } else {
                        style.selection_inactive
                    };
                    for rect in row.rectangles(from..to.min(row.char_len), spans_next) {
                        window.paint_quad(fill(rect, selection));
                    }
                }
            }
            for inner in &row.rows {
                let origin =
                    row.origin + point(px(0.), row.line_height * inner.visual_start as f32);
                let _ =
                    inner
                        .line
                        .paint(origin, row.line_height, TextAlign::Left, None, window, cx);
                for code in &inner.inline_code {
                    let _ = code.line.paint(
                        row.origin
                            + point(code.text_left(), row.line_height * code.visual_row as f32),
                        row.line_height,
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    );
                }
            }
            for hint in &row.hints {
                paint_hint(row, hint.clone(), &style, window);
            }
            if let Some(marker) = &row.marker {
                paint_marker(row, marker, &style, window, cx);
            }
            if row.index == 0
                && let Some(text) = placeholder.as_ref().filter(|text| !text.is_empty())
            {
                // Presentation only: the empty block still owns hit testing and IME coordinates.
                let run = TextRun {
                    len: text.len(),
                    font: font(".SystemUIFont"),
                    color: style.muted_text,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                if let Ok(lines) = window.text_system().shape_text(
                    text.clone(),
                    style.body_size,
                    &[run],
                    Some(row.width),
                    Some(1),
                ) {
                    for line in lines {
                        let _ = line.paint(
                            row.origin,
                            row.line_height,
                            TextAlign::Left,
                            None,
                            window,
                            cx,
                        );
                    }
                }
            }
            if let Some((start, end)) = marked
                && end > row.from
                && start <= row.to()
            {
                let from = row.pos_to_offset(start);
                let to = row.pos_to_offset(end);
                for mut rect in row.rectangles(from..to, false) {
                    rect.origin.y += rect.size.height - px(2.);
                    rect.size.height = px(1.5);
                    window.paint_quad(fill(rect, style.marker));
                }
            }
            if focused && caret_visible && a == b && row.contains(caret_pos) {
                let mut color = style.marker;
                if caret_shape == CaretShape::Block {
                    color.a = BLOCK_CARET_ALPHA;
                }
                window.paint_quad(fill(
                    caret_quad(
                        row,
                        row.pos_to_offset(caret_pos),
                        caret_next
                            .filter(|next| row.contains(*next))
                            .map(|next| row.pos_to_offset(next)),
                        upstream,
                        caret_shape,
                    ),
                    color,
                ));
            }
        }
    }
}

/// Draw the chrome of every table in `rows`.
fn paint_tables(rows: &[LayoutLine], style: &EditorStyle, caret: usize, window: &mut Window) {
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
        paint_table(&rows[start..end], style, caret, window);
        start = end;
    }
}

/// Draw one table: the header band, the grid, and the border round the cell
/// the caret is in.
///
/// The separators are hairlines drawn along each cell's own bottom and right
/// edge rather than a border per cell, so a shared edge is one pixel wide and
/// not two, and the outer rectangle is drawn last so its rounded corners sit
/// over the band.
fn paint_table(cells: &[LayoutLine], style: &EditorStyle, caret: usize, window: &mut Window) {
    let boxes: Vec<(TableCell, Bounds<Pixels>)> = cells
        .iter()
        .filter_map(|line| Some((line.table?, line.cell_bounds()?)))
        .collect();
    let Some((_, first)) = boxes.first() else {
        return;
    };
    let outer = boxes
        .iter()
        .fold(*first, |all, (_, bounds)| all.union(bounds));
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
        .and_then(LayoutLine::cell_bounds)
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

/// Draw one inline atom's pill over the fillers reserving its slot.
fn paint_pill(
    row: &LayoutLine,
    pill: &InlinePill,
    style: &EditorStyle,
    window: &mut Window,
    cx: &mut App,
) {
    let top = row.origin.y + row.line_height * pill.visual_row as f32;
    if let Some((image, drawn)) = &pill.image {
        let bounds = Bounds::new(point(row.origin.x + pill.left, top), *drawn);
        let _ = window.paint_image(
            bounds,
            bounds,
            Corners::all(style.code_radius),
            image.clone(),
            0,
            false,
        );
        return;
    }
    let inset = (row.line_height * 0.1).round();
    let bounds = Bounds::new(
        point(row.origin.x + pill.left, top + inset),
        size(pill.slot, row.line_height - inset * 2.),
    );
    window.paint_quad(fill(bounds, style.inline_code_background).corner_radii(style.code_radius));
    let icon = if pill.picture {
        PILL_ICON + PILL_ICON_GAP
    } else {
        px(0.)
    };
    let left = bounds.origin.x + (bounds.size.width - icon - pill.label.width).max(px(0.)) / 2.;
    if pill.picture {
        paint_picture(
            point(left, bounds.center().y - PILL_ICON * 0.5),
            style,
            window,
        );
    }
    let _ = pill.label.paint(
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
fn paint_picture(origin: Point<Pixels>, style: &EditorStyle, window: &mut Window) {
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

/// Draw the dotted rule under text that sits inside an inline span the line
/// shows nothing else for.
fn paint_hint(row: &LayoutLine, chars: Range<usize>, style: &EditorStyle, window: &mut Window) {
    for rect in row.rectangles(chars, false) {
        let y = rect.origin.y + row.line_height - HINT_PITCH;
        let mut x = rect.origin.x;
        while x + HINT_DOT <= rect.origin.x + rect.size.width {
            window.paint_quad(fill(
                Bounds::new(point(x, y), size(HINT_DOT, HINT_DOT)),
                style.rule,
            ));
            x += HINT_PITCH;
        }
    }
}

fn paint_marker(
    row: &LayoutLine,
    marker: &Marker,
    style: &EditorStyle,
    window: &mut Window,
    cx: &mut App,
) {
    let bounds = row.marker_bounds().expect("marker has bounds");
    match marker {
        Marker::Number(line) => {
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
        Marker::Task { checked } => {
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

fn paint_code_header(
    surface: &EditorSurface,
    row: &LayoutLine,
    header: &CodeHeader,
    style: &EditorStyle,
    window: &mut Window,
    cx: &mut App,
) {
    let label = row.header_bounds().expect("a header has label bounds");
    let _ = header.language.paint(
        label.origin + point(px(6.), px(3.)),
        px(18.),
        TextAlign::Left,
        None,
        window,
        cx,
    );
    // A raw block's header is a caption: nothing to pick, nothing to copy.
    let (Some(language), Some(copy)) = (row.code_language_bounds(), row.code_copy_bounds()) else {
        return;
    };
    let mut icons = PathBuilder::stroke(px(1.2));
    let chevron = point(
        language.right() - CODE_CHEVRON_WIDTH,
        language.top() + px(10.5),
    );
    icons.move_to(chevron + point(px(2.), px(0.)));
    icons.line_to(chevron + point(px(5.), px(3.)));
    icons.line_to(chevron + point(px(8.), px(0.)));
    // Clipboard: a board with a clip on its top edge.
    let board = copy.origin + point(px(9.), px(6.5));
    let (w, h, r) = (px(10.), px(12.), px(2.));
    icons.move_to(board + point(r, px(0.)));
    icons.line_to(board + point(w - r, px(0.)));
    icons.line_to(board + point(w, r));
    icons.line_to(board + point(w, h - r));
    icons.line_to(board + point(w - r, h));
    icons.line_to(board + point(r, h));
    icons.line_to(board + point(px(0.), h - r));
    icons.line_to(board + point(px(0.), r));
    icons.line_to(board + point(r, px(0.)));
    icons.move_to(board + point(px(3.), px(1.5)));
    icons.line_to(board + point(px(3.), px(-1.5)));
    icons.line_to(board + point(px(7.), px(-1.5)));
    icons.line_to(board + point(px(7.), px(1.5)));
    if let Ok(path) = icons.build() {
        window.paint_path(path, style.muted_text);
    }
    let Some((language_box, copy_box)) = &row.code_hitboxes else {
        return;
    };
    window.set_cursor_style(CursorStyle::PointingHand, language_box);
    window.set_cursor_style(CursorStyle::PointingHand, copy_box);
    let language_box = language_box.clone();
    let copy_box = copy_box.clone();
    let editor = surface.editor.clone();
    let index = row.index;
    let code_pos = row.code_pos;
    window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
        if !phase.bubble() || event.button != MouseButton::Left {
            return;
        }
        let language_clicked = language_box.is_hovered_at(event.position, window);
        let copy_clicked = copy_box.is_hovered_at(event.position, window);
        if (language_clicked || copy_clicked) && editor.read(cx).is_composing() {
            editor.update(cx, |editor, cx| editor.cancel_composition(cx));
            // Cancelling can restore a different document. The next click must use
            // the freshly rendered hitboxes and positions.
            cx.stop_propagation();
            return;
        }
        if language_clicked {
            if let Some(pos) = code_pos {
                editor.update(cx, |_, cx| {
                    cx.emit(EditorEvent::CodeLanguageRequested { pos });
                });
            }
            cx.stop_propagation();
        } else if copy_clicked {
            let view = editor.read(cx);
            if let Some(text) = view.projection().line_text(index) {
                cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned()));
                editor.update(cx, |_, cx| cx.emit(EditorEvent::CodeCopied));
            }
            cx.stop_propagation();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{
        CELL_MIN_WIDTH, CELL_PADDING_X, CELL_PADDING_Y, CODE_PADDING, LayoutLine, ShapeInput,
        Widening, cell_under, column_widths, drawn_image, file_name, gap_below, hints_of,
        marker_of, merge_row_centers, place_table, raw_block_label,
    };
    use crate::style::EditorStyle;
    use crate::typeahead::tests::state_of;
    use crate::types::DocTypes;
    use gpui::{Bounds, Pixels, point, px, size};
    use markraft_commonmark::{commonmark_doc_type_names, commonmark_schema};
    use markraft_core::commands::ColumnAlignment;
    use markraft_core::projection::{Projection, projection_of};
    use markraft_core::{Mark, MarkSet, Node, Schema};

    /// A row carrying only the token range it stands for, which is all the
    /// caret and selection geometry decides with.
    fn probe(index: usize, line: &markraft_core::projection::Line) -> LayoutLine {
        LayoutLine {
            source: line.clone(),
            index,
            from: line.from,
            char_len: line.len(),
            rows: Vec::new(),
            origin: point(px(0.), px(0.)),
            line_height: px(10.),
            height: px(10.),
            width: px(100.),
            top_gap: Pixels::ZERO,
            code_pos: None,
            marker: None,
            decoration: None,
            code_header: None,
            code_hitboxes: None,
            widenings: Vec::new(),
            pills: Vec::new(),
            hints: Vec::new(),
            table: None,
        }
    }

    /// The rows a document's projection would be laid out into, as ranges.
    fn rows_of(source: &str) -> Vec<LayoutLine> {
        let state = state_of(source);
        let projection = projection_of(&state);
        projection
            .lines()
            .iter()
            .enumerate()
            .map(|(index, line)| probe(index, line))
            .collect()
    }

    /// The document the live session ended with: every shape that makes a row
    /// stand for something other than one plain paragraph — a multi-row code
    /// block, a leaf block with no text, an empty paragraph — followed by the
    /// line holding the caret.
    const SHAPE: &str = "# Head\n\npara\n\n- a\n- b\n- [ ] c\n- [x] d\n\n1. x\n1. y\n\n\
                         > - q\n\n```rust\na\nb\nc\n```\n\n---\n\nLast paragraph\n\n\
                         h\n\nh\n\nh\n\nh\n\n<br>\n\nh";

    #[test]
    fn a_hit_on_a_divider_stays_on_that_leaf() {
        let state = state_of("text\n\n***");
        let projection = projection_of(&state);
        let divider = &projection.lines()[1];
        let row = probe(1, divider);
        assert_eq!(row.hit_position(0, &projection), divider.from);
    }

    #[test]
    fn layout_coordinates_skip_inline_container_boundaries() {
        let rows = rows_of("*a **b** c*");
        let row = &rows[0];
        assert_eq!(row.char_len, 5);
        assert!(row.to() - row.from > row.char_len);
        for offset in 0..=row.char_len {
            let pos = row.offset_to_pos(offset);
            assert_eq!(row.pos_to_offset(pos), offset);
            assert!(row.contains(pos));
        }
        assert_eq!(row.pos_to_offset(row.from), 0);
        assert_eq!(row.pos_to_offset(row.to()), row.char_len);
    }

    #[test]
    fn exactly_one_row_holds_any_caret_position() {
        let rows = rows_of(SHAPE);
        assert!(rows.len() > 15, "the shape lays out as many rows");
        let last = rows.last().expect("a last row");
        for pos in 0..=last.to() {
            let holding: Vec<usize> = rows
                .iter()
                .filter(|row| row.contains(pos))
                .map(|row| row.index)
                .collect();
            assert!(
                holding.len() <= 1,
                "position {pos} is claimed by rows {holding:?}"
            );
        }
    }

    /// The bug the live smoke test found: the caret on the document's last line
    /// was painted five rows above it. The row that draws it is the one whose
    /// own range holds the caret, so it can only ever be the right one.
    #[test]
    fn the_caret_on_the_last_line_belongs_to_the_last_row() {
        let rows = rows_of(SHAPE);
        let last = rows.last().expect("a last row").clone();
        assert_eq!(last.char_len, 1, "the last line holds one character");
        for caret in [last.from, last.to()] {
            let drawn: Vec<usize> = rows
                .iter()
                .filter(|row| row.contains(caret))
                .map(|row| row.index)
                .collect();
            assert_eq!(drawn, vec![last.index], "caret {caret}");
        }
    }

    /// Gaps far enough apart that the number a line carries says which spacing
    /// rule produced it.
    fn spaced_style() -> EditorStyle {
        EditorStyle {
            paragraph_gap: px(11.),
            list_gap: px(3.),
            heading_bottom_gap: px(5.),
            ..EditorStyle::notes()
        }
    }

    /// The gap every line of a document carries below it.
    fn gaps_of(source: &str, style: &EditorStyle) -> Vec<Pixels> {
        let state = state_of(source);
        let projection = projection_of(&state);
        let schema = commonmark_schema();
        let types = DocTypes::from_schema_names(&schema, &commonmark_doc_type_names());
        let input = ShapeInput {
            doc: state.doc(),
            types: &types,
            projection: &projection,
            style,
            single_line: false,
        };
        projection
            .lines()
            .iter()
            .enumerate()
            .map(|(index, line)| {
                gap_below(
                    &input,
                    index,
                    line,
                    types.heading_level(line),
                    types.is_code_block(line),
                    &marker_of(&types, line, style, None),
                )
            })
            .collect()
    }

    /// The tight list gap belongs between a list's own lines. The block that
    /// closes one used to inherit it, which left a quote or paragraph sitting
    /// against the last item.
    #[test]
    fn a_list_closes_with_the_ordinary_block_gap() {
        let style = spaced_style();
        for following in ["> quote", "para", "```\nx\n```"] {
            let gaps = gaps_of(&format!("- a\n- b\n\n{following}"), &style);
            assert_eq!(gaps[0], style.list_gap, "between two items");
            assert_eq!(
                gaps[1], style.paragraph_gap,
                "below the item that closes the list, above {following:?}"
            );
        }
    }

    #[test]
    fn a_nested_item_still_belongs_to_the_list_above_it() {
        let style = spaced_style();
        let gaps = gaps_of("- a\n  - b\n\npara", &style);
        assert_eq!(gaps[0], style.list_gap, "above the nested item");
        assert_eq!(gaps[1], style.paragraph_gap, "below the nested item");
    }

    /// A heading is spaced off whatever follows it by the same amount, so a list
    /// under one is not tighter than a paragraph under one.
    #[test]
    fn a_heading_keeps_its_own_gap_above_any_block() {
        let style = spaced_style();
        for following in ["- a", "para"] {
            let gaps = gaps_of(&format!("# Head\n\n{following}"), &style);
            assert_eq!(gaps[0], style.heading_bottom_gap, "above {following:?}");
        }
    }

    /// The code fill is drawn past the last row, so the gap below has to clear it
    /// even where a quote would otherwise sit tight.
    #[test]
    fn a_code_block_keeps_its_bottom_padding_above_a_quote() {
        let style = spaced_style();
        let gaps = gaps_of("```\nx\n```\n\n> quote", &style);
        assert_eq!(gaps[0], CODE_PADDING + style.paragraph_gap);
    }

    /// A quote is a block: spaced off what comes before and after it, close
    /// only between its own lines and where one quote opens inside another.
    #[test]
    fn a_quote_is_spaced_off_the_blocks_around_it() {
        let style = spaced_style();
        let gaps = gaps_of("para\n\n> a\n>\n> b\n\npara", &style);
        assert_eq!(gaps[0], style.paragraph_gap, "above the quote");
        assert_eq!(gaps[1], style.list_gap, "between the quote's own lines");
        assert_eq!(gaps[2], style.paragraph_gap, "below the quote");
    }

    /// A quote opening inside a quote keeps the bar unbroken.
    #[test]
    fn a_nested_quote_still_sits_tight() {
        let style = spaced_style();
        let gaps = gaps_of("> a\n>\n> > b\n\npara", &style);
        assert_eq!(gaps[0], Pixels::ZERO, "above the nested quote");
        assert_eq!(gaps[1], style.paragraph_gap, "leaving both quotes");
    }

    /// The source says whether a raw block holds a table or an HTML block; only
    /// those two ever reach one.
    #[test]
    fn a_raw_block_is_captioned_by_its_source() {
        assert_eq!(raw_block_label("| a | b |\n| - | - |"), "Table");
        assert_eq!(raw_block_label("\n\n  | a |"), "Table");
        assert_eq!(raw_block_label("<div>x</div>"), "HTML");
        assert_eq!(raw_block_label(""), "HTML");
    }

    /// A two-by-one RGBA PNG, so a decode can be checked against a known
    /// aspect ratio without shipping a fixture file.
    const PNG: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0xf4,
        0x22, 0x7f, 0x8a, 0x00, 0x00, 0x00, 0x0e, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8,
        0xcf, 0xc0, 0x00, 0x42, 0xff, 0x01, 0x0f, 0xf9, 0x03, 0xfd, 0x85, 0x11, 0x99, 0x76, 0x00,
        0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    /// Decoding runs on the layout pass, outside any window or app context, so
    /// it has to stand on its own. Only a file the note can actually read is
    /// drawn; everything else keeps the placeholder.
    #[test]
    fn a_local_file_is_decoded_and_fitted_to_the_column() {
        let path = std::env::temp_dir().join("markraft-surface-test.png");
        std::fs::write(&path, PNG).expect("a writable temp directory");
        let src = path.to_str().expect("a UTF-8 temp path");
        let (_, drawn) = drawn_image(src, px(100.)).expect("the PNG decodes");
        assert_eq!(drawn.width, px(2.), "a small image is not blown up");
        assert_eq!(drawn.height, px(1.));
        assert!(drawn_image("https://host/a.png", px(100.)).is_none());
        assert!(drawn_image("/no/such/file.png", px(100.)).is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn an_image_falls_back_to_the_name_of_its_file() {
        assert_eq!(file_name("images/shot.png"), Some("shot.png"));
        assert_eq!(file_name("/a/b/shot.png?v=2#x"), Some("shot.png"));
        assert_eq!(file_name("https://host/"), Some("host"));
        assert_eq!(file_name(""), None);
    }

    /// A pill needs far more room than the one character its atom is, so the
    /// display text holds a run of fillers in its place. Every geometry query
    /// still speaks in projection offsets, which the two maps have to preserve.
    #[test]
    fn a_pill_placeholder_stays_one_caret_stop() {
        let mut rows = rows_of("ab![alt](x.png)cd");
        let row = &mut rows[0];
        assert_eq!(row.char_len, 5, "the atom is one visible character");
        row.widenings = vec![Widening {
            source: 2,
            display: 2,
            len: 5,
        }];
        for offset in 0..=row.char_len {
            assert_eq!(row.to_source(row.to_display(offset)), offset, "at {offset}");
        }
        assert_eq!(row.to_display(2), 2, "the atom starts where its pill does");
        assert_eq!(row.to_display(3), 7, "and ends where its pill does");
        assert_eq!(row.to_source(3), 2, "the pill's left half is before it");
        assert_eq!(row.to_source(6), 3, "and its right half is after it");
        assert_eq!(row.to_display(5), 9, "text past the pill keeps its order");
    }

    /// A document whose paragraph holds a bare inline span: the CommonMark
    /// reader never builds one, because every span it makes carries the mark it
    /// stands for, so this is assembled by hand.
    fn doc_with_span(marks: MarkSet) -> (Schema, Node) {
        let schema = commonmark_schema();
        let span = schema
            .node("inline_span", [schema.text("Cmd")])
            .expect("text is valid span content")
            .mark(marks);
        let paragraph = schema
            .node("paragraph", [schema.text("a"), span])
            .expect("a span is valid paragraph content");
        let doc = schema.doc([paragraph]).expect("a valid document");
        (schema, doc)
    }

    fn hints_for(marks: impl Fn(&Schema) -> MarkSet) -> Vec<std::ops::Range<usize>> {
        let schema = commonmark_schema();
        let (schema, doc) = doc_with_span(marks(&schema));
        let projection = Projection::of(&doc, &schema);
        let types = DocTypes::from_schema_names(&schema, &commonmark_doc_type_names());
        let style = EditorStyle::notes();
        let input = ShapeInput {
            doc: &doc,
            types: &types,
            projection: &projection,
            style: &style,
            single_line: false,
        };
        hints_of(&input, &projection.lines()[0])
    }

    /// An inline span is the one construct a line can carry without showing
    /// anything for it — unless its own marks already say it is there.
    #[test]
    fn only_a_span_with_nothing_to_show_is_hinted() {
        assert_eq!(hints_for(|_| MarkSet::empty()), vec![1..4]);
        assert_eq!(
            hints_for(|schema| {
                let strong = schema.mark_id("strong").expect("the preset has strong");
                MarkSet::from_marks(schema, [Mark::new(strong)])
            }),
            Vec::<std::ops::Range<usize>>::new(),
            "a span that became bold already shows what it is"
        );
    }

    /// A table of two rows of two cells, as the measuring pass leaves them:
    /// projection lines with a natural width and nothing placed yet.
    fn table_cells(natural: Pixels) -> Vec<LayoutLine> {
        let mut cells = rows_of("| a | b |\n| - | - |\n| c | d |");
        assert_eq!(cells.len(), 4, "two rows of two cells");
        for cell in &mut cells {
            cell.width = natural;
        }
        cells
    }

    /// The table `table_cells` builds, placed on a 100 by 80 grid.
    fn placed_table(alignment: ColumnAlignment, natural: Pixels) -> Vec<LayoutLine> {
        let mut cells = table_cells(natural);
        let natural = vec![natural; cells.len()];
        place_table(
            &mut cells,
            6,
            &[(0, 0), (0, 1), (1, 0), (1, 1)],
            &[px(100.), px(80.)],
            &[px(30.), px(40.)],
            &natural,
            &[alignment; 2],
            px(4.),
            px(7.),
        );
        cells
    }

    /// What `prepaint` does with a shaped line list: stack the lines by their
    /// own gap and height, from `top`.
    fn stack(lines: &mut [LayoutLine], top: Pixels) {
        let mut y = top;
        for line in lines {
            y += line.top_gap;
            line.origin.y += y;
            y += line.height;
        }
    }

    /// A content-sized table is not stretched to the editor's width; one that
    /// does not fit gives up width in proportion to what each column asked for,
    /// and never goes below the minimum.
    #[test]
    fn a_table_keeps_its_content_width_until_the_grid_has_to_shrink() {
        let preferred = [px(100.), px(200.), px(100.)];
        assert_eq!(
            column_widths(&preferred, px(400.)),
            preferred.to_vec(),
            "room to spare leaves every column as it asked"
        );
        assert_eq!(
            column_widths(&preferred, px(200.)),
            vec![CELL_MIN_WIDTH, px(88.), CELL_MIN_WIDTH],
            "the two narrow columns stop at the floor and the wide one takes the rest"
        );
        // A grid that will not fit even at the floor overflows to the right
        // rather than squeezing its text away.
        let tight = [CELL_MIN_WIDTH; 3];
        assert_eq!(column_widths(&tight, px(100.)), tight.to_vec());
    }

    /// The grid is expressed in the terms `prepaint` stacks lines in: the cells
    /// of one row keep one y and only the last of them advances it, so the run
    /// of lines lands as a rectangle of boxes rather than a column of them.
    #[test]
    fn a_tables_cells_share_one_band_of_y_and_only_the_last_of_a_row_advances() {
        let mut cells = placed_table(ColumnAlignment::None, px(10.));
        assert!(cells.iter().all(|cell| cell.top_gap == Pixels::ZERO));
        assert!(cells.iter().all(|cell| cell.origin.y == CELL_PADDING_Y));
        assert_eq!(cells[0].height, Pixels::ZERO, "a cell mid-row advances not");
        assert_eq!(cells[1].height, px(30.), "the last of a row carries it");
        assert_eq!(cells[2].height, Pixels::ZERO);
        assert_eq!(
            cells[3].height,
            px(40.) + px(7.),
            "and the last of the table carries the gap below it too"
        );
        stack(&mut cells, px(0.));
        let boxes: Vec<Bounds<Pixels>> = cells
            .iter()
            .map(|cell| cell.cell_bounds().expect("a placed cell has a box"))
            .collect();
        assert_eq!(
            boxes[0],
            Bounds::new(point(px(4.), px(0.)), size(px(100.), px(30.)))
        );
        assert_eq!(
            boxes[1],
            Bounds::new(point(px(104.), px(0.)), size(px(80.), px(30.)))
        );
        assert_eq!(
            boxes[2],
            Bounds::new(point(px(4.), px(30.)), size(px(100.), px(40.)))
        );
        assert_eq!(
            boxes[3],
            Bounds::new(point(px(104.), px(30.)), size(px(80.), px(40.)))
        );
        // The text sits inside its own box, clear of the padding.
        assert_eq!(
            cells[2].origin,
            point(px(4.) + CELL_PADDING_X, px(30.) + CELL_PADDING_Y)
        );
    }

    /// An alignment moves the text inside the column and leaves the box where
    /// it is, so the grid stays rectangular whatever the columns declare.
    #[test]
    fn a_column_alignment_offsets_the_text_within_the_cell() {
        // The first column's content box is 100 less its two paddings.
        let content = px(100.) - CELL_PADDING_X * 2.;
        for (alignment, shift) in [
            (ColumnAlignment::None, px(0.)),
            (ColumnAlignment::Left, px(0.)),
            (ColumnAlignment::Center, (content - px(24.)) * 0.5),
            (ColumnAlignment::Right, content - px(24.)),
        ] {
            let cells = placed_table(alignment, px(24.));
            assert_eq!(
                cells[0].origin.x,
                px(4.) + CELL_PADDING_X + shift,
                "{alignment:?} starts the text here"
            );
            assert_eq!(
                cells[0].origin.x + cells[0].width,
                px(4.) + px(100.) - CELL_PADDING_X,
                "{alignment:?} still ends at the column's own edge"
            );
            assert_eq!(
                cells[0].cell_bounds().expect("a box").left(),
                px(4.),
                "{alignment:?} leaves the box alone"
            );
        }
        // Text too wide for its column has no slack to be offset by.
        let cells = placed_table(ColumnAlignment::Right, px(400.));
        assert_eq!(cells[0].origin.x, px(4.) + CELL_PADDING_X);
    }

    /// Walking the lines by y lands on whichever cell closes the row, so the
    /// grid has to say which column a point was in.
    #[test]
    fn a_point_over_a_grid_resolves_to_the_cell_it_is_in() {
        let mut cells = placed_table(ColumnAlignment::None, px(10.));
        stack(&mut cells, px(0.));
        let at = |x: f32, y: f32| {
            cell_under(&cells, 6, point(px(x), px(y)))
                .expect("a table has a nearest cell")
                .index
        };
        assert_eq!(at(20., 10.), 0);
        assert_eq!(at(120., 10.), 1);
        assert_eq!(at(20., 50.), 2);
        assert_eq!(at(120., 50.), 3);
        // Beside the grid, the nearest cell of the row the point is level with.
        assert_eq!(at(500., 50.), 3, "past the right edge");
        assert_eq!(at(-50., 50.), 2, "before the left edge");
        assert_eq!(at(120., -80.), 1, "above the grid");
        assert!(cell_under(&cells, 99, point(px(20.), px(10.))).is_none());
    }

    /// Every cell of a table row occupies the same band, so without merging
    /// them one press of Down would step through that row once per column.
    #[test]
    fn the_cells_of_one_row_offer_the_arrow_keys_a_single_visual_row() {
        let centers = merge_row_centers(vec![px(20.), px(8.), px(20.2), px(8.), px(40.)]);
        assert_eq!(centers, vec![px(8.), px(20.), px(40.)]);
        assert_eq!(merge_row_centers(Vec::new()), Vec::<Pixels>::new());
    }

    /// A table is one block: only the cell that closes the grid is spaced off
    /// what follows it, and the block before it opens the same gap any pair of
    /// blocks gets.
    #[test]
    fn a_table_is_one_block_whose_cells_sit_tight() {
        let style = spaced_style();
        let gaps = gaps_of("para\n\n| a | b |\n| - | - |\n| c | d |\n\npara", &style);
        assert_eq!(gaps[0], style.paragraph_gap, "above the table");
        assert_eq!(&gaps[1..4], [Pixels::ZERO; 3], "between its cells");
        assert_eq!(gaps[4], style.paragraph_gap, "below the table");
    }

    /// A layout left over from a document with fewer lines draws no caret at
    /// all rather than drawing it on whichever row happens to share an index.
    #[test]
    fn a_stale_layout_draws_no_caret_rather_than_the_wrong_one() {
        let rows = rows_of(SHAPE);
        let caret = rows.last().expect("a last row").to();
        let stale = &rows[..rows.len() - 5];
        assert!(
            stale.iter().all(|row| !row.contains(caret)),
            "a row that does not hold the caret never draws it"
        );
    }
}
