mod formatting;
mod icons;

use super::*;
use icons::{Icon, icon};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone)]
enum Intent {
    New,
    Browse,
    Trash,
    Actions,
    ToggleFormatToolbar,
    FormatMenu(FormatMenu),
    Settings,
    Back,
    Delete,
    Pin,
    PinNote(String),
    TrashNote(String),
    Copy,
    Export,
    Import,
    Select(String),
    Restore(String),
    Theme(Option<bool>),
    Login,
    AutoHeight,
    Shortcut,
    Reveal,
    Recover,
    Retry,
    SaveCopy,
    Reload,
    Mark(Mark),
    Block(BlockKind),
}
impl NotesApp {
    fn intent(&mut self, intent: Intent, window: &mut Window, cx: &mut Context<Self>) {
        match intent {
            Intent::New => self.new_note(window, cx),
            Intent::Browse => self.open_panel(Panel::Browse, window, cx),
            Intent::Trash => self.open_panel(Panel::Trash, window, cx),
            Intent::Actions => self.open_panel(Panel::Actions, window, cx),
            Intent::ToggleFormatToolbar => {
                self.format_toolbar = !self.format_toolbar;
                self.format_menu = None;
                self.panel = Panel::Editor;
                self.focus_editor(window, cx);
                cx.notify();
            }
            Intent::FormatMenu(menu) => self.open_format_menu(menu, window, cx),
            Intent::Settings => self.open_panel(Panel::Settings, window, cx),
            Intent::Back => {
                self.query.update(cx, |e, cx| e.cancel_composition(cx));
                self.panel = Panel::Editor;
                self.focus_editor(window, cx);
                cx.notify();
            }
            Intent::Delete => self.delete_note(window, cx),
            Intent::Pin => {
                let id = self.library.active_id.clone();
                self.toggle_pin(&id, cx);
                self.intent(Intent::Back, window, cx);
            }
            Intent::PinNote(id) => self.toggle_pin(&id, cx),
            Intent::TrashNote(id) => self.trash_note(&id, window, cx),
            Intent::Select(id) => self.select_note(&id, window, cx),
            Intent::Restore(id) => self.restore_note(&id, cx),
            Intent::Copy => {
                self.copy_markdown(cx);
                self.intent(Intent::Back, window, cx);
            }
            Intent::Export => {
                self.intent(Intent::Back, window, cx);
                self.export(cx);
            }
            Intent::Import => self.import(window, cx),
            Intent::Theme(mode) => {
                self.library.preferences.dark_mode = mode;
                self.apply_theme(window, cx);
                self.changed(cx);
            }
            Intent::Login => {
                if let Some(platform) = &mut self.platform {
                    let enabled = platform.launch_at_login_enabled();
                    match platform.set_launch_at_login(!enabled) {
                        Ok(()) => self.inform("Login setting updated", cx),
                        Err(e) => {
                            self.platform_error = Some(e);
                            cx.notify();
                        }
                    }
                }
            }
            Intent::AutoHeight => {
                self.library.preferences.auto_height = !self.library.preferences.auto_height;
                self.changed(cx);
            }
            Intent::Shortcut => self.apply_shortcut(cx),
            Intent::Reveal => cx.reveal_path(&self.path),
            Intent::Recover => self.recover(true, window, cx),
            Intent::Retry => self.recover(false, window, cx),
            Intent::SaveCopy => self.save_copy(cx),
            Intent::Reload => self.reload(window, cx),
            Intent::Mark(mark) => {
                self.format_menu = None;
                self.editor().update(cx, |e, cx| e.toggle_mark(mark, cx));
                self.panel = Panel::Editor;
                self.focus_editor(window, cx);
            }
            Intent::Block(kind) => {
                self.format_menu = None;
                self.editor().update(cx, |e, cx| e.set_block_kind(kind, cx));
                self.panel = Panel::Editor;
                self.focus_editor(window, cx);
            }
        }
    }
    fn muted(&self) -> Hsla {
        if self.dark {
            rgb(0x93959d)
        } else {
            rgb(0x82858b)
        }
        .into()
    }
    fn hover_color(&self) -> Hsla {
        if self.dark {
            rgb(0x36373a)
        } else {
            rgb(0xeaeaea)
        }
        .into()
    }
    fn selected_color(&self) -> Hsla {
        if self.dark {
            rgb(0x414246)
        } else {
            rgb(0xe2e2e2)
        }
        .into()
    }
    fn pressed_color(&self) -> Hsla {
        if self.dark {
            rgb(0x505156)
        } else {
            rgb(0xd5d5d5)
        }
        .into()
    }
    fn control_text(&self) -> Hsla {
        if self.dark {
            rgb(0xe6e6e8)
        } else {
            rgb(0x272727)
        }
        .into()
    }
    fn border_color(&self) -> Hsla {
        if self.dark {
            rgb(0x383a40)
        } else {
            rgb(0xdcdcdc)
        }
        .into()
    }
    fn button(
        &self,
        id: &'static str,
        label: impl Into<SharedString>,
        intent: Intent,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        self.button_with_hover(id, label, intent, self.hover_color(), cx)
    }
    /// GPUI accepts a single hover style per element, so callers that need a
    /// different hover fill choose it here instead of chaining another `.hover`.
    fn button_with_hover(
        &self,
        id: &'static str,
        label: impl Into<SharedString>,
        intent: Intent,
        hover: Hsla,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let label: SharedString = label.into();
        let accessible_label = match id {
            "auto-height" => "Automatic height",
            "launch-at-login" => "Launch at login",
            _ => label.as_ref(),
        }
        .to_string();
        div()
            .id(id)
            .role(Role::Button)
            .aria_label(accessible_label)
            .when(matches!(id, "auto-height" | "launch-at-login"), |s| {
                s.role(Role::Switch)
                    .aria_toggled(if label.as_ref() == "On" {
                        accesskit::Toggled::True
                    } else {
                        accesskit::Toggled::False
                    })
            })
            .px_2()
            .h(px(28.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.))
            .cursor_pointer()
            .text_size(px(13.))
            .text_color(self.muted())
            .hover(move |s| s.bg(hover))
            .active(|s| s.bg(self.pressed_color()).text_color(self.control_text()))
            .on_click(
                cx.listener(move |this, _, window, cx| this.intent(intent.clone(), window, cx)),
            )
            .child(if matches!(id, "auto-height" | "launch-at-login") {
                let enabled = label.as_ref() == "On";
                div()
                    .w(px(28.))
                    .h(px(17.))
                    .p(px(2.))
                    .rounded_full()
                    .bg(if enabled {
                        notes_style(self.dark).marker
                    } else if self.dark {
                        rgb(0x595b62).into()
                    } else {
                        rgb(0xc5c6cb).into()
                    })
                    .child(
                        div()
                            .size(px(13.))
                            .rounded_full()
                            .bg(rgb(0xffffff))
                            .when(enabled, |s| s.ml(px(11.))),
                    )
                    .into_any_element()
            } else {
                label.into_any_element()
            })
    }
    fn surface_color(&self) -> Hsla {
        if self.dark {
            rgb(0x2e2f33)
        } else {
            rgb(0xf8f8f8)
        }
        .into()
    }
    fn shortcut(&self, hint: &str) -> Div {
        div().flex().gap(px(3.)).children(hint.chars().map(|key| {
            div()
                .w(px(17.))
                .h(px(18.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(5.))
                .bg(if self.dark {
                    rgba(0xffffff05)
                } else {
                    rgba(0xffffff20)
                })
                .border_1()
                .border_color(self.border_color())
                .text_size(px(11.))
                .text_color(self.muted())
                .child(key.to_string())
        }))
    }
    fn icon_button(
        &self,
        id: impl Into<ElementId>,
        label: &'static str,
        kind: Icon,
        intent: Intent,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let expanded = match intent {
            Intent::Browse => Some(self.panel == Panel::Browse || self.panel == Panel::Trash),
            Intent::Actions => Some(self.panel == Panel::Actions),
            _ => None,
        };
        div()
            .id(id)
            .role(Role::Button)
            .aria_label(label)
            .size(px(28.))
            .opacity(0.7)
            .when_some(expanded, |s, expanded| s.aria_expanded(expanded))
            .when(expanded == Some(true), |s| {
                s.bg(self.selected_color()).opacity(1.)
            })
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.))
            .cursor_pointer()
            .hover(|s| {
                s.bg(if expanded == Some(true) {
                    self.selected_color()
                } else {
                    self.hover_color()
                })
                .opacity(1.)
            })
            .active(|s| s.bg(self.pressed_color()).opacity(1.))
            .tooltip(self.hint(label))
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.intent(intent.clone(), window, cx);
            }))
            .child(icon(kind, self.chrome_icon_color()))
    }
    /// Toolbar icons recede while another application is active.
    pub(super) fn chrome_icon_color(&self) -> Hsla {
        if self.window_active {
            self.control_text()
        } else {
            self.muted()
        }
    }
    /// Tooltip builder; a label may carry a shortcut after " · ".
    pub(super) fn hint(
        &self,
        label: &'static str,
    ) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
        let dark = self.dark;
        move |_, cx| cx.new(|_| Hint { label, dark }).into()
    }
    fn row(
        &self,
        id: &'static str,
        label: &'static str,
        hint: &'static str,
        intent: Intent,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let kind = match &intent {
            Intent::New => Icon::Plus,
            Intent::Browse => Icon::Notes,
            Intent::Pin => Icon::Pin,
            Intent::Delete => Icon::Trash,
            Intent::Copy | Intent::SaveCopy => Icon::Copy,
            Intent::Export | Intent::Import => Icon::Export,
            Intent::Settings => Icon::Settings,
            Intent::Mark(Mark::Bold) => Icon::Bold,
            Intent::Mark(Mark::Italic) => Icon::Italic,
            Intent::Mark(Mark::Code) => Icon::Code,
            Intent::Mark(Mark::Strikethrough) => Icon::Strikethrough,
            Intent::Mark(Mark::Underline) => Icon::Underline,
            Intent::Block(BlockKind::Heading(_)) => Icon::Heading,
            Intent::Block(BlockKind::Quote) => Icon::Quote,
            Intent::Block(BlockKind::Bullet) => Icon::Bullet,
            Intent::Block(BlockKind::Task { .. }) => Icon::Task,
            _ => Icon::Paragraph,
        };
        div()
            .id(id)
            .role(Role::Button)
            .aria_label(label)
            .flex()
            .items_center()
            .gap(px(8.))
            .h(px(36.))
            .px_2()
            .rounded(px(8.))
            .cursor_pointer()
            .text_size(px(13.))
            .text_color(self.control_text())
            .hover(|s| s.bg(self.selected_color()))
            .active(|s| s.bg(self.pressed_color()))
            .when(matches!(intent, Intent::Delete), |s| {
                s.text_color(if self.dark {
                    rgb(0xf18a8a)
                } else {
                    rgb(0xc44d4d)
                })
            })
            .on_click(
                cx.listener(move |this, _, window, cx| this.intent(intent.clone(), window, cx)),
            )
            .child(icon(kind, self.control_text()))
            .child(div().flex_1().min_w_0().truncate().child(label))
            .child(self.shortcut(hint))
    }
    fn search_field(&self) -> Div {
        div()
            .h(px(44.))
            .flex_shrink_0()
            .px_4()
            .pt(px(12.))
            .child(self.query.clone())
    }
    fn picker(&self, cx: &mut Context<Self>) -> Div {
        let deleted = self.panel == Panel::Trash;
        let query = self.query.read(cx).document().plain_text();
        let notes = self.matching_notes(query.trim(), deleted);
        let mut list = div()
            .id("note-results")
            .track_scroll(&self.picker_scroll)
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_2()
            .pb_2();
        if notes.is_empty() {
            list = list.child(
                div()
                    .py_6()
                    .text_center()
                    .text_size(px(13.))
                    .text_color(self.muted())
                    .child(if deleted {
                        "No deleted notes"
                    } else {
                        "No matching notes"
                    }),
            );
        }
        for (index, note) in notes.iter().enumerate() {
            let id = note.id.clone();
            let intent = if deleted {
                Intent::Restore(id.clone())
            } else {
                Intent::Select(id.clone())
            };
            let current = note.id == self.library.active_id;
            let selected = index == self.selected;
            let count = note.document.plain_text().graphemes(true).count();
            let meta = if deleted {
                "Restore note"
            } else if current {
                "Current"
            } else if note.pinned {
                "Pinned"
            } else {
                "Note"
            };
            let mut controls = div()
                .absolute()
                .right(px(6.))
                .top(px(15.))
                .flex()
                .gap(px(4.));
            if !deleted && selected {
                controls = controls
                    .child(
                        self.icon_button(
                            SharedString::from(format!("pin-{id}")),
                            if note.pinned {
                                "Unpin Note"
                            } else {
                                "Pin Note"
                            },
                            Icon::Pin,
                            Intent::PinNote(id.clone()),
                            cx,
                        )
                        .size(px(24.))
                        .opacity(1.)
                        .aria_toggled(if note.pinned {
                            accesskit::Toggled::True
                        } else {
                            accesskit::Toggled::False
                        })
                        .when(note.pinned, |s| s.bg(self.hover_color())),
                    )
                    .child(
                        self.icon_button(
                            SharedString::from(format!("trash-{id}")),
                            "Move to Recently Deleted",
                            Icon::Trash,
                            Intent::TrashNote(id.clone()),
                            cx,
                        )
                        .size(px(24.)),
                    );
            } else if deleted && selected {
                controls = controls.child(
                    self.icon_button(
                        SharedString::from(format!("restore-{id}")),
                        "Restore Note",
                        Icon::Restore,
                        Intent::Restore(id.clone()),
                        cx,
                    )
                    .size(px(24.)),
                );
            } else if note.pinned && !deleted {
                controls = controls.child(
                    div()
                        .size(px(24.))
                        .opacity(1.)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(icon(Icon::Pin, self.muted())),
                );
            }
            list = list.child(
                div()
                    .id(SharedString::from(id))
                    .role(Role::Button)
                    .aria_label(note.title())
                    .aria_selected(selected)
                    .relative()
                    .h(px(58.))
                    .flex_shrink_0()
                    .rounded(px(9.))
                    .px_2()
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .when(selected, |s| s.bg(self.selected_color()))
                    .hover(|s| s.bg(self.selected_color()))
                    .active(|s| s.bg(self.pressed_color()))
                    .on_mouse_move(cx.listener(move |this, _, _, cx| {
                        if this.selected != index {
                            this.selected = index;
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.intent(intent.clone(), window, cx)
                    }))
                    .child(
                        div()
                            .w_full()
                            .pr(px(60.))
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .line_height(px(18.))
                                    .text_color(self.control_text())
                                    .truncate()
                                    .child(note.title()),
                            )
                            .child(
                                div()
                                    .mt(px(2.))
                                    .flex()
                                    .items_center()
                                    .gap(px(4.))
                                    .when(current && !deleted, |s| {
                                        s.child(
                                            div()
                                                .size(px(4.))
                                                .rounded_full()
                                                .bg(notes_style(self.dark).marker),
                                        )
                                    })
                                    .child(
                                        div()
                                            .text_size(px(12.))
                                            .line_height(px(18.))
                                            .text_color(self.muted())
                                            .truncate()
                                            .child(format!("{meta} · {count} characters")),
                                    ),
                            ),
                    )
                    .child(controls),
            );
        }
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(self.search_field())
            .child(
                div()
                    .px_4()
                    .pt_1()
                    .pb_2()
                    .text_size(px(11.))
                    .text_color(self.muted())
                    .child(if deleted { "Recently Deleted" } else { "Notes" }),
            )
            .child(list)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .h(px(36.))
                    .flex_shrink_0()
                    .px_2()
                    .border_t_1()
                    .border_color(self.border_color())
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(self.muted())
                            .child("↑↓  navigate   ↵  open"),
                    )
                    .child(self.button(
                        "browse-trash",
                        if deleted {
                            "All Notes"
                        } else {
                            "Recently Deleted"
                        },
                        if deleted {
                            Intent::Browse
                        } else {
                            Intent::Trash
                        },
                        cx,
                    )),
            )
    }
    fn settings(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let mut themes = div()
            .flex()
            .p(px(2.))
            .rounded(px(7.))
            .bg(self.hover_color());
        for (id, label, mode) in [
            ("theme-system", "Auto", None),
            ("theme-light", "Light", Some(false)),
            ("theme-dark", "Dark", Some(true)),
        ] {
            // The selected segment is raised above its track in both themes, and keeps
            // that fill while hovered (the track itself is the ordinary hover color).
            let selected = self.library.preferences.dark_mode == mode;
            let raised: Hsla = if self.dark {
                rgb(0x4a4b50).into()
            } else {
                self.surface_color()
            };
            let hover = if selected {
                raised
            } else {
                self.selected_color()
            };
            themes = themes.child(
                self.button_with_hover(id, label, Intent::Theme(mode), hover, cx)
                    .aria_label(format!("{label} appearance"))
                    .when(selected, |s| {
                        s.bg(raised).text_color(notes_style(self.dark).text)
                    }),
            );
        }
        let login = self
            .platform
            .as_ref()
            .is_some_and(|p| p.launch_at_login_enabled());
        div()
            .id("settings-content")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_4()
            .pb_4()
            .text_size(px(13.))
            .child(
                div()
                    .py_2()
                    .flex()
                    .justify_between()
                    .items_center()
                    .child("Appearance")
                    .child(themes),
            )
            .child(
                div()
                    .py_2()
                    .flex()
                    .justify_between()
                    .items_center()
                    .child("Grow with content")
                    .child(self.button(
                        "auto-height",
                        if self.library.preferences.auto_height {
                            "On"
                        } else {
                            "Off"
                        },
                        Intent::AutoHeight,
                        cx,
                    )),
            )
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(self.muted())
                    .child("Manual resizing keeps your chosen height."),
            )
            .child(
                div()
                    .py_3()
                    .flex()
                    .justify_between()
                    .items_center()
                    .child("Launch at login")
                    .child(self.button(
                        "launch-at-login",
                        if login { "On" } else { "Off" },
                        Intent::Login,
                        cx,
                    )),
            )
            .child(
                div()
                    .mt_2()
                    .pt_3()
                    .border_t_1()
                    .border_color(self.border_color())
                    .text_size(px(11.))
                    .text_color(self.muted())
                    .child("Global Shortcut"),
            )
            .child(
                div()
                    .mt_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .w_0()
                            .h(px(34.))
                            .px_2()
                            .pt(px(6.))
                            .rounded(px(6.))
                            .border_1()
                            .border_color(self.border_color())
                            .child(self.query.clone()),
                    )
                    .child(self.button("apply-shortcut", "Apply", Intent::Shortcut, cx)),
            )
            .child(
                div()
                    .mt_2()
                    .text_size(px(11.))
                    .text_color(self.muted())
                    .child("Alt+N or Ctrl+Shift+Space. Leave empty to disable."),
            )
            .child(
                div()
                    .mt_4()
                    .pt_2()
                    .border_t_1()
                    .border_color(self.border_color())
                    .child(self.row("import-notes", "Import Notes…", "⌘O", Intent::Import, cx))
                    .child(self.row(
                        "export-library",
                        "Export Library Backup…",
                        "",
                        Intent::SaveCopy,
                        cx,
                    ))
                    .child(self.row(
                        "show-storage",
                        "Show Library in Finder",
                        "",
                        Intent::Reveal,
                        cx,
                    )),
            )
            .child(
                div()
                    .mt_3()
                    .text_size(px(11.))
                    .text_color(self.muted())
                    .child("Saved on this Mac. Closing the window keeps Notes running."),
            )
    }
    fn panel_key(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.format_menu.is_some() {
            return self.format_key(key, window, cx);
        }
        if self.panel == Panel::Editor || self.query.read(cx).is_composing() {
            return false;
        }
        if self.panel == Panel::Settings {
            if key == "enter" {
                self.apply_shortcut(cx);
                return true;
            }
            return false;
        }
        if self.panel == Panel::Actions {
            let items = self.filtered_actions(cx);
            match key {
                "up" => self.selected = self.selected.saturating_sub(1),
                "down" => self.selected = (self.selected + 1).min(items.len().saturating_sub(1)),
                "enter" => {
                    if let Some((_, _, _, intent)) = items.get(self.selected) {
                        self.intent(intent.clone(), window, cx);
                    }
                }
                _ => return false,
            }
            self.actions_scroll.scroll_to_item(self.selected);
        } else {
            let query = self.query.read(cx).committed_document().plain_text();
            let notes = self.matching_notes(query.trim(), self.panel == Panel::Trash);
            match key {
                "up" => self.selected = self.selected.saturating_sub(1),
                "down" => self.selected = (self.selected + 1).min(notes.len().saturating_sub(1)),
                "enter" => {
                    if let Some(note) = notes.get(self.selected) {
                        let id = note.id.clone();
                        if self.panel == Panel::Trash {
                            self.restore_note(&id, cx);
                        } else {
                            self.select_note(&id, window, cx);
                        }
                    }
                }
                _ => return false,
            }
            self.picker_scroll.scroll_to_item(self.selected);
        }
        cx.notify();
        true
    }
    fn action_items(&self) -> Vec<(&'static str, &'static str, &'static str, Intent)> {
        vec![
            ("new-action", "New Note", "⌘N", Intent::New),
            ("browse-action", "Browse Notes", "⌘P", Intent::Browse),
            (
                "pin-note",
                if self.library.active_note().pinned {
                    "Unpin Note"
                } else {
                    "Pin Note"
                },
                "",
                Intent::Pin,
            ),
            ("copy-markdown", "Copy as Markdown", "⇧⌘C", Intent::Copy),
            ("export-note", "Export Markdown…", "⇧⌘E", Intent::Export),
            ("format-bold", "Bold", "⌘B", Intent::Mark(Mark::Bold)),
            ("format-italic", "Italic", "⌘I", Intent::Mark(Mark::Italic)),
            (
                "format-strikethrough",
                "Strikethrough",
                "⇧⌘S",
                Intent::Mark(Mark::Strikethrough),
            ),
            (
                "format-underline",
                "Underline",
                "⌘U",
                Intent::Mark(Mark::Underline),
            ),
            ("format-code", "Inline Code", "⌘E", Intent::Mark(Mark::Code)),
            (
                "format-heading",
                "Heading",
                "⌥⌘1",
                Intent::Block(BlockKind::Heading(1)),
            ),
            (
                "format-quote",
                "Quote",
                "⇧⌘B",
                Intent::Block(BlockKind::Quote),
            ),
            (
                "format-paragraph",
                "Paragraph",
                "⌥⌘0",
                Intent::Block(BlockKind::Paragraph),
            ),
            (
                "format-bullet",
                "Bullet List",
                "⇧⌘8",
                Intent::Block(BlockKind::Bullet),
            ),
            (
                "format-task",
                "Task List",
                "⇧⌘9",
                Intent::Block(BlockKind::Task { checked: false }),
            ),
            (
                "delete-note",
                "Move to Recently Deleted",
                "",
                Intent::Delete,
            ),
            ("open-settings", "Settings…", "⌘,", Intent::Settings),
        ]
    }
    fn filtered_actions(
        &self,
        cx: &App,
    ) -> Vec<(&'static str, &'static str, &'static str, Intent)> {
        let query = self
            .query
            .read(cx)
            .document()
            .plain_text()
            .trim()
            .to_lowercase();
        self.action_items()
            .into_iter()
            .filter(|(_, label, _, _)| label.to_lowercase().contains(&query))
            .collect()
    }
    fn actions_panel(&self, cx: &mut Context<Self>) -> Div {
        let items = self.filtered_actions(cx);
        let mut list = div()
            .id("actions-list")
            .track_scroll(&self.actions_scroll)
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_2()
            .pb_2();
        if items.is_empty() {
            list = list.child(
                div()
                    .py_6()
                    .text_center()
                    .text_size(px(13.))
                    .text_color(self.muted())
                    .child("No matching actions"),
            );
        }
        for (index, (id, label, hint, intent)) in items.into_iter().enumerate() {
            let separator = index > 0 && matches!(id, "format-bold" | "delete-note");
            list = list.child(
                div()
                    .when(separator, |s| {
                        s.child(
                            div()
                                .h(px(1.))
                                .my(px(8.))
                                .mx(px(8.))
                                .bg(self.border_color()),
                        )
                    })
                    .child(
                        self.row(id, label, hint, intent, cx)
                            .aria_selected(index == self.selected)
                            .when(index == self.selected, |s| s.bg(self.selected_color()))
                            .on_mouse_move(cx.listener(move |this, _, _, cx| {
                                if this.selected != index {
                                    this.selected = index;
                                    cx.notify();
                                }
                            })),
                    ),
            );
        }
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(self.search_field())
            .child(list)
    }
    fn overlay(&self, window: &Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let viewport = window.bounds().size;
        let top = if viewport.height < px(400.) {
            px(44.)
        } else {
            px(72.)
        };
        let width = px(if self.panel == Panel::Settings {
            360.
        } else {
            320.
        })
        .min(viewport.width - px(32.));
        let available = (viewport.height - top - px(16.)).max(px(120.));
        let desired = match self.panel {
            Panel::Browse | Panel::Trash => {
                let n = self
                    .library
                    .search(
                        self.query.read(cx).document().plain_text().trim(),
                        self.panel == Panel::Trash,
                    )
                    .len();
                px(110. + 58. * n.max(1) as f32)
            }
            Panel::Actions => {
                let items = self.filtered_actions(cx);
                let count = items.len();
                let separators = items
                    .iter()
                    .enumerate()
                    .filter(|(index, (id, _, _, _))| {
                        *index > 0 && matches!(*id, "format-bold" | "delete-note")
                    })
                    .count();
                if count == 0 {
                    px(132.)
                } else {
                    px(52. + 36. * count as f32 + 17. * separators as f32)
                }
            }
            _ => px(470.),
        };
        let contents = match self.panel {
            Panel::Browse | Panel::Trash => self.picker(cx),
            Panel::Actions => self.actions_panel(cx),
            _ => div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .child(
                    div()
                        .h(px(44.))
                        .flex_shrink_0()
                        .px_4()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_size(px(13.))
                                .font_weight(FontWeight::MEDIUM)
                                .child("Settings"),
                        )
                        .child(self.button("settings-done", "Done", Intent::Back, cx)),
                )
                .child(self.settings(cx)),
        };
        div()
            .id("notes-overlay")
            .absolute()
            .top(top)
            .left((viewport.width - width) / 2.)
            .w(width)
            .h(desired.min(available).min(px(440.)))
            .flex()
            .flex_col()
            .rounded(px(14.))
            .bg(self.surface_color())
            .border_1()
            .border_color(if self.dark {
                rgba(0xffffff18)
            } else {
                rgba(0xffffffcc)
            })
            .shadow(vec![BoxShadow {
                color: rgba(0x00000030).into(),
                offset: point(px(0.), px(8.)),
                blur_radius: px(28.),
                spread_radius: px(0.),
                inset: false,
            }])
            .occlude()
            .overflow_hidden()
            .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                this.dismiss(window, cx);
                cx.stop_propagation();
            }))
            .child(contents)
    }
}
impl Render for NotesApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = self.library.active_note().title();
        window.set_window_title(&title);
        let style = notes_style(self.dark);
        let root = div()
            .key_context("MarkraftApp")
            .track_focus(&self.panel_focus)
            .capture_action(cx.listener(|this, _: &markraft_gpui::Up, w, cx| {
                if this.panel_key("up", w, cx) {
                    cx.stop_propagation();
                } else {
                    cx.propagate();
                }
            }))
            .capture_action(cx.listener(|this, _: &markraft_gpui::Down, w, cx| {
                if this.panel_key("down", w, cx) {
                    cx.stop_propagation();
                } else {
                    cx.propagate();
                }
            }))
            .capture_action(cx.listener(|this, _: &markraft_gpui::Enter, w, cx| {
                if this.panel_key("enter", w, cx) {
                    cx.stop_propagation();
                } else {
                    cx.propagate();
                }
            }))
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(style.background)
            .text_color(style.text)
            .font_family(".SystemUIFont")
            .on_action(cx.listener(|this, _: &Save, _, cx| {
                this.flush(cx);
            }))
            .on_action(cx.listener(|this, _: &CopyMarkdown, _, cx| this.copy_markdown(cx)))
            .on_action(cx.listener(|this, _: &Quit, _, cx| this.quit(cx)))
            .on_action(cx.listener(|this, _: &Hide, w, cx| this.dismiss(w, cx)))
            .on_action(cx.listener(|this, _: &NewNote, w, cx| this.new_note(w, cx)))
            .on_action(cx.listener(|this, _: &Browse, w, cx| this.open_panel(Panel::Browse, w, cx)))
            .on_action(
                cx.listener(|this, _: &Actions, w, cx| this.open_panel(Panel::Actions, w, cx)),
            )
            .on_action(
                cx.listener(|this, _: &Settings, w, cx| this.open_panel(Panel::Settings, w, cx)),
            )
            .on_action(cx.listener(|this, _: &Export, _, cx| this.export(cx)))
            .on_action(cx.listener(|this, _: &Import, w, cx| this.import(w, cx)));
        let actions = self
            .capsule()
            .p(px(3.))
            .gap(px(1.))
            .child(
                self.icon_button(
                    "actions",
                    "Actions · ⌘K",
                    Icon::Command,
                    Intent::Actions,
                    cx,
                )
                .size(px(24.))
                .rounded_full()
                .opacity(1.),
            )
            .child(
                self.icon_button(
                    "browse",
                    "Browse Notes · ⌘P",
                    Icon::Notes,
                    Intent::Browse,
                    cx,
                )
                .size(px(24.))
                .rounded_full()
                .opacity(1.),
            )
            .child(
                self.icon_button("new-note", "New Note · ⌘N", Icon::Plus, Intent::New, cx)
                    .size(px(24.))
                    .rounded_full()
                    .opacity(1.),
            );
        let toolbar = div()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(TOOLBAR_HEIGHT)
            .child(
                div()
                    .absolute()
                    .left(px(112.))
                    .right(px(112.))
                    .top(px(17.))
                    .text_center()
                    .text_size(px(12.))
                    .text_color(self.muted())
                    .truncate()
                    .child(title)
                    .with_spring(
                        "title-fade",
                        Self::chrome_spring(self.chrome_visible()),
                        |s, phase| s.opacity(phase.interpolate_clamped(0.6, 1.)),
                    ),
            )
            .child(
                div()
                    .absolute()
                    .right(px(8.))
                    .top(px(11.))
                    .child(actions)
                    .with_spring(
                        "capsule-fade",
                        Self::chrome_spring(self.chrome_visible()),
                        |s, phase| s.opacity(phase.interpolate_clamped(0., 1.)),
                    ),
            );
        if self.persistence.is_none() {
            let explanation = "The original file is unchanged. You can retry or restore the \
                previous valid backup. Restoration keeps a copy of the original file.";
            let actions = div()
                .mt_4()
                .flex()
                .gap_2()
                .child(self.button("retry-open", "Retry", Intent::Retry, cx))
                .when(Store::read_backup(&self.path).is_ok(), |s| {
                    s.child(self.button("recover-backup", "Restore Backup", Intent::Recover, cx))
                })
                .child(self.button("reveal-library", "Show File", Intent::Reveal, cx));
            let detail = div()
                .mt_3()
                .text_size(px(12.))
                .text_color(self.muted())
                .child(self.error.clone().unwrap_or_default());
            return root.child(toolbar).child(
                div()
                    .flex_1()
                    .p_6()
                    .pt(TOOLBAR_HEIGHT + px(24.))
                    .text_size(px(14.))
                    .child("Your notes could not be opened")
                    .child(detail)
                    .child(div().mt_4().text_size(px(12.)).child(explanation))
                    .child(actions),
            );
        }
        let text = self.editor().read(cx).document().plain_text();
        let count = if self.show_words {
            let n = text.unicode_words().count();
            format!("{n} {}", if n == 1 { "word" } else { "words" })
        } else {
            let n = text.graphemes(true).count();
            format!("{n} {}", if n == 1 { "character" } else { "characters" })
        };
        // The toolbar and footer float over the note. Each pair of fades reaches the
        // background color only at its window edge, so no opaque band forms; the short
        // one keeps the title or count legible. The top pair is shallower so the first
        // line is not dimmed while the note rests at its start.
        let mut clear = style.background;
        clear.a = 0.;
        let fade = |height: Pixels, top: bool| {
            let edge = div().absolute().left_0().right_0().h(height);
            let (edge, angle) = if top {
                (edge.top_0(), 0.)
            } else {
                (edge.bottom_0(), 180.)
            };
            edge.bg(linear_gradient(
                angle,
                linear_color_stop(clear, 0.),
                linear_color_stop(style.background, 1.),
            ))
        };
        let body = div()
            .flex_1()
            .min_h_0()
            .relative()
            .child(self.editor())
            .child(fade(px(80.), true))
            .child(fade(px(56.), true))
            .child(fade(px(128.), false))
            .child(fade(px(64.), false))
            .child(toolbar)
            .child(
                div()
                    .absolute()
                    .bottom_0()
                    .left_0()
                    .right_0()
                    .child(self.footer(count, cx)),
            );
        root.child(body)
            .when_some(self.platform_error.clone(), |s, error| {
                s.child(
                    div()
                        .px_6()
                        .py_2()
                        .text_size(px(11.))
                        .text_color(self.muted())
                        .child(format!("{error} · Change the shortcut in Settings.")),
                )
            })
            .when_some(self.error.clone(), |s, error| {
                s.child(
                    div()
                        .px_6()
                        .py_2()
                        .text_size(px(11.))
                        .text_color(if self.dark {
                            rgb(0xffa4a4)
                        } else {
                            rgb(0xa53232)
                        })
                        .child(format!("{error}  ⌘S retry · ⇧⌘C copy")),
                )
            })
            .when(self.error.is_some(), |s| {
                s.child(
                    div()
                        .flex()
                        .px_4()
                        .gap_2()
                        .child(self.button(
                            "save-library-copy",
                            "Save Library Copy…",
                            Intent::SaveCopy,
                            cx,
                        ))
                        .child(self.button(
                            "reload-library",
                            "Reload from Disk…",
                            Intent::Reload,
                            cx,
                        )),
                )
            })
            .when_some(self.notice.clone(), |s, (notice, _)| {
                s.child(
                    div()
                        .absolute()
                        .bottom(px(52.))
                        .right(px(12.))
                        .px_3()
                        .py_1()
                        .rounded(px(6.))
                        .bg(self.surface_color())
                        .text_size(px(11.))
                        .text_color(self.muted())
                        .child(notice),
                )
            })
            .when(
                self.format_menu.is_some() && self.panel == Panel::Editor,
                |s| {
                    s.child(popover_enter(
                        "format-enter",
                        self.format_popover(window, cx),
                        true,
                    ))
                },
            )
            .when(self.panel != Panel::Editor, |s| {
                s.child(popover_enter(
                    "overlay-enter",
                    self.overlay(window, cx),
                    false,
                ))
            })
    }
}

/// Popovers fade in over a short travel. Spring state is dropped once a popover stops
/// rendering, so each opening replays it while switching panels does not.
fn popover_enter(
    id: &'static str,
    popover: Stateful<Div>,
    anchored_bottom: bool,
) -> SpringAnimationElement<Stateful<Div>> {
    popover.with_spring(
        id,
        SpringAnimation::new(SpringConfig::new(900., 60., 1.))
            .to(true)
            .from(false),
        move |s, phase| {
            // Travel away from the anchored edge.
            let offset = phase.interpolate_clamped(px(-4.), px(0.));
            let s = s.opacity(phase.interpolate_clamped(0., 1.));
            if anchored_bottom {
                s.mb(offset)
            } else {
                s.mt(offset)
            }
        },
    )
}

struct Hint {
    label: &'static str,
    dark: bool,
}
impl Render for Hint {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let (text, keys) = self.label.split_once(" · ").unwrap_or((self.label, ""));
        let (surface, border, ink, keycap) = if self.dark {
            (
                rgba(0x3a3b40f2),
                rgba(0xffffff1f),
                rgb(0xf2f2f3),
                rgba(0xffffff1a),
            )
        } else {
            (
                rgba(0xf6f6f6f2),
                rgba(0x00000024),
                rgb(0x272727),
                rgba(0x00000014),
            )
        };
        div()
            .flex()
            .items_center()
            .gap(px(6.))
            .px_2()
            .h(px(24.))
            .rounded(px(7.))
            .bg(surface)
            .border_1()
            .border_color(border)
            .text_color(ink)
            .text_size(px(11.))
            .child(text)
            .when(!keys.is_empty(), |s| {
                s.child(div().flex().gap(px(2.)).children(keys.chars().map(|key| {
                    div()
                        .min_w(px(16.))
                        .h(px(16.))
                        .px(px(3.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(4.))
                        .bg(keycap)
                        .child(key.to_string())
                })))
            })
    }
}
