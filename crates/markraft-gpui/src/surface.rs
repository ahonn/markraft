//! Layout and painting, driven by the document's [`Projection`].
//!
//! One [`LayoutLine`] is laid out per projection [`Line`]: a textblock or a
//! block-level leaf. A line's text may hold newlines — a code block is one
//! textblock — so a line is shaped into one [`LayoutRow`] per newline-separated
//! row, each of which may wrap further.
//!
//! Every geometry query is in visible `char` offsets. The projection retained
//! by each layout maps document positions past hidden inline boundaries.
//!
//! HTML is never rendered or interpreted: a raw block and an inline HTML
//! primitive alike are drawn as the source they hold, in the code font, so a
//! note shows exactly what it will be written back as.

use crate::conceal::{Reveal, Shown};
use crate::style::EditorStyle;
use crate::types::DocTypes;
use crate::{CaretShape, EditorView};
use gpui::{prelude::*, *};
use markraft_core::commands::ColumnAlignment;
use markraft_core::projection::{Line, LineKind, Projection, Run, RunContent};
use markraft_core::{MarkSet, Node};
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

/// An inline atom the view draws itself is one character of the projection and
/// takes far more room than that character advances, so the display text holds
/// a run of [`PILL_FILLER`] in its place; see [`Widening`]. An image's stand-in
/// is a pill like inline code; an inline HTML primitive is drawn as the source
/// it stands for, because the view never renders HTML.
const PILL_SCALE: f32 = 0.82;
const PILL_PADDING: Pixels = px(7.);
const PILL_ICON: Pixels = px(13.);
const PILL_ICON_GAP: Pixels = px(5.);
/// No-break, so an atom never wraps in the middle of its own placeholder.
const PILL_FILLER: char = '\u{00a0}';
/// The widest a drawn atom may grow, as a share of the column.
const PILL_MAX_RATIO: f32 = 0.9;

/// An image the note can read off the disk is drawn for real, at the column's
/// width, on the line it has to itself. Everything else — a remote source, a
/// format no decoder handles, an image sharing its line with text — keeps the
/// placeholder pill.
const IMAGE_MAX_HEIGHT: Pixels = px(320.);
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
/// wide as its widest cell wants to be, never narrower than its own
/// min-content width, and the grid as a whole is content-sized rather than
/// stretched to the editor's width; a grid that still does not fit scrolls
/// sideways within the note.
const CELL_PADDING_X: Pixels = px(8.);
const CELL_PADDING_Y: Pixels = px(6.);
const CELL_MIN_WIDTH: Pixels = px(56.);
/// How much of one unbreakable unit a column reserves room for. Past this a
/// very long word — a bare URL, a path — is left to wrap at whatever
/// opportunities the wrapper does find inside it rather than widening the grid
/// without bound.
const CELL_MAX_MIN_CONTENT: Pixels = px(240.);
const TABLE_LINE: Pixels = px(1.);
/// How wide the fade at a clipped edge of a scrolling grid is.
const TABLE_FADE: Pixels = px(14.);
/// A quote bar, drawn beside a line and beside a whole grid alike.
const QUOTE_BAR: Pixels = px(2.);

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

/// What an inline atom is drawn as.
///
/// A [`AtomShape::Pill`] is chrome the row cannot shape, so it is painted over
/// a run of fillers reserving its slot. The other two *are* text, so the row
/// shapes their label itself: a placeholder rounded up to a whole number of
/// fillers would leave a gap after the label, and punctuation after a wiki link
/// has to sit where it would after any other word.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum AtomShape {
    /// An image's stand-in: a picture glyph and a label on a rounded fill.
    Pill,
    /// The source of an inline HTML primitive, shaped as the quiet code-font
    /// text it is — the view keeps HTML verbatim and never renders it.
    Source,
    /// A wiki link's label, shaped as the prose it stands in, in the link
    /// colour. Its brackets and its target are source the view does not show.
    Link,
}

impl AtomShape {
    /// What the shape draws around its label, which is all an atom reserves
    /// beyond the width of the text itself.
    fn chrome(self) -> Pixels {
        match self {
            AtomShape::Pill => PILL_PADDING * 2. + PILL_ICON + PILL_ICON_GAP,
            AtomShape::Source | AtomShape::Link => px(0.),
        }
    }

    /// Whether the row shapes the atom's own label in place of a placeholder,
    /// so it takes exactly the width its glyphs advance.
    fn is_own_text(self) -> bool {
        matches!(self, AtomShape::Source | AtomShape::Link)
    }
}

/// One painted inline atom, once shaping says which slot it landed in.
#[derive(Clone)]
struct InlineAtom {
    /// Relative to the line's origin.
    left: Pixels,
    slot: Pixels,
    /// Visual row within the whole block.
    visual_row: usize,
    label: Rc<ShapedLine>,
    /// A decoded image drawn in place of the pill, at the size it was measured
    /// for.
    image: Option<(Arc<RenderImage>, Size<Pixels>)>,
    /// The pill stands for a note rather than a picture, so it wears a page.
    note: bool,
}

/// Where a line's display text holds a different number of characters than the
/// projection does.
///
/// Every geometry query the view answers is in projection offsets, and the rows
/// are shaped from the display text, so the two spaces have to be mapped onto
/// each other. An atom *widens* one projected character into a label; a
/// concealed run — characters that spell rather than say, see
/// [`crate::conceal`] — *collapses* to nothing, or is *substituted* by what it
/// displays, which is as long as it happens to be.
#[derive(Clone, Copy)]
struct Widening {
    /// `char` offset of the remapped span within the projection line.
    source: usize,
    /// How many projection `char`s the span covers. Atoms are one; a
    /// concealed run is the length of that run.
    source_len: usize,
    /// `char` offset of the replacement within the display text.
    display: usize,
    /// How many `char`s the replacement takes. Zero when the span is hidden.
    len: usize,
    /// What an atom's placeholder holds, which is what says whether its own
    /// characters reach the screen. `None` for a concealed run, whose
    /// replacement is drawn as the text around it is.
    shape: Option<AtomShape>,
    /// A wiki link the host says it cannot open. Decided while the atom is shaped,
    /// because that is where the node is; read where the row's text is coloured,
    /// because a link's label is the row's own text rather than the atom's.
    broken: bool,
}

/// How many quote levels can carry a tone of their own; deeper ones fall back to
/// the ordinary bar.
const QUOTE_TONES: usize = 8;

#[derive(Clone, Copy)]
enum Decoration {
    /// One bar per block quote the line sits in; a level joins with the line below
    /// when that line sits in the same quote.
    Quote {
        levels: usize,
        joined: usize,
        /// The accent of each drawn level's callout, outermost first. `None` for
        /// an ordinary quote, and for levels past what the array holds.
        tones: [Option<Hsla>; QUOTE_TONES],
    },
    Divider,
    /// A code block's rounded background, behind the whole line.
    Code,
}

/// A callout's header: the line drawn above its first block, saying what kind
/// of note it is. It is chrome, not content — no caret ever lands in it.
#[derive(Clone)]
struct CalloutHeader {
    label: Rc<ShapedLine>,
    /// What the label says, for a reader that cannot see it.
    text: String,
}

/// The room a callout's header takes above the block it opens.
const CALLOUT_HEADER_HEIGHT: Pixels = px(22.);

#[derive(Clone)]
enum Marker {
    /// An ordered-list number, right-aligned against the text.
    Number(Rc<ShapedLine>),
    Bullet {
        depth: usize,
    },
    Task {
        checked: bool,
        number: Option<Rc<ShapedLine>>,
        /// Focused spelling `- [ ] ` / `- [x] `, drawn instead of the checkbox.
        source: Option<Rc<ShapedLine>>,
    },
    /// Markdown source prefix shown while a heading or list line is focused.
    Source(Rc<ShapedLine>),
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
    /// How many block quotes the grid sits in. A cell carries no decoration of
    /// its own, so the grid draws those bars for all of its cells at once.
    quotes: usize,
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
    /// The narrowest the content can be drawn at without splitting a unit the
    /// line wrapper keeps together. Measured on the table pass only, which is
    /// the one caller that has to know how far a box may shrink.
    pub(crate) min_width: Pixels,
    pub(crate) top_gap: Pixels,
    /// Position directly before a code block's node, for the host's language picker.
    pub(crate) code_pos: Option<usize>,
    marker: Option<Marker>,
    decoration: Option<Decoration>,
    /// The shaped language label of a code block's header.
    code_header: Option<Rc<ShapedLine>>,
    /// Closing fence line drawn under a focused code block.
    code_footer: Option<Rc<ShapedLine>>,
    /// The marker each quote level draws in the gutter while the line is
    /// focused, as the host's kind spells it.
    quote_marker: Option<Rc<ShapedLine>>,
    /// The shaped header of a callout, on the line that opens it.
    callout_header: Option<CalloutHeader>,
    code_hitboxes: Option<(Hitbox, Hitbox)>,
    /// Sorted by `source`; see [`Widening`].
    widenings: Vec<Widening>,
    atoms: Vec<InlineAtom>,
    /// Where the line sits in a table, when it is a cell of one.
    pub(crate) table: Option<TableCell>,
}

/// Host popovers also anchor document and node selections, whose opening token can
/// precede the first text row. A caret outside the layout still has no anchor.
pub(crate) fn selection_anchor_row(
    layout: &[LayoutLine],
    start: usize,
    end: usize,
) -> Option<&LayoutLine> {
    layout.iter().find(|row| row.contains(start)).or_else(|| {
        (start < end)
            .then(|| {
                layout
                    .iter()
                    .find(|row| row.from >= start && row.from < end)
            })
            .flatten()
    })
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
        let mut shift: isize = 0;
        for widening in &self.widenings {
            if offset <= widening.source {
                break;
            }
            if offset >= widening.source + widening.source_len {
                shift += widening.len as isize - widening.source_len as isize;
                continue;
            }
            // Inside a remapped span: a collapsed run parks at the display
            // point; an atom or a substituted run keeps its left edge.
            return widening.display;
        }
        offset
            .checked_add_signed(shift)
            .expect("display offset stays in range")
    }

    /// The projection `char` offset a display offset stands at. Inside a
    /// replacement the nearer of the span's two edges wins, so a click on the
    /// right half of an atom — or of the `&` an entity shows — puts the caret
    /// after the whole span. A collapsed span has no interior.
    fn to_source(&self, display: usize) -> usize {
        let mut shift: isize = 0;
        for widening in &self.widenings {
            let delta = widening.len as isize - widening.source_len as isize;
            if widening.len == 0 {
                if display > widening.display {
                    shift += delta;
                } else {
                    break;
                }
                continue;
            }
            if display >= widening.display + widening.len {
                shift += delta;
            } else if display > widening.display {
                let past = display - widening.display >= widening.len.div_ceil(2);
                return widening.source + if past { widening.source_len } else { 0 };
            } else {
                break;
            }
        }
        display
            .checked_add_signed(-shift)
            .expect("source offset stays in range")
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

    /// What a callout's header says and the box it is drawn in, relative to the
    /// line's own origin — available only on the line that opens one.
    /// Whether `y` falls in the band a callout's header is drawn in. The band is
    /// chrome above the line, so a click there means the start of the body rather
    /// than whichever character happens to sit under it.
    pub(crate) fn in_callout_header(&self, y: Pixels) -> bool {
        self.callout_header.is_some()
            && y >= self.origin.y - CALLOUT_HEADER_HEIGHT
            && y < self.origin.y
    }

    pub(crate) fn callout_header(&self) -> Option<(&str, Bounds<Pixels>)> {
        let header = self.callout_header.as_ref()?;
        Some((
            header.text.as_str(),
            Bounds::new(
                point(self.origin.x, self.origin.y - CALLOUT_HEADER_HEIGHT),
                size(header.label.width, CALLOUT_HEADER_HEIGHT),
            ),
        ))
    }

    /// Window-space bounds of the language button, available only on a code
    /// line that is not showing its fence spelling.
    pub(crate) fn code_language_bounds(&self) -> Option<Bounds<Pixels>> {
        // Focused fences put the open fence on the left; the language chip
        // stays off so a click does not open the picker over source text.
        if self.code_footer.is_some() {
            return None;
        }
        let label = self.code_header.as_ref()?;
        let label_width = label.width + CODE_CHEVRON_WIDTH + px(12.);
        Some(Bounds::new(
            point(
                self.origin.x + self.width - (CODE_COPY_WIDTH + px(2.)) - label_width,
                self.origin.y - CODE_HEADER_HEIGHT - CODE_HEADER_LIFT,
            ),
            size(label_width, CODE_HEADER_HEIGHT),
        ))
    }

    pub(crate) fn code_copy_bounds(&self) -> Option<Bounds<Pixels>> {
        self.code_header.as_ref()?;
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
            Marker::Number(line) | Marker::Source(line) => {
                (line.width + NUMBER_GAP, line.width, self.line_height)
            }
            Marker::Task {
                source: Some(line), ..
            } => (line.width + NUMBER_GAP, line.width, self.line_height),
            // Drawn markers share one center, 15px left of the text.
            Marker::Bullet { .. } => (px(17.5), px(5.), px(5.)),
            Marker::Task { .. } => (px(22.), px(14.), px(14.)),
        };
        Some(Bounds::new(
            self.origin + point(-offset, (self.line_height - height) * 0.5),
            size(width, height),
        ))
    }

    pub(crate) fn task_marker(&self) -> Option<(bool, Bounds<Pixels>)> {
        let Marker::Task { checked, .. } = self.marker.as_ref()? else {
            return None;
        };
        Some((*checked, self.marker_bounds()?))
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
    /// a placeholder reports nothing selectable.
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
    pub images: &'a crate::images::Images,
    pub doc: &'a Node,
    pub types: &'a DocTypes,
    pub projection: &'a Projection,
    pub style: &'a EditorStyle,
    pub single_line: bool,
    /// Whether a wiki link target names something the host can open. Only the host
    /// knows, and one that has not said treats every link as followable.
    pub wiki: Option<&'a crate::WikiResolver>,
    /// How the host's kind spells the parts of itself a focused line shows as
    /// source. Without it a line is drawn the same focused or not.
    pub spelling: Option<&'a dyn markraft_core::kind::SourceSpelling>,
    /// Document selection range, which is what reveals a syntax run. Inclusive
    /// of both ends in the ProseMirror sense (`from`..`to`).
    pub selection: Range<usize>,
    /// An input method's marked range, which reveals delimiters the way the
    /// selection does.
    pub composition: Option<Range<usize>>,
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

/// The document's rows, shaped again only when shaping would answer differently.
///
/// Every frame asks for these twice — once to measure the note's height, once to
/// draw it — and a redraw is usually about something shaping never reads: the
/// caret blinked, the selection moved, a popup opened above. Shaping walks every
/// line of the document and asks the platform to lay out its text, so repeating
/// it for those frames is the single largest avoidable cost in the view.
///
/// What it reads is the projection (which stands for the document), the width,
/// and the inputs [`Shaping`](crate::shaping::Shaping) holds. An image file that
/// changed under the editor reaches those through
/// [`EditorView::refresh_images`], which the host polls.
pub(crate) fn shape_cached(
    view: &crate::EditorView,
    width: Pixels,
    text_system: &WindowTextSystem,
) -> Vec<LayoutLine> {
    let projection = view.projection_arc();
    let input = view.shape_input();
    let reveal = reveal_key(&input);
    if let Some(lines) = view.shaping().rows(projection, width, reveal) {
        return lines;
    }
    let lines = shape(&input, width, text_system);
    view.shaping().keep(projection, width, reveal, &lines);
    lines
}

pub(crate) fn shape(
    input: &ShapeInput<'_>,
    width: Pixels,
    text_system: &WindowTextSystem,
) -> Vec<LayoutLine> {
    input.images.retain_sources(
        input
            .projection
            .lines()
            .iter()
            .flat_map(|line| &line.runs)
            .filter_map(|run| match &run.content {
                RunContent::Atom(node) => picture_source(input.types, node),
                _ => None,
            }),
    );
    let mut lines: Vec<LayoutLine> = (0..input.projection.line_count())
        .map(|index| {
            let cell = table_cell(input, index).map(|_| CellWidth::Natural);
            shape_line(input, index, width, cell, text_system)
        })
        .collect();
    shape_tables(input, &mut lines, width, text_system);
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
    text_system: &WindowTextSystem,
) -> LayoutLine {
    let ShapeInput {
        doc,
        types,
        projection,
        style,
        single_line,
        ..
    } = *input;
    let line = &projection.lines()[index];
    let heading = types.heading_level(line);
    let code = types.is_code_block(line);
    let font_size = style.font_size(heading, code);
    let max_indent = max_indent(style, width);
    let number = ordered_marker(doc, types, line, style, font_size, text_system);
    let focused = line_focused(input, line);
    let marker = chrome_marker(
        input,
        line,
        number.as_ref().map(|(shaped, _)| shaped.clone()),
        focused,
        font_size,
        text_system,
    );
    let decoration = decoration_of(input, index, line, cell.is_some(), max_indent);
    // Headings park ATX hashes in a dedicated gutter. List source spellings
    // reuse the ordered-number reserve so `- `, `1. `, and `- [ ] ` all fit.
    let heading_gutter = match &marker {
        Some(Marker::Source(label)) if heading.is_some() => label.width + NUMBER_GAP,
        _ => px(0.),
    };
    let marker_width = match &marker {
        Some(Marker::Source(label)) if heading.is_none() => Some(label.width),
        Some(Marker::Task {
            source: Some(label),
            ..
        }) => Some(label.width),
        _ => number.as_ref().map(|(_, w)| *w),
    };
    let indent = (indent_of(types, line, style, marker_width) + heading_gutter).min(max_indent);

    let wrap_width = match cell {
        Some(CellWidth::Column(content)) => content.max(px(16.)),
        // The measuring pass is unconstrained, but an atom still needs a nominal
        // column to size itself against.
        Some(CellWidth::Natural) => (width - indent).max(px(40.)),
        None => (width - indent - if code { CODE_PADDING } else { px(0.) }).max(px(40.)),
    };
    let unwrapped = single_line || cell == Some(CellWidth::Natural);
    let text = display_text(input, line, index, font_size, wrap_width, text_system);
    // A drawn image needs the whole row it was measured for.
    let line_height = text
        .line_height
        .unwrap_or(font_size * style.line_height_ratio);
    let runs = text_runs(input, line, &text, heading, code, font_size, style);
    let shaped = text_system
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

    let gap = gap_below(input, index, line, heading, code, &marker);
    // A code block's own top padding holds its header, at the top of the document
    // as anywhere else. Everything else starts flush and only a heading claims
    // space.
    let top_gap = if code {
        CODE_PADDING + CODE_HEADER_HEIGHT
    } else if index == 0 {
        px(0.)
    } else if let Some(level) = heading {
        style.heading_top_gap(level)
    } else {
        px(0.)
    };
    let fence = (code && focused)
        .then(|| {
            input
                .spelling
                .and_then(|spelling| spelling.verbatim_fence(line))
        })
        .flatten();
    let code_header = code.then(|| {
        let label = match &fence {
            Some((open, _)) => open.clone(),
            None => {
                crate::syntax::language_label(types.code_language(line).unwrap_or("")).to_owned()
            }
        };
        code_header(&label, width, style, text_system)
    });
    let code_footer = fence
        .as_ref()
        .map(|(_, close)| shape_source_label(close, font_size, style.muted_text, text_system));
    let quote_marker = (focused && matches!(decoration, Some(Decoration::Quote { .. })))
        .then(|| {
            let spelling = input.spelling?;
            let marker = spelling.container_marker(types.blockquote?)?;
            Some(shape_source_label(
                &marker,
                style.body_size,
                style.muted_text,
                text_system,
            ))
        })
        .flatten();
    // A callout says what kind of note it is on a line of its own above the
    // block it opens. The line is chrome: it holds no caret stop, so it lives
    // in the room the block reserves above itself rather than in the text.
    let above = index.checked_sub(1).map(|above| &projection.lines()[above]);
    let callout_header = crate::callout::header_of(types, line, above).map(|head| CalloutHeader {
        label: callout_label(
            &head.label,
            style.callout_tone(head.tone),
            style,
            text_system,
        ),
        text: head.label,
    });
    let top_gap = top_gap
        + if callout_header.is_some() {
            CALLOUT_HEADER_HEIGHT
        } else {
            px(0.)
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
        min_width: px(0.),
        top_gap,
        code_pos: code
            .then(|| line.ancestors.last().map(|a| a.before))
            .flatten(),
        marker,
        decoration,
        code_header,
        code_footer,
        quote_marker,
        callout_header,
        code_hitboxes: None,
        widenings: text.widenings,
        atoms: Vec::new(),
        table: None,
    };
    if single_line && let Some(row) = layout.rows.first() {
        layout.width = row.line.size(line_height).width.max(wrap_width);
    }
    if cell == Some(CellWidth::Natural) {
        // What the content wants, not what it was given: the grid reads each
        // column's preferred width off this, and how far the column may shrink
        // off the min-content width beside it.
        layout.width = layout
            .rows
            .iter()
            .map(|row| row.line.size(line_height).width)
            .fold(px(0.), |widest, width| widest.max(width));
        layout.min_width = min_content_width(&layout, &runs.code);
    }
    layout.height = layout.text_height() + gap;
    shape_inline_code(&mut layout, &text.text, &runs.code, font_size, text_system);
    place_atoms(&mut layout, text.atoms);
    layout
}

/// The decoration drawn behind or beside a line: a code block's panel, the rule
/// of a thematic break, or one bar per block quote the line sits in. A raw block
/// has none — its source is shown as source, not fenced off as a panel.
///
/// A cell carries no block decoration of its own: the grid is the table's, and a
/// cell's own height is zero except on the last of its row, which a quote bar or
/// a panel has no way to draw against.
fn decoration_of(
    input: &ShapeInput<'_>,
    index: usize,
    line: &Line,
    in_cell: bool,
    max_indent: Pixels,
) -> Option<Decoration> {
    let ShapeInput {
        types,
        projection,
        style,
        ..
    } = *input;
    let quote_levels = types.quote_depth(line);
    if in_cell {
        None
    } else if types.is_code_block(line) {
        Some(Decoration::Code)
    } else if types.horizontal_rule.is_some() && line.node_type() == types.horizontal_rule {
        Some(Decoration::Divider)
    } else if quote_levels > 0 {
        let levels = quote_levels.min(visible_levels(style, max_indent));
        // The bars drawn are the innermost `levels`, so the tones are too.
        let beside = crate::callout::tones_beside(types, line);
        let mut tones = [None; QUOTE_TONES];
        for (slot, tone) in tones
            .iter_mut()
            .zip(beside.iter().skip(beside.len().saturating_sub(levels)))
        {
            *slot = tone.map(|tone| style.callout_tone(tone));
        }
        Some(Decoration::Quote {
            levels,
            joined: joined_quote_levels(projection, index, types).min(quote_levels),
            tones,
        })
    } else {
        None
    }
}

/// The narrowest a line's content can be drawn at without splitting a unit the
/// line wrapper keeps together.
///
/// The line has to have been shaped unwrapped, which is what the measuring pass
/// gives a cell: every row then sits on one visual row, so the advance between
/// two byte offsets is the width of the text between them. A unit wider than
/// [`CELL_MAX_MIN_CONTENT`] only counts for that much, so one very long word
/// cannot widen a column without bound.
fn min_content_width(layout: &LayoutLine, code: &[(Range<usize>, Font, Hsla)]) -> Pixels {
    let mut widest = px(0.);
    for (index, row) in layout.rows.iter().enumerate() {
        let start = row_byte_start(layout, index);
        let text = row.text();
        // A code span is drawn as one pill; split over two rows of a cell it
        // reads as two spans, so it is held together as one unit.
        let glue: Vec<Range<usize>> = code
            .iter()
            .filter(|(range, _, _)| range.end > start && range.start < start + text.len())
            .map(|(range, _, _)| range.start.saturating_sub(start)..range.end - start)
            .collect();
        let x = |byte: usize| {
            row.line
                .position_for_index(byte, layout.line_height)
                .map_or(px(0.), |position| position.x)
        };
        for unit in unbreakable_units(text, &glue) {
            widest = widest.max((x(unit.end) - x(unit.start)).min(CELL_MAX_MIN_CONTENT));
        }
    }
    widest
}

/// The byte ranges of `text` the line wrapper never breaks inside, each with
/// its surrounding whitespace trimmed off.
///
/// A break opportunity opens before a word character that follows a space, and
/// before any character that is neither a space nor a word character — so CJK
/// text breaks between any two characters, and `/`, `?` and `&` break a path or
/// a query apart. An opportunity strictly inside a range of `glue` is ignored.
fn unbreakable_units(text: &str, glue: &[Range<usize>]) -> Vec<Range<usize>> {
    let mut starts = vec![0usize];
    let mut previous = '\0';
    let mut seen = false;
    for (byte, c) in text.char_indices() {
        let opportunity = if is_word_char(c) {
            previous == ' ' && seen
        } else {
            c != ' ' && seen
        };
        if opportunity
            && !glue
                .iter()
                .any(|range| range.start < byte && byte < range.end)
        {
            starts.push(byte);
        }
        seen |= c != ' ';
        previous = c;
    }
    starts
        .iter()
        .enumerate()
        .filter_map(|(index, &start)| {
            let end = starts.get(index + 1).copied().unwrap_or(text.len());
            let unit = &text[start..end];
            let lead = unit.len() - unit.trim_start().len();
            let trimmed = unit.trim();
            (!trimmed.is_empty()).then(|| start + lead..start + lead + trimmed.len())
        })
        .collect()
}

/// Whether the line wrapper treats `c` as part of a word, which is what decides
/// where a cell's text may break.
///
/// Mirrors gpui's own `LineWrapper::is_word_char`, which is not public: Latin,
/// Cyrillic, Vietnamese and Bengali letters, digits, the punctuation that binds
/// to a word and the closing punctuation that never starts a line. Everything
/// else — CJK above all — is a break opportunity of its own.
fn is_word_char(c: char) -> bool {
    // The punctuation that binds to a word — `a-b`, `var_name`, `3.14`,
    // `Self::new` — together with the closing marks that never start a line and
    // the glue characters that never break at all.
    const BINDING: &str = "-_.'\u{2019}\u{2018}$%@#^~,=:;!)]}\"\u{201d}\u{00bb}\u{2026}\u{22ef}\u{202f}\u{00a0}\u{2011}";
    // Latin-1 Supplement through Latin Extended-B, combining diacritics,
    // Cyrillic, Bengali, and Latin Extended Additional for Vietnamese.
    const SCRIPTS: [std::ops::RangeInclusive<char>; 5] = [
        '\u{00c0}'..='\u{024f}',
        '\u{0300}'..='\u{036f}',
        '\u{0400}'..='\u{04ff}',
        '\u{0980}'..='\u{09ff}',
        '\u{1e00}'..='\u{1eff}',
    ];
    c.is_ascii_alphanumeric()
        || BINDING.contains(c)
        || SCRIPTS.iter().any(|range| range.contains(&c))
}

/// Lay every table out as a grid.
///
/// Each cell has already been shaped unwrapped, so its `width` is what its
/// content wants. Consecutive lines naming the same table form one grid: its
/// columns are sized from those widths, each cell is reshaped inside the column
/// it was given, and the cells of one row are placed on one band of y.
fn shape_tables(
    input: &ShapeInput<'_>,
    lines: &mut [LayoutLine],
    width: Pixels,
    text_system: &WindowTextSystem,
) {
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
        shape_table(input, table, &mut lines[start..end], width, text_system);
        start = end;
    }
}

/// Size, reshape and place the cells of one table.
fn shape_table(
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
fn column_demands(
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
/// stays two words wide, as it does in Bear, rather than being blown up to the
/// column the way Typora does it. Only when they do not fit are the columns
/// shrunk, proportionally to what they asked for and never below their own
/// entry in `minimum`, which is the widest unbreakable unit the column holds:
/// shrinking may wrap a cell's text, never split a word. A grid that will not
/// fit even at those minimums keeps its width and scrolls sideways inside the
/// note.
fn column_widths(preferred: &[Pixels], minimum: &[Pixels], available: Pixels) -> Vec<Pixels> {
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
fn place_atoms(layout: &mut LayoutLine, pending: Vec<PendingAtom>) {
    for atom in pending {
        let Some(slot) = layout
            .display_rectangles(atom.chars.clone(), false)
            .into_iter()
            .next()
        else {
            continue;
        };
        layout.atoms.push(InlineAtom {
            left: slot.origin.x - layout.origin.x,
            slot: slot.size.width,
            visual_row: ((slot.origin.y - layout.origin.y) / layout.line_height).round() as usize,
            label: atom.label,
            image: atom.image,
            note: atom.note,
        });
    }
}

/// A line's display text, and whether it stands in for content the caret cannot
/// enter.
struct DisplayText {
    text: String,
    /// True when the text is a stand-in — a placeholder space for an empty line
    /// — so no run may be derived from the content.
    synthetic: bool,
    /// The display byte length of each of the line's projection runs, which is
    /// the run's own length except where an atom's placeholder widened it.
    run_bytes: Vec<usize>,
    widenings: Vec<Widening>,
    atoms: Vec<PendingAtom>,
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
            atoms: Vec::new(),
            line_height: None,
        }
    }
}

/// A painted atom the display text has reserved room for, before shaping says
/// where on the block it landed.
struct PendingAtom {
    /// `char` range of the placeholder within the display text.
    chars: Range<usize>,
    label: Rc<ShapedLine>,
    image: Option<(Arc<RenderImage>, Size<Pixels>)>,
    note: bool,
}

fn display_text(
    input: &ShapeInput<'_>,
    line: &Line,
    index: usize,
    font_size: Pixels,
    column: Pixels,
    text_system: &WindowTextSystem,
) -> DisplayText {
    let projection = input.projection;
    if line.kind == LineKind::LeafBlock {
        return DisplayText::stand_in(" ".to_owned());
    }
    let source = projection.line_text(index).unwrap_or_default();
    if source.is_empty() {
        return DisplayText::stand_in(" ".to_owned());
    }
    // An image that has its line to itself is drawn at full size; one sharing
    // its line with text has to stay within a row.
    let alone = line.runs.len() == 1 && line.len() == 1;
    let shown = crate::conceal::shown(input.types.syntax, line, &reveal_of(input));
    let mut text = String::with_capacity(source.len());
    let mut run_bytes = Vec::with_capacity(line.runs.len());
    let mut widenings = Vec::new();
    let mut atoms = Vec::new();
    let mut byte = 0usize;
    let mut display = 0usize;
    let mut filler: Option<Pixels> = None;
    let mut line_height = None;
    for (index_in_line, run) in line.runs.iter().enumerate() {
        let chars = run.char_to - run.char_from;
        let len: usize = source[byte..].chars().take(chars).map(char::len_utf8).sum();
        let slice = &source[byte..byte + len];
        byte += len;
        match atom_of(input, run, font_size, column, alone, text_system) {
            Some(atom) => {
                // An atom the row can shape *is* its text: writing the label
                // into the display text gives it exactly the width its glyphs
                // advance, so what follows sits against it. A placeholder
                // rounded up to whole fillers would leave a gap behind.
                let count = if atom.shape.is_own_text() && !atom.text.is_empty() {
                    text.push_str(&atom.text);
                    run_bytes.push(atom.text.len());
                    atom.text.chars().count()
                } else {
                    // One atom is one character, and a pill advances nowhere
                    // near far enough for what is drawn over it, so the row
                    // reserves the width in fillers. An atom with nothing to
                    // shape keeps one, which is its caret stop.
                    let unit = *filler.get_or_insert_with(|| filler_width(font_size, text_system));
                    let count = (atom.width / unit).ceil().max(1.) as usize;
                    text.extend(std::iter::repeat_n(PILL_FILLER, count));
                    run_bytes.push(count * PILL_FILLER.len_utf8());
                    if let Some((_, size)) = &atom.image {
                        line_height = Some(size.height);
                    }
                    atoms.push(PendingAtom {
                        chars: display..display + count,
                        note: atom.note,
                        label: atom.label,
                        image: atom.image,
                    });
                    count
                };
                widenings.push(Widening {
                    source: run.char_from,
                    source_len: 1,
                    display,
                    len: count,
                    shape: Some(atom.shape),
                    broken: atom.broken,
                });
                display += count;
            }
            None => match shown.get(index_in_line).copied().unwrap_or(Shown::Source) {
                Shown::Hidden => {
                    widenings.push(Widening {
                        source: run.char_from,
                        source_len: chars,
                        display,
                        len: 0,
                        shape: None,
                        broken: false,
                    });
                    run_bytes.push(0);
                }
                Shown::Display(shows) => {
                    let count = shows.chars().count();
                    text.push_str(shows);
                    run_bytes.push(shows.len());
                    widenings.push(Widening {
                        source: run.char_from,
                        source_len: chars,
                        display,
                        len: count,
                        shape: None,
                        broken: false,
                    });
                    display += count;
                }
                Shown::Source | Shown::Revealed => {
                    text.push_str(slice);
                    run_bytes.push(len);
                    display += chars;
                }
            },
        }
    }
    DisplayText {
        text,
        synthetic: false,
        run_bytes,
        widenings,
        atoms,
        line_height,
    }
}

/// What reveals a concealed span while this input is shaped.
fn reveal_of(input: &ShapeInput<'_>) -> Reveal {
    Reveal::at(input.selection.clone(), input.composition.clone())
}

/// Which concealed spans and atoms stand revealed, as the shaping cache's own
/// key.
///
/// Shaping is the view's largest cost and it now depends on where the caret
/// stands, but almost every caret move leaves every span exactly as it was.
/// Walking the runs is orders of magnitude cheaper than laying the text out
/// again, so the rows are kept until the *revealed set* changes rather than
/// until the selection does.
pub(crate) fn reveal_key(input: &ShapeInput<'_>) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let reveal = reveal_of(input);
    for line in input.projection.lines() {
        // Only a line the selection or the marked text reaches can have
        // anything revealed on it.
        if reveal.touches(line.from, line.to) {
            let shown = crate::conceal::shown(input.types.syntax, line, &reveal);
            for (run, shown) in line.runs.iter().zip(shown) {
                if shown == Shown::Revealed {
                    run.from.hash(&mut hasher);
                }
                if matches!(run.content, RunContent::Atom(_)) && reveal.touches(run.from, run.to) {
                    run.from.hash(&mut hasher);
                    1u8.hash(&mut hasher);
                }
            }
        }
        // Any line that draws differently while it has the caret: a heading's
        // hashes, a list marker's spelling, a fence, a quote's marker. Asking
        // the kind only for the lines the caret actually touches keeps this to
        // a couple of calls per move.
        if affinity_touches(input, line.from, line.to) && focus_chrome(input, line) {
            line.from.hash(&mut hasher);
            2u8.hash(&mut hasher);
        }
    }
    hasher.finish()
}

/// The advance of one [`PILL_FILLER`], which is the unit a placeholder reserves
/// its width in.
fn filler_width(font_size: Pixels, text_system: &WindowTextSystem) -> Pixels {
    const SAMPLE: usize = 8;
    let text: String = std::iter::repeat_n(PILL_FILLER, SAMPLE).collect();
    let shaped = text_system.shape_line(
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

/// An inline atom's content and the width it needs.
struct Atom {
    shape: AtomShape,
    /// The label as it reaches the screen, shortened where it would not fit the
    /// column. The row shapes this itself where the shape is its own text.
    text: String,
    label: Rc<ShapedLine>,
    width: Pixels,
    image: Option<(Arc<RenderImage>, Size<Pixels>)>,
    /// A wiki link leading nowhere; see [`Widening::broken`].
    broken: bool,
    /// An `![[…]]` whose target is a note in the host's index rather than a file
    /// beside it. The note is not unfolded here, so the pill stands for it.
    note: bool,
}

/// A node's attribute, trimmed, or the empty string where it has none.
fn attr<'a>(node: &'a Node, name: &str) -> &'a str {
    node.attrs()
        .get(name)
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .trim()
}

/// What an inline atom is drawn as, for the atoms the view draws itself: an
/// image's label, the verbatim source of an inline HTML primitive, or a wiki
/// link's label. Every other atom keeps the object-replacement character the
/// projection gave it, which is blank.
fn atom_label<'a>(types: &DocTypes, node: &'a Node) -> Option<(AtomShape, &'a str)> {
    let ty = node.type_id();
    if Some(ty) == types.image {
        let label = match (attr(node, "alt"), file_name(attr(node, "src"))) {
            ("", Some(name)) => name,
            ("", None) => "Image",
            (alt, _) => alt,
        };
        Some((AtomShape::Pill, label))
    } else if Some(ty) == types.raw_inline {
        // HTML is kept verbatim and shown as source, so the tag reads exactly as
        // it was written — a closing tag included.
        Some((AtomShape::Source, attr(node, "source")))
    } else if Some(ty) == types.wiki_link {
        if crate::wiki::wiki_link_embed(node) {
            // `![[…]]` puts a file in the note rather than pointing at a page, so
            // it reads as the picture it is: its alias, or the file it names.
            let label = match (attr(node, "alias"), file_name(attr(node, "target"))) {
                ("", Some(name)) => name,
                ("", None) => "Embed",
                (alias, _) => alias,
            };
            return Some((AtomShape::Pill, label));
        }
        // The alias is what the author wrote it to read as; without one the
        // target stands in, with whatever `#heading` or `^block` it names,
        // because that is what the link says.
        Some((AtomShape::Link, crate::wiki::wiki_link_label(node)))
    } else {
        None
    }
}

/// The file an atom draws a picture of, where it draws one. `![](path)` and
/// `![[path]]` are the same picture written two ways, so the view loads, measures and
/// draws them alike; only the syntax they were written in differs.
fn picture_source<'a>(types: &DocTypes, node: &'a Node) -> Option<&'a str> {
    let ty = node.type_id();
    if Some(ty) == types.image {
        Some(attr(node, "src"))
    } else if Some(ty) == types.wiki_link && crate::wiki::wiki_link_embed(node) {
        Some(attr(node, "target"))
    } else {
        None
    }
}

/// The atom an inline run is drawn as, shaped and measured.
fn atom_of(
    input: &ShapeInput<'_>,
    run: &Run,
    font_size: Pixels,
    column: Pixels,
    alone: bool,
    text_system: &WindowTextSystem,
) -> Option<Atom> {
    let RunContent::Atom(node) = &run.content else {
        return None;
    };
    let ShapeInput {
        types,
        style,
        images,
        wiki,
        ..
    } = *input;
    let revealed = {
        let touches = |range: &Range<usize>| {
            if range.start == range.end {
                range.start >= run.from && range.start <= run.to
            } else {
                range.start < run.to && range.end > run.from
            }
        };
        touches(&input.selection) || input.composition.as_ref().is_some_and(touches)
    };
    // An atom under the caret shows the source it was read from, which only the
    // host's kind can spell — it is the same text a save writes.
    let source = revealed
        .then(|| {
            input
                .spelling
                .and_then(|spelling| spelling.atom_source(node))
        })
        .flatten();
    let (shape, original) = match source {
        Some(source) => (AtomShape::Source, source),
        None => {
            let (shape, label) = atom_label(types, node)?;
            (shape, label.to_owned())
        }
    };
    // A picture stands for the atom; where the atom is showing its source there
    // is nothing to stand for.
    let picture = match shape {
        AtomShape::Source => None,
        _ => picture_source(types, node),
    };
    // `![[…]]` is written for a file, but a note answers to the same spelling and
    // the host's index is what knows which this is. Reporting a missing picture
    // for a note that is plainly there tells the reader something untrue.
    let embed = Some(node.type_id()) == types.wiki_link && crate::wiki::wiki_link_embed(node);
    let note = embed && wiki.is_some_and(|resolves| resolves(attr(node, "target")));
    let text = match picture {
        Some(_) if note => format!("Embedded note: {original}"),
        Some(source) => match images.load(source) {
            // `![[…]]` never said the target was a picture, so when nothing of
            // that name is there, neither did we.
            Err(crate::images::ImageError::Missing) if embed => {
                format!("Embed not found: {original}")
            }
            Err(error) => format!("{}: {original}", error.label()),
            Ok(_) if !alone => format!("Inline image: {original}"),
            Ok(_) => original.to_owned(),
        },
        None => original,
    };
    // A local file the note can read is drawn for real where it has the line to
    // itself; the placeholder is still built, and stands in wherever it is not.
    let drawn = (alone && !note)
        .then_some(picture)
        .flatten()
        .and_then(|source| drawn_image(images, source, column));
    // A pill's label is smaller than the text around it, as inline code is;
    // source text and a wiki link's label sit in the sentence at the
    // sentence's own size, the one in the code font it is and the other in the
    // link colour, which is what says it can be followed.
    let (face, size) = match shape {
        AtomShape::Pill => (font(".SystemUIFont"), font_size * PILL_SCALE),
        AtomShape::Source => (font(CODE_FONT), font_size),
        AtomShape::Link => (font(".SystemUIFont"), font_size),
    };
    // A link the host says it cannot open is still drawn as a link, because that is
    // what the source says it is — but not in the colour that invites a click, since
    // clicking it only reports that there is nothing there. A link's label is the
    // row's own text, so the colour is applied where the row is coloured; this only
    // decides it, while the node is at hand.
    let broken = shape == AtomShape::Link
        && wiki.is_some_and(|resolves| !resolves(&crate::wiki::wiki_link_target(node)));
    let ink = match shape {
        AtomShape::Link if broken => style.broken_link,
        AtomShape::Link => style.link,
        AtomShape::Pill | AtomShape::Source => style.muted_text,
    };
    let room = (column * PILL_MAX_RATIO - shape.chrome()).max(px(16.));
    let shaped = |label: String| {
        Rc::new(text_system.shape_line(
            label.clone().into(),
            size,
            &[TextRun {
                len: label.len(),
                font: face.clone(),
                color: ink,
                background_color: None,
                underline: None,
                strikethrough: None,
            }],
            None,
        ))
    };
    let mut graphemes = text.graphemes(true).collect::<Vec<_>>();
    let mut shown = graphemes.concat();
    let mut label = shaped(shown.clone());
    // A label has to stay within the column. A pill's placeholder is one
    // unbreakable run, so nothing downstream could shorten it; a label the row
    // shapes itself could wrap, but a target or a tag long enough to need it is
    // better read short than spread over three lines.
    while label.width > room && graphemes.len() > 1 {
        graphemes.pop();
        shown = format!("{}…", graphemes.concat());
        label = shaped(shown.clone());
    }
    Some(Atom {
        width: match &drawn {
            Some((_, size)) => size.width,
            None => label.width + shape.chrome(),
        },
        shape,
        text: shown,
        label,
        image: drawn,
        broken,
        note,
    })
}

/// A decoded local image and the size it is drawn at: the column's width, or
/// the image's own where that is narrower, capped at [`IMAGE_MAX_HEIGHT`].
fn drawn_image(
    images: &crate::images::Images,
    src: &str,
    column: Pixels,
) -> Option<(Arc<RenderImage>, Size<Pixels>)> {
    let image = images.load(src).ok()?;
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
    // A raw block's source is shown as source: monospaced and quiet, so it reads
    // as the markup it is rather than as prose. It carries no chrome, so the
    // face and the colour are all that say so.
    let raw = types.is_raw_block(line);
    let text_color = if checked_item || raw {
        style.muted_text
    } else {
        style.text
    };
    // A ticked item is greyed and struck through as a whole, but an atom and a
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
        if raw {
            face = font(CODE_FONT);
        }
        return Runs {
            runs: vec![TextRun {
                len: text.text.len(),
                font: face,
                color: text_color,
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
        let is_code = code_block || raw || has(types.code, marks);
        let is_link = has(types.link, marks);
        let mut face = font(if is_code { CODE_FONT } else { ".SystemUIFont" });
        if has(types.strong, marks) || heading.is_some() || header {
            face.weight = FontWeight::BOLD;
        }
        if has(types.em, marks) {
            face.style = FontStyle::Italic;
        }
        let atom = matches!(run.content, RunContent::Atom(_));
        // What an atom's placeholder holds: a pill is painted over its fillers,
        // and the other shapes are the row's own text. A wiki link's label is
        // prose, so it keeps the face this run already decided on — the
        // heading's weight, the emphasis around it — in the link colour an atom
        // draws in anyway; an HTML primitive is source, and reads as the quiet
        // monospaced markup it is wherever it sits.
        let widening = text
            .widenings
            .iter()
            .find(|widening| widening.source == run.char_from);
        let placeholder = widening.and_then(|widening| widening.shape);
        let source_atom = placeholder == Some(AtomShape::Source);
        if source_atom {
            face = font(CODE_FONT);
        }
        let widened = placeholder == Some(AtomShape::Pill);
        let ink = if source_atom {
            style.muted_text
        } else if widening.is_some_and(|widening| widening.broken) {
            style.broken_link
        } else if is_link || (atom && !code_block) {
            style.link
        } else if has(types.code, marks) {
            style.inline_code_text
        } else {
            text_color
        };
        // Inline code only reserves its space here; see `InlineCode`.
        let color = if widened {
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
            underline: (!widened && (has(types.underline, marks) || is_link)).then_some(
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
    // A revealed syntax run is a run of its own; without merging, each
    // backtick would paint a separate pill and the background would break.
    Runs {
        runs,
        code: merge_adjacent_code(code),
    }
}

/// Join contiguous inline-code byte ranges into one pill.
fn merge_adjacent_code(code: Vec<(Range<usize>, Font, Hsla)>) -> Vec<(Range<usize>, Font, Hsla)> {
    let mut merged: Vec<(Range<usize>, Font, Hsla)> = Vec::with_capacity(code.len());
    for (range, face, ink) in code {
        match merged.last_mut() {
            Some((last, _, last_ink)) if last.end == range.start && *last_ink == ink => {
                last.end = range.end;
            }
            _ => merged.push((range, face, ink)),
        }
    }
    merged
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

/// How far a line may be indented before its text would be squeezed away.
///
/// Keeps deeply nested imported content editable in a narrow note. Only the
/// visual indentation is capped; document depth is preserved.
fn max_indent(style: &EditorStyle, width: Pixels) -> Pixels {
    (width - px(80.)).max(style.quote_indent)
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
    for (index, ancestor) in line.ancestors.iter().enumerate() {
        let ty = ancestor.node_type;
        if Some(ty) == types.blockquote {
            indent += style.quote_indent;
        } else if types.is_list(ty) {
            indent += style.list_indent;
        } else if Some(ty) == types.task_item
            && index > 0
            && Some(line.ancestors[index - 1].node_type) == types.ordered_list
        {
            // Nested blocks and lists keep the checkbox slot of every enclosing item.
            indent += px(22.);
        } else if Some(ty) == types.code_block {
            indent += CODE_PADDING;
        }
    }
    if let Some(width) = number_width {
        // An ordered list's items share the indent its widest number needs.
        indent += (width + NUMBER_GAP - style.list_indent).max(px(0.));
    }
    indent
}

/// The shaped ordinal and width every line of an ordered-list item reserves.
/// Only its first line paints the ordinal, but continuation lines align with it.
fn ordered_marker(
    doc: &Node,
    types: &DocTypes,
    line: &Line,
    style: &EditorStyle,
    font_size: Pixels,
    text_system: &WindowTextSystem,
) -> Option<(Rc<ShapedLine>, Pixels)> {
    let (item, list) = types.item_of(line)?;
    if Some(list.node_type) != types.ordered_list {
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
        Rc::new(text_system.shape_line(
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

/// Whether a line is its list item's very first line, which is where the marker
/// is drawn.
///
/// Every ancestor between the item and the line has to be the first child of
/// the one above it, not just the line's own block: a table or a quote inside an
/// item is a container whose own first block starts it, and asking only about
/// the immediate parent drew a bullet beside every row of such a table and a
/// second check box beside such a quote.
fn starts_item(types: &DocTypes, line: &Line) -> bool {
    let Some(item) = line
        .ancestors
        .iter()
        .rposition(|ancestor| types.is_item(ancestor.node_type))
    else {
        return false;
    };
    item + 1 < line.ancestors.len()
        && line.ancestors[item + 1..]
            .iter()
            .all(|ancestor| ancestor.index == 0)
}

/// Whether this line has anything to draw differently while it is focused.
///
/// A line with no source spelling of its own — an ordinary paragraph — looks the
/// same either way, so the caret passing through it does not invalidate the
/// shaped rows.
fn focus_chrome(input: &ShapeInput<'_>, line: &Line) -> bool {
    let quoted = input.types.blockquote.is_some_and(|quote| {
        line.ancestors
            .iter()
            .any(|ancestor| ancestor.node_type == quote)
    });
    quoted
        || input.spelling.is_some_and(|spelling| {
            spelling.line_prefix(line).is_some() || spelling.verbatim_fence(line).is_some()
        })
}

/// Whether the selection or composition touches this projection line.
fn line_focused(input: &ShapeInput<'_>, line: &Line) -> bool {
    affinity_touches(input, line.from, line.to)
}

fn affinity_touches(input: &ShapeInput<'_>, from: usize, to: usize) -> bool {
    let touches = |range: &Range<usize>| {
        if range.start == range.end {
            range.start >= from && range.start <= to
        } else {
            range.start < to && range.end > from
        }
    };
    touches(&input.selection) || input.composition.as_ref().is_some_and(touches)
}

fn shape_source_label(
    text: &str,
    font_size: Pixels,
    color: Hsla,
    text_system: &WindowTextSystem,
) -> Rc<ShapedLine> {
    Rc::new(text_system.shape_line(
        text.to_owned().into(),
        font_size,
        &[TextRun {
            len: text.len(),
            font: font(CODE_FONT),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        }],
        None,
    ))
}

/// The gutter marker a line draws: its source spelling while the caret is on
/// it, and the rendered bullet or ordinal otherwise.
///
/// What the source *is* comes from the host's kind, not from here — a view that
/// spelled `##` or `- [x] ` itself would only be able to draw one kind of
/// document, and would have to keep its escaping in step with the codec's.
fn chrome_marker(
    input: &ShapeInput<'_>,
    line: &Line,
    number: Option<Rc<ShapedLine>>,
    focused: bool,
    font_size: Pixels,
    text_system: &WindowTextSystem,
) -> Option<Marker> {
    let types = input.types;
    let style = input.style;
    let source = focused
        .then(|| {
            input
                .spelling
                .and_then(|spelling| spelling.line_prefix(line))
        })
        .flatten()
        .map(|text| shape_source_label(&text, font_size, style.muted_text, text_system));
    if types.heading_level(line).is_some() {
        // A heading has nothing to draw when it is not showing its hashes.
        return source.map(Marker::Source);
    }
    let (item, _) = types.item_of(line)?;
    if !starts_item(types, line) {
        return None;
    }
    match source {
        Some(label) if Some(item.node_type) == types.task_item => Some(Marker::Task {
            checked: DocTypes::task_checked(&item.attrs),
            number: None,
            source: Some(label),
        }),
        Some(label) => Some(Marker::Source(label)),
        None => chrome_marker_unfocused(types, line, number),
    }
}

fn chrome_marker_unfocused(
    types: &DocTypes,
    line: &Line,
    number: Option<Rc<ShapedLine>>,
) -> Option<Marker> {
    let (item, list) = types.item_of(line)?;
    if !starts_item(types, line) {
        return None;
    }
    if Some(item.node_type) == types.task_item {
        return Some(Marker::Task {
            checked: DocTypes::task_checked(&item.attrs),
            number,
            source: None,
        });
    }
    if let Some(number) = number {
        return Some(Marker::Number(number));
    }
    if Some(list.node_type) != types.bullet_list {
        return None;
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

/// The shaped language label of a code block's header, trimmed to the room the
/// picker and the copy button leave it.
/// A callout's header label, shaped in the accent of its tone: the sentence
/// face at the body size, in the weight that says it names the note rather
/// than being part of it.
fn callout_label(
    label: &str,
    tone: Hsla,
    style: &EditorStyle,
    text_system: &WindowTextSystem,
) -> Rc<ShapedLine> {
    let mut face = font(".SystemUIFont");
    face.weight = FontWeight::BOLD;
    let text: SharedString = label.to_owned().into();
    Rc::new(text_system.shape_line(
        text.clone(),
        style.body_size,
        &[TextRun {
            len: text.len(),
            font: face,
            color: tone,
            background_color: None,
            underline: None,
            strikethrough: None,
        }],
        None,
    ))
}

fn code_header(
    label: &str,
    width: Pixels,
    style: &EditorStyle,
    text_system: &WindowTextSystem,
) -> Rc<ShapedLine> {
    let shape_label = |label: String| {
        Rc::new(text_system.shape_line(
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
    let language_width = (width - CODE_COPY_WIDTH - CODE_CHEVRON_WIDTH - px(30.)).max(px(20.));
    let mut label = label.graphemes(true).take(64).collect::<Vec<_>>();
    let mut shaped = shape_label(label.concat());
    while shaped.width > language_width && !label.is_empty() {
        label.pop();
        shaped = shape_label(format!("{}…", label.concat()));
    }
    shaped
}

/// Shape the smaller text drawn inside each inline-code pill, and record the
/// slots it sits in.
fn shape_inline_code(
    layout: &mut LayoutLine,
    text: &str,
    code_ranges: &[(Range<usize>, Font, Hsla)],
    font_size: Pixels,
    text_system: &WindowTextSystem,
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
            let line = text_system.shape_line(
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
                let rows = shape_cached(view, width, window.text_system());
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
        let mut rows = shape_cached(view, bounds.size.width, window.text_system());
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
        // Where each grid stands before it is moved: a grid that does not fit
        // keeps whatever the reader scrolled it to, clamped to what is left of
        // it, and one this frame does not draw loses its entry.
        let overflows = table_overflows(&rows, bounds);
        let mut scroll = view.tables.clone();
        scroll.retain(|table, _| overflows.contains_key(table));
        for (table, overflow) in &overflows {
            let entry = scroll.entry(*table).or_default();
            entry.overflow = *overflow;
            entry.offset = entry.offset.clamp(px(0.), *overflow);
        }
        // The caret's own cell is brought into the visible strip, as the
        // vertical reveal below brings its line into the viewport. Nothing is
        // animated either way, so reduced motion has nothing to turn off.
        if view.reveal
            && !view.single_line
            && let Some((cell, box_)) = rows
                .iter()
                .find(|row| row.contains(head))
                .and_then(|row| Some((row.table?, row.cell_bounds()?)))
            && let Some(grid) = scroll.get_mut(&cell.table)
        {
            grid.offset = reveal_offset(box_, bounds, grid.offset, grid.overflow);
        }
        // Every consumer uses these translated rows: paint, hit testing,
        // selection, input-method rectangles, and accessible text bounds.
        for row in &mut rows {
            row.origin.x -= single_line_scroll_x;
            if let Some(cell) = row.table
                && let Some(entry) = scroll.get(&cell.table)
            {
                row.origin.x -= entry.offset;
            }
            if let Some(copy) = row.code_copy_bounds() {
                let language = row
                    .code_language_bounds()
                    .unwrap_or_else(|| Bounds::from_corners(copy.origin, copy.origin));
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
                &view.types,
                &rows,
                window.scale_factor(),
            );
        }
        self.editor.update(cx, |editor, cx| {
            editor.single_line_scroll_x = single_line_scroll_x;
            editor.tables = scroll;
            editor.content_bounds = bounds;
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
                    let top = viewport.top() + editor.style().top_overlay;
                    let bottom = viewport.bottom() - editor.style().bottom_overlay;
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
        if window.is_a11y_active() {
            editor
                .accessible_text
                .borrow()
                .bind_controls(&self.editor, window);
        }
        let projection = editor.projection();
        let state = editor.state();
        let doc = state.doc();
        let selection = state.selection();
        let (a, b) = (selection.from(doc), selection.to(doc));
        let caret_pos = selection.head(doc);
        let marked = markraft_core::composition::composition_range(state)
            .map(|range| (range.from, range.to));
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
        let style = editor.style().clone();
        let scroll = editor.tables.clone();
        let placeholder = (projection.line_count() == 1 && projection.plain_text().is_empty())
            .then(|| editor.placeholder.clone());
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.editor.clone()),
            cx,
        );
        // A grid wider than the note is drawn scrolled, so the painting below is
        // held inside the editor's own content box. Nothing else ever draws
        // outside it, and the mask only goes up where a grid needs it.
        let strips = visible_strips(rows, &scroll, bounds);
        route_table_wheel(&self.editor, &strips, window);
        let mask = (!strips.is_empty()).then_some(ContentMask { bounds });
        window.with_content_mask(mask, |window| {
            // The grids first: their bands and lines sit under everything a cell
            // draws, including the selection.
            paint_tables(rows, &style, caret_pos, &scroll, window);
            for row in rows.iter() {
                for inner in &row.rows {
                    for code in &inner.inline_code {
                        // A pill shorter than the line.
                        let inset = (row.line_height * 0.1).round();
                        let pill = Bounds::new(
                            row.origin
                                + point(
                                    code.left,
                                    row.line_height * code.visual_row as f32 + inset,
                                ),
                            size(code.slot, row.line_height - inset * 2.),
                        );
                        window.paint_quad(
                            fill(pill, style.inline_code_background)
                                .corner_radii(style.code_radius),
                        );
                    }
                }
                for atom in &row.atoms {
                    paint_atom(row, atom, &style, window, cx);
                }
                match row.decoration {
                    Some(Decoration::Quote {
                        levels,
                        joined,
                        tones,
                    }) => {
                        // A callout's header sits above the block, so the bars
                        // beside it reach up over it: its own, and those of the
                        // quotes around it, which would otherwise break there.
                        let lift = row
                            .callout_header
                            .as_ref()
                            .map_or(px(0.), |_| CALLOUT_HEADER_HEIGHT);
                        for level in 0..levels {
                            let height = if level < joined {
                                row.height
                            } else {
                                row.text_height()
                            };
                            let (top, height) = (row.origin.y - lift, height + lift);
                            let bar_x = row.origin.x - style.quote_indent * (levels - level) as f32;
                            window.paint_quad(fill(
                                Bounds::new(point(bar_x, top), size(QUOTE_BAR, height)),
                                tones.get(level).copied().flatten().unwrap_or(style.marker),
                            ));
                            if let Some(marker) = &row.quote_marker {
                                // A focused quote shows its own marker beside
                                // each bar, one per level it sits in. The
                                // marker ends where the next one — or the text
                                // — begins, so the space its spelling carries
                                // is the gap the reader sees. A gutter too
                                // narrow for it keeps it off the bar instead.
                                let next =
                                    row.origin.x - style.quote_indent * (levels - level - 1) as f32;
                                let x = (next - marker.width).max(bar_x + QUOTE_BAR);
                                let _ = marker.paint(
                                    point(x, row.origin.y),
                                    row.line_height,
                                    TextAlign::Left,
                                    None,
                                    window,
                                    cx,
                                );
                            }
                        }
                    }
                    Some(Decoration::Divider) => window.paint_quad(fill(
                        Bounds::new(
                            point(row.origin.x, row.origin.y + row.line_height * 0.5),
                            size(row.width, px(1.5)),
                        ),
                        style.rule,
                    )),
                    Some(Decoration::Code) => {
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
                if let Some(label) = &row.code_header {
                    paint_code_header(self, row, label, &style, window, cx);
                }
                if let Some(footer) = &row.code_footer {
                    let _ = footer.paint(
                        point(
                            row.origin.x,
                            row.origin.y + row.text_height() + CODE_PADDING * 0.25,
                        ),
                        row.line_height,
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    );
                }
                if let Some(header) = &row.callout_header {
                    let _ = header.label.paint(
                        point(row.origin.x, row.origin.y - CALLOUT_HEADER_HEIGHT),
                        CALLOUT_HEADER_HEIGHT,
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    );
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
                    let _ = inner.line.paint(
                        origin,
                        row.line_height,
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    );
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
            // Last, over the cells: the fade is what says a clipped grid continues.
            for (table, strip) in &strips {
                if let Some(grid) = scroll.get(table) {
                    paint_table_fades(*strip, *grid, &style, window);
                }
            }
        });
    }
}

/// Send a horizontal wheel over a scrolling grid to that grid.
///
/// The note's own scroller reads a purely horizontal delta as a vertical one, so
/// an event a grid takes has to be stopped before it bubbles there. An event
/// that also carries a vertical component keeps going, and still scrolls the
/// note.
fn route_table_wheel(
    editor: &Entity<EditorView>,
    strips: &[(usize, Bounds<Pixels>)],
    window: &mut Window,
) {
    if strips.is_empty() {
        return;
    }
    let strips = strips.to_vec();
    let editor = editor.clone();
    let line_height = window.line_height();
    window.on_mouse_event(move |event: &ScrollWheelEvent, phase, _, cx| {
        let delta = event.delta.pixel_delta(line_height);
        // Only a gesture that is mostly sideways; the stray x of a vertical
        // swipe would otherwise drag the grid along with the note.
        if phase != DispatchPhase::Capture || delta.x.abs() <= delta.y.abs() {
            return;
        }
        let Some((table, _)) = strips
            .iter()
            .find(|(_, bounds)| bounds.contains(&event.position))
        else {
            return;
        };
        editor.update(cx, |editor, cx| {
            let Some(grid) = editor.tables.get_mut(table) else {
                return;
            };
            let moved = (grid.offset - delta.x).clamp(px(0.), grid.overflow);
            if moved != grid.offset {
                grid.offset = moved;
                cx.notify();
            }
        });
        if delta.y == px(0.) {
            cx.stop_propagation();
        }
    });
}

/// The quote bars a grid draws for the quote levels it sits in.
///
/// A cell's own height is zero except on the last of its row, so the ordinary
/// quote painter has nothing to draw against inside a grid; the grid draws one
/// bar per level over its whole height instead, at the x positions that painter
/// uses. `offset` undoes the grid's own horizontal scroll, so a bar stays at
/// the quote's indent.
fn quote_bars(
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
fn paint_tables(
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
fn paint_table_fades(
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
fn paint_table(
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

/// Draw one inline atom over the fillers reserving its slot.
fn paint_atom(
    row: &LayoutLine,
    atom: &InlineAtom,
    style: &EditorStyle,
    window: &mut Window,
    cx: &mut App,
) {
    let top = row.origin.y + row.line_height * atom.visual_row as f32;
    if let Some((image, drawn)) = &atom.image {
        let bounds = Bounds::new(point(row.origin.x + atom.left, top), *drawn);
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
        point(row.origin.x + atom.left, top + inset),
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

/// A page glyph: a sheet with a folded corner and two lines of writing, drawn at
/// [`PILL_ICON`] square from `origin`. It stands where a picture glyph would, for
/// an embed that names a note.
fn paint_page(origin: Point<Pixels>, style: &EditorStyle, window: &mut Window) {
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

fn paint_marker(
    row: &LayoutLine,
    marker: &Marker,
    style: &EditorStyle,
    window: &mut Window,
    cx: &mut App,
) {
    let bounds = row.marker_bounds().expect("marker has bounds");
    match marker {
        Marker::Number(line) | Marker::Source(line) => {
            let _ = line.paint(
                bounds.origin,
                row.line_height,
                TextAlign::Left,
                None,
                window,
                cx,
            );
        }
        Marker::Task {
            source: Some(line), ..
        } => {
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
        Marker::Task {
            checked,
            number,
            source: None,
        } => {
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

fn paint_code_header(
    surface: &EditorSurface,
    row: &LayoutLine,
    label: &ShapedLine,
    style: &EditorStyle,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(copy) = row.code_copy_bounds() else {
        return;
    };
    // Focused fences spell ` ```lang ` on the left; unfocused keep the
    // language chip on the right next to the copy control.
    let focused_fence = row.code_footer.is_some();
    let label_origin = if focused_fence {
        point(
            row.origin.x,
            row.origin.y - CODE_HEADER_HEIGHT - CODE_HEADER_LIFT + px(3.),
        )
    } else {
        let Some(language) = row.code_language_bounds() else {
            return;
        };
        language.origin + point(px(6.), px(3.))
    };
    let _ = label.paint(label_origin, px(18.), TextAlign::Left, None, window, cx);
    let mut icons = PathBuilder::stroke(px(1.2));
    if !focused_fence {
        let Some(language) = row.code_language_bounds() else {
            return;
        };
        let chevron = point(
            language.right() - CODE_CHEVRON_WIDTH,
            language.top() + px(10.5),
        );
        icons.move_to(chevron + point(px(2.), px(0.)));
        icons.line_to(chevron + point(px(5.), px(3.)));
        icons.line_to(chevron + point(px(8.), px(0.)));
    }
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
    if row.code_language_bounds().is_some() {
        window.set_cursor_style(CursorStyle::PointingHand, language_box);
    }
    window.set_cursor_style(CursorStyle::PointingHand, copy_box);
    let language_box = language_box.clone();
    let copy_box = copy_box.clone();
    let language_enabled = row.code_language_bounds().is_some();
    let editor = surface.editor.clone();
    let code_pos = row.code_pos;
    window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
        if !phase.bubble() || event.button != MouseButton::Left {
            return;
        }
        let language_clicked =
            language_enabled && language_box.is_hovered_at(event.position, window);
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
                editor.update(cx, |editor, cx| {
                    editor.run_control(
                        crate::accessibility::ControlAction::CodeLanguage(pos),
                        true,
                        window,
                        cx,
                    );
                });
            }
            cx.stop_propagation();
        } else if copy_clicked {
            if let Some(pos) = code_pos {
                editor.update(cx, |editor, cx| {
                    editor.run_control(
                        crate::accessibility::ControlAction::CopyCode(pos),
                        true,
                        window,
                        cx,
                    );
                });
            }
            cx.stop_propagation();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{
        AtomShape, CELL_MIN_WIDTH, CELL_PADDING_X, CELL_PADDING_Y, CODE_PADDING, Decoration,
        LayoutLine, LayoutRow, Marker, QUOTE_BAR, ShapeInput, TableScroll, Widening, atom_label,
        cell_under, chrome_marker_unfocused, column_demands, column_widths, decoration_of,
        drawn_image, file_name, gap_below, max_indent, merge_row_centers, picture_source,
        place_table, quote_bars, reveal_offset, shape, table_overflows, unbreakable_units,
        visible_strips,
    };
    use crate::style::EditorStyle;
    use crate::typeahead::tests::state_of;
    use crate::types::DocTypes;
    use gpui::{Bounds, NoopTextSystem, Pixels, TextSystem, WindowTextSystem, point, px, size};
    use markraft_commonmark::{commonmark_doc_type_names, commonmark_schema};
    use markraft_core::commands::ColumnAlignment;
    use markraft_core::projection::{Line, RunContent, projection_of};
    use std::collections::HashMap;
    use std::sync::Arc;

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
            min_width: px(0.),
            top_gap: Pixels::ZERO,
            code_pos: None,
            marker: None,
            decoration: None,
            code_header: None,
            code_footer: None,
            quote_marker: None,
            callout_header: None,
            code_hitboxes: None,
            widenings: Vec::new(),
            atoms: Vec::new(),
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

    #[test]
    fn popovers_anchor_all_selection_at_the_first_content_row() {
        for source in ["Markraft interface check", "- Parent\n  - Child"] {
            let state = state_of(source);
            let selection = markraft_core::Selection::All;
            let rows = rows_of(source);
            let start = selection.from(state.doc());
            let end = selection.to(state.doc());
            assert!(
                !rows[0].contains(start),
                "the document starts before its text"
            );
            let anchor = super::selection_anchor_row(&rows, start, end).unwrap();
            assert_eq!(anchor.index, 0);
            assert_eq!(anchor.pos_to_offset(start), 0);
        }

        let rows = rows_of("First\n\nSecond");
        let start = rows[1].from + 1;
        let anchor = super::selection_anchor_row(&rows, start, start + 2).unwrap();
        assert_eq!(anchor.index, 1, "a partial selection stays on its own row");
        assert_eq!(anchor.pos_to_offset(start), 1);
        assert!(super::selection_anchor_row(&rows, 0, 0).is_none());
        let outside = rows.last().unwrap().to() + 1;
        assert!(super::selection_anchor_row(&rows, outside, outside + 2).is_none());
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
        let rows = shaped("*a **b** c*");
        let row = &rows[0];
        // The delimiter characters are real projection offsets. With the
        // caret outside the span they are collapsed in the display map.
        assert_eq!(row.char_len, 11);
        let display_shift: isize = row
            .widenings
            .iter()
            .map(|w| w.len as isize - w.source_len as isize)
            .sum();
        assert_eq!(
            row.char_len as isize + display_shift,
            5,
            "six delimiter chars collapse, five prose chars remain"
        );
        assert!(
            row.widenings.iter().any(|w| w.len == 0 && w.source_len > 0),
            "at least one delimiter run is collapsed"
        );
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

    /// Run `each` over every line of `source`, with the shaping input the layout
    /// pass would have built for it.
    fn per_line<T>(
        source: &str,
        style: &EditorStyle,
        each: impl Fn(&ShapeInput<'_>, usize, &Line) -> T,
    ) -> Vec<T> {
        let state = state_of(source);
        let projection = projection_of(&state);
        let schema = commonmark_schema();
        let types = DocTypes::from_schema_names(&schema, &commonmark_doc_type_names());
        let images = crate::images::Images::default();
        let spelling = markraft_commonmark::CommonMarkSpelling::new(state.schema().clone());
        let input = ShapeInput {
            images: &images,
            spelling: Some(&spelling),
            wiki: None,
            doc: state.doc(),
            types: &types,
            projection: &projection,
            style,
            single_line: false,
            selection: 0..0,
            composition: None,
        };
        projection
            .lines()
            .iter()
            .enumerate()
            .map(|(index, line)| each(&input, index, line))
            .collect()
    }

    /// The gap every line of a document carries below it.
    fn gaps_of(source: &str, style: &EditorStyle) -> Vec<Pixels> {
        per_line(source, style, |input, index, line| {
            gap_below(
                input,
                index,
                line,
                input.types.heading_level(line),
                input.types.is_code_block(line),
                &chrome_marker_unfocused(input.types, line, None),
            )
        })
    }

    /// The decoration every line of a document carries, at a note's width.
    fn decorations_of(source: &str, style: &EditorStyle) -> Vec<Option<Decoration>> {
        per_line(source, style, |input, index, line| {
            decoration_of(input, index, line, false, max_indent(input.style, px(600.)))
        })
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

    /// The view keeps HTML verbatim and shows it as source, so a raw block is
    /// drawn like a paragraph of monospace text: nothing behind it, and the
    /// ordinary gap below.
    #[test]
    fn a_raw_block_is_drawn_as_a_plain_block() {
        let style = spaced_style();
        let source = "<div>\n  x\n</div>\n\npara";
        assert!(
            per_line(source, &style, |input, _, line| input
                .types
                .is_raw_block(line))[0],
            "the HTML block is the one kept verbatim"
        );
        assert_eq!(gaps_of(source, &style)[0], style.paragraph_gap);
        assert!(
            decorations_of(source, &style)[0].is_none(),
            "no panel behind a raw block"
        );
        assert!(
            matches!(
                decorations_of("```\nx\n```\n\npara", &style)[0],
                Some(Decoration::Code)
            ),
            "a code block keeps its own",
        );
    }

    /// A window-free text system, so the shaping pass can be run in a test.
    /// Every character comes out the same width, which is all a row count and a
    /// caret stop ask of it.
    fn text_system() -> WindowTextSystem {
        WindowTextSystem::new(Arc::new(TextSystem::new(Arc::new(NoopTextSystem::new()))))
    }

    /// Every line of `source`, laid out at a note's width.
    fn shaped(source: &str) -> Vec<LayoutLine> {
        shaped_with(source, callout_types())
    }

    /// What a CommonMark host hands the view: the preset's roles, plus the two
    /// block-quote attributes that spell a callout, which no role table names.
    fn callout_types() -> DocTypes {
        let schema = commonmark_schema();
        DocTypes {
            callout: Some(crate::CalloutAttrs {
                kind: "callout",
                title: "title",
            }),
            ..DocTypes::from_schema_names(&schema, &commonmark_doc_type_names())
        }
    }

    fn shaped_with(source: &str, types: DocTypes) -> Vec<LayoutLine> {
        let state = state_of(source);
        let projection = projection_of(&state);
        let images = crate::images::Images::default();
        let spelling = markraft_commonmark::CommonMarkSpelling::new(state.schema().clone());
        let style = EditorStyle::notes();
        let input = ShapeInput {
            images: &images,
            spelling: Some(&spelling),
            wiki: None,
            doc: state.doc(),
            types: &types,
            projection: &projection,
            style: &style,
            single_line: false,
            selection: 0..0,
            composition: None,
        };
        shape(&input, px(600.), &text_system())
    }

    /// A raw block's source is its own text, so it goes through the rows a code
    /// block goes through: one per `\n`, each holding the caret stops of its own
    /// line and reported to AccessKit as the text it is rather than as an atom.
    #[test]
    fn a_raw_blocks_rows_follow_its_newlines() {
        let source = "<div>\n  x\n</div>";
        let lines = shaped(&format!("{source}\n\npara"));
        let raw = &lines[0];
        assert_eq!(
            raw.rows.iter().map(LayoutRow::text).collect::<Vec<_>>(),
            vec!["<div>", "  x", "</div>"],
            "one row per source line"
        );
        assert_eq!(raw.visual_rows(), 3, "and none of them wrapped");
        assert_eq!(raw.char_len, source.chars().count());
        // Caret movement, selection and hit testing all locate an offset by its
        // row, so every row has to answer for its own stretch of the block.
        for (offset, row) in [(0, 0), (5, 0), (6, 1), (9, 1), (10, 2), (raw.char_len, 2)] {
            assert_eq!(raw.locate(offset).0, row, "offset {offset}");
        }
        assert_eq!(
            raw.accessible_rows(),
            vec![0..5, 6..9, 10..raw.char_len],
            "every row is selectable text"
        );
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
        let images = crate::images::Images::default();
        let (_, drawn) = drawn_image(&images, src, px(100.)).expect("the PNG decodes");
        assert_eq!(drawn.width, px(2.), "a small image is not blown up");
        assert_eq!(drawn.height, px(1.));
        assert!(drawn_image(&images, "https://host/a.png", px(100.)).is_none());
        assert!(drawn_image(&images, "/no/such/file.png", px(100.)).is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn an_image_falls_back_to_the_name_of_its_file() {
        assert_eq!(file_name("images/shot.png"), Some("shot.png"));
        assert_eq!(file_name("/a/b/shot.png?v=2#x"), Some("shot.png"));
        assert_eq!(file_name("https://host/"), Some("host"));
        assert_eq!(file_name(""), None);
    }

    /// A drawn atom needs far more room than the one character it is, so the
    /// display text holds a run of fillers in its place. Every geometry query
    /// still speaks in projection offsets, which the two maps have to preserve.
    #[test]
    fn an_atom_placeholder_stays_one_caret_stop() {
        let mut rows = rows_of("ab![alt](x.png)cd");
        let row = &mut rows[0];
        assert_eq!(row.char_len, 5, "the atom is one visible character");
        row.widenings = vec![Widening {
            source: 2,
            source_len: 1,
            display: 2,
            len: 5,
            shape: Some(AtomShape::Pill),
            broken: false,
        }];
        for offset in 0..=row.char_len {
            assert_eq!(row.to_source(row.to_display(offset)), offset, "at {offset}");
        }
        assert_eq!(row.to_display(2), 2, "the atom starts where its slot does");
        assert_eq!(row.to_display(3), 7, "and ends where its slot does");
        assert_eq!(row.to_source(3), 2, "the atom's left half is before it");
        assert_eq!(row.to_source(6), 3, "and its right half is after it");
        assert_eq!(row.to_display(5), 9, "text past the atom keeps its order");
    }

    /// HTML is never rendered: an inline primitive is drawn as the source it
    /// stands for, so the row reserves the width of exactly that text and
    /// nothing around it.
    #[test]
    fn a_raw_inline_atom_reserves_the_width_of_its_own_source() {
        let state = state_of("press <kbd>K</kbd> twice");
        let projection = projection_of(&state);
        let schema = commonmark_schema();
        let types = DocTypes::from_schema_names(&schema, &commonmark_doc_type_names());
        let drawn: Vec<(AtomShape, &str)> = projection.lines()[0]
            .runs
            .iter()
            .filter_map(|run| match &run.content {
                RunContent::Atom(node) => atom_label(&types, node),
                _ => None,
            })
            .collect();
        assert_eq!(
            drawn,
            vec![(AtomShape::Source, "<kbd>"), (AtomShape::Source, "</kbd>")],
            "both tags read as they were written"
        );
        assert_eq!(
            AtomShape::Source.chrome(),
            px(0.),
            "source text reserves its own width and no padding"
        );
        // And the row shapes that source itself, so the text after a tag sits
        // against it exactly as it does after a wiki link.
        let row = &shaped("press <kbd>K</kbd> twice")[0];
        assert_eq!(row.rows[0].text(), "press <kbd>K</kbd> twice");
        assert!(!row.rows[0].text().contains(super::PILL_FILLER));
    }

    /// A wiki link is drawn as the prose it stands in: its alias, or its
    /// target with whatever it names, and none of its brackets. An embed is not a
    /// link at all — it names a file the note shows — so it reads as an image does.
    #[test]
    fn a_wiki_link_atom_is_drawn_as_its_label_and_nothing_around_it() {
        let state =
            state_of("see [[Note#Top]] and [[a/b|Alias]] and ![[pics/x.png]] and ![[y.png|Cover]]");
        let projection = projection_of(&state);
        let schema = commonmark_schema();
        let types = DocTypes::from_schema_names(&schema, &commonmark_doc_type_names());
        let drawn: Vec<(AtomShape, &str)> = projection.lines()[0]
            .runs
            .iter()
            .filter_map(|run| match &run.content {
                RunContent::Atom(node) => atom_label(&types, node),
                _ => None,
            })
            .collect();
        assert_eq!(
            drawn,
            vec![
                (AtomShape::Link, "Note#Top"),
                (AtomShape::Link, "Alias"),
                (AtomShape::Pill, "x.png"),
                (AtomShape::Pill, "Cover"),
            ]
        );
        assert_eq!(AtomShape::Link.chrome(), px(0.));
        assert!(AtomShape::Link.is_own_text());
        // An embed is the same picture as `![](…)` written another way, so both name
        // the same file to load; a plain link names no picture at all.
        let sources: Vec<Option<&str>> = projection.lines()[0]
            .runs
            .iter()
            .filter_map(|run| match &run.content {
                RunContent::Atom(node) => Some(picture_source(&types, node)),
                _ => None,
            })
            .collect();
        assert_eq!(sources, vec![None, None, Some("pics/x.png"), Some("y.png")]);
    }

    /// A wiki link is shaped as the text it reads as, so the punctuation after
    /// it sits against it rather than after a placeholder rounded up to whole
    /// fillers.
    #[test]
    fn text_after_a_wiki_link_starts_at_the_labels_right_edge() {
        let lines = shaped("see [[Missing Page]]. end");
        let row = &lines[0];
        // The display text holds the label itself; nothing stands in for it.
        assert_eq!(row.rows[0].text(), "see Missing Page. end");
        assert!(!row.rows[0].text().contains(super::PILL_FILLER));
        // Four projection characters — `see ` — then the atom, then `. end`.
        assert_eq!(row.char_len, 4 + 1 + 5);
        let atom = row.rectangles(4..5, false);
        let after = row.rectangles(5..6, false);
        assert_eq!(atom.len(), 1);
        assert_eq!(after.len(), 1);
        assert_eq!(atom[0].right(), after[0].left());
    }

    /// A callout says what it is on a line of its own above its first block,
    /// which is chrome: it takes room, not caret stops.
    #[test]
    fn a_callout_reserves_a_header_row_above_its_first_block_only() {
        let lines = shaped("> [!tip] Custom title\n> First\n>\n> Second\n\nafter");
        let (first, second, after) = (&lines[0], &lines[1], &lines[2]);
        assert_eq!(
            first.callout_header().map(|(label, _)| label),
            Some("Custom title")
        );
        assert_eq!(second.callout_header().map(|(label, _)| label), None);
        assert_eq!(after.callout_header().map(|(label, _)| label), None);
        // The room is above the block, so no caret stop moved.
        assert!(first.top_gap >= super::CALLOUT_HEADER_HEIGHT);
        let (_, box_) = first.callout_header().expect("a header");
        assert_eq!(box_.bottom(), first.origin.y);
        // The marker line is not in the text, so the first line is the body's.
        assert_eq!(first.rows[0].text(), "First");
        // A callout with no title reads as its type, capitalised.
        let lines = shaped("> [!warning]\n> Body");
        assert_eq!(
            lines[0].callout_header().map(|(label, _)| label),
            Some("Warning")
        );
        // Where the callout sits in the note does not matter, and a first block
        // holding a hard break is still one line with one header.
        let lines = shaped("before\n\n> [!note]\n> One  \n> two\n>\n> > [!tip]\n> > Inner");
        let labels: Vec<_> = lines
            .iter()
            .map(|line| line.callout_header().map(|(label, _)| label.to_owned()))
            .collect();
        assert_eq!(
            labels,
            [None, Some("Note".to_owned()), Some("Tip".to_owned())]
        );
        // Each bar keeps its own callout's tone: beside the nested tip the outer
        // bar is still the note's.
        let Some(Decoration::Quote { levels, tones, .. }) = lines[2].decoration else {
            panic!("a nested callout line is decorated as a quote");
        };
        assert_eq!(levels, 2);
        assert!(tones[0].is_some() && tones[1].is_some());
        assert_ne!(tones[0], tones[1]);
        let Some(Decoration::Quote { tones: outer, .. }) = lines[1].decoration else {
            panic!("a callout line is decorated as a quote");
        };
        assert_eq!(outer[0], tones[0]);
        // The band is the header's, and nothing below or above it is.
        let band = lines[1].origin.y - super::CALLOUT_HEADER_HEIGHT / 2.;
        assert!(lines[1].in_callout_header(band));
        assert!(!lines[1].in_callout_header(lines[1].origin.y));
        assert!(!lines[0].in_callout_header(band));
        // An ordinary quote has no header and no extra room.
        let lines = shaped("> plain");
        assert!(lines[0].callout_header().is_none());
        assert_eq!(lines[0].top_gap, Pixels::ZERO);
    }

    /// Callouts are the host's to ask for: the attributes that spell one are not
    /// in any role table, so a host that names none gets the block quotes its
    /// schema declares and nothing drawn around them — however those attributes
    /// happen to be spelled.
    #[test]
    fn a_host_that_names_no_callout_attributes_draws_plain_quotes() {
        let schema = commonmark_schema();
        let types = DocTypes::from_schema_names(&schema, &commonmark_doc_type_names());
        assert!(types.callout.is_none(), "no role table names them");
        let lines = shaped_with("> [!tip] Custom title\n> Body", types);
        assert!(lines[0].callout_header().is_none());
        assert_eq!(lines[0].top_gap, Pixels::ZERO);
        // The quote is still a quote, with a bar of the ordinary tone.
        let Some(Decoration::Quote { levels, tones, .. }) = lines[0].decoration else {
            panic!("a quote line is decorated as a quote");
        };
        assert_eq!(levels, 1);
        assert_eq!(tones[0], None);
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
        let floor = [CELL_MIN_WIDTH; 3];
        assert_eq!(
            column_widths(&preferred, &floor, px(400.)),
            preferred.to_vec(),
            "room to spare leaves every column as it asked"
        );
        assert_eq!(
            column_widths(&preferred, &floor, px(200.)),
            vec![CELL_MIN_WIDTH, px(88.), CELL_MIN_WIDTH],
            "the two narrow columns stop at the floor and the wide one takes the rest"
        );
        // A grid that will not fit even at the floor keeps its width, and the
        // view scrolls it sideways rather than squeezing its text away.
        let tight = [CELL_MIN_WIDTH; 3];
        assert_eq!(column_widths(&tight, &floor, px(100.)), tight.to_vec());
        // A column whose widest word needs more than the flat floor keeps it:
        // shrinking may wrap a cell, never split a word.
        let minimum = [px(120.), CELL_MIN_WIDTH, CELL_MIN_WIDTH];
        assert_eq!(
            column_widths(&preferred, &minimum, px(200.)),
            vec![px(120.), CELL_MIN_WIDTH, CELL_MIN_WIDTH],
            "the word's column holds its width and the grid overflows instead"
        );
    }

    /// A column is floored by the widest word of the widest cell in it, as that
    /// cell will be painted — a header cell is bold, so the floor is the bold
    /// width, not the regular one.
    ///
    /// The live bug this pins: the bold header "Second" measured 52.0004px, the
    /// column reserved `52.0004 + 16`, and handing the cell back `that - 16`
    /// returned 52.000397px — a fraction short in `f32`, which is all the
    /// wrapper needs to break on strictly greater and split the word as
    /// `Secon / d`. Whole pixels at both ends make the round trip exact.
    #[test]
    fn a_bold_header_floors_its_column_at_the_width_the_bold_word_needs() {
        // What the measuring pass leaves behind: a bold one-word header and the
        // narrow body cell under it. One word, so content and min-content are
        // the same width.
        let bold = px(52.0004);
        let body = px(9.1);
        let mut cells = table_cells(px(0.));
        for (index, cell) in cells.iter_mut().enumerate() {
            let measured = if index < 2 { bold } else { body };
            cell.width = measured;
            cell.min_width = measured;
        }
        let grid = [(0, 0), (0, 1), (1, 0), (1, 1)];
        let (preferred, minimum) = column_demands(&cells, &grid, 2);
        assert_eq!(
            preferred, minimum,
            "a one-word column cannot give anything up"
        );
        for (column, floor) in minimum.iter().enumerate() {
            assert!(
                *floor >= bold + CELL_PADDING_X * 2.,
                "column {column} floors below the bold header it holds"
            );
            // The width the cell is actually reshaped at, which is what the
            // wrapper compares the word against.
            let content = *floor - CELL_PADDING_X * 2.;
            assert!(
                content >= bold,
                "column {column} hands back {content:?}, short of the {bold:?} word"
            );
        }
        // And it holds through a grid far too narrow for those columns, which
        // is exactly when the shrinking runs.
        for content in column_widths(&preferred, &minimum, px(80.))
            .iter()
            .map(|width| *width - CELL_PADDING_X * 2.)
        {
            assert!(content >= bold, "shrinking split the word after all");
        }
    }

    /// What a column may never be narrower than: the widest run of text the
    /// line wrapper would not break. These ranges have to agree with where gpui
    /// actually wraps, or a column floors at a width that still splits a word.
    #[test]
    fn a_cells_unbreakable_units_are_the_runs_the_wrapper_keeps_together() {
        let units = |text: &str, glue: &[std::ops::Range<usize>]| -> Vec<String> {
            unbreakable_units(text, glue)
                .into_iter()
                .map(|range| text[range].to_owned())
                .collect()
        };
        assert_eq!(units("gamma delta", &[]), ["gamma", "delta"]);
        assert_eq!(units("  leading  spaces ", &[]), ["leading", "spaces"]);
        // A hyphen and an underscore bind a word together; a path separator and
        // a query do not, so a long URL still has somewhere to break.
        assert_eq!(units("well-known_name", &[]), ["well-known_name"]);
        assert_eq!(units("a/b?c&d", &[]), ["a", "/b", "?c", "&d"]);
        // CJK breaks between any two characters, so one character is all a
        // column has to reserve room for.
        assert_eq!(units("表格标题", &[]), ["表", "格", "标", "题"]);
        // A code span is drawn as one pill, so it counts as one unit however
        // many words it holds.
        let span = std::slice::from_ref(&(4usize..14usize));
        assert_eq!(
            units("say cargo test now", span),
            ["say", "cargo test", "now"]
        );
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

    /// What marker, if any, each line of a document draws.
    fn markers_of(source: &str) -> Vec<Option<&'static str>> {
        let state = state_of(source);
        let projection = projection_of(&state);
        let schema = commonmark_schema();
        let types = DocTypes::from_schema_names(&schema, &commonmark_doc_type_names());
        projection
            .lines()
            .iter()
            .map(|line| {
                chrome_marker_unfocused(&types, line, None).map(|marker| match marker {
                    Marker::Number(_) => "number",
                    Marker::Bullet { .. } => "bullet",
                    Marker::Task { .. } => "task",
                    Marker::Source(_) => "source",
                })
            })
            .collect()
    }

    /// A container inside a list item is not a second start of that item. The
    /// marker used to be attached to the first block of whatever held the line,
    /// which drew a check box beside a nested quote and one beside every row of
    /// a nested table.
    #[test]
    fn only_an_items_very_first_line_carries_its_marker() {
        assert_eq!(
            markers_of("- [ ] task\n\n  > quoted\n"),
            vec![Some("task"), None],
            "the quote inside the item starts nothing"
        );
        assert_eq!(
            markers_of("- item\n\n  | a | b |\n  | - | - |\n  | c | d |\n"),
            vec![Some("bullet"), None, None, None, None],
            "every cell of the nested table used to start the item again"
        );
        // Two items still get one marker each, and a nested list its own.
        assert_eq!(
            markers_of("- a\n- [x] b\n  - c\n"),
            vec![Some("bullet"), Some("task"), Some("bullet")]
        );
    }

    #[test]
    fn ordered_tasks_draw_separate_checkbox_and_keep_continuation_indent() {
        let rows = shaped("7. [ ] open\n\n   continuation\n\n8. [x] done\n9. plain");
        let first = &rows[0];
        let (checked, checkbox) = first.task_marker().expect("ordered task has checkbox");
        assert!(!checked);
        let Some(Marker::Task {
            number: Some(number),
            ..
        }) = &first.marker
        else {
            panic!("ordered task also keeps its number");
        };
        let ordinal_center = point(
            checkbox.left() - super::NUMBER_GAP - number.width / 2.,
            checkbox.center().y,
        );
        assert!(
            !checkbox.contains(&ordinal_center),
            "the number is not a checkbox hit target"
        );
        assert_eq!(
            first.origin.x, rows[1].origin.x,
            "continuation text keeps its alignment"
        );
        assert!(
            rows[1].task_marker().is_none(),
            "only the task's first line has a checkbox"
        );
        assert!(rows[2].task_marker().unwrap().0);
        assert!(
            rows[3].task_marker().is_none(),
            "plain ordered items never toggle tasks"
        );
    }

    /// A cell's own height is zero except on the last of its row, so the quote
    /// a grid sits in has nothing to draw a bar against; the grid draws them
    /// itself, over its whole height.
    #[test]
    fn a_grid_inside_a_quote_draws_its_own_bars() {
        let mut cells = placed_table(ColumnAlignment::None, px(10.));
        stack(&mut cells, px(0.));
        let grid = cells
            .iter()
            .filter_map(LayoutLine::cell_bounds)
            .reduce(|all, bounds| all.union(&bounds))
            .expect("a placed table has a grid");
        assert_eq!(grid.size.height, px(70.), "rows of 30 and 40");
        assert_eq!(
            quote_bars(grid, 2, px(0.), px(12.)),
            vec![
                Bounds::new(point(px(-20.), px(0.)), size(QUOTE_BAR, px(70.))),
                Bounds::new(point(px(-8.), px(0.)), size(QUOTE_BAR, px(70.))),
            ],
            "one bar per level, at the indents the ordinary painter uses"
        );
        // A scrolling grid slides under its bars rather than taking them along.
        assert_eq!(
            quote_bars(grid, 1, px(30.), px(12.))[0].left(),
            px(4.) + px(30.) - px(12.)
        );
        assert!(quote_bars(grid, 0, px(0.), px(12.)).is_empty());
    }

    /// The caret's cell is brought into the visible strip, as the vertical
    /// reveal brings its line into the viewport.
    #[test]
    fn the_caret_pulls_its_own_cell_into_the_visible_strip() {
        let strip = Bounds::new(point(px(0.), px(0.)), size(px(100.), px(50.)));
        let cell = |left: f32| Bounds::new(point(px(left), px(0.)), size(px(40.), px(20.)));
        assert_eq!(
            reveal_offset(cell(10.), strip, px(0.), px(200.)),
            px(0.),
            "a cell already in view keeps the reader's place"
        );
        assert_eq!(
            reveal_offset(cell(150.), strip, px(0.), px(200.)),
            px(90.),
            "one off the right edge is pulled just inside it"
        );
        assert_eq!(
            reveal_offset(cell(10.), strip, px(80.), px(200.)),
            px(10.),
            "and one off the left edge of a scrolled grid is pulled back"
        );
        assert_eq!(
            reveal_offset(cell(500.), strip, px(0.), px(200.)),
            px(200.),
            "never past what there is to scroll"
        );
    }

    /// A grid too wide for the note keeps its width and is drawn scrolled. Every
    /// geometry query reads the moved origins, so a click lands in the cell the
    /// reader sees under the pointer.
    #[test]
    fn a_grid_wider_than_the_note_scrolls_as_one_and_takes_its_geometry_along() {
        let mut cells = placed_table(ColumnAlignment::None, px(10.));
        stack(&mut cells, px(0.));
        let content = Bounds::new(point(px(0.), px(0.)), size(px(120.), px(200.)));
        // The grid runs from 4 to 184 and the note's content ends at 120.
        assert_eq!(
            table_overflows(&cells, content).get(&6).copied(),
            Some(px(64.))
        );
        assert!(
            table_overflows(
                &cells,
                Bounds::new(point(px(0.), px(0.)), size(px(400.), px(200.)))
            )
            .values()
            .all(|overflow| *overflow == Pixels::ZERO),
            "a grid that fits has nothing to scroll"
        );
        for cell in &mut cells {
            cell.origin.x -= px(64.);
        }
        assert_eq!(
            cell_under(&cells, 6, point(px(60.), px(10.)))
                .expect("a table has a nearest cell")
                .index,
            1,
            "the second column is what sits under that point now"
        );
        let scroll = HashMap::from([(
            6usize,
            TableScroll {
                offset: px(64.),
                overflow: px(64.),
            },
        )]);
        assert_eq!(
            visible_strips(&cells, &scroll, content),
            vec![(
                6,
                Bounds::new(point(px(0.), px(0.)), size(px(120.), px(70.)))
            )],
            "the strip the wheel and the toolbar answer for is the visible part"
        );
        assert!(
            visible_strips(&cells, &HashMap::new(), content).is_empty(),
            "a grid with nothing to scroll takes no wheel"
        );
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

    /// One line may hold several spans of one style. The caret opens the span it
    /// is in and leaves the others spelled the way a reader wants them.
    #[test]
    fn only_the_span_the_caret_is_in_opens_its_delimiters() {
        let source = "**aa** plain **bb**";
        let state = state_of(source);
        let projection = projection_of(&state);
        let plain = projection.plain_text();
        let caret = projection
            .line_offset_to_pos(0, plain.find("bb").expect("bb") + 1)
            .expect("a position");
        let rows = shaped_revealing(source, caret..caret, None);
        let hidden: Vec<_> = rows[0]
            .widenings
            .iter()
            .filter(|widening| widening.len == 0)
            .collect();
        assert_eq!(
            hidden.len(),
            2,
            "the other span's pair stays collapsed: {:?}",
            rows[0]
                .widenings
                .iter()
                .map(|w| (w.source, w.len))
                .collect::<Vec<_>>()
        );
        // With the caret outside both, all four delimiter runs collapse.
        let away = shaped_revealing(source, 0..0, None);
        assert_eq!(
            away[0].widenings.iter().filter(|w| w.len == 0).count(),
            4,
            "every pair collapses when the caret is elsewhere"
        );
    }

    #[test]
    fn focused_heading_and_list_draw_source_markers() {
        let text = text_system();
        let state = state_of("# Hello\n\n- item\n\n1. numbered\n\n- [ ] task");
        let projection = projection_of(&state);
        let schema = commonmark_schema();
        let types = DocTypes::from_schema_names(&schema, &commonmark_doc_type_names());
        let style = EditorStyle::default();
        let images = crate::images::Images::default();
        let spelling = markraft_commonmark::CommonMarkSpelling::new(state.schema().clone());
        // Caret in the heading.
        let heading_pos = projection.lines()[0].from;
        let input = ShapeInput {
            images: &images,
            spelling: Some(&spelling),
            doc: state.doc(),
            types: &types,
            projection: &projection,
            style: &style,
            single_line: false,
            wiki: None,
            selection: heading_pos..heading_pos,
            composition: None,
        };
        let rows = shape(&input, px(400.), &text);
        assert!(
            matches!(rows[0].marker, Some(Marker::Source(_))),
            "focused heading shows ATX hashes"
        );
        // Caret in the bullet item.
        let bullet_pos = projection.lines()[1].from;
        let input = ShapeInput {
            selection: bullet_pos..bullet_pos,
            ..input
        };
        let rows = shape(&input, px(400.), &text);
        assert!(
            matches!(rows[1].marker, Some(Marker::Source(_))),
            "focused bullet shows `- ` spelling"
        );
        // Unfocused list keeps chrome.
        let input = ShapeInput {
            selection: heading_pos..heading_pos,
            ..input
        };
        let rows = shape(&input, px(400.), &text);
        assert!(
            matches!(rows[1].marker, Some(Marker::Bullet { .. })),
            "unfocused bullet stays a drawn marker"
        );
    }

    #[test]
    fn focused_code_block_shows_fence_spelling() {
        let text = text_system();
        let state = state_of("```rust\nfn main() {}\n```");
        let projection = projection_of(&state);
        let schema = commonmark_schema();
        let types = DocTypes::from_schema_names(&schema, &commonmark_doc_type_names());
        let style = EditorStyle::default();
        let images = crate::images::Images::default();
        let spelling = markraft_commonmark::CommonMarkSpelling::new(state.schema().clone());
        let pos = projection.lines()[0].from + 1;
        let input = ShapeInput {
            images: &images,
            spelling: Some(&spelling),
            doc: state.doc(),
            types: &types,
            projection: &projection,
            style: &style,
            single_line: false,
            wiki: None,
            selection: pos..pos,
            composition: None,
        };
        let rows = shape(&input, px(400.), &text);
        assert!(rows[0].code_footer.is_some(), "closing fence while focused");
        assert!(rows[0].code_header.is_some());
    }

    #[test]
    fn focused_quote_shows_source_markers() {
        let text = text_system();
        let state = state_of("> quoted\n\npara");
        let projection = projection_of(&state);
        let schema = commonmark_schema();
        let types = DocTypes::from_schema_names(&schema, &commonmark_doc_type_names());
        let style = EditorStyle::default();
        let images = crate::images::Images::default();
        let spelling = markraft_commonmark::CommonMarkSpelling::new(state.schema().clone());
        let quote_pos = projection.lines()[0].from;
        let input = ShapeInput {
            images: &images,
            spelling: Some(&spelling),
            doc: state.doc(),
            types: &types,
            projection: &projection,
            style: &style,
            single_line: false,
            wiki: None,
            selection: quote_pos..quote_pos,
            composition: None,
        };
        let rows = shape(&input, px(400.), &text);
        // Whatever the kind spells a quote level with, marker and all — the
        // view draws that and nothing of its own.
        let expected = markraft_core::kind::SourceSpelling::container_marker(
            &spelling,
            types.blockquote.expect("the quote type"),
        )
        .expect("a quote marker");
        assert_eq!(
            rows[0].quote_marker.as_ref().map(|line| line.len),
            Some(expected.len()),
            "focused quote shows its own spelling"
        );
        let para_pos = projection.lines()[1].from;
        let input = ShapeInput {
            selection: para_pos..para_pos,
            ..input
        };
        let rows = shape(&input, px(400.), &text);
        assert!(
            rows[0].quote_marker.is_none(),
            "unfocused quote hides its `>` spelling"
        );
    }

    /// An input method's marked text opens the span it is being typed into, and
    /// only that one.
    #[test]
    fn marked_text_opens_the_span_it_sits_in() {
        let source = "**aa** plain **bb**";
        let state = state_of(source);
        let projection = projection_of(&state);
        let plain = projection.plain_text();
        let at = |needle: &str| {
            projection
                .line_offset_to_pos(0, plain.find(needle).expect("needle") + 1)
                .expect("a position")
        };
        let marked = at("bb");
        let rows = shaped_revealing(source, 0..0, Some(marked..marked));
        assert_eq!(
            rows[0].widenings.iter().filter(|w| w.len == 0).count(),
            2,
            "the marked span opens even though the selection is elsewhere"
        );
    }

    /// What a laid-out line shows, row by row.
    fn shown_text(line: &LayoutLine) -> Vec<&str> {
        line.rows.iter().map(LayoutRow::text).collect()
    }

    /// A caret at the `offset`th source character of the first line.
    fn caret_at(source: &str, offset: usize) -> std::ops::Range<usize> {
        let state = state_of(source);
        let pos = projection_of(&state).lines()[0]
            .offset_to_pos(offset)
            .expect("a position in the line");
        pos..pos
    }

    /// An entity away from the caret is drawn as the character it stands for,
    /// and every offset around it still maps to a caret stop: a click on the
    /// `&` lands before or after the whole entity, never inside it.
    #[test]
    fn a_concealed_entity_is_drawn_as_what_it_displays() {
        let source = "a &amp; b";
        let rows = shaped_revealing(source, 0..0, None);
        let row = &rows[0];
        assert_eq!(shown_text(row), vec!["a & b"]);
        let entity: Vec<_> = row
            .widenings
            .iter()
            .map(|w| (w.source, w.source_len, w.display, w.len, w.shape.is_none()))
            .collect();
        assert_eq!(entity, vec![(2, 5, 2, 1, true)]);
        assert_eq!(row.to_display(2), 2, "the entity starts where its `&` does");
        assert_eq!(row.to_display(7), 3, "and ends where it does");
        assert_eq!(row.to_display(9), 5, "the text after it follows on");
        assert_eq!(row.to_source(2), 2);
        assert_eq!(row.to_source(3), 7, "past the `&` is past the whole entity");
        assert_eq!(row.to_source(5), 9);
        for offset in [0, 1, 2, 7, 8, 9] {
            assert_eq!(row.to_source(row.to_display(offset)), offset, "at {offset}");
        }
        // With the caret on it the entity is its source again.
        let revealed = shaped_revealing(source, caret_at(source, 4), None);
        assert_eq!(shown_text(&revealed[0]), vec!["a &amp; b"]);
        assert!(revealed[0].widenings.is_empty());
    }

    /// A hard break's backslash is hidden until the caret ends its row, and
    /// shown then.
    #[test]
    fn a_hard_break_spelling_shows_only_at_the_caret() {
        let source = "ab\\\ncd";
        let away = shaped_revealing(source, 0..0, None);
        assert_eq!(shown_text(&away[0]), vec!["ab", "cd"]);
        let at_end = shaped_revealing(source, caret_at(source, 3), None);
        assert_eq!(shown_text(&at_end[0]), vec!["ab\\", "cd"]);
        let next_row = shaped_revealing(source, caret_at(source, 4), None);
        assert_eq!(shown_text(&next_row[0]), vec!["ab", "cd"]);
    }

    /// A caret beside an escape opens that escape and nothing else; a caret in
    /// the span beside it opens the span and leaves the escape concealed.
    #[test]
    fn an_escape_and_the_span_beside_it_reveal_apart() {
        let source = "\\***a**";
        let escape = shaped_revealing(source, caret_at(source, 1), None);
        assert_eq!(shown_text(&escape[0]), vec!["\\*a"]);
        let span = shaped_revealing(source, caret_at(source, 5), None);
        assert_eq!(shown_text(&span[0]), vec!["***a**"]);
    }

    /// Nested spans open by their own extent: a caret in the inner one opens
    /// both, one only in the outer one opens just the outer pair.
    #[test]
    fn nested_spans_reveal_by_their_own_extent() {
        let source = "*a **b** c*";
        let inner = shaped_revealing(source, caret_at(source, 5), None);
        assert_eq!(shown_text(&inner[0]), vec!["*a **b** c*"]);
        let outer = shaped_revealing(source, caret_at(source, 9), None);
        assert_eq!(shown_text(&outer[0]), vec!["*a b c*"]);
    }

    /// Marked text inside an entity's span opens it, the way it opens a style.
    #[test]
    fn marked_text_opens_an_entity() {
        let source = "a &amp; b";
        let marked = caret_at(source, 4);
        let rows = shaped_revealing(source, 0..0, Some(marked.start..marked.end + 1));
        assert_eq!(shown_text(&rows[0]), vec!["a &amp; b"]);
    }

    /// The shaping cache's key moves when what is revealed moves, and only
    /// then: a caret walking within one span, or outside every span, keeps the
    /// rows it was shaped with.
    #[test]
    fn the_reveal_key_changes_only_with_the_revealed_set() {
        let source = "x **ab** y &amp; z";
        let state = state_of(source);
        let projection = projection_of(&state);
        let images = crate::images::Images::default();
        let style = EditorStyle::notes();
        let types = callout_types();
        let key = |offset: usize, composition: Option<usize>| {
            let pos = |offset| projection.lines()[0].offset_to_pos(offset).unwrap();
            let input = ShapeInput {
                images: &images,
                spelling: None,
                wiki: None,
                doc: state.doc(),
                types: &types,
                projection: &projection,
                style: &style,
                single_line: false,
                selection: pos(offset)..pos(offset),
                composition: composition.map(|offset| pos(offset)..pos(offset) + 1),
            };
            super::reveal_key(&input)
        };
        assert_eq!(key(0, None), key(1, None), "outside every span");
        assert_eq!(key(3, None), key(5, None), "inside the same span");
        assert_eq!(key(2, None), key(8, None), "at either edge of it");
        assert_ne!(key(1, None), key(2, None), "onto the span's edge");
        assert_ne!(key(5, None), key(12, None), "from the span to the entity");
        assert_ne!(
            key(0, None),
            key(0, Some(12)),
            "marked text opens the entity"
        );
        assert_eq!(
            key(0, Some(9)),
            key(1, None),
            "marked text outside every span"
        );
    }

    fn shaped_revealing(
        source: &str,
        selection: std::ops::Range<usize>,
        composition: Option<std::ops::Range<usize>>,
    ) -> Vec<LayoutLine> {
        let state = state_of(source);
        let projection = projection_of(&state);
        let images = crate::images::Images::default();
        let spelling = markraft_commonmark::CommonMarkSpelling::new(state.schema().clone());
        let style = EditorStyle::notes();
        let types = callout_types();
        let input = ShapeInput {
            images: &images,
            spelling: Some(&spelling),
            wiki: None,
            doc: state.doc(),
            types: &types,
            projection: &projection,
            style: &style,
            single_line: false,
            selection,
            composition,
        };
        shape(&input, px(600.), &text_system())
    }
}
