use super::*;

type FormatItem = (&'static str, &'static str, Intent, bool);

impl NotesApp {
    pub(super) fn capsule(&self) -> Div {
        div()
            .flex()
            .items_center()
            .rounded_full()
            .bg(self.surface_color())
            .border_1()
            .border_color(if self.dark {
                rgba(0xffffff18)
            } else {
                rgba(0xffffffcc)
            })
    }

    pub(super) fn open_format_menu(
        &mut self,
        menu: FormatMenu,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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

    fn format_items(&self, cx: &App) -> Vec<FormatItem> {
        let editor = self.editor().read(cx);
        let marks = editor.active_marks();
        let kind = editor.active_block_kind();
        match self.format_menu {
            Some(FormatMenu::Block) => {
                let mut items = vec![(
                    "Paragraph",
                    "⌥⌘0",
                    Intent::Block(BlockKind::Paragraph),
                    kind == Some(BlockKind::Paragraph),
                )];
                for (level, label, shortcut) in [
                    (1, "Heading 1", "⌥⌘1"),
                    (2, "Heading 2", "⌥⌘2"),
                    (3, "Heading 3", "⌥⌘3"),
                ] {
                    items.push((
                        label,
                        shortcut,
                        Intent::Block(BlockKind::Heading(level)),
                        kind == Some(BlockKind::Heading(level)),
                    ));
                }
                items
            }
            Some(FormatMenu::Inline) => vec![
                ("Bold", "⌘B", Intent::Mark(Mark::Bold), marks.bold),
                ("Italic", "⌘I", Intent::Mark(Mark::Italic), marks.italic),
                (
                    "Strikethrough",
                    "⇧⌘S",
                    Intent::Mark(Mark::Strikethrough),
                    marks.strikethrough,
                ),
                (
                    "Underline",
                    "⌘U",
                    Intent::Mark(Mark::Underline),
                    marks.underline,
                ),
                ("Inline Code", "⌘E", Intent::Mark(Mark::Code), marks.code),
            ],
            Some(FormatMenu::List) => vec![
                (
                    "No List",
                    "",
                    Intent::Block(BlockKind::Paragraph),
                    kind == Some(BlockKind::Paragraph),
                ),
                (
                    "Bullet List",
                    "⇧⌘8",
                    Intent::Block(BlockKind::Bullet),
                    kind == Some(BlockKind::Bullet),
                ),
                (
                    "Task List",
                    "⇧⌘9",
                    Intent::Block(BlockKind::Task { checked: false }),
                    matches!(kind, Some(BlockKind::Task { .. })),
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

    fn format_button(
        &self,
        id: &'static str,
        label: &'static str,
        kind: Icon,
        intent: Intent,
        active: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let menu = match intent {
            Intent::FormatMenu(menu) => Some(menu),
            _ => None,
        };
        let expanded = menu.is_some() && self.format_menu == menu;
        div()
            .id(id)
            .role(Role::Button)
            .aria_label(label)
            .when_some(menu, |s, _| s.aria_expanded(expanded))
            .aria_toggled(if active {
                accesskit::Toggled::True
            } else {
                accesskit::Toggled::False
            })
            .h(px(26.))
            .w(px(if menu.is_some() { 40. } else { 28. }))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(8.))
            .cursor_pointer()
            .when(active || expanded, |s| s.bg(self.selected_color()))
            .hover(|s| s.bg(self.hover_color()))
            .active(|s| s.bg(self.pressed_color()))
            .tooltip(self.hint(label))
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.intent(intent.clone(), window, cx);
            }))
            .child(icon(
                kind,
                if active {
                    self.control_text()
                } else {
                    self.muted()
                },
            ))
            .when(menu.is_some(), |s| {
                s.child(icon(Icon::ChevronDown, self.muted()))
            })
    }

    pub(super) fn footer(&self, count: String, cx: &mut Context<Self>) -> Div {
        let editor = self.editor().read(cx);
        let marks = editor.active_marks();
        let kind = editor.active_block_kind();
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
                        Self::chrome_spring(self.chrome_visible() || self.format_toolbar),
                        |s, phase| s.opacity(phase.interpolate_clamped(0., 1.)),
                    ),
            )
            .when(!self.format_toolbar, |s| {
                s.child(
                    div()
                        .id("word-count")
                        .role(Role::Button)
                        .aria_label("Toggle character and word count")
                        .h(px(24.))
                        .px_2()
                        .flex()
                        .items_center()
                        .rounded(px(5.))
                        .text_size(px(12.))
                        .text_color(self.muted())
                        .cursor_pointer()
                        .hover(|s| s.bg(self.hover_color()))
                        .active(|s| s.bg(self.pressed_color()))
                        .tooltip(self.hint("Toggle between character count and word count"))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.show_words = !this.show_words;
                            this.focus_editor(window, cx);
                            cx.notify();
                        }))
                        .child(count),
                )
            })
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
                            matches!(kind, Some(BlockKind::Heading(_))),
                            cx,
                        ))
                        .child(self.format_button(
                            "format-inline-menu",
                            "Text Formatting",
                            Icon::Italic,
                            Intent::FormatMenu(FormatMenu::Inline),
                            marks.bold || marks.italic || marks.strikethrough || marks.underline,
                            cx,
                        ))
                        .child(self.format_button(
                            "format-inline-code",
                            "Inline Code · ⌘E",
                            Icon::Code,
                            Intent::Mark(Mark::Code),
                            marks.code,
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
                            if matches!(kind, Some(BlockKind::Task { .. })) {
                                Icon::Task
                            } else {
                                Icon::Bullet
                            },
                            Intent::FormatMenu(FormatMenu::List),
                            matches!(kind, Some(BlockKind::Bullet | BlockKind::Task { .. })),
                            cx,
                        )),
                )
            })
    }

    pub(super) fn format_popover(&self, window: &Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let items = self.format_items(cx);
        let width = px(216.);
        let height = px(8. + items.len() as f32 * 32.).min(window.bounds().size.height - px(100.));
        let mut list = div()
            .id("format-menu-items")
            .track_scroll(&self.format_scroll)
            .overflow_y_scroll()
            .size_full()
            .p(px(4.));
        for (index, (label, hint, intent, checked)) in items.into_iter().enumerate() {
            list = list.child(
                div()
                    .id(("format-choice", index))
                    .role(Role::Button)
                    .aria_label(label)
                    .aria_selected(index == self.format_selected)
                    .aria_toggled(if checked {
                        accesskit::Toggled::True
                    } else {
                        accesskit::Toggled::False
                    })
                    .h(px(32.))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded(px(6.))
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
                        this.intent(intent.clone(), window, cx)
                    }))
                    .child(
                        div()
                            .w(px(14.))
                            .when(checked, |s| s.child(icon(Icon::Check, self.control_text()))),
                    )
                    .child(div().flex_1().child(label))
                    .child(self.shortcut(hint)),
            );
        }
        div()
            .id("format-menu")
            .absolute()
            .bottom(px(48.))
            .left((window.bounds().size.width - width) / 2.)
            .w(width)
            .h(height)
            .rounded(px(10.))
            .bg(self.surface_color())
            .border_1()
            .border_color(self.border_color())
            .shadow(vec![BoxShadow {
                color: rgba(0x00000025).into(),
                offset: point(px(0.), px(4.)),
                blur_radius: px(18.),
                spread_radius: px(0.),
                inset: false,
            }])
            .occlude()
            .overflow_hidden()
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
