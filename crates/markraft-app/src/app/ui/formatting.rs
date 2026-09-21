use super::*;

type FormatItem = (&'static str, &'static str, Intent, bool);

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
/// [`NotesApp::file_states`] builds them in.
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
    pub actions: Vec<(&'static str, Intent)>,
}

impl FileState {
    /// The whole state as assistive technology hears it: the label alone is two
    /// words and says too little on its own.
    pub(super) fn announced(&self) -> String {
        format!("{}. {}", self.label, self.detail)
    }
}

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
        if self.interaction.html().is_some() {
            return;
        }
        let closing = self.interaction.format_menu() == Some(menu);
        self.editor()
            .update(cx, |editor, cx| editor.cancel_composition(cx));
        if closing {
            self.close_popover(cx);
        } else {
            self.show_popover(Popover::Format(menu), cx);
        }
        self.format_selected = self
            .format_items(cx)
            .iter()
            .position(|item| item.3)
            .unwrap_or(0);
        if self.interaction.format_menu().is_some() {
            window.focus(&self.panel_focus, cx);
            let weak = cx.entity().downgrade();
            window.on_next_frame(move |_, cx| {
                let _ = weak.update(cx, |this, cx| {
                    if this.interaction.format_menu() == Some(menu) {
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
        match self.interaction.format_menu() {
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
        let expanded = menu.is_some() && self.interaction.format_menu() == menu;
        let active = toggled == Some(true);
        // A control that takes the whole table away is written in the destructive ink,
        // as its ⌘K row is.
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
                // A tooltip beside an open menu only covers the menu.
                .when(!expanded, |s| s.tooltip(self.hint(label)))
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
        let states = self.file_states();
        // The toolbar is 268px wide and centered, so its left edge sits 91px from a
        // 450px window's. The full mode label takes about that much; adding the
        // indicator and its gap needs some 22px more, which a 500px window has.
        let compact_vim = self.format_toolbar
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

    /// Everything about the open note and its folder the user has to deal with,
    /// most pressing first.
    ///
    /// A save failure comes first because it is the only one losing work for as long
    /// as it holds; a conflict next, because autosave is stopped until it is settled.
    /// The rest are true but not urgent.
    pub(in crate::app) fn has_file_status(&self) -> bool {
        !self.file_states().is_empty()
    }

    pub(super) fn file_states(&self) -> Vec<FileState> {
        let mut states = Vec::new();
        if self.persistence.is_none() {
            return states;
        }
        let note = self.library.active_note();
        if let Some(error) = &self.error {
            states.push(FileState {
                id: "state-unsaved",
                icon: Icon::Alert,
                label: "Not saved".into(),
                detail: error.clone(),
                urgent: true,
                actions: vec![
                    ("Retry", Intent::Retry),
                    ("Save a Copy…", Intent::SaveCopy),
                    ("Reload from Disk…", Intent::Reload),
                ],
            });
        }
        if note.conflicted {
            states.push(FileState {
                id: "state-conflict",
                icon: Icon::Conflict,
                label: "Conflict".into(),
                detail: "Another app changed this file. Autosave is paused until you \
                         choose which version to keep."
                    .into(),
                urgent: true,
                actions: vec![("Resolve…", Intent::ReviewConflict)],
            });
        }
        if let Some(reason) = &note.read_only {
            states.push(FileState {
                id: "state-read-only",
                icon: Icon::Lock,
                label: "Read-only".into(),
                detail: reason.clone(),
                urgent: false,
                actions: if note.path.is_some() {
                    vec![
                        ("Open in Default Editor", Intent::OpenExternally),
                        ("Reveal", Intent::RevealNote),
                    ]
                } else {
                    Vec::new()
                },
            });
        }
        // Without a folder there is nowhere to file a new note, so it waits in
        // recovery for as long as it takes. A draft whose name is still settling files
        // itself in a moment and needs nothing said about it; this one is waiting for
        // the user, and nothing else on the window says so.
        if self.unfiled_draft() && crate::app::is_draft(note) {
            states.push(FileState {
                id: "state-unfiled",
                icon: Icon::Drafts,
                // Not urgent: nothing is being lost while it holds. The note comes
                // back from recovery on the next launch, so this is about the file
                // the writer may be expecting to find, not about the words.
                label: "Unsaved".into(),
                detail: "This note has never been saved to a file. It is kept inside \
                         Markraft until you choose where it goes."
                    .into(),
                urgent: false,
                actions: vec![("Save As…", Intent::Save)],
            });
        }
        // The capsule is a pointer to work that is not on screen. When the only note
        // that is not in a file is the one being written, it points at itself, and the
        // note already says what it is — the card above, where it has no file to go to.
        let drafts = self.draft_count();
        let elsewhere = drafts - usize::from(crate::app::is_draft(note));
        if elsewhere > 0 {
            states.push(FileState {
                id: "state-drafts",
                icon: Icon::Drafts,
                label: format!("{drafts} draft{}", if drafts == 1 { "" } else { "s" }),
                detail: format!(
                    "{drafts} note{} not in a file the way you left {}.",
                    if drafts == 1 { " is" } else { "s are" },
                    if drafts == 1 { "it" } else { "them" }
                ),
                urgent: false,
                actions: vec![("Show Drafts…", Intent::Drafts)],
            });
        }
        if self.folder_was_created {
            states.push(FileState {
                id: "state-new-folder",
                icon: Icon::Open,
                label: "New folder".into(),
                detail: "The notes folder in your settings was not there, so an empty \
                         one was made. Choose another folder if the old one moved."
                    .into(),
                urgent: false,
                actions: vec![
                    ("Choose Folder…", Intent::ChooseFolder),
                    ("Show Folder", Intent::Reveal),
                ],
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
        let lit = self
            .file_status_flash
            .is_some_and(|until| Instant::now() < until);
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
                    .child(format!("+{}", states.len() - shown))
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
            .aria_label(state.announced())
            .aria_expanded(self.interaction.file_status())
            // The card opens right beside it, so a tooltip would be drawn over the
            // card's own words.
            .when(!self.interaction.file_status(), |s| {
                s.tooltip(self.hint("Click for what to do about it"))
            })
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
    /// kind of thing, they stack, and a person dealing with a conflict wants to know
    /// the save also failed.
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
                .overflow_y_scroll()
                .p(px(4.))
                .flex()
                .flex_col()
                .rounded(POPOVER_RADIUS)
                .bg(self.surface_color())
                .border_1()
                .border_color(self.border_color())
                .shadow(popover_shadow())
                .occlude()
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
                                        *label,
                                        intent.clone(),
                                        cx,
                                    )
                                }),
                            ))
                        })
                })),
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
                if event.position.y < window.bounds().size.height - FOOTER_HEIGHT {
                    this.close_popover(cx);
                    this.focus_editor(window, cx);
                    cx.notify();
                    cx.stop_propagation();
                }
            }))
            .child(list)
    }
}
