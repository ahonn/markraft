use super::*;

type FormatItem = (&'static str, &'static str, Intent, bool);

// Three 38px menus, four 24px actions, two 9px separators, eight 4px gaps,
// and 4px padding on each side. Keep menu anchors tied to this geometry.
const TOOLBAR_CAPSULE: Pixels = px(268.);

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

    /// Chrome uses a foreground tint rather than the stronger list selection fill.
    pub(super) fn chrome_fill(&self, opacity: f32) -> Hsla {
        let mut color: Hsla = if self.dark {
            rgb(0xffffff)
        } else {
            rgb(0x000000)
        }
        .into();
        color.a = opacity;
        color
    }

    pub(super) fn chrome_capsule(&self) -> Div {
        self.capsule().border_0().shadow(vec![BoxShadow {
            color: self.chrome_fill(0.08),
            offset: point(px(0.), px(0.)),
            blur_radius: px(0.),
            spread_radius: px(1.),
            inset: true,
        }])
    }

    fn format_divider(&self) -> Div {
        div()
            .w(px(1.))
            .h(px(16.))
            .mx(px(4.))
            .flex_shrink_0()
            .bg(self.chrome_fill(0.10))
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
                    (4, "Heading 4", "⌥⌘4"),
                    (5, "Heading 5", "⌥⌘5"),
                    (6, "Heading 6", "⌥⌘6"),
                ] {
                    items.push((
                        label,
                        shortcut,
                        Intent::Block(doc::Block::Heading(level)),
                        kind == Some(doc::Block::Heading(level)),
                    ));
                }
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
        self.format_button_base(id, label, kind, intent, toggled, cx)
            .hover(|s| s.bg(self.hover_color()))
            .active(|s| s.bg(self.pressed_color()))
    }

    fn format_button_base(
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
                .tooltip(self.hint(label))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.intent(intent.clone(), window, cx);
                }))
                .child(icon(kind, ink))
                .when(menu.is_some(), |s| {
                    s.gap(px(2.))
                        .child(sized_icon(Icon::ChevronDown, self.muted(), 12.))
                }),
        )
    }

    fn toolbar_button(
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
        let selected = toggled == Some(true) && menu.is_none();
        self.format_button_base(id, label, kind, intent, toggled, cx)
            .h(px(24.))
            .w(px(if menu.is_some() { 38. } else { 24. }))
            .flex_shrink_0()
            .rounded(px(4.))
            .when(menu == Some(FormatMenu::Block), |s| s.rounded_l(px(12.)))
            .when(menu == Some(FormatMenu::List), |s| s.rounded_r(px(12.)))
            .bg(self.chrome_fill(if selected {
                0.10
            } else if expanded {
                0.05
            } else {
                0.
            }))
            .opacity(if expanded { 0.8 } else { 1. })
            .hover(|s| s.bg(self.chrome_fill(if selected { 0.10 } else { 0.05 })))
            .active(|s| {
                s.bg(self.chrome_fill(if selected { 0.10 } else { 0.05 }))
                    .opacity(0.8)
            })
    }

    /// The count and formatting toolbar share the center, while the toggle stays put.
    /// Vim keeps a separate mode indicator on the left, compact in a narrow window.
    pub(super) fn footer(&self, count: String, viewport: Pixels, cx: &mut Context<Self>) -> Div {
        let editor = self.editor().read(cx);
        let marks = editor.active_marks();
        let kind = doc::Block::active(editor.state(), &editor.projection());
        let reduce_motion = cx.reduce_motion();
        let linked = editor.active_link().is_some();
        let note = self.library.active_note();
        let file_status = if let Some(reason) = &note.read_only {
            let mut label = format!("Read-only: {reason}");
            if note.conflicted {
                label.push_str(" Autosave paused.");
            }
            Some((Icon::Lock, label, "Read-only. Click for ways to edit it."))
        } else if note.conflicted {
            Some((
                Icon::Pause,
                "Autosave paused".to_owned(),
                "Autosave paused. Click to review.",
            ))
        } else {
            None
        };
        // The toolbar is 268px wide and centered, so its left edge sits 91px from a
        // 450px window's. The full mode label takes about that much; adding the
        // indicator and its gap needs some 22px more, which a 500px window has.
        let compact_vim = self.format_toolbar
            && (viewport < px(450.) || (file_status.is_some() && viewport < px(500.)));
        div()
            .h(px(48.))
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
                    .gap(px(4.))
                    .children(self.vim_badge(compact_vim))
                    .when_some(file_status, |s, (symbol, label, hint)| {
                        s.child(self.file_status_button(symbol, label, hint, cx))
                    }),
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
            })
            .child(
                div()
                    .absolute()
                    .right(px(8.))
                    .child(
                        self.chrome_capsule().size(px(32.)).justify_center().child(
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
                            .size(px(32.))
                            .rounded_full()
                            .opacity(1.)
                            .aria_expanded(self.format_toolbar),
                        ),
                    )
                    .with_spring(
                        "format-toggle-fade",
                        // The close button follows window hover even while the
                        // center formatting toolbar remains expanded.
                        Self::chrome_spring(self.pointer_inside, reduce_motion),
                        |s, phase| s.opacity(phase.interpolate_clamped(0., 1.)),
                    ),
            )
            .when(self.format_toolbar, |s| {
                s.child(
                    self.chrome_capsule()
                        .p(px(4.))
                        .gap(px(4.))
                        .child(self.toolbar_button(
                            "format-block-menu",
                            "Headings",
                            Icon::Heading,
                            Intent::FormatMenu(FormatMenu::Block),
                            Some(matches!(kind, Some(doc::Block::Heading(_)))),
                            cx,
                        ))
                        .child(
                            self.toolbar_button(
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
                        .child(self.toolbar_button(
                            "format-link",
                            "Link · ⌘L",
                            Icon::Link,
                            Intent::Link,
                            Some(linked),
                            cx,
                        ))
                        .child(self.toolbar_button(
                            "format-inline-code",
                            "Inline Code · ⌘E",
                            Icon::Code,
                            Intent::Mark(doc::Inline::Code),
                            Some(doc::Inline::Code.is_active(&marks)),
                            cx,
                        ))
                        .child(self.format_divider())
                        .child(self.toolbar_button(
                            "format-code-block",
                            "Code Block · ⌥⌘C",
                            Icon::CodeBlock,
                            Intent::Block(doc::Block::Code),
                            Some(kind == Some(doc::Block::Code)),
                            cx,
                        ))
                        .child(self.toolbar_button(
                            "format-quote",
                            "Quote · ⇧⌘B",
                            Icon::Quote,
                            Intent::Block(doc::Block::Quote),
                            Some(kind == Some(doc::Block::Quote)),
                            cx,
                        ))
                        .child(self.format_divider())
                        .child(self.toolbar_button(
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

    /// The lower-left lock or pause. It says what state the file is in and opens the
    /// way out of it, and it lights up when a keystroke was refused, because a
    /// read-only file has no notice of its own to show for each one.
    fn file_status_button(
        &self,
        symbol: Icon,
        label: String,
        hint: &'static str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let lit = self
            .file_status_flash
            .is_some_and(|until| Instant::now() < until);
        let (resting, attention) = (self.hover_color(), self.pressed_color());
        // Only the lock discloses a card; the pause opens a dialog, which is not a
        // state this control is in.
        let discloses = self.library.active_note().read_only.is_some();
        div()
            .id("file-status-indicator")
            .role(Role::Button)
            .aria_label(label)
            .when(discloses, |s| s.aria_expanded(self.file_status_popover))
            .tooltip(self.hint(hint))
            .flex_shrink_0()
            .size(px(18.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(4.))
            .cursor_pointer()
            .hover(|s| s.bg(self.selected_color()))
            .active(|s| s.bg(attention))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, _, window, cx| {
                cx.stop_propagation();
                this.intent(Intent::FileStatus, window, cx);
            }))
            .child(sized_icon(symbol, self.muted(), 11.))
            // Reduced motion keeps both fills and drops the travel between them, so
            // the indicator simply stands out until the flash expires.
            .with_spring(
                "file-status-flash",
                Self::chrome_spring(lit, cx.reduce_motion()),
                move |s, phase| s.bg(phase.interpolate_clamped(resting, attention)),
            )
    }

    /// What a read-only file is, and the two apps that can still change it. A note
    /// that is also conflicted says so here and offers the dialog, since its own
    /// indicator is taken by the lock.
    pub(super) fn file_status_card(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        // Only a read-only note has a card; anything else closes it where it stands.
        if self.panel != Panel::Editor || self.library.active_note().read_only.is_none() {
            self.file_status_popover = false;
        }
        if !self.file_status_popover {
            return None;
        }
        let note = self.library.active_note();
        let reason = note.read_only.clone()?;
        let conflicted = note.conflicted;
        let openable = note.path.is_some();
        let width = px(300.).min(window.bounds().size.width - px(16.));
        Some(
            div()
                .id("file-status-card")
                .absolute()
                .bottom(px(48.))
                .left(px(8.))
                .w(width)
                .p(px(12.))
                .flex()
                .flex_col()
                .gap_2()
                .rounded(POPOVER_RADIUS)
                .bg(self.surface_color())
                .border_1()
                .border_color(self.border_color())
                .shadow(popover_shadow())
                .occlude()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_mouse_down_out(cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    // The indicator closes the card itself; preserve its click.
                    if event.position.y < window.bounds().size.height - px(44.) {
                        this.file_status_popover = false;
                        this.focus_editor(window, cx);
                        cx.notify();
                        cx.stop_propagation();
                    }
                }))
                .child(
                    div()
                        .text_size(px(12.))
                        .line_height(px(17.))
                        .text_color(self.control_text())
                        .child(if conflicted {
                            format!("{reason} Autosave is paused until the conflict is resolved.")
                        } else {
                            reason
                        }),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .when(openable, |s| {
                            s.child(self.button(
                                "file-status-open",
                                "Open in Default Editor",
                                Intent::OpenExternally,
                                cx,
                            ))
                        })
                        .child(self.button(
                            "file-status-reveal",
                            "Reveal in Finder",
                            Intent::RevealNote,
                            cx,
                        ))
                        .when(conflicted, |s| {
                            s.child(self.button(
                                "file-status-conflict",
                                "Review Conflict…",
                                Intent::ReviewConflict,
                                cx,
                            ))
                        }),
                ),
        )
    }

    pub(super) fn format_popover(&self, window: &Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let items = self.format_items(cx);
        let width = px(216.);
        let viewport = window.bounds().size.width;
        let toolbar_left = (viewport - TOOLBAR_CAPSULE) / 2.;
        let left = match self.format_menu {
            Some(FormatMenu::Inline) => toolbar_left + px(42.),
            Some(FormatMenu::List) => toolbar_left + TOOLBAR_CAPSULE - width,
            _ => toolbar_left,
        }
        .clamp(px(8.), viewport - width - px(8.));
        let height =
            (px(10.) + ROW_HEIGHT * items.len() as f32).min(window.bounds().size.height - px(100.));
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
            .left(left)
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
