use super::*;

type FormatItem = (&'static str, &'static str, Intent, bool);

/// How wide the formatting toolbar sits in the middle of the footer. It is fixed: the
/// same five controls, whatever the note is.
const TOOLBAR_CAPSULE: Pixels = px(171.);

impl NotesApp {
    pub(super) fn capsule(&self) -> Div {
        div()
            .flex()
            .items_center()
            .rounded_full()
            .bg(self.surface_color())
            .border_1()
            .border_color(self.border_color())
    }

    pub(super) fn open_format_menu(
        &mut self,
        menu: FormatMenu,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.code_language_block = None;
        self.link_popover = None;
        self.chrome_focus = None;
        self.query
            .update(cx, |editor, cx| editor.cancel_composition(cx));
        self.editor()
            .update(cx, |editor, cx| editor.cancel_composition(cx));
        self.format_menu = if self.format_menu == Some(menu) {
            None
        } else {
            Some(menu)
        };
        self.format_selected = self
            .format_items(cx)
            .iter()
            .position(|item| item.3)
            .unwrap_or(0);
        if self.format_menu.is_some() {
            window.focus(&self.panel_focus, cx);
            let weak = cx.entity().downgrade();
            window.on_next_frame(move |_, cx| {
                let _ = weak.update(cx, |this, cx| {
                    if this.format_menu == Some(menu) {
                        this.format_scroll.scroll_to_item(this.format_selected);
                        cx.notify();
                    }
                });
            });
        } else {
            self.focus_editor(window, cx);
        }
        cx.notify();
    }

    pub(super) fn format_items(&self, cx: &App) -> Vec<FormatItem> {
        let editor = self.editor().read(cx);
        let marks = editor.active_marks();
        let kind = doc::Block::active(editor.state(), &editor.projection());
        match self.format_menu {
            Some(FormatMenu::Block) => {
                let mut items = vec![(
                    "Paragraph",
                    "⌥⌘0",
                    Intent::Block(doc::Block::Paragraph),
                    kind == Some(doc::Block::Paragraph),
                )];
                for (level, label, shortcut) in [
                    (1, "Heading 1", "⌥⌘1"),
                    (2, "Heading 2", "⌥⌘2"),
                    (3, "Heading 3", "⌥⌘3"),
                ] {
                    items.push((
                        label,
                        shortcut,
                        Intent::Block(doc::Block::Heading(level)),
                        kind == Some(doc::Block::Heading(level)),
                    ));
                }
                items.push((
                    "Quote",
                    "⇧⌘B",
                    Intent::Block(doc::Block::Quote),
                    kind == Some(doc::Block::Quote),
                ));
                items.push((
                    "Code Block",
                    "⌥⌘C",
                    Intent::Block(doc::Block::Code),
                    kind == Some(doc::Block::Code),
                ));
                items
            }
            Some(FormatMenu::Inline) => vec![
                (
                    "Bold",
                    "⌘B",
                    Intent::Mark(doc::Inline::Bold),
                    doc::Inline::Bold.is_active(&marks),
                ),
                (
                    "Italic",
                    "⌘I",
                    Intent::Mark(doc::Inline::Italic),
                    doc::Inline::Italic.is_active(&marks),
                ),
                (
                    "Strikethrough",
                    "⇧⌘S",
                    Intent::Mark(doc::Inline::Strikethrough),
                    doc::Inline::Strikethrough.is_active(&marks),
                ),
                (
                    "Underline",
                    "⌘U",
                    Intent::Mark(doc::Inline::Underline),
                    doc::Inline::Underline.is_active(&marks),
                ),
                (
                    "Inline Code",
                    "⌘E",
                    Intent::Mark(doc::Inline::Code),
                    doc::Inline::Code.is_active(&marks),
                ),
                ("Link", "⌘L", Intent::Link, editor.active_link().is_some()),
            ],
            Some(FormatMenu::List) => vec![
                (
                    "No List",
                    "",
                    Intent::Block(doc::Block::Paragraph),
                    kind == Some(doc::Block::Paragraph),
                ),
                (
                    "Ordered List",
                    "⇧⌘7",
                    Intent::Block(doc::Block::Ordered),
                    kind == Some(doc::Block::Ordered),
                ),
                (
                    "Bullet List",
                    "⇧⌘8",
                    Intent::Block(doc::Block::Bullet),
                    kind == Some(doc::Block::Bullet),
                ),
                (
                    "Task List",
                    "⇧⌘9",
                    Intent::Block(doc::Block::Task),
                    kind == Some(doc::Block::Task),
                ),
            ],
            None => vec![],
        }
    }

    pub(super) fn format_key(
        &mut self,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let items = self.format_items(cx);
        match key {
            "up" => self.format_selected = self.format_selected.saturating_sub(1),
            "down" => {
                self.format_selected = (self.format_selected + 1).min(items.len().saturating_sub(1))
            }
            "enter" => {
                if let Some((_, _, intent, _)) = items.get(self.format_selected) {
                    self.intent(intent.clone(), window, cx);
                }
            }
            _ => return false,
        }
        self.format_scroll.scroll_to_item(self.format_selected);
        cx.notify();
        true
    }

    /// One small icon control. `toggled` is `Some` for a control that carries a state
    /// the interface can show as on or off — a format that is in force, a column's
    /// alignment — and `None` for one that only acts, which stays a plain button rather
    /// than reaching the accessibility tree as a checkbox.
    pub(super) fn format_button(
        &self,
        id: &'static str,
        label: &'static str,
        kind: Icon,
        intent: Intent,
        toggled: Option<bool>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let menu = match intent {
            Intent::FormatMenu(menu) => Some(menu),
            _ => None,
        };
        let expanded = menu.is_some() && self.format_menu == menu;
        let active = toggled == Some(true);
        // A control that takes the whole table away is written in the destructive ink,
        // as its ⌘K row is.
        let ink = if matches!(intent, Intent::Table(TableEdit::DeleteTable)) {
            self.danger()
        } else if active {
            self.control_text()
        } else {
            self.muted()
        };
        self.ring(
            id,
            ROW_RADIUS,
            div()
                .id(id)
                .role(Role::Button)
                .aria_label(label)
                .when_some(menu, |s, _| s.aria_expanded(expanded))
                // A control that opens a menu reports whether the menu is open; it is
                // not a toggle, whatever fill the format in force gives it.
                .when_some(toggled.filter(|_| menu.is_none()), |s, on| {
                    s.aria_toggled(if on {
                        accesskit::Toggled::True
                    } else {
                        accesskit::Toggled::False
                    })
                })
                .h(px(26.))
                .w(px(if menu.is_some() { 40. } else { 28. }))
                .flex()
                .items_center()
                .justify_center()
                .rounded(ROW_RADIUS)
                .cursor_pointer()
                .when(active || expanded, |s| s.bg(self.selected_color()))
                .hover(|s| s.bg(self.hover_color()))
                .active(|s| s.bg(self.pressed_color()))
                .tooltip(self.hint(label))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.intent(intent.clone(), window, cx);
                }))
                .child(icon(kind, ink))
                .when(menu.is_some(), |s| {
                    s.child(icon(Icon::ChevronDown, self.muted()))
                }),
        )
    }

    /// One row along the bottom of the note: what mode the editor is in and how much
    /// text there is on the left, the formatting toolbar in the middle, and the toggle
    /// that opens it on the right. Opening the toolbar adds to the row rather than
    /// taking the count's place, so nothing the row says moves.
    pub(super) fn footer(&self, count: String, viewport: Pixels, cx: &mut Context<Self>) -> Div {
        let editor = self.editor().read(cx);
        let marks = editor.active_marks();
        let kind = doc::Block::active(editor.state(), &editor.projection());
        let reduce_motion = cx.reduce_motion();
        // What is left of the row's left half once the centred toolbar has its width.
        // In a window too narrow for both, the toolbar is the one that has to be there.
        let room = if self.format_toolbar {
            (viewport - TOOLBAR_CAPSULE) / 2. - px(20.)
        } else {
            viewport
        };
        div()
            .h(px(44.))
            .flex_shrink_0()
            .relative()
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .absolute()
                    .left(px(12.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .children(self.vim_badge())
                    .when(room > px(110.), |s| {
                        s.child(
                            div()
                                .id("word-count")
                                .role(Role::Button)
                                .aria_label("Toggle character and word count")
                                .h(px(24.))
                                .px_2()
                                .flex()
                                .items_center()
                                .rounded(ROW_RADIUS)
                                .text_size(px(12.))
                                .text_color(self.muted())
                                .cursor_pointer()
                                .hover(|s| s.bg(self.hover_color()))
                                .active(|s| s.bg(self.pressed_color()))
                                .tooltip(self.hint("Toggle between character count and word count"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.intent(Intent::ToggleCount, window, cx)
                                }))
                                .child(count),
                        )
                    }),
            )
            .child(
                div()
                    .absolute()
                    .right(px(8.))
                    .child(
                        self.capsule().size(px(32.)).justify_center().child(
                            self.icon_button(
                                "format-toolbar-toggle",
                                if self.format_toolbar {
                                    "Hide Formatting Toolbar"
                                } else {
                                    "Show Formatting Toolbar"
                                },
                                if self.format_toolbar {
                                    Icon::Close
                                } else {
                                    Icon::Text
                                },
                                Intent::ToggleFormatToolbar,
                                cx,
                            )
                            .size(px(30.))
                            .rounded_full()
                            .opacity(1.)
                            .aria_expanded(self.format_toolbar),
                        ),
                    )
                    .with_spring(
                        "format-toggle-fade",
                        // An expanded toolbar keeps its close button regardless of the pointer.
                        Self::chrome_spring(
                            self.chrome_visible() || self.format_toolbar,
                            reduce_motion,
                        ),
                        |s, phase| s.opacity(phase.interpolate_clamped(CHROME_REST, 1.)),
                    ),
            )
            .when(self.format_toolbar, |s| {
                s.child(
                    self.capsule()
                        .p(px(3.))
                        .gap(px(2.))
                        .child(self.format_button(
                            "format-block-menu",
                            "Text Style",
                            Icon::Heading,
                            Intent::FormatMenu(FormatMenu::Block),
                            Some(matches!(kind, Some(doc::Block::Heading(_)))),
                            cx,
                        ))
                        .child(
                            self.format_button(
                                "format-inline-menu",
                                "Text Formatting",
                                Icon::Italic,
                                Intent::FormatMenu(FormatMenu::Inline),
                                Some(
                                    [
                                        doc::Inline::Bold,
                                        doc::Inline::Italic,
                                        doc::Inline::Strikethrough,
                                        doc::Inline::Underline,
                                    ]
                                    .iter()
                                    .any(|inline| inline.is_active(&marks)),
                                ),
                                cx,
                            ),
                        )
                        .child(self.format_button(
                            "format-inline-code",
                            "Inline Code · ⌘E",
                            Icon::Code,
                            Intent::Mark(doc::Inline::Code),
                            Some(doc::Inline::Code.is_active(&marks)),
                            cx,
                        ))
                        .child(
                            div()
                                .w(px(1.))
                                .h(px(16.))
                                .mx(px(4.))
                                .bg(self.border_color()),
                        )
                        .child(self.format_button(
                            "format-list-menu",
                            "Lists",
                            match kind {
                                Some(doc::Block::Task) => Icon::Task,
                                Some(doc::Block::Ordered) => Icon::Ordered,
                                _ => Icon::Bullet,
                            },
                            Intent::FormatMenu(FormatMenu::List),
                            Some(matches!(
                                kind,
                                Some(doc::Block::Bullet | doc::Block::Ordered | doc::Block::Task)
                            )),
                            cx,
                        )),
                )
            })
    }

    pub(super) fn format_popover(&self, window: &Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let items = self.format_items(cx);
        let width = px(216.);
        let height =
            (px(8.) + ROW_HEIGHT * items.len() as f32).min(window.bounds().size.height - px(100.));
        let total = items.len();
        let mut list = div()
            .id("format-menu-items")
            .role(Role::ListBox)
            .aria_label("Formatting")
            .track_scroll(&self.format_scroll)
            .overflow_y_scroll()
            .size_full()
            .p(px(4.));
        for (index, (label, hint, intent, checked)) in items.into_iter().enumerate() {
            let stop = SharedString::from(format!("format-choice-{index}"));
            list = list.child(
                self.ring(
                    &stop,
                    ROW_RADIUS,
                    div()
                        .id(stop.clone())
                        .role(Role::Button)
                        .aria_label(label)
                        .aria_selected(index == self.format_selected)
                        .aria_position_in_set(index + 1)
                        .aria_size_of_set(total)
                        .aria_toggled(if checked {
                            accesskit::Toggled::True
                        } else {
                            accesskit::Toggled::False
                        })
                        .h(ROW_HEIGHT)
                        .px_2()
                        .flex()
                        .items_center()
                        .gap_2()
                        .rounded(ROW_RADIUS)
                        .text_size(px(13.))
                        .cursor_pointer()
                        .when(index == self.format_selected, |s| {
                            s.bg(self.selected_color())
                        })
                        .hover(|s| s.bg(self.selected_color()))
                        .active(|s| s.bg(self.pressed_color()))
                        .on_mouse_move(cx.listener(move |this, _, _, cx| {
                            if this.format_selected != index {
                                this.format_selected = index;
                                cx.notify();
                            }
                        }))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.intent(intent.clone(), window, cx)
                        }))
                        .child(
                            div()
                                .w(px(14.))
                                .when(checked, |s| s.child(icon(Icon::Check, self.control_text()))),
                        )
                        .child(div().flex_1().child(label))
                        .child(self.shortcut(hint)),
                ),
            );
        }
        div()
            .id("format-menu")
            .absolute()
            .bottom(px(48.))
            .left((window.bounds().size.width - width) / 2.)
            .w(width)
            .h(height)
            .rounded(POPOVER_RADIUS)
            .bg(self.surface_color())
            .border_1()
            .border_color(self.border_color())
            .shadow(popover_shadow())
            .occlude()
            .overflow_hidden()
            // Preserve the editor selection until the chosen format is applied.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down_out(cx.listener(|this, event: &MouseDownEvent, window, cx| {
                // Footer controls switch or close menus themselves; preserve their click.
                if event.position.y < window.bounds().size.height - px(44.) {
                    this.format_menu = None;
                    this.focus_editor(window, cx);
                    cx.notify();
                    cx.stop_propagation();
                }
            }))
            .child(list)
    }
}
