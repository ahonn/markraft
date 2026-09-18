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
use markraft_core::projection::{Line, LineKind, Projection, RunContent};
use markraft_core::{MarkSet, Node};
use std::ops::Range;
use std::rc::Rc;
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
// SF Mono; Menlo is wider and heavier at this size.
const CODE_FONT: &str = ".AppleSystemUIFontMonospaced";
const CODE_PADDING: Pixels = px(12.);
const CODE_HEADER_HEIGHT: Pixels = px(24.);
// The header controls sit in the block's top padding rather than below it.
const CODE_HEADER_LIFT: Pixels = px(8.);
const CODE_CHEVRON_WIDTH: Pixels = px(14.);
const CODE_COPY_WIDTH: Pixels = px(28.);
const CODE_RADIUS: Pixels = px(12.);

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

#[derive(Clone)]
struct CodeHeader {
    language: Rc<ShapedLine>,
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
    /// A code block's rounded background, behind the whole line.
    Code,
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

    /// The row a `char` offset falls in, and the byte offset within it.
    fn locate(&self, offset: usize) -> (usize, usize) {
        let index = self
            .rows
            .iter()
            .rposition(|row| row.char_start <= offset)
            .unwrap_or(0);
        let row = &self.rows[index];
        let local = offset.saturating_sub(row.char_start).min(row.char_len());
        (index, char_to_byte(row.text(), local))
    }

    /// Window-space button bounds, available only on a code line.
    pub(crate) fn code_language_bounds(&self) -> Option<Bounds<Pixels>> {
        let header = self.code_header.as_ref()?;
        let language_width = header.language.width + CODE_CHEVRON_WIDTH + px(12.);
        Some(Bounds::new(
            point(
                self.origin.x + self.width - CODE_COPY_WIDTH - language_width - px(2.),
                self.origin.y - CODE_HEADER_HEIGHT - CODE_HEADER_LIFT,
            ),
            size(language_width, CODE_HEADER_HEIGHT),
        ))
    }

    fn code_copy_bounds(&self) -> Option<Bounds<Pixels>> {
        self.code_header.as_ref()?;
        Some(Bounds::new(
            point(
                self.origin.x + self.width - CODE_COPY_WIDTH,
                self.origin.y - CODE_HEADER_HEIGHT - CODE_HEADER_LIFT,
            ),
            size(CODE_COPY_WIDTH, CODE_HEADER_HEIGHT),
        ))
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
        row.char_start + byte_to_char(row.text(), byte)
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
                let from = (row.char_start + byte_to_char(text, start)).min(self.char_len);
                let to = (row.char_start + byte_to_char(text, end)).min(self.char_len);
                ranges.push(from..to);
            }
        }
        ranges
    }

    /// The quads covering `chars`, one per visual row it touches. `newline` adds a
    /// stub past the end of the line, for a selection that spans into the next.
    pub(crate) fn rectangles(&self, chars: Range<usize>, newline: bool) -> Vec<Bounds<Pixels>> {
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
                let trailing = newline && absolute == last_visual && chars.end >= self.char_len;
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

pub(crate) fn shape(input: &ShapeInput<'_>, width: Pixels, window: &Window) -> Vec<LayoutLine> {
    (0..input.projection.line_count())
        .map(|index| shape_line(input, index, width, window))
        .collect()
}

fn shape_line(input: &ShapeInput<'_>, index: usize, width: Pixels, window: &Window) -> LayoutLine {
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
    let font_size = style.font_size(heading, code);
    let line_height = font_size * style.line_height_ratio;
    // Keep deeply nested imported content editable in a narrow note. Only its
    // visual indentation is capped; document depth is preserved.
    let max_indent = (width - px(80.)).max(style.quote_indent);
    let quote_levels = types.quote_depth(line);
    let number = ordered_marker(doc, types, line, style, font_size, window);
    let indent = indent_of(types, line, style, number.as_ref().map(|(_, w)| *w)).min(max_indent);
    let marker = marker_of(types, line, style, number.map(|(shaped, _)| shaped));
    let decoration = if code {
        Some(Decoration::Code)
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

    let text = display_text(projection, types, line, index);
    let runs = text_runs(input, line, &text, heading, code, font_size, style);
    let wrap_width = (width - indent - if code { CODE_PADDING } else { px(0.) }).max(px(40.));
    let shaped = window
        .text_system()
        .shape_text(
            text.text.clone().into(),
            font_size,
            &runs.runs,
            (!single_line).then_some(wrap_width),
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
    let top_gap = if index == 0 {
        if code {
            CODE_PADDING + CODE_HEADER_HEIGHT
        } else {
            px(0.)
        }
    } else if code {
        CODE_PADDING + CODE_HEADER_HEIGHT
    } else if let Some(level) = heading {
        style.heading_top_gap(level)
    } else {
        px(0.)
    };
    let code_header = code
        .then(|| {
            code_header(
                types.code_language(line).unwrap_or(""),
                width,
                style,
                window,
            )
        })
        .flatten();

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
    };
    if single_line && let Some(row) = layout.rows.first() {
        layout.width = row.line.size(line_height).width.max(wrap_width);
    }
    layout.height = layout.text_height() + gap;
    shape_inline_code(&mut layout, &text.text, &runs.code, font_size, window);
    layout
}

/// A line's display text, and whether it stands in for content the caret cannot
/// enter.
struct DisplayText {
    text: String,
    /// True when the text is a stand-in — a placeholder space for an empty line,
    /// or the source of a raw block — so no run may be derived from the content.
    synthetic: bool,
}

fn display_text(
    projection: &Projection,
    types: &DocTypes,
    line: &Line,
    index: usize,
) -> DisplayText {
    if line.kind == LineKind::LeafBlock {
        let source = line
            .ancestors
            .last()
            .filter(|own| Some(own.node_type) == types.raw_block)
            .and_then(|own| own.attrs.get("source"))
            .and_then(|value| value.as_str())
            .unwrap_or(" ");
        return DisplayText {
            text: source.to_owned(),
            synthetic: true,
        };
    }
    let text = projection.line_text(index).unwrap_or_default();
    if text.is_empty() {
        return DisplayText {
            text: " ".to_owned(),
            synthetic: true,
        };
    }
    DisplayText {
        text: text.to_owned(),
        synthetic: false,
    }
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
    let checked_item = types.item_of(line).is_some_and(|(item, _)| {
        Some(item.node_type) == types.task_item && DocTypes::task_checked(&item.attrs)
    });
    let text_color = if checked_item {
        style.muted_text
    } else {
        style.text
    };
    if text.synthetic {
        let mut face = font(".SystemUIFont");
        if heading.is_some() {
            face.weight = FontWeight::BOLD;
        }
        let raw = line.kind == LineKind::LeafBlock && text.text != " ";
        if raw {
            face = font(CODE_FONT);
        }
        return Runs {
            runs: vec![TextRun {
                len: text.text.len(),
                font: face,
                color: if raw { style.muted_text } else { text_color },
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
    for run in &line.runs {
        let chars = run.char_to - run.char_from;
        let len: usize = text.text[byte..]
            .chars()
            .take(chars)
            .map(char::len_utf8)
            .sum();
        let range = byte..byte + len;
        byte += len;
        if len == 0 {
            continue;
        }
        let marks = &run.marks;
        let is_code = code_block || has(types.code, marks);
        let is_link = has(types.link, marks);
        let mut face = font(if is_code { CODE_FONT } else { ".SystemUIFont" });
        if has(types.strong, marks) || heading.is_some() {
            face.weight = FontWeight::BOLD;
        }
        if has(types.em, marks) {
            face.style = FontStyle::Italic;
        }
        let atom = matches!(run.content, RunContent::Atom(_));
        let ink = if is_link || (atom && !code_block) {
            style.link
        } else if has(types.code, marks) {
            style.inline_code_text
        } else {
            text_color
        };
        // Inline code only reserves its space here; see `InlineCode`.
        let color = if has(types.code, marks) && !code_block {
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
            underline: (has(types.underline, marks) || is_link).then_some(UnderlineStyle {
                thickness: px(1.),
                color: Some(ink),
                wavy: false,
            }),
            strikethrough: has(types.strikethrough, marks).then_some(StrikethroughStyle {
                thickness: px(1.),
                color: Some(ink),
            }),
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
    // A quote sits tight against the line above it, and quote lines against each other.
    if input
        .projection
        .line(index + 1)
        .is_some_and(|next| input.types.quote_depth(next) > 0)
    {
        return px(0.);
    }
    let _ = line;
    if code {
        return CODE_PADDING + style.paragraph_gap;
    }
    if marker.is_some() || input.types.item_of(line).is_some() {
        return style.list_gap;
    }
    if heading.is_some() {
        return style.heading_bottom_gap;
    }
    style.paragraph_gap
}

fn code_header(
    language: &str,
    width: Pixels,
    style: &EditorStyle,
    window: &Window,
) -> Option<CodeHeader> {
    let label = crate::syntax::code_languages()
        .iter()
        .find(|(id, _)| *id == language)
        .map_or(language, |(_, label)| *label);
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
    let language_width = (width - CODE_COPY_WIDTH - CODE_CHEVRON_WIDTH - px(30.)).max(px(20.));
    let mut label = label.graphemes(true).take(64).collect::<Vec<_>>();
    let mut shaped = shape_label(label.concat());
    while shaped.width > language_width && !label.is_empty() {
        label.pop();
        shaped = shape_label(format!("{}…", label.concat()));
    }
    Some(CodeHeader { language: shaped })
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
        let slots = layout.rectangles(chars.clone(), false);
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
                let Some(slot) = layout.rectangles(chars, false).into_iter().next() else {
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
            if let Some(header) = &row.code_header {
                paint_code_header(self, row, header, &style, window, cx);
            }
            if a != b {
                let from = row.pos_to_offset(a);
                let to = row.pos_to_offset(b);
                if b > row.from && a <= row.to() {
                    let spans_next = b > row.to();
                    for rect in row.rectangles(from..to.min(row.char_len), spans_next) {
                        window.paint_quad(fill(
                            rect,
                            if focused {
                                rgba(0xb9d5efb0)
                            } else {
                                rgba(0xd4d9de90)
                            },
                        ));
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
    let language = row
        .code_language_bounds()
        .expect("code header has button bounds");
    let copy = row
        .code_copy_bounds()
        .expect("code header has button bounds");
    let _ = header.language.paint(
        language.origin + point(px(6.), px(3.)),
        px(18.),
        TextAlign::Left,
        None,
        window,
        cx,
    );
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
    use super::LayoutLine;
    use crate::typeahead::tests::state_of;
    use gpui::{Pixels, point, px};
    use markraft_core::projection::projection_of;

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
