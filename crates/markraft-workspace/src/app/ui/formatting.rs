use super::*;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests;

type FormatItem = (String, &'static str, Intent, bool);

/// How many capsules stand beside the mode badge before the rest fold into a count.
const CAPSULES_SHOWN: usize = 2;
/// A capsule's height. Its corner is half of this, so it reads as one of the
/// rounded controls the chrome is made of.
const CAPSULE_HEIGHT: Pixels = px(24.);
/// How far the mode badge and the capsules beside it stand from the window's edge.
/// The card they open is anchored to the same line.
const CAPSULES_LEFT: Pixels = px(12.);
/// The box a capsule's symbol is drawn into. A symbol carries its own margin, so a
/// box the size of the label's type draws a glyph shorter than the label's capitals;
/// this is measured against the 11px text beside it rather than set to match it.
const CAPSULE_ICON: f32 = 14.;

/// One thing about the open note the user has to deal with, said in the corner and
/// explained in the card the corner opens.
///
/// These are *states*, not events: each one outlives any sentence about it, which is
/// why none of them is a notice. Ordered most pressing first, which is the order
/// [`WorkspaceView::file_states`] builds them in.
pub(super) struct FileState {
    /// Element id, and the spring's key; stable per kind so the entry animation
    /// does not replay as the label's number changes.
    pub id: &'static str,
    pub icon: Icon,
    /// Two words at most: it sits in 11px beside the mode badge.
    pub label: String,
    /// What the card says, ending in what to do about it.
    pub detail: String,
    /// Drawn in the colour destructive controls use. Reserved for a state that is
    /// losing work for as long as it holds.
    pub urgent: bool,
    /// What the card offers, left to right.
    pub actions: Vec<(String, Intent)>,
}

impl FileState {
    /// The whole state as assistive technology hears it: the label alone is two
    /// words and says too little on its own.
    pub(super) fn announced(&self, i18n: &crate::locale::I18n) -> String {
        i18n.text_with(
            "surfaces.file.announced",
            &[("label", &self.label), ("detail", &self.detail)],
        )
    }
}

// Three 38px menus, four 24px actions, two 9px separators, eight 4px gaps,
// and 4px padding on each side. Keep menu anchors tied to this geometry.
const TOOLBAR_CAPSULE: Pixels = px(268.);

impl WorkspaceView {
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
        let closing = self.interaction.format_menu() == Some(menu);
        self.editor()
            .update(cx, |editor, cx| editor.cancel_composition(cx));
        if closing {
            self.close_popover(cx);
        } else {
            self.show_popover(Popover::Format(menu), cx);
        }
        let selected = self
            .format_items(cx)
            .iter()
            .position(|item| item.3)
            .unwrap_or(0);
        self.format.select(selected);
        if self.interaction.format_menu().is_some() {
            window.focus(self.ring.panel(), cx);
            let weak = cx.entity().downgrade();
            window.on_next_frame(move |_, cx| {
                let _ = weak.update(cx, |this, cx| {
                    if this.interaction.format_menu() == Some(menu) {
                        this.format.select(this.format.row());
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
        let block = |label: &'static str, block: doc::Block| {
            let active = kind == Some(block);
            (label, Intent::Block(block), active)
        };
        let mark = |label: &'static str, inline: doc::Inline| {
            let active = inline.is_active(&marks);
            (label, Intent::Mark(inline), active)
        };
        let items: Vec<(&'static str, Intent, bool)> = match self.interaction.format_menu() {
            Some(FormatMenu::Block) => {
                let mut items = vec![block("command.paragraph", doc::Block::Paragraph)];
                for (level, label) in [
                    (1, "command.heading-1"),
                    (2, "command.heading-2"),
                    (3, "command.heading-3"),
                    (4, "command.heading-4"),
                    (5, "command.heading-5"),
                    (6, "command.heading-6"),
                ] {
                    items.push(block(label, doc::Block::Heading(level)));
                }
                items
            }
            Some(FormatMenu::Inline) => vec![
                mark("command.bold", doc::Inline::Bold),
                mark("command.italic", doc::Inline::Italic),
                mark("command.strikethrough", doc::Inline::Strikethrough),
            ],
            Some(FormatMenu::List) => vec![
                block("surfaces.format.no-list", doc::Block::Paragraph),
                block("command.ordered-list", doc::Block::Ordered),
                block("command.bullet-list", doc::Block::Bullet),
                block("command.task-list", doc::Block::Task),
            ],
            None => vec![],
        };
        items
            .into_iter()
            .map(|(label, intent, active)| {
                // "No List" is the paragraph again, but the row is about leaving the
                // list, not about ⌘0, so it carries no hint.
                let shortcut = if self.interaction.format_menu() == Some(FormatMenu::List)
                    && matches!(intent, Intent::Block(doc::Block::Paragraph))
                {
                    ""
                } else {
                    super::shortcut_label(&intent)
                };
                (self.i18n.text(label), shortcut, intent, active)
            })
            .collect()
    }

    pub(super) fn format_key(
        &mut self,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let items = self.format_items(cx);
        match key {
            "up" => self.format.up(),
            "down" => self.format.down(items.len()),
            "enter" => {
                if let Some((_, _, intent, _)) = items.get(self.format.row()) {
                    self.intent(intent.clone(), window, cx);
                }
            }
            _ => return false,
        }
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
        label: String,
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
        label: String,
        kind: Icon,
        intent: Intent,
        toggled: Option<bool>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let menu = match intent {
            Intent::FormatMenu(menu) => Some(menu),
            _ => None,
        };
        let enabled = self.editing_enabled(&intent, cx);
        let expanded = menu.is_some() && self.interaction.format_menu() == menu;
        let active = toggled == Some(true);
        // A control that takes a row, a column or the whole table away is written in
        // the destructive ink, as its ⌘K row is.
        let ink = if matches!(
            intent,
            Intent::Table(TableEdit::DeleteTable | TableEdit::DeleteRow | TableEdit::DeleteColumn)
        ) {
            self.danger()
        } else if active {
            self.control_text()
        } else {
            self.muted()
        };
        div()
            .id(id)
            .role(Role::Button)
            .aria_label(label.clone())
            .when(!enabled, |s| s.opacity(0.45))
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
            // A tooltip beside an open menu only covers the menu.
            .when(!expanded && super::shows_tooltip(&intent), |s| {
                s.tooltip(self.hint(label))
            })
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.intent(intent.clone(), window, cx);
            }))
            .child(icon(kind, ink))
            .when(menu.is_some(), |s| {
                s.gap(px(2.))
                    .child(sized_icon(Icon::ChevronDown, self.muted(), 12.))
            })
    }

    fn toolbar_button(
        &self,
        id: &'static str,
        label: String,
        kind: Icon,
        intent: Intent,
        toggled: Option<bool>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let menu = match intent {
            Intent::FormatMenu(menu) => Some(menu),
            _ => None,
        };
        let enabled = self.editing_enabled(&intent, cx);
        let expanded = menu.is_some() && self.interaction.format_menu() == menu;
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
            .opacity(if !enabled {
                0.45
            } else if expanded {
                0.8
            } else {
                1.
            })
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
        let states = self.file_states();
        // The toolbar is 268px wide and centered, so its left edge sits 91px from a
        // 450px window's. The full mode label takes about that much; adding the
        // indicator and its gap needs some 22px more, which a 500px window has.
        let compact_vim = self.toolbar.shown()
            && (viewport < px(450.) || (!states.is_empty() && viewport < px(500.)));
        div()
            .h(FOOTER_HEIGHT)
            .flex_shrink_0()
            .relative()
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .absolute()
                    .left(CAPSULES_LEFT)
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .children(self.vim_badge(compact_vim))
                    .children(self.status_capsules(&states, cx)),
            )
            .when(
                self.preferences.show_word_count && !self.toolbar.shown(),
                |s| {
                    s.child(
                        div()
                            .id("word-count")
                            .debug_selector(|| "word-count".into())
                            .role(Role::Button)
                            .aria_label(self.i18n.text("surfaces.format.toggle-count"))
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
                            // The footer floats over the editor. Keep this press from
                            // moving its caret or starting a double-click selection.
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.intent(Intent::ToggleCount, window, cx)
                            }))
                            .child(count)
                            .with_spring(
                                "word-count-enter",
                                SpringAnimation::new(super::FORMAT_SPRING)
                                    .to(true)
                                    .from(false)
                                    .playback(playback(reduce_motion)),
                                |s, phase| s.opacity(phase.interpolate_clamped(0., 1.)),
                            ),
                    )
                },
            )
            .child(
                div()
                    .absolute()
                    .right(px(8.))
                    .child(
                        self.chrome_capsule().size(px(32.)).justify_center().child(
                            self.icon_button(
                                "format-toolbar-toggle",
                                if self.toolbar.shown() {
                                    self.i18n.text("surfaces.format.hide-toolbar")
                                } else {
                                    self.i18n.text("surfaces.format.show-toolbar")
                                },
                                if self.toolbar.shown() {
                                    Icon::Close
                                } else {
                                    Icon::Text
                                },
                                Intent::ToggleFormatToolbar,
                                cx,
                            )
                            .debug_selector(|| "format-toolbar-toggle".into())
                            .size(px(32.))
                            .rounded_full()
                            .opacity(1.)
                            .aria_expanded(self.toolbar.shown()),
                        ),
                    )
                    .with_spring(
                        "format-toggle-fade",
                        // The close button follows window hover even while the
                        // center formatting toolbar remains expanded.
                        Self::chrome_spring(self.presence.pointer_inside(), reduce_motion),
                        |s, phase| s.opacity(phase.interpolate_clamped(0., 1.)),
                    ),
            )
            .when(self.toolbar.shown(), |s| {
                s.child(
                    self.chrome_capsule()
                        .p(px(4.))
                        .gap(px(4.))
                        .child(self.toolbar_button(
                            "format-block-menu",
                            self.i18n.text("surfaces.format.headings"),
                            Icon::Heading,
                            Intent::FormatMenu(FormatMenu::Block),
                            Some(matches!(kind, Some(doc::Block::Heading(_)))),
                            cx,
                        ))
                        .child(
                            self.toolbar_button(
                                "format-inline-menu",
                                self.i18n.text("surfaces.format.text"),
                                Icon::Italic,
                                Intent::FormatMenu(FormatMenu::Inline),
                                Some(
                                    [
                                        doc::Inline::Bold,
                                        doc::Inline::Italic,
                                        doc::Inline::Strikethrough,
                                    ]
                                    .iter()
                                    .any(|inline| inline.is_active(&marks)),
                                ),
                                cx,
                            ),
                        )
                        .child(self.toolbar_button(
                            "format-link",
                            self.i18n.text("surfaces.format.link"),
                            Icon::Link,
                            Intent::Link,
                            Some(linked),
                            cx,
                        ))
                        .child(self.toolbar_button(
                            "format-inline-code",
                            self.i18n.text("surfaces.format.inline-code"),
                            Icon::Code,
                            Intent::Mark(doc::Inline::Code),
                            Some(doc::Inline::Code.is_active(&marks)),
                            cx,
                        ))
                        .child(self.format_divider())
                        .child(self.toolbar_button(
                            "format-code-block",
                            self.i18n.text("surfaces.format.code-block"),
                            Icon::CodeBlock,
                            Intent::Block(doc::Block::Code),
                            Some(kind == Some(doc::Block::Code)),
                            cx,
                        ))
                        .child(self.toolbar_button(
                            "format-quote",
                            self.i18n.text("surfaces.format.quote"),
                            Icon::Quote,
                            Intent::Block(doc::Block::Quote),
                            Some(kind == Some(doc::Block::Quote)),
                            cx,
                        ))
                        .child(self.format_divider())
                        .child(self.toolbar_button(
                            "format-list-menu",
                            self.i18n.text("surfaces.format.lists"),
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
                        ))
                        .with_spring(
                            "format-toolbar-enter",
                            SpringAnimation::new(super::FORMAT_SPRING)
                                .to(true)
                                .from(false)
                                .playback(playback(reduce_motion)),
                            |s, phase| {
                                s.opacity(phase.interpolate_clamped(0., 1.))
                                    .mt(phase.interpolate_clamped(px(4.), px(0.)))
                            },
                        ),
                )
            })
    }

    pub(in crate::app) fn has_file_status(&self) -> bool {
        !self.file_states().is_empty()
    }

    /// Everything about the open note and its folder the user has to deal with,
    /// most pressing first.
    ///
    /// A save failure comes first because it is the only one losing work for as long
    /// as it holds. The rest are true but not urgent.
    pub(super) fn file_states(&self) -> Vec<FileState> {
        let mut states = Vec::new();
        if self.notes.persistence.is_none() {
            return states;
        }
        let note = self.notes.library.active_note();
        if let Some(error) = self.feedback.error() {
            states.push(FileState {
                id: "state-unsaved",
                icon: Icon::Alert,
                label: self.i18n.text("surfaces.file.not-saved"),
                detail: error.render(&self.i18n),
                urgent: true,
                actions: vec![
                    (self.i18n.text("surfaces.file.retry"), Intent::Retry),
                    (self.i18n.text("surfaces.file.save-as"), Intent::SaveAs),
                    (self.i18n.text("surfaces.file.reload"), Intent::Reload),
                ],
            });
        }
        if let Some(reason) = &note.read_only {
            states.push(FileState {
                id: "state-read-only",
                icon: Icon::Lock,
                label: self.i18n.text("surfaces.file.read-only"),
                detail: reason.render(&self.i18n),
                urgent: false,
                actions: if note.path.is_some() {
                    vec![
                        (
                            self.i18n.text("surfaces.file.open-externally"),
                            Intent::OpenExternally,
                        ),
                        (self.i18n.text("surfaces.file.reveal"), Intent::RevealNote),
                    ]
                } else {
                    Vec::new()
                },
            });
        }
        states
    }

    /// The capsules in the lower left: one per state the file is in that the user
    /// has to deal with, most pressing first.
    ///
    /// Two fit beside the mode badge; the rest fold into a count, because the card
    /// they all open lists every one of them anyway. A capsule says its state in
    /// words rather than in an icon alone, which is what lets a refused keystroke
    /// go unexplained elsewhere: the reason is already legible here.
    fn status_capsules(&self, states: &[FileState], cx: &mut Context<Self>) -> Vec<AnyElement> {
        if states.is_empty() {
            return Vec::new();
        }
        let lit = self.feedback.file_status_flashing();
        let shown = states.len().min(CAPSULES_SHOWN);
        let mut row: Vec<AnyElement> = states[..shown]
            .iter()
            .enumerate()
            .map(|(index, state)| {
                self.status_capsule(state, lit && index == 0, cx)
                    .into_any_element()
            })
            .collect();
        if states.len() > shown {
            row.push(
                div()
                    .h(CAPSULE_HEIGHT)
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .rounded(CAPSULE_HEIGHT / 2.)
                    .bg(self.chrome_fill(0.05))
                    .text_size(px(11.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(self.muted())
                    .child(self.i18n.text_with(
                        "surfaces.file.more",
                        &[("count", &(states.len() - shown).to_string())],
                    ))
                    .into_any_element(),
            );
        }
        row
    }

    /// One capsule. `lit` is a keystroke the file just refused, which lights the
    /// most pressing state rather than every one of them.
    fn status_capsule(
        &self,
        state: &FileState,
        lit: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let urgent = state.urgent || lit;
        let ink = if urgent { self.danger() } else { self.muted() };
        // A refusal is not a hover, so it is not drawn as one: an urgent capsule
        // takes the colour destructive controls use.
        let (resting, attention) = if urgent {
            (self.danger().opacity(0.16), self.danger().opacity(0.28))
        } else {
            (self.chrome_fill(0.05), self.chrome_fill(0.12))
        };
        let clear = self.chrome_fill(0.);
        div()
            .id(state.id)
            .role(Role::Button)
            .aria_label(state.announced(&self.i18n))
            .aria_expanded(self.interaction.file_status())
            .flex_shrink_0()
            .h(CAPSULE_HEIGHT)
            .pl(px(7.))
            .pr(px(9.))
            .flex()
            .items_center()
            .gap(px(5.))
            .rounded(CAPSULE_HEIGHT / 2.)
            .cursor_pointer()
            .hover(|s| s.bg(attention))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, _, window, cx| {
                cx.stop_propagation();
                this.intent(Intent::FileStatus, window, cx);
            }))
            .child(sized_icon(state.icon, ink, CAPSULE_ICON))
            .child(
                div()
                    .text_size(px(11.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(ink)
                    .child(state.label.clone()),
            )
            // A state the user has to deal with arrives rather than appears, so the
            // eye is sent to it once. Reduced motion keeps the fill and drops the
            // travel.
            .with_spring(
                state.id,
                Self::chrome_spring(true, cx.reduce_motion()),
                move |s, phase| s.bg(phase.interpolate_clamped(clear, resting)),
            )
    }

    /// Every state the file and its folder are in, each with what to do about it.
    ///
    /// One card for all of them rather than one control per state: they are the same
    /// kind of thing, they stack, and a person dealing with a read-only file wants to
    /// know the save also failed.
    pub(super) fn file_status_card(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        let states = self.file_states();
        if !self.interaction.file_status() || states.is_empty() {
            return None;
        }
        let width = px(300.).min(window.bounds().size.width - px(16.));
        // It grows upward from the footer, and several states at once can ask for
        // more than the window has. What does not fit scrolls rather than being cut
        // off above the title bar.
        let room = (window.bounds().size.height - px(64.)).max(px(120.));
        let last = states.len() - 1;
        Some(
            div()
                .id("file-status-card")
                .absolute()
                .bottom(FOOTER_HEIGHT)
                // Flush with the capsules it belongs to, which start at the footer's
                // own left inset.
                .left(CAPSULES_LEFT)
                .w(width)
                .max_h(room)
                .flex()
                .flex_col()
                .rounded(POPOVER_RADIUS)
                .bg(self.surface_color())
                .border_1()
                .border_color(self.border_color())
                .shadow(popover_shadow())
                .occlude()
                .overflow_hidden()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_mouse_down_out(cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    // The capsules close the card themselves; preserve their click.
                    if event.position.y < window.bounds().size.height - FOOTER_HEIGHT {
                        this.close_popover(cx);
                        this.focus_editor(window, cx);
                        cx.notify();
                        cx.stop_propagation();
                    }
                }))
                .child(
                    div()
                        .id("file-status-states")
                        .track_scroll(&self.file_status_scroll)
                        .overflow_y_scroll()
                        .min_h_0()
                        .p(px(4.))
                        .flex()
                        .flex_col()
                        .children(states.iter().enumerate().map(|(index, state)| {
                            let ink = if state.urgent {
                                self.danger()
                            } else {
                                self.muted()
                            };
                            div()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .px(px(12.))
                                .py(px(12.))
                                .when(index < last, |s| {
                                    s.border_b_1().border_color(self.border_color())
                                })
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(7.))
                                        .child(sized_icon(state.icon, ink, 15.))
                                        .child(
                                            div()
                                                .text_size(px(12.))
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_color(ink)
                                                .child(state.label.clone()),
                                        ),
                                )
                                .child(
                                    div()
                                        .text_size(px(12.))
                                        .line_height(px(17.))
                                        .text_color(self.control_text())
                                        .child(state.detail.clone()),
                                )
                                .when(!state.actions.is_empty(), |s| {
                                    s.child(div().flex().flex_wrap().gap_2().children(
                                        state.actions.iter().map(|(label, intent)| {
                                            self.button(
                                                SharedString::from(format!("{}-{label}", state.id)),
                                                label.clone(),
                                                intent.clone(),
                                                cx,
                                            )
                                        }),
                                    ))
                                })
                        })),
                )
                .child(self.scrollbar("file-status-scrollbar", &self.file_status_scroll)),
        )
    }

    pub(super) fn format_popover(&self, window: &Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let items = self.format_items(cx);
        let width = px(216.);
        let viewport = window.bounds().size.width;
        let toolbar_left = (viewport - TOOLBAR_CAPSULE) / 2.;
        let left = match self.interaction.format_menu() {
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
            .aria_label(self.i18n.text("surfaces.format.title"))
            .track_scroll(self.format.scroll())
            .overflow_y_scroll()
            .size_full()
            .p(px(4.))
            .pr(thumb_lane(px(4.), self.format.scroll()));
        for (index, (label, hint, intent, checked)) in items.into_iter().enumerate() {
            let stop = SharedString::from(format!("format-choice-{index}"));
            list = list.child(
                div()
                    .id(stop.clone())
                    .role(Role::Button)
                    .aria_label(label.clone())
                    .aria_selected(index == self.format.row())
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
                    .when(index == self.format.row(), |s| s.bg(self.selected_color()))
                    .active(|s| s.bg(self.pressed_color()))
                    .on_mouse_move(cx.listener(move |this, _, _, cx| {
                        if this.format.row() != index {
                            this.format.point_at(index);
                            cx.notify();
                        }
                    }))
                    // A row that the wheel brings under a resting pointer is picked
                    // too, so one row is lit and not two.
                    .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                        if *hovered && this.format.row() != index {
                            this.format.point_at(index);
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
                if event.position.y < window.bounds().size.height - FOOTER_HEIGHT {
                    this.close_popover(cx);
                    this.focus_editor(window, cx);
                    cx.notify();
                    cx.stop_propagation();
                }
            }))
            .child(list)
            .child(self.scrollbar("format-menu-scrollbar", self.format.scroll()))
    }
}
