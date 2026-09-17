use crate::{EditorStyle, EditorView};
use gpui::{prelude::*, *};
use markraft_core::{BlockKind, Document};
use std::ops::Range;

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
    code_ranges: Vec<Range<usize>>,
}

#[derive(Clone)]
enum Marker {
    Glyph(std::rc::Rc<ShapedLine>),
    Bullet,
    Task { checked: bool },
}

impl LayoutBlock {
    pub(crate) fn marker_bounds(&self) -> Option<Bounds<Pixels>> {
        let marker = self.marker.as_ref()?;
        let (offset, width, height) = match marker {
            Marker::Glyph(line) => (px(26.), line.width, self.line_height),
            // Drawn markers share one center, 15px left of the text.
            Marker::Bullet => (px(17.5), px(5.), px(5.)),
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
        self.origin
            + self
                .line
                .position_for_index(byte.min(self.text_len), self.line_height)
                .unwrap_or_default()
    }
    fn rectangles(&self, range: Range<usize>, newline: bool) -> Vec<Bounds<Pixels>> {
        let mut starts = vec![0];
        starts.extend(
            self.line
                .wrap_boundaries()
                .iter()
                .map(|b| self.line.runs()[b.run_ix].glyphs[b.glyph_ix].index),
        );
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
                    p.x
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
    document
        .blocks
        .iter()
        .enumerate()
        .map(|(index, block)| {
            let font_size = style.font_size(&block.kind);
            let line_height = font_size * style.line_height_ratio;
            let marker = match block.kind {
                BlockKind::Bullet => Some("•"),
                BlockKind::Task { checked: false } => Some("☐"),
                BlockKind::Task { checked: true } => Some("☑"),
                _ => None,
            };
            let indent = if marker.is_some() {
                style.list_indent
            } else {
                px(0.)
            };
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
                    if span.marks.code {
                        code_ranges.push(byte_offset..byte_offset + span.text.len());
                    }
                    byte_offset += span.text.len();
                    let mut face = font(if span.marks.code {
                        "Menlo"
                    } else {
                        ".SystemUIFont"
                    });
                    if span.marks.bold || matches!(block.kind, BlockKind::Heading(_)) {
                        face.weight = FontWeight::BOLD;
                    }
                    if span.marks.italic {
                        face.style = FontStyle::Italic;
                    }
                    TextRun {
                        len: span.text.len(),
                        font: face,
                        color: text_color,
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    }
                })
                .collect();
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
            let wrap_width = (width - indent).max(px(40.));
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
            let marker = marker.map(|text| {
                if style.draw_markers {
                    return match block.kind {
                        BlockKind::Task { checked } => Marker::Task { checked },
                        _ => Marker::Bullet,
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
            });
            let gap = if single_line {
                px(0.)
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
            let top_gap = if index > 0 && matches!(block.kind, BlockKind::Heading(_)) {
                style.heading_top_gap
            } else {
                px(0.)
            };
            LayoutBlock {
                line: std::rc::Rc::new(line),
                origin: point(indent, px(0.)),
                line_height,
                height,
                width,
                text_len,
                marker,
                code_ranges,
                top_gap,
            }
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
                    let bottom = viewport.bottom() - editor.style.bottom_overlay;
                    let correction = if caret.y < viewport.top() + margin {
                        viewport.top() + margin - caret.y
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
            && editor.document().blocks[0].text().is_empty())
        .then(|| editor.placeholder.clone());
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.editor.clone()),
            cx,
        );
        for (i, row) in rows.iter().enumerate() {
            for range in &row.code_ranges {
                for rect in row.rectangles(range.clone(), false) {
                    window.paint_quad(
                        fill(rect, style.code_background).corner_radii(style.code_radius),
                    );
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
            if let Some(marker) = &row.marker {
                let marker_bounds = row.marker_bounds().expect("marker has bounds");
                match marker {
                    Marker::Glyph(line) => {
                        let _ = line.paint(
                            marker_bounds.origin,
                            row.line_height,
                            TextAlign::Left,
                            None,
                            window,
                            cx,
                        );
                    }
                    Marker::Bullet => {
                        window.paint_quad(fill(marker_bounds, style.marker).corner_radii(px(2.5)))
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
