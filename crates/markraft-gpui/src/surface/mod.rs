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
//! HTML is never rendered or interpreted: a raw block is drawn as the source
//! it holds, in the code font, and an inline HTML primitive as its source in
//! the prose around it, so a note shows exactly what it will be written back
//! as. The one tag drawn as what it means is a `<br>` in a table cell, which
//! is the cell's line break there, as Typora draws it.

use crate::style::EditorStyle;
use crate::{CaretShape, EditorView};
use gpui::{prelude::*, *};
use markraft_core::commands::ColumnAlignment;
use markraft_core::kind::DocTypes;
use markraft_core::kind::SourceHighlight;
use markraft_core::kind::conceal::{Reveal, Shown};
use markraft_core::projection::{Line, LineKind, Projection, Run, RunContent};
use markraft_core::{MarkSet, Node};
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use unicode_segmentation::UnicodeSegmentation;

mod atoms;
mod breaking;
mod chrome;
mod layout_line;
mod paint;
mod runs;
mod shape;
mod table;
#[cfg(test)]
mod tests;

pub(crate) use self::layout_line::{LayoutLine, selection_anchor_row};
pub(crate) use self::shape::ShapeInput;
#[cfg(test)]
pub(crate) use self::shape::shape;
pub(crate) use self::table::{TableScroll, cell_under, merge_row_centers};

use self::atoms::*;
use self::breaking::*;
use self::chrome::*;
use self::layout_line::*;
use self::paint::*;
use self::runs::*;
use self::shape::*;
use self::table::*;

/// A block caret over text is translucent rather than inverted: the run under it keeps
/// its own colour, weight and syntax highlighting, which repainting one grapheme would
/// have to reproduce.
const BLOCK_CARET_ALPHA: f32 = 0.42;
/// How wide a block caret is where there is no grapheme to cover, past the last one of a
/// line, as a share of the row's line height.
const EMPTY_BLOCK_CARET_RATIO: f32 = 0.4;
const CARET_THICKNESS: Pixels = px(2.);

const INLINE_CODE_SCALE: f32 = 0.86;
/// How large superscript and subscript are drawn, and how far a superscript's
/// row is raised and a subscript's lowered, as shares of the line's font size.
const SUPERSCRIPT_SCALE: f32 = 0.72;
const SUPERSCRIPT_LIFT: f32 = 0.3;
const SUBSCRIPT_DROP: f32 = 0.18;
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

/// The tallest the frame of a picture still being fetched is held open for.
const LOADING_FRAME_MAX_HEIGHT: Pixels = px(320.);
/// The widest a picture still being fetched is held open for. Its own size is not
/// known until it arrives, so the frame only has to read as a picture's place.
const LOADING_FRAME_MAX_WIDTH: Pixels = px(480.);
// SF Mono; Menlo is wider and heavier at this size.
const CODE_FONT: &str = ".AppleSystemUIFontMonospaced";
/// The face of the editor's own chrome — a pill's label, an emoji atom —,
/// which stays the system's whatever [`EditorStyle::font_family`] the note's
/// text is set in.
const UI_FONT: &str = ".SystemUIFont";
/// The rounded system design, which has no italic of its own.
const ROUNDED_FONT: &str = ".AppleSystemUIFontRounded";
const CODE_PADDING: Pixels = px(12.);
/// How far a code block's panel reaches above and below its text. It spells no
/// fences, focused or not — as in Typora — so this is padding and nothing else,
/// and the caret coming or going never changes the block's height.
const CODE_INSET: Pixels = px(12.);
/// The language tag a focused code block shows in its panel's top-right
/// corner, flush with the panel's top and right edges: its height and its
/// horizontal padding. It is drawn over the block's own text, so it takes no
/// room from the layout and never reaches past the panel.
const CODE_LANGUAGE_HEIGHT: Pixels = px(18.);
const CODE_LANGUAGE_PADDING: Pixels = px(6.);
const CODE_RADIUS: Pixels = px(12.);
/// The space between a picture's spelled-out source and the picture drawn
/// under it while the caret is in the source.
const PREVIEW_GAP: Pixels = px(6.);
/// How far a picture sharing its line with text stands inside the row, above
/// and below: it is drawn as tall as the row allows and no taller.
const INLINE_IMAGE_INSET: Pixels = px(2.);

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

/// What one paint produced, handed to the view as a value once prepaint is
/// done: the laid-out rows, where each grid stands, the box the content was
/// drawn in and the single-line field's sideways scroll.
///
/// The frame describes the last paint. After an edit it is a frame behind the
/// projection until the next prepaint replaces it, so a reader that maps a
/// position through it right after an edit may miss; hit testing and the
/// caret's geometry are asked between paints, when the two agree.
#[derive(Default)]
pub(crate) struct FrameLayout {
    rows: Vec<LayoutLine>,
    /// How far each grid that does not fit the note is scrolled sideways, keyed
    /// by the position before its table node. Kept across frames — it is the
    /// reader's place in the grid — and dropped when the grid is gone.
    tables: HashMap<usize, TableScroll>,
    /// The box the last frame drew the note's content in, which is what a grid
    /// is clipped to and what the table toolbar anchors inside.
    content_bounds: Bounds<Pixels>,
    single_line_scroll_x: Pixels,
}

impl FrameLayout {
    /// The laid-out rows, in document order.
    pub(crate) fn rows(&self) -> &[LayoutLine] {
        &self.rows
    }

    /// Where each overflowing grid stands.
    pub(crate) fn tables(&self) -> &HashMap<usize, TableScroll> {
        &self.tables
    }

    /// The grids' places, for a wheel event that moves one.
    pub(crate) fn tables_mut(&mut self) -> &mut HashMap<usize, TableScroll> {
        &mut self.tables
    }

    /// The box the content was drawn in.
    pub(crate) fn content_bounds(&self) -> Bounds<Pixels> {
        self.content_bounds
    }

    /// How far the single-line field is scrolled sideways.
    pub(crate) fn single_line_scroll_x(&self) -> Pixels {
        self.single_line_scroll_x
    }

    /// Forget the rows and the grids' places when the document is replaced;
    /// the content box stays what the last paint made it.
    pub(crate) fn clear(&mut self) {
        self.rows.clear();
        self.tables.clear();
        self.single_line_scroll_x = px(0.);
    }
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
                let column = column_width(width, view.style().max_line_width);
                let rows = shape_cached(view, column, window.text_system());
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
        // The element spans the view; the text is laid out in the column
        // inside it, and every row is placed there. Hit testing, the caret,
        // the selection and every popup read the rows, so they follow.
        let bounds = column_bounds(bounds, view.style().max_line_width);
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
                        view.frame.single_line_scroll_x(),
                        row.caret(offset, view.caret.upstream()).x - bounds.left(),
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
        let mut scroll = view.frame.tables().clone();
        scroll.retain(|table, _| overflows.contains_key(table));
        for (table, overflow) in &overflows {
            let entry = scroll.entry(*table).or_default();
            entry.overflow = *overflow;
            entry.offset = entry.offset.clamp(px(0.), *overflow);
        }
        // The caret's own cell is brought into the visible strip, as the
        // vertical reveal below brings its line into the viewport. Nothing is
        // animated either way, so reduced motion has nothing to turn off.
        if view.caret.reveal_pending()
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
            if editor.shaping().images().has_requests() {
                editor.fetch_remote_images(cx);
            }
            editor.set_frame(FrameLayout {
                rows: rows.clone(),
                tables: scroll,
                content_bounds: bounds,
                single_line_scroll_x,
            });
            if editor.caret.take_reveal() {
                if editor.single_line {
                    return;
                }
                let head = editor.head();
                if let Some((row, offset)) = rows.iter().find(|row| row.contains(head)).map(|row| {
                    let offset = row.pos_to_offset(head);
                    (row, offset)
                }) {
                    let caret = row.caret(offset, editor.caret.upstream());
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
        let upstream = editor.caret.upstream();
        let style = editor.style().clone();
        let scroll = editor.frame.tables().clone();
        let placeholder = (projection.line_count() == 1 && projection.plain_text().is_empty())
            .then(|| editor.placeholder.clone());
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.editor.clone()),
            cx,
        );
        // The text's own column, where prepaint placed the rows.
        let bounds = column_bounds(bounds, style.max_line_width);
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
                            let bar_x = row.quote_bar_x(levels, level, &style);
                            window.paint_quad(fill(
                                Bounds::new(point(bar_x, top), size(QUOTE_BAR, height)),
                                tones.get(level).copied().flatten().unwrap_or(style.marker),
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
                    Some(Decoration::Code {
                        levels,
                        joined,
                        tones,
                    }) => {
                        // The panel stands where a paragraph's text would in
                        // the same quotes, so the bars keep their places; they
                        // reach over the panel's padding.
                        let top = row.origin.y - row.top_gap;
                        for level in 0..levels {
                            let below = if level < joined {
                                row.height
                            } else {
                                row.text_height() + row.code_panel_room()
                            };
                            let bar_x = row.quote_bar_x(levels, level, &style);
                            window.paint_quad(fill(
                                Bounds::new(
                                    point(bar_x, top),
                                    size(QUOTE_BAR, row.top_gap + below),
                                ),
                                tones.get(level).copied().flatten().unwrap_or(style.marker),
                            ));
                        }
                        window.paint_quad(
                            fill(
                                Bounds::new(
                                    point(
                                        row.origin.x - CODE_PADDING,
                                        row.origin.y - row.code_panel_room(),
                                    ),
                                    size(
                                        row.width + CODE_PADDING * 2.,
                                        row.text_height() + row.code_panel_room() * 2.,
                                    ),
                                ),
                                style.code_background,
                            )
                            .corner_radii(Corners::all(CODE_RADIUS)),
                        );
                    }
                    None => {}
                }
                if let Some(header) = &row.callout_header {
                    let _ = header.label.paint(
                        row.callout_header_origin(),
                        CALLOUT_HEADER_HEIGHT,
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    );
                }
                // A run's own fill (a `==highlight==`) is not drawn by the line's
                // `paint`, which only lays down glyphs and their decorations. It
                // goes down here, over the block's chrome and under the
                // selection, so a selected highlight still reads as selected.
                for inner in &row.rows {
                    let origin =
                        row.origin + point(px(0.), row.line_height * inner.visual_start as f32);
                    let _ = inner.line.paint_background(
                        origin,
                        row.line_height,
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    );
                }
                // Inline code pills go over the fill: a code span inside a
                // highlight carries the fill across its whole slot, so the band
                // runs unbroken through it and the pill sits on top.
                for inner in &row.rows {
                    for code in inner.inline_code.iter().filter(|code| !code.raised) {
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
                        if matches!(row.decoration, Some(Decoration::Divider)) {
                            // A divider has no text to cover: a selection over it
                            // covers the whole rule, or a selected divider would
                            // show as the stub a selection leaves past a line end.
                            window.paint_quad(fill(
                                Bounds::new(row.origin, size(row.width, row.line_height)),
                                selection,
                            ));
                        } else {
                            for rect in row.rectangles(from..to.min(row.char_len), spans_next) {
                                window.paint_quad(fill(rect, selection));
                            }
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
                                + point(
                                    code.text_left(),
                                    row.line_height * code.visual_row as f32 - code.lift,
                                ),
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
                if let Some((image, size)) = &row.preview {
                    let bounds = Bounds::new(
                        row.origin + point(px(0.), row.text_height() + PREVIEW_GAP),
                        *size,
                    );
                    let _ = window.paint_image(
                        bounds,
                        bounds,
                        Corners::all(style.code_radius),
                        image.clone(),
                        0,
                        false,
                    );
                }
                if row.index == 0
                    && let Some(text) = placeholder.as_ref().filter(|text| !text.is_empty())
                {
                    // Presentation only: the empty block still owns hit testing and IME coordinates.
                    let run = TextRun {
                        len: text.len(),
                        font: font(style.font_family.clone()),
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
            // A focused code block's language tag sits in its panel's corner,
            // over the block's own text, so it goes down after every line.
            for row in rows.iter() {
                if let (Some(label), Some(bounds)) =
                    (&row.code_language, row.code_language_bounds())
                {
                    // A shade darker than the panel it sits on, and cut to its
                    // corner: rounded where the panel is, and on the one
                    // corner that stands inside it.
                    let mut tone = style.code_background;
                    tone.l = (tone.l - 0.04).max(0.);
                    window.paint_quad(fill(bounds, tone).corner_radii(Corners {
                        top_left: px(0.),
                        top_right: CODE_RADIUS,
                        bottom_right: px(0.),
                        bottom_left: px(6.),
                    }));
                    let _ = label.paint(
                        point(
                            bounds.left() + CODE_LANGUAGE_PADDING,
                            bounds.top() + (CODE_LANGUAGE_HEIGHT - row.line_height) * 0.5,
                        ),
                        row.line_height,
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    );
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
            let Some(grid) = editor.frame.tables_mut().get_mut(table) else {
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
