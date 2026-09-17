use crate::{EditorEvent, EditorStyle, EditorView};
use gpui::{prelude::*, *};
use markraft_core::{BlockKind, Document};
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone)]
pub(crate) struct LayoutBlock {
    pub line: std::rc::Rc<WrappedLine>,
    pub origin: Point<Pixels>,
    pub line_height: Pixels,
    pub height: Pixels,
    pub width: Pixels,
    pub text_len: usize,
    pub(crate) top_gap: Pixels,
    marker: Option<Marker>,
    decoration: Option<Decoration>,
    inline_code: Vec<InlineCode>,
    code_header: Option<CodeHeader>,
    code_hitboxes: Option<(Hitbox, Hitbox)>,
}

/// Inline code on one visual row. Text runs share one font size, so the main line only
/// reserves the space and the code is painted again, smaller, centred in that slot.
/// What is left of the slot on either side becomes the pill's padding.
#[derive(Clone)]
struct InlineCode {
    range: Range<usize>,
    row: usize,
    /// Relative to the block's origin.
    left: Pixels,
    slot: Pixels,
    line: std::rc::Rc<ShapedLine>,
}

impl InlineCode {
    fn text_left(&self) -> Pixels {
        self.left + (self.slot - self.line.width) / 2.
    }
}

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

#[derive(Clone)]
struct CodeHeader {
    language: std::rc::Rc<ShapedLine>,
}

#[derive(Clone, Copy)]
enum Decoration {
    /// One bar per visible nesting level; shared levels join across quote gaps.
    Quote {
        levels: usize,
        joined_levels: usize,
    },
    Divider,
    /// One line of a code block; the first and last lines carry its padding.
    Code {
        first: bool,
        last: bool,
    },
}

#[derive(Clone)]
enum Marker {
    Glyph(std::rc::Rc<ShapedLine>),
    /// An ordered-list number, right-aligned against the text.
    Number(std::rc::Rc<ShapedLine>),
    Bullet {
        depth: u8,
    },
    Task {
        checked: bool,
    },
}

impl LayoutBlock {
    /// Window-space button bounds, available only on the first code line.
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
        let width = CODE_COPY_WIDTH;
        Some(Bounds::new(
            point(
                self.origin.x + self.width - width,
                self.origin.y - CODE_HEADER_HEIGHT - CODE_HEADER_LIFT,
            ),
            size(width, CODE_HEADER_HEIGHT),
        ))
    }
    pub(crate) fn marker_bounds(&self) -> Option<Bounds<Pixels>> {
        let marker = self.marker.as_ref()?;
        let (offset, width, height) = match marker {
            Marker::Glyph(line) => (px(26.), line.width, self.line_height),
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
    pub fn caret(&self, byte: usize, upstream: bool) -> Point<Pixels> {
        if !upstream {
            for (row, boundary) in self.line.wrap_boundaries().iter().enumerate() {
                if self.line.runs()[boundary.run_ix].glyphs[boundary.glyph_ix].index == byte {
                    return self.origin + point(px(0.), self.line_height * (row + 1));
                }
            }
        }
        let mut position = self
            .line
            .position_for_index(byte.min(self.text_len), self.line_height)
            .unwrap_or_default();
        let row = (position.y / self.line_height).round() as usize;
        if let Some(x) = self.inline_code_x(byte, row) {
            position.x = x;
        }
        self.origin + position
    }
    /// Where a position strictly inside inline code is drawn. Its edges keep the
    /// slot's own bounds, so the caret rests outside the pill there.
    fn inline_code_x(&self, byte: usize, row: usize) -> Option<Pixels> {
        let code = self
            .inline_code
            .iter()
            .find(|code| code.row == row && code.range.start < byte && byte < code.range.end)?;
        Some(code.text_left() + code.line.x_for_index(byte - code.range.start))
    }
    /// The position under `local` (relative to the origin) when it falls on inline code.
    pub(crate) fn inline_code_index(&self, local: Point<Pixels>) -> Option<usize> {
        let row = (local.y / self.line_height).floor() as usize;
        let code = self.inline_code.iter().find(|code| {
            code.row == row && code.left <= local.x && local.x <= code.left + code.slot
        })?;
        Some(code.range.start + code.line.closest_index_for_x(local.x - code.text_left()))
    }
    fn row_starts(&self) -> Vec<usize> {
        let mut starts = vec![0];
        starts.extend(
            self.line
                .wrap_boundaries()
                .iter()
                .map(|b| self.line.runs()[b.run_ix].glyphs[b.glyph_ix].index),
        );
        starts
    }
    pub(crate) fn rectangles(&self, range: Range<usize>, newline: bool) -> Vec<Bounds<Pixels>> {
        let starts = self.row_starts();
        let mut rectangles = vec![];
        for (row, &start) in starts.iter().enumerate() {
            let end = starts.get(row + 1).copied().unwrap_or(self.text_len);
            let a = range.start.max(start);
            let b = range.end.min(end);
            if a >= b && !(newline && row + 1 == starts.len() && range.end >= self.text_len) {
                continue;
            }
            let y = self.origin.y + self.line_height * row;
            // Query the unwrapped layout indirectly using the visual row hit positions.
            let x_for = |index: usize| {
                if index == start {
                    return px(0.);
                }
                let p = self
                    .line
                    .position_for_index(index, self.line_height)
                    .unwrap_or_default();
                if p.y < self.line_height * row {
                    px(0.)
                } else if p.y > self.line_height * row {
                    self.width
                } else {
                    self.inline_code_x(index, row).unwrap_or(p.x)
                }
            };
            let left = x_for(a);
            let right = if b == end && row + 1 < starts.len() {
                self.width
            } else {
                x_for(b)
            };
            rectangles.push(Bounds::new(
                point(self.origin.x + left, y),
                size(
                    (right - left
                        + if newline && row + 1 == starts.len() {
                            px(7.)
                        } else {
                            px(0.)
                        })
                    .max(px(2.)),
                    self.line_height,
                ),
            ));
        }
        rectangles
    }
}

fn shape(
    document: &Document,
    style: &EditorStyle,
    single_line: bool,
    width: Pixels,
    window: &Window,
) -> Vec<LayoutBlock> {
    let mut highlights = vec![None; document.blocks.len()];
    for (index, block) in document.blocks.iter().enumerate() {
        if let BlockKind::Code { language } = &block.kind
            && let Some(range) = document.code_block_range(index)
            && range.start == index
        {
            let text = document.blocks[range.clone()]
                .iter()
                .map(|block| block.text())
                .collect::<Vec<_>>()
                .join("\n");
            let lines = crate::syntax::highlight(&text, language, style.background.l < 0.5);
            for (offset, row) in range.enumerate() {
                highlights[row] = lines.get(offset).cloned();
            }
        }
    }
    document
        .blocks
        .iter()
        .enumerate()
        .map(|(index, block)| {
            let font_size = style.font_size(&block.kind);
            let line_height = font_size * style.line_height_ratio;
            // Keep deeply nested imported content editable in a narrow note.
            // Only its visual indentation is capped; document depth is preserved.
            let max_indent = (width - px(80.)).max(style.quote_indent);
            let quote_levels = (block.depth as usize + 1)
                .min((max_indent / style.quote_indent.max(px(1.))) as usize)
                .max(1);
            let marker = match block.kind {
                BlockKind::Bullet => Some("•"),
                BlockKind::Task { checked: false } => Some("☐"),
                BlockKind::Task { checked: true } => Some("☑"),
                _ => None,
            };
            let decoration = match block.kind {
                BlockKind::Quote => Some(Decoration::Quote {
                    levels: quote_levels,
                    joined_levels: document
                        .blocks
                        .get(index + 1)
                        .filter(|next| next.kind == BlockKind::Quote)
                        .map_or(0, |next| (next.depth as usize + 1).min(quote_levels)),
                }),
                BlockKind::Divider => Some(Decoration::Divider),
                BlockKind::Code { .. } => {
                    let range = document
                        .code_block_range(index)
                        .expect("code line has a range");
                    Some(Decoration::Code {
                        first: range.start == index,
                        last: range.end == index + 1,
                    })
                }
                _ => None,
            };
            let shape_number = |ordinal: usize| {
                let text = format!("{ordinal}.");
                window.text_system().shape_line(
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
                )
            };
            let number = document
                .ordinal(index)
                .map(|ordinal| std::rc::Rc::new(shape_number(ordinal)));
            // Every item of a run shares the indent its widest number needs.
            let number_width = document.ordinal(index).map(|ordinal| {
                let last = document.blocks[index + 1..]
                    .iter()
                    .enumerate()
                    .take_while(|(_, next)| {
                        next.depth > block.depth
                            || (next.depth == block.depth && next.kind == BlockKind::Ordered)
                    })
                    .filter(|(_, next)| next.depth == block.depth)
                    .filter_map(|(offset, _)| document.ordinal(index + offset + 1))
                    .last()
                    .unwrap_or(ordinal);
                shape_number(last).width
            });
            let indent = if let Some(width) = number_width {
                style.list_indent * block.depth as usize + style.list_indent.max(width + NUMBER_GAP)
            } else if marker.is_some() {
                style.list_indent * (block.depth as usize + 1)
            } else if block.kind == BlockKind::Quote {
                style.quote_indent * quote_levels
            } else if matches!(block.kind, BlockKind::Code { .. }) {
                CODE_PADDING
            } else {
                px(0.)
            }
            .min(max_indent);
            let text_color = if matches!(block.kind, BlockKind::Task { checked: true }) {
                style.muted_text
            } else {
                style.text
            };
            let mut code_ranges = vec![];
            let mut byte_offset = 0;
            let text = block.text();
            let text_len = text.len();
            let mut runs: Vec<TextRun> = block
                .spans
                .iter()
                .map(|span| {
                    let span_range = byte_offset..byte_offset + span.text.len();
                    byte_offset += span.text.len();
                    let code_block = matches!(block.kind, BlockKind::Code { .. });
                    let mut face = font(if span.marks.code || code_block {
                        CODE_FONT
                    } else {
                        ".SystemUIFont"
                    });
                    if span.marks.bold || matches!(block.kind, BlockKind::Heading(_)) {
                        face.weight = FontWeight::BOLD;
                    }
                    if span.marks.italic {
                        face.style = FontStyle::Italic;
                    }
                    let ink = if span.link.is_some() {
                        style.link
                    } else if span.marks.code {
                        style.inline_code_text
                    } else {
                        text_color
                    };
                    // Inline code only reserves its space here; see `InlineCode`.
                    let color = if span.marks.code {
                        code_ranges.push((span_range, face.clone(), ink));
                        gpui::transparent_black()
                    } else {
                        ink
                    };
                    TextRun {
                        len: span.text.len(),
                        font: face,
                        color,
                        background_color: None,
                        underline: (span.marks.underline || span.link.is_some()).then_some(
                            UnderlineStyle {
                                thickness: px(1.),
                                color: Some(ink),
                                wavy: false,
                            },
                        ),
                        strikethrough: span.marks.strikethrough.then_some(StrikethroughStyle {
                            thickness: px(1.),
                            color: Some(ink),
                        }),
                    }
                })
                .collect();
            if let Some(highlighted) = &highlights[index]
                && highlighted.iter().map(|(len, _)| len).sum::<usize>() == text_len
            {
                runs = highlighted
                    .iter()
                    .map(|(len, syntax)| {
                        let mut face = font(CODE_FONT);
                        // Themes embolden keywords; at note size colour alone reads calmer.
                        if syntax
                            .font_style
                            .contains(syntect::highlighting::FontStyle::ITALIC)
                        {
                            face.style = FontStyle::Italic;
                        }
                        let color = syntax.foreground;
                        TextRun {
                            len: *len,
                            font: face,
                            color: rgb(((color.r as u32) << 16)
                                | ((color.g as u32) << 8)
                                | color.b as u32)
                            .into(),
                            background_color: None,
                            underline: None,
                            strikethrough: None,
                        }
                    })
                    .collect();
            }
            let display = if text.is_empty() {
                runs = vec![TextRun {
                    len: 1,
                    font: font(".SystemUIFont"),
                    color: text_color,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                }];
                " ".to_string()
            } else {
                text
            };
            let code = match decoration {
                Some(Decoration::Code { first, last }) => Some((first, last)),
                _ => None,
            };
            let wrap_width =
                (width - indent - if code.is_some() { CODE_PADDING } else { px(0.) }).max(px(40.));
            let line = window
                .text_system()
                .shape_text(
                    display.into(),
                    font_size,
                    &runs,
                    (!single_line).then_some(wrap_width),
                    None,
                )
                .expect("valid UTF-8 text can be shaped")
                .remove(0);
            let marker = number.map(Marker::Number).or_else(|| {
                marker.map(|text| {
                    if style.draw_markers {
                        return match block.kind {
                            BlockKind::Task { checked } => Marker::Task { checked },
                            _ => Marker::Bullet { depth: block.depth },
                        };
                    }
                    Marker::Glyph(std::rc::Rc::new(window.text_system().shape_line(
                        text.into(),
                        px(18.),
                        &[TextRun {
                            len: text.len(),
                            font: font(".SystemUIFont"),
                            color: style.marker,
                            background_color: None,
                            underline: None,
                            strikethrough: None,
                        }],
                        None,
                    )))
                })
            });
            let quote_follows = document
                .blocks
                .get(index + 1)
                .is_some_and(|next| next.kind == BlockKind::Quote);
            // A quote sits tight against the line above it, and quote lines against each other.
            let gap = if single_line || quote_follows {
                px(0.)
            } else if let Some((_, last)) = code {
                if last {
                    CODE_PADDING + style.paragraph_gap
                } else {
                    px(0.)
                }
            } else if marker.is_some() {
                style.list_gap
            } else if matches!(block.kind, BlockKind::Heading(_)) {
                style.heading_bottom_gap
            } else {
                style.paragraph_gap
            };
            let height = line.size(line_height).height + gap;
            let width = if single_line {
                line.size(line_height).width.max(wrap_width)
            } else {
                wrap_width
            };
            let top_gap = if let (true, BlockKind::Heading(level)) = (index > 0, &block.kind) {
                style.heading_top_gaps[usize::from(*level).clamp(1, 3) - 1]
            } else if let Some((true, _)) = code {
                CODE_PADDING + CODE_HEADER_HEIGHT
            } else {
                px(0.)
            };
            let code_header = if let Some((true, _)) = code {
                let language = match &block.kind {
                    BlockKind::Code { language } => language.as_str(),
                    _ => "",
                };
                let label = crate::syntax::code_languages()
                    .iter()
                    .find(|(id, _)| *id == language)
                    .map_or(language, |(_, label)| *label);
                let shape_label = |label: String| {
                    std::rc::Rc::new(window.text_system().shape_line(
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
                let language_width =
                    (width - CODE_COPY_WIDTH - CODE_CHEVRON_WIDTH - px(30.)).max(px(20.));
                let mut label = label.graphemes(true).take(64).collect::<Vec<_>>();
                let mut language = shape_label(label.concat());
                while language.width > language_width && !label.is_empty() {
                    label.pop();
                    language = shape_label(format!("{}…", label.concat()));
                }
                Some(CodeHeader { language })
            } else {
                None
            };
            let mut layout = LayoutBlock {
                line: std::rc::Rc::new(line),
                origin: point(indent, px(0.)),
                line_height,
                height,
                width,
                text_len,
                marker,
                decoration,
                inline_code: vec![],
                top_gap,
                code_header,
                code_hitboxes: None,
            };
            let text = block.text();
            let starts = layout.row_starts();
            for (range, face, ink) in code_ranges {
                let parts: Vec<_> = starts
                    .iter()
                    .enumerate()
                    .filter_map(|(row, &start)| {
                        let end = starts.get(row + 1).copied().unwrap_or(text_len);
                        let part = range.start.max(start)..range.end.min(end);
                        let slot = layout.rectangles(part.clone(), false).into_iter().next()?;
                        (!part.is_empty()).then_some((row, part, slot))
                    })
                    .collect();
                // The slot is as wide as full-size text. Shrinking by a fixed ratio would
                // leave long spans mostly padding, so the size is chosen to leave about
                // `INLINE_CODE_PADDING` on either side of each row instead.
                let reserved: Pixels = parts.iter().map(|(_, _, slot)| slot.size.width).sum();
                let padding = INLINE_CODE_PADDING * 2. * parts.len() as f32;
                let scale =
                    ((reserved - padding) / reserved.max(px(1.))).clamp(INLINE_CODE_SCALE, 1.);
                for (row, part, slot) in parts {
                    let line = window.text_system().shape_line(
                        text[part.clone()].to_owned().into(),
                        font_size * scale,
                        &[TextRun {
                            len: part.len(),
                            font: face.clone(),
                            color: ink,
                            background_color: None,
                            underline: None,
                            strikethrough: None,
                        }],
                        None,
                    );
                    layout.inline_code.push(InlineCode {
                        range: part,
                        row,
                        left: slot.origin.x - layout.origin.x,
                        slot: slot.size.width,
                        line: std::rc::Rc::new(line),
                    });
                }
            }
            layout
        })
        .collect()
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
    type PrepaintState = Vec<LayoutBlock>;
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
                let rows = shape(
                    view.document(),
                    &view.style,
                    view.single_line,
                    width,
                    window,
                );
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
    ) -> Vec<LayoutBlock> {
        let view = self.editor.read(cx);
        let mut rows = shape(
            view.document(),
            &view.style,
            view.single_line,
            bounds.size.width,
            window,
        );
        let mut y = bounds.top();
        for row in &mut rows {
            y += row.top_gap;
            row.origin += point(bounds.left(), y);
            y += row.height;
        }
        let single_line_scroll_x = if view.single_line {
            rows.first()
                .map(|row| {
                    crate::single_line::scroll_offset(
                        view.single_line_scroll_x,
                        row.caret(view.core.selection().head.byte, view.upstream).x - bounds.left(),
                        row.line.size(row.line_height).width,
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
                view.document(),
                view.core.selection(),
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
                let head = editor.core.selection().head;
                if let Some(row) = rows.get(head.block) {
                    let caret = row.caret(head.byte, editor.upstream);
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
        rows: &mut Vec<LayoutBlock>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let editor = self.editor.read(cx);
        let selection = editor.core.selection();
        let key = |p: markraft_core::Position| (p.block, p.byte);
        let (a, b) = if key(selection.anchor) <= key(selection.head) {
            (selection.anchor, selection.head)
        } else {
            (selection.head, selection.anchor)
        };
        let marked = editor.core.marked_range().map(|r| {
            (
                editor.core.utf16_to_position(r.start),
                editor.core.utf16_to_position(r.end),
            )
        });
        let focused = editor.focus.is_focused(window);
        let caret_visible = editor.caret_blink.visible && window.is_window_active();
        let focus = editor.focus.clone();
        let upstream = editor.upstream;
        let style = editor.style.clone();
        let placeholder = (editor.document().blocks.len() == 1
            && editor.document().blocks[0].kind == BlockKind::Paragraph
            && editor.document().blocks[0].text().is_empty())
        .then(|| editor.placeholder.clone());
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.editor.clone()),
            cx,
        );
        for (i, row) in rows.iter().enumerate() {
            for code in &row.inline_code {
                // A pill shorter than the line.
                let inset = (row.line_height * 0.1).round();
                let pill = Bounds::new(
                    row.origin + point(code.left, row.line_height * code.row + inset),
                    size(code.slot, row.line_height - inset * 2.),
                );
                window.paint_quad(
                    fill(pill, style.inline_code_background).corner_radii(style.code_radius),
                );
            }
            match row.decoration {
                Some(Decoration::Quote {
                    levels,
                    joined_levels,
                }) => {
                    for level in 0..levels {
                        let height = if level < joined_levels {
                            row.height
                        } else {
                            row.line.size(row.line_height).height
                        };
                        window.paint_quad(fill(
                            Bounds::new(
                                point(
                                    row.origin.x - style.quote_indent * (levels - level),
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
                Some(Decoration::Code { first, last }) => {
                    let top = if first {
                        CODE_PADDING + CODE_HEADER_HEIGHT
                    } else {
                        px(0.)
                    };
                    let bottom = if last { CODE_PADDING } else { px(0.) };
                    let radius = |rounded: bool| if rounded { CODE_RADIUS } else { px(0.) };
                    window.paint_quad(
                        fill(
                            Bounds::new(
                                point(row.origin.x - CODE_PADDING, row.origin.y - top),
                                size(
                                    row.width + CODE_PADDING * 2.,
                                    row.line.size(row.line_height).height + top + bottom,
                                ),
                            ),
                            style.code_background,
                        )
                        .corner_radii(Corners {
                            top_left: radius(first),
                            top_right: radius(first),
                            bottom_left: radius(last),
                            bottom_right: radius(last),
                        }),
                    );
                }
                None => {}
            }
            if let Some(header) = &row.code_header {
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
                if let Some((language, copy)) = &row.code_hitboxes {
                    window.set_cursor_style(CursorStyle::PointingHand, language);
                    window.set_cursor_style(CursorStyle::PointingHand, copy);
                    let language = language.clone();
                    let copy = copy.clone();
                    let editor = self.editor.clone();
                    window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                        if !phase.bubble() || event.button != MouseButton::Left {
                            return;
                        }
                        let language_clicked = language.is_hovered_at(event.position, window);
                        let copy_clicked = copy.is_hovered_at(event.position, window);
                        if (language_clicked || copy_clicked) && editor.read(cx).is_composing() {
                            editor.update(cx, |editor, cx| editor.cancel_composition(cx));
                            // Cancelling can restore a different block list. The next click
                            // must use the freshly rendered hitboxes and block indices.
                            cx.stop_propagation();
                            return;
                        }
                        if language_clicked {
                            editor.update(cx, |_, cx| {
                                cx.emit(EditorEvent::CodeLanguageRequested { block: i });
                            });
                            cx.stop_propagation();
                        } else if copy_clicked {
                            let view = editor.read(cx);
                            if let Some(range) = view.document().code_block_range(i) {
                                let text = view.document().blocks[range]
                                    .iter()
                                    .map(|block| block.text())
                                    .collect::<Vec<_>>()
                                    .join("\n");
                                cx.write_to_clipboard(ClipboardItem::new_string(text));
                                editor.update(cx, |_, cx| cx.emit(EditorEvent::CodeCopied));
                            }
                            cx.stop_propagation();
                        }
                    });
                }
            }
            if a != b && i >= a.block && i <= b.block {
                let start = if i == a.block { a.byte } else { 0 };
                let end = if i == b.block { b.byte } else { row.text_len };
                for rect in row.rectangles(start..end, i < b.block) {
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
            let _ = row.line.paint(
                row.origin,
                row.line_height,
                TextAlign::Left,
                None,
                window,
                cx,
            );
            for code in &row.inline_code {
                let _ = code.line.paint(
                    row.origin + point(code.text_left(), row.line_height * code.row),
                    row.line_height,
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                );
            }
            if let Some(marker) = &row.marker {
                let marker_bounds = row.marker_bounds().expect("marker has bounds");
                match marker {
                    Marker::Glyph(line) | Marker::Number(line) => {
                        let _ = line.paint(
                            marker_bounds.origin,
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
                                marker_bounds,
                                radius,
                                style.background,
                                px(1.),
                                style.marker,
                                BorderStyle::Solid,
                            ));
                        } else {
                            window
                                .paint_quad(fill(marker_bounds, style.marker).corner_radii(radius));
                        }
                    }
                    Marker::Task { checked } => {
                        window.paint_quad(quad(
                            marker_bounds,
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
                            let origin = marker_bounds.origin;
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
            if i == 0
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
                && i >= start.block
                && i <= end.block
            {
                let a = if i == start.block { start.byte } else { 0 };
                let b = if i == end.block {
                    end.byte
                } else {
                    row.text_len
                };
                for mut rect in row.rectangles(a..b, false) {
                    rect.origin.y += rect.size.height - px(2.);
                    rect.size.height = px(1.5);
                    window.paint_quad(fill(rect, style.marker));
                }
            }
            if focused && caret_visible && a == b && i == b.block {
                window.paint_quad(fill(
                    Bounds::new(row.caret(b.byte, upstream), size(px(2.), row.line_height)),
                    style.marker,
                ));
            }
        }
    }
}
