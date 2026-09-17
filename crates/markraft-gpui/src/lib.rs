//! Native editing adapter. The core owns document semantics; the host owns persistence.
mod accessibility;
mod caret;
mod format_state;
mod single_line;
mod style;
mod surface;
pub use style::EditorStyle;

use gpui::{prelude::*, *};
use markraft_core::{BlockKind, Document, Editor, Mark, Position, Selection};
use std::{
    cell::RefCell,
    ops::Range,
    rc::Rc,
    time::{Duration, Instant},
};
use surface::{EditorSurface, LayoutBlock};

// All bindings are scoped so embedding hosts retain their own shortcuts.
actions!(
    markraft,
    [
        Backspace,
        Delete,
        Enter,
        Left,
        Right,
        Up,
        Down,
        SelectLeft,
        SelectRight,
        SelectUp,
        SelectDown,
        Home,
        End,
        SelectHome,
        SelectEnd,
        SelectAll,
        Copy,
        Cut,
        Paste,
        Undo,
        Redo,
        Bold,
        Italic,
        Code,
        Strikethrough,
        Underline,
        Paragraph,
        Heading,
        Heading2,
        Heading3,
        Bullet,
        Task,
        ToggleTask,
        CharacterPalette,
        WordLeft,
        WordRight,
        SelectWordLeft,
        SelectWordRight,
        DeleteWordBackward,
        DeleteWordForward,
        DocumentStart,
        DocumentEnd,
        SelectDocumentStart,
        SelectDocumentEnd,
        CancelComposition
    ]
);

pub fn bind_keys(cx: &mut App) {
    macro_rules! bind { ($($key:literal => $action:ident),* $(,)?) => {
        cx.bind_keys([$(KeyBinding::new($key, $action, Some("Markraft"))),*]);
    }; }
    bind! {
        "backspace" => Backspace, "delete" => Delete, "enter" => Enter,
        "left" => Left, "right" => Right, "up" => Up, "down" => Down,
        "shift-left" => SelectLeft, "shift-right" => SelectRight,
        "shift-up" => SelectUp, "shift-down" => SelectDown,
        "cmd-left" => Home, "cmd-right" => End,
        "cmd-shift-left" => SelectHome, "cmd-shift-right" => SelectEnd,
        "home" => Home, "end" => End, "cmd-a" => SelectAll,
        "cmd-c" => Copy, "cmd-x" => Cut, "cmd-v" => Paste,
        "cmd-z" => Undo, "cmd-shift-z" => Redo, "cmd-b" => Bold,
        "cmd-i" => Italic, "cmd-e" => Code,
        "cmd-shift-s" => Strikethrough, "cmd-u" => Underline, "cmd-alt-0" => Paragraph,
        "cmd-alt-1" => Heading, "cmd-alt-2" => Heading2,
        "cmd-alt-3" => Heading3, "cmd-shift-8" => Bullet,
        "cmd-shift-9" => Task, "cmd-enter" => ToggleTask,
        "ctrl-cmd-space" => CharacterPalette,
        "alt-left" => WordLeft, "alt-right" => WordRight,
        "alt-shift-left" => SelectWordLeft, "alt-shift-right" => SelectWordRight,
        "alt-backspace" => DeleteWordBackward, "alt-delete" => DeleteWordForward,
        "cmd-up" => DocumentStart, "cmd-down" => DocumentEnd,
        "cmd-shift-up" => SelectDocumentStart, "cmd-shift-down" => SelectDocumentEnd,
        "escape" => CancelComposition,
    }
}

#[derive(Clone, Debug)]
pub enum EditorEvent {
    Changed { revision: u64 },
}

pub struct EditorView {
    pub(crate) core: Editor,
    pub(crate) style: EditorStyle,
    pub(crate) placeholder: SharedString,
    pub(crate) single_line: bool,
    pub(crate) single_line_scroll_x: Pixels,
    pub(crate) focus: FocusHandle,
    pub(crate) layout: Vec<LayoutBlock>,
    pub(crate) scroll: ScrollHandle,
    pub(crate) reveal: bool,
    pub(crate) upstream: bool,
    selecting: bool,
    preferred_x: Option<Pixels>,
    published_revision: u64,
    typing_group: u64,
    last_typed_at: Option<Instant>,
    accessible_text: Rc<RefCell<accessibility::AccessibleText>>,
    focus_subscriptions: Option<[Subscription; 3]>,
    pub(crate) caret_blink: caret::CaretBlink,
    caret_focused: bool,
    caret_blink_task: Option<gpui::Task<()>>,
    // The overlay scrollbar shows while scrolling and fades once scrolling stops.
    scrollbar_active: bool,
    scrollbar_task: Option<gpui::Task<()>>,
}

impl EventEmitter<EditorEvent> for EditorView {}
impl Focusable for EditorView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EditorView {
    pub fn new(document: Document, cx: &mut Context<Self>) -> Self {
        Self {
            core: Editor::new(document),
            style: EditorStyle::default(),
            placeholder: SharedString::default(),
            single_line: false,
            single_line_scroll_x: px(0.),
            focus: cx.focus_handle(),
            layout: vec![],
            scroll: ScrollHandle::new(),
            reveal: false,
            upstream: false,
            selecting: false,
            preferred_x: None,
            published_revision: 0,
            typing_group: 0,
            last_typed_at: None,
            accessible_text: Rc::default(),
            focus_subscriptions: None,
            caret_blink: caret::CaretBlink::default(),
            caret_focused: false,
            caret_blink_task: None,
            scrollbar_active: false,
            scrollbar_task: None,
        }
    }
    pub fn with_style(mut self, style: EditorStyle) -> Self {
        self.style = style;
        self
    }
    pub fn with_placeholder(mut self, placeholder: impl Into<SharedString>) -> Self {
        self.placeholder = placeholder.into();
        self
    }
    pub fn set_placeholder(
        &mut self,
        placeholder: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        self.placeholder = placeholder.into();
        cx.notify();
    }
    /// A literal, single-line input for host-owned query and settings fields.
    /// Enter is left to the host; native composition and selection stay enabled.
    pub fn with_single_line(mut self) -> Self {
        self.single_line = true;
        self.core = Editor::new(single_line::document(self.document().clone()));
        self
    }
    pub fn set_style(&mut self, style: EditorStyle, cx: &mut Context<Self>) {
        self.style = style;
        self.reveal = true;
        cx.notify();
    }
    /// Height at the most recently laid-out width, including editor padding.
    pub fn content_height(&self) -> Option<Pixels> {
        (!self.layout.is_empty()).then(|| {
            self.layout.iter().fold(
                (if self.single_line { px(0.) } else { px(40.) })
                    + self.style.padding * 2.
                    + self.style.top_overlay
                    + self.style.bottom_overlay,
                |height, row| height + row.top_gap + row.height,
            )
        })
    }
    pub fn committed_document(&self) -> &Document {
        self.core.committed_document()
    }
    pub fn is_composing(&self) -> bool {
        self.core.is_composing()
    }
    pub fn cancel_composition(&mut self, cx: &mut Context<Self>) {
        self.edit(cx, |core| {
            core.cancel_composition();
        });
    }
    pub fn document(&self) -> &Document {
        self.core.document()
    }
    /// Marks shared by all selected text, or the current typing marks at a caret.
    pub fn active_marks(&self) -> markraft_core::Marks {
        format_state::active_marks(&self.core)
    }
    /// The selected blocks' common kind; mixed block formats return `None`.
    pub fn active_block_kind(&self) -> Option<BlockKind> {
        format_state::active_block_kind(&self.core)
    }
    pub fn replace_document(&mut self, document: Document, cx: &mut Context<Self>) {
        self.last_typed_at = None;
        self.single_line_scroll_x = px(0.);
        let document = if self.single_line {
            single_line::document(document)
        } else {
            document
        };
        self.core = Editor::new(document);
        self.layout.clear();
        self.scroll.set_offset(point(px(0.), px(0.)));
        self.reset_caret_blink(cx);
        self.publish(cx);
    }
    pub fn toggle_mark(&mut self, mark: Mark, cx: &mut Context<Self>) {
        if self.single_line {
            return;
        }
        self.edit(cx, |core| {
            core.toggle_mark(mark);
        });
    }
    pub fn set_block_kind(&mut self, kind: BlockKind, cx: &mut Context<Self>) {
        if self.single_line {
            return;
        }
        self.edit(cx, |core| {
            core.set_block_kind(kind);
        });
    }
    fn publish(&mut self, cx: &mut Context<Self>) {
        self.published_revision += 1;
        cx.emit(EditorEvent::Changed {
            revision: self.published_revision,
        });
        cx.notify();
    }
    fn sync_caret_focus(&mut self, window: &Window, cx: &mut Context<Self>) {
        let focused = window.is_window_active() && self.focus.is_focused(window);
        if self.caret_focused != focused {
            self.caret_focused = focused;
            self.reset_caret_blink(cx);
            cx.notify();
        }
    }
    fn flash_scrollbar(&mut self, cx: &mut Context<Self>) {
        self.scrollbar_active = true;
        // Replacing the task cancels the previous hide timer.
        self.scrollbar_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(1200))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.scrollbar_active = false;
                cx.notify();
            });
        }));
        cx.notify();
    }
    /// Thumb offset and height within the editor, from the previous frame's scroll
    /// geometry. None while the content fits.
    fn scrollbar_thumb(&self) -> Option<(Pixels, Pixels)> {
        let inset = px(4.);
        let viewport = self.scroll.bounds().size.height;
        let max = self.scroll.max_offset().y;
        let track = viewport - inset * 2. - self.style.top_overlay - self.style.bottom_overlay;
        if self.single_line || max <= px(1.) || track <= px(48.) {
            return None;
        }
        let height = (track * (viewport / (viewport + max))).max(px(28.));
        let progress = (-self.scroll.offset().y / max).clamp(0., 1.);
        Some((
            self.style.top_overlay + inset + (track - height) * progress,
            height,
        ))
    }
    fn reset_caret_blink(&mut self, cx: &mut Context<Self>) {
        // Dropping the previous task cancels its pending timer. Only the focused
        // editor with a collapsed, committed selection owns a ticking task.
        self.caret_blink_task = None;
        if !self.caret_blink.reset(
            self.caret_focused,
            self.core.is_composing(),
            self.core.selection().is_empty(),
        ) {
            return;
        }
        self.caret_blink_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(caret::BLINK_INTERVAL).await;
                let keep_blinking = this.update(cx, |this, cx| {
                    if this.caret_blink.tick() {
                        cx.notify();
                        true
                    } else {
                        false
                    }
                });
                if !matches!(keep_blinking, Ok(true)) {
                    break;
                }
            }
        }));
    }
    fn edit(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Editor)) {
        self.last_typed_at = None;
        let before = self.core.revision();
        let composing = self.core.is_composing();
        f(&mut self.core);
        self.upstream = false;
        if self.core.revision() != before || composing != self.core.is_composing() {
            self.publish(cx);
        }
        self.preferred_x = None;
        self.reveal = true;
        self.reset_caret_blink(cx);
        cx.notify();
    }
    fn select(&mut self, head: Position, extend: bool, cx: &mut Context<Self>) {
        self.last_typed_at = None;
        let composing = self.core.is_composing();
        self.core.finish_composition();
        let anchor = if extend {
            self.core.selection().anchor
        } else {
            head
        };
        self.core.set_selection(Selection { anchor, head });
        if composing {
            self.publish(cx);
        }
        self.reveal = true;
        self.reset_caret_blink(cx);
        cx.notify();
    }
    fn range(&self) -> Range<usize> {
        let s = self.core.selection();
        let a = self.core.position_to_utf16(s.anchor);
        let b = self.core.position_to_utf16(s.head);
        a.min(b)..a.max(b)
    }
    pub(crate) fn hit(&self, point: Point<Pixels>) -> Position {
        let Some(last) = self.layout.last() else {
            return Position { block: 0, byte: 0 };
        };
        if point.y >= last.origin.y + last.height {
            return Position {
                block: self.layout.len() - 1,
                byte: last.text_len,
            };
        }
        let (block, row) = self
            .layout
            .iter()
            .enumerate()
            .find(|(_, row)| point.y < row.origin.y + row.height)
            .unwrap_or((0, &self.layout[0]));
        let local = gpui::point(
            (point.x - row.origin.x).max(px(0.)),
            (point.y - row.origin.y)
                .max(px(0.))
                .min(row.line.size(row.line_height).height - px(1.)),
        );
        let byte = row
            .line
            .closest_index_for_position(local, row.line_height)
            .unwrap_or_else(|i| i);
        Position {
            block,
            byte: byte.min(row.text_len),
        }
    }
    fn select_point(&mut self, point: Point<Pixels>, extend: bool, cx: &mut Context<Self>) {
        let position = self.hit(point);
        self.upstream = self
            .layout
            .get(position.block)
            .is_some_and(|row| row.caret(position.byte, false).y > point.y);
        self.select(position, extend, cx);
    }
    fn vertical(&mut self, delta: f32, extend: bool, cx: &mut Context<Self>) {
        let head = self.core.selection().head;
        if let Some(row) = self.layout.get(head.block) {
            let caret = row.caret(head.byte, self.upstream);
            let x = self.preferred_x.unwrap_or(caret.x);
            let centers: Vec<_> = self
                .layout
                .iter()
                .flat_map(|row| {
                    (0..=row.line.wrap_boundaries().len())
                        .map(|i| row.origin.y + row.line_height * (i as f32 + 0.5))
                })
                .collect();
            let current = centers
                .iter()
                .position(|&y| y > caret.y)
                .unwrap_or(centers.len() - 1);
            let next =
                (current as isize + delta as isize).clamp(0, centers.len() as isize - 1) as usize;
            self.select_point(point(x, centers[next]), extend, cx);
            self.preferred_x = Some(x);
        }
    }
    fn line_edge(&mut self, end: bool, extend: bool, cx: &mut Context<Self>) {
        let head = self.core.selection().head;
        if let Some(row) = self.layout.get(head.block) {
            let caret = row.caret(head.byte, self.upstream);
            let target = point(
                if end {
                    row.origin.x + row.width
                } else {
                    row.origin.x
                },
                caret.y + row.line_height * 0.5,
            );
            self.preferred_x = None;
            self.select_point(target, extend, cx);
        }
    }
    fn copy(&mut self, cx: &mut Context<Self>) {
        let text = self.core.selection_text();
        if !text.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }
    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        self.selecting = true;
        self.preferred_x = None;
        let position = self.hit(event.position);
        // Task markers are presentation outside the text coordinate space.
        if let Some(row) = self.layout.get(position.block)
            && row
                .marker_bounds()
                .is_some_and(|bounds| bounds.contains(&event.position))
            && let BlockKind::Task { checked } = self.document().blocks[position.block].kind
        {
            self.select(position, false, cx);
            self.set_block_kind(BlockKind::Task { checked: !checked }, cx);
            self.selecting = false;
            return;
        }
        self.select_point(event.position, event.modifiers.shift, cx);
        if event.click_count >= 3 {
            let end = Position {
                block: position.block,
                byte: self.document().blocks[position.block].text().len(),
            };
            self.core.set_selection(Selection {
                anchor: Position {
                    block: position.block,
                    byte: 0,
                },
                head: end,
            });
        } else if event.click_count == 2 {
            use unicode_segmentation::UnicodeSegmentation;
            let text = self.document().blocks[position.block].text();
            if let Some((start, word)) = text
                .split_word_bound_indices()
                .find(|(start, word)| *start <= position.byte && position.byte < start + word.len())
            {
                self.core.set_selection(Selection {
                    anchor: Position {
                        block: position.block,
                        byte: start,
                    },
                    head: Position {
                        block: position.block,
                        byte: start + word.len(),
                    },
                });
            }
        }
        self.reset_caret_blink(cx);
        cx.notify();
    }
    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.selecting {
            self.select_point(event.position, true, cx);
        }
    }
    fn mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.selecting = false;
    }
}

impl EntityInputHandler for EditorView {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        actual: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let start = self
            .core
            .position_to_utf16(self.core.utf16_to_position(range.start));
        let end = self
            .core
            .position_to_utf16(self.core.utf16_to_position(range.end))
            .max(start);
        *actual = Some(start..end);
        Some(String::from_utf16_lossy(
            &self
                .document()
                .plain_text()
                .encode_utf16()
                .collect::<Vec<_>>()[start..end],
        ))
    }
    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let s = self.core.selection();
        Some(UTF16Selection {
            range: self.range(),
            reversed: (s.anchor.block, s.anchor.byte) > (s.head.block, s.head.byte),
        })
    }
    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.core.marked_range()
    }
    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.edit(cx, |c| {
            c.finish_composition();
        });
    }
    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = single_line::text(text, self.single_line);
        let text = text.as_ref();
        let single_line = self.single_line;
        let now = Instant::now();
        let grouped = range.is_none()
            && !self.core.is_composing()
            && !text.is_empty()
            && !text.contains(['\n', '\r']);
        if grouped {
            if self
                .last_typed_at
                .is_none_or(|last| now.duration_since(last) > Duration::from_millis(750))
            {
                self.typing_group = self.typing_group.wrapping_add(1);
            }
            let group = self.typing_group;
            self.edit(cx, |core| {
                if single_line {
                    core.insert_text_plain_grouped(text, group);
                } else {
                    core.insert_text_grouped(text, group);
                }
            });
            self.last_typed_at = Some(now);
        } else {
            self.edit(cx, |core| {
                if single_line {
                    core.commit_composition_plain(range, text);
                } else {
                    core.commit_composition(range, text);
                }
            });
        }
    }
    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let selected = if self.single_line {
            single_line::selected_range(text, selected)
        } else {
            selected
        };
        let text = single_line::text(text, self.single_line);
        self.edit(cx, |c| {
            c.set_composition(range, &text, selected);
        });
    }
    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let start = self.core.utf16_to_position(range.start);
        let end = self.core.utf16_to_position(range.end);
        let row = self.layout.get(start.block)?;
        let a = row.caret(start.byte, self.upstream);
        let b = if end.block == start.block {
            row.caret(end.byte, self.upstream)
        } else {
            a
        };
        let width = if a.y == b.y {
            (b.x - a.x).max(px(2.))
        } else {
            px(2.)
        };
        Some(Bounds::new(a, size(width, row.line_height)))
    }
    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.core.position_to_utf16(self.hit(point)))
    }
}

impl Render for EditorView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.focus_subscriptions.is_none() {
            self.focus_subscriptions = Some([
                cx.on_focus(&self.focus, window, |this, window, cx| {
                    this.sync_caret_focus(window, cx);
                }),
                cx.on_blur(&self.focus, window, |this, window, cx| {
                    this.last_typed_at = None;
                    this.selecting = false;
                    this.sync_caret_focus(window, cx);
                    // The input method owns commit/unmark ordering. A host that
                    // dismisses an editor can explicitly cancel composition.
                }),
                cx.observe_window_activation(window, |this, window, cx| {
                    this.last_typed_at = None;
                    this.sync_caret_focus(window, cx);
                }),
            ]);
        }
        self.sync_caret_focus(window, cx);
        let accessible_text = self.accessible_text.clone();
        let selection_editor = cx.entity();
        let replacement_editor = cx.entity();
        let value_editor = cx.entity();
        let mut root = div()
            .id("markraft-editor")
            .role(if self.single_line {
                Role::TextInput
            } else {
                Role::MultilineTextInput
            })
            .aria_label("Text editor")
            .aria_placeholder(self.placeholder.clone())
            .a11y_synthetic_children(move |builder| accessible_text.borrow_mut().write(builder))
            .on_a11y_action(
                AccessibleAction::SetTextSelection,
                move |data, window, cx| {
                    if let Some(accesskit::ActionData::SetTextSelection(selection)) = data {
                        selection_editor.update(cx, |this, cx| {
                            let selection = this.accessible_text.borrow().selection(selection);
                            if let Some(selection) = selection {
                                window.focus(&this.focus, cx);
                                this.edit(cx, |core| core.set_selection(selection));
                            }
                        });
                    }
                },
            )
            .on_a11y_action(
                AccessibleAction::ReplaceSelectedText,
                move |data, window, cx| {
                    if let Some(accesskit::ActionData::Value(value)) = data {
                        replacement_editor.update(cx, |this, cx| {
                            window.focus(&this.focus, cx);
                            let single_line = this.single_line;
                            let value = single_line::text(value, single_line);
                            this.edit(cx, |core| {
                                if single_line {
                                    core.commit_composition_plain(None, &value);
                                } else {
                                    core.commit_composition(None, &value);
                                }
                            });
                        });
                    }
                },
            )
            .on_a11y_action(AccessibleAction::SetValue, move |data, window, cx| {
                if let Some(accesskit::ActionData::Value(value)) = data {
                    value_editor.update(cx, |this, cx| {
                        window.focus(&this.focus, cx);
                        let single_line = this.single_line;
                        let value = single_line::text(value, single_line);
                        this.edit(cx, |core| {
                            core.set_selection(Selection {
                                anchor: Position::default(),
                                head: core.utf16_to_position(usize::MAX),
                            });
                            if single_line {
                                core.insert_text_plain(&value);
                            } else {
                                core.insert_text(&value);
                            }
                        });
                    });
                }
            })
            .size_full()
            .key_context("Markraft")
            .track_focus(&self.focus)
            .cursor(CursorStyle::IBeam)
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up));
        macro_rules! edit_action {
            ($action:ty, $call:expr) => {
                root = root.on_action(cx.listener(|this, _: &$action, _, cx| {
                    this.edit(cx, $call);
                }));
            };
        }
        edit_action!(Backspace, |c| {
            c.backspace();
        });
        edit_action!(Delete, |c| {
            c.delete_forward();
        });
        root = root.on_action(cx.listener(|this, _: &Enter, _, cx| {
            if this.single_line {
                cx.propagate();
            } else {
                this.edit(cx, |core| {
                    core.insert_text("\n");
                });
            }
        }));
        edit_action!(WordLeft, |c| {
            c.move_word_left(false);
        });
        edit_action!(WordRight, |c| {
            c.move_word_right(false);
        });
        edit_action!(SelectWordLeft, |c| {
            c.move_word_left(true);
        });
        edit_action!(SelectWordRight, |c| {
            c.move_word_right(true);
        });
        edit_action!(DeleteWordBackward, |c| {
            c.delete_word_backward();
        });
        edit_action!(DeleteWordForward, |c| {
            c.delete_word_forward();
        });
        edit_action!(DocumentStart, |c| {
            c.move_document_start(false);
        });
        edit_action!(DocumentEnd, |c| {
            c.move_document_end(false);
        });
        edit_action!(SelectDocumentStart, |c| {
            c.move_document_start(true);
        });
        edit_action!(SelectDocumentEnd, |c| {
            c.move_document_end(true);
        });
        root = root.on_action(cx.listener(|this, _: &CancelComposition, _, cx| {
            if this.is_composing() {
                this.cancel_composition(cx);
            } else {
                cx.propagate();
            }
        }));
        edit_action!(Left, |c| {
            c.move_left(false);
        });
        edit_action!(Right, |c| {
            c.move_right(false);
        });
        edit_action!(SelectLeft, |c| {
            c.move_left(true);
        });
        edit_action!(SelectRight, |c| {
            c.move_right(true);
        });
        edit_action!(Undo, |c| {
            c.undo();
        });
        edit_action!(Redo, |c| {
            c.redo();
        });
        macro_rules! format_action {
            ($action:ty, $call:expr) => {
                root = root.on_action(cx.listener(|this, _: &$action, _, cx| {
                    if !this.single_line {
                        this.edit(cx, $call);
                    }
                }));
            };
        }
        format_action!(Bold, |c| {
            c.toggle_mark(Mark::Bold);
        });
        format_action!(Italic, |c| {
            c.toggle_mark(Mark::Italic);
        });
        format_action!(Code, |c| {
            c.toggle_mark(Mark::Code);
        });
        format_action!(Strikethrough, |c| {
            c.toggle_mark(Mark::Strikethrough);
        });
        format_action!(Underline, |c| {
            c.toggle_mark(Mark::Underline);
        });
        format_action!(Paragraph, |c| {
            c.set_block_kind(BlockKind::Paragraph);
        });
        format_action!(Heading, |c| {
            c.set_block_kind(BlockKind::Heading(1));
        });
        format_action!(Heading2, |c| {
            c.set_block_kind(BlockKind::Heading(2));
        });
        format_action!(Heading3, |c| {
            c.set_block_kind(BlockKind::Heading(3));
        });
        format_action!(Bullet, |c| {
            c.set_block_kind(BlockKind::Bullet);
        });
        format_action!(Task, |c| {
            c.set_block_kind(BlockKind::Task { checked: false });
        });
        let root = root
            .on_action(cx.listener(|this, _: &Up, _, cx| this.vertical(-1., false, cx)))
            .on_action(cx.listener(|this, _: &Down, _, cx| this.vertical(1., false, cx)))
            .on_action(cx.listener(|this, _: &SelectUp, _, cx| this.vertical(-1., true, cx)))
            .on_action(cx.listener(|this, _: &SelectDown, _, cx| this.vertical(1., true, cx)))
            .on_action(cx.listener(|this, _: &Home, _, cx| this.line_edge(false, false, cx)))
            .on_action(cx.listener(|this, _: &End, _, cx| this.line_edge(true, false, cx)))
            .on_action(cx.listener(|this, _: &SelectHome, _, cx| this.line_edge(false, true, cx)))
            .on_action(cx.listener(|this, _: &SelectEnd, _, cx| this.line_edge(true, true, cx)))
            .on_action(cx.listener(|this, _: &SelectAll, _, cx| {
                this.edit(cx, |core| {
                    let head = core.utf16_to_position(usize::MAX);
                    core.set_selection(Selection {
                        anchor: Position { block: 0, byte: 0 },
                        head,
                    });
                });
            }))
            .on_action(cx.listener(|this, _: &Copy, _, cx| this.copy(cx)))
            .on_action(cx.listener(|this, _: &Cut, _, cx| {
                this.copy(cx);
                let single_line = this.single_line;
                this.edit(cx, |c| {
                    if single_line {
                        c.insert_text_plain("");
                    } else {
                        c.insert_text("");
                    }
                });
            }))
            .on_action(cx.listener(|this, _: &Paste, _, cx| {
                if let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) {
                    let single_line = this.single_line;
                    let text = single_line::text(&text, single_line);
                    this.edit(cx, |c| {
                        if single_line {
                            c.insert_text_plain(&text);
                        } else {
                            c.insert_text(&text.replace("\r\n", "\n").replace('\r', "\n"));
                        }
                    });
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleTask, _, cx| {
                let head = this.core.selection().head;
                if let BlockKind::Task { checked } = this.document().blocks[head.block].kind {
                    this.set_block_kind(BlockKind::Task { checked: !checked }, cx);
                }
            }))
            .on_action(
                cx.listener(|_, _: &CharacterPalette, window, _| window.show_character_palette()),
            )
            .when(self.single_line, |this| {
                this.overflow_x_hidden().overflow_y_hidden()
            })
            .when(!self.single_line, |this| this.overflow_y_scroll())
            .track_scroll(&self.scroll)
            .on_scroll_wheel(cx.listener(|this, _, _, cx| this.flash_scrollbar(cx)))
            .p(self.style.padding)
            .pt(self.style.padding + self.style.top_overlay)
            .pb(self.style.padding + self.style.bottom_overlay)
            .bg(self.style.background)
            .text_color(self.style.text)
            .child(EditorSurface {
                editor: cx.entity(),
            });
        // The thumb sits beside the scroller rather than inside it, so it does not
        // scroll with the content. It is an indicator only and takes no pointer input.
        let active = self.scrollbar_active;
        let color = self.style.scrollbar;
        div().size_full().relative().child(root).when_some(
            self.scrollbar_thumb(),
            |this, (top, height)| {
                this.child(
                    div()
                        .absolute()
                        .top(top)
                        .right(px(3.))
                        .w(px(6.))
                        .h(height)
                        .rounded_full()
                        .bg(color)
                        .with_spring(
                            "scrollbar-fade",
                            SpringAnimation::new(SpringConfig::new(500., 45., 1.)).to(active),
                            |s, phase| s.opacity(phase.interpolate_clamped(0., 1.)),
                        ),
                )
            },
        )
    }
}
