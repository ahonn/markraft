use super::*;

impl NotesApp {
    pub(in crate::app) fn open_code_language(&mut self, pos: usize, cx: &mut Context<Self>) {
        let editor = self.editor();
        let Some(active) = editor.read(cx).code_language(pos) else {
            return;
        };
        let active = active.trim().to_lowercase();
        let selected = markraft_gpui::code_languages()
            .iter()
            .position(|(language, _)| canonical_language(language) == canonical_language(&active))
            .unwrap_or(0);
        self.format_menu = None;
        self.link_popover = None;
        self.query
            .update(cx, |editor, cx| editor.cancel_composition(cx));
        self.code_language_block = Some(pos);
        self.code_language_selected = selected;
        self.code_language_focus_pending = true;
        self.set_query(String::new(), "Search languages…", cx);
        cx.notify();
    }

    fn matching_code_languages(&self, cx: &App) -> Vec<(&'static str, &'static str)> {
        let query = self.query.read(cx).text().to_owned();
        let query = query.trim().to_lowercase();
        markraft_gpui::code_languages()
            .iter()
            .copied()
            .filter(|(id, label)| {
                id.to_lowercase().contains(&query)
                    || label.to_lowercase().contains(&query)
                    || canonical_language(id) == canonical_language(&query)
            })
            .collect()
    }

    fn apply_code_language(&mut self, language: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(pos) = self.code_language_block.take() {
            self.editor().update(cx, |editor, cx| {
                editor.set_code_language_at(pos, language, cx)
            });
        }
        self.query
            .update(cx, |editor, cx| editor.cancel_composition(cx));
        self.focus_editor(window, cx);
        cx.notify();
    }

    pub(super) fn code_language_key(
        &mut self,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let languages = self.matching_code_languages(cx);
        match key {
            "up" => self.code_language_selected = self.code_language_selected.saturating_sub(1),
            "down" => {
                self.code_language_selected =
                    (self.code_language_selected + 1).min(languages.len().saturating_sub(1));
            }
            "enter" => {
                if let Some((language, _)) = languages.get(self.code_language_selected) {
                    self.apply_code_language(language, window, cx);
                }
            }
            _ => return false,
        }
        self.code_language_scroll
            .scroll_to_item(self.code_language_selected);
        cx.notify();
        true
    }

    pub(super) fn code_language_popover(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        if self.panel != Panel::Editor
            || (!self.code_language_focus_pending
                && !self.query.focus_handle(cx).is_focused(window))
        {
            self.code_language_block = None;
        }
        let pos = self.code_language_block?;
        let editor = self.editor().read(cx);
        let active = editor.code_language(pos)?.trim().to_lowercase();
        let anchor = editor.code_header_bounds(pos)?;
        let languages = self.matching_code_languages(cx);
        let empty = languages.is_empty();
        let viewport = window.bounds().size;
        let width = px(248.).min((viewport.width - px(16.)).max(px(0.)));
        let height = px(48. + languages.len().max(1) as f32 * 32.)
            .min(px(304.))
            .min((viewport.height - px(52.)).max(px(0.)));
        let left = (anchor.right() - width)
            .min(viewport.width - width - px(8.))
            .max(px(8.));
        let below = anchor.bottom() + px(6.);
        let top = if below + height <= viewport.height - px(8.) {
            below
        } else {
            (anchor.top() - height - px(6.)).max(px(36.))
        };
        let mut list = div()
            .id("code-language-list")
            .track_scroll(&self.code_language_scroll)
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p(px(4.));
        for (index, (language, label)) in languages.into_iter().enumerate() {
            let checked = canonical_language(language) == canonical_language(&active);
            list = list.child(
                div()
                    .id(("code-language", index))
                    .role(Role::Button)
                    .aria_label(label)
                    .aria_selected(index == self.code_language_selected)
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
                    .when(index == self.code_language_selected, |s| {
                        s.bg(self.selected_color())
                    })
                    .hover(|s| s.bg(self.selected_color()))
                    .active(|s| s.bg(self.pressed_color()))
                    .on_mouse_move(cx.listener(move |this, _, _, cx| {
                        if this.code_language_selected != index {
                            this.code_language_selected = index;
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.apply_code_language(language, window, cx);
                    }))
                    .child(
                        div()
                            .w(px(14.))
                            .when(checked, |s| s.child(icon(Icon::Check, self.control_text()))),
                    )
                    .child(label),
            );
        }
        if empty {
            list = list.child(
                div()
                    .h(px(32.))
                    .px_2()
                    .text_size(px(13.))
                    .text_color(self.muted())
                    .child("No matching languages"),
            );
        }
        Some(
            div()
                .id("code-language-popover")
                .absolute()
                .top(top)
                .left(left)
                .w(width)
                .h(height)
                .flex()
                .flex_col()
                .rounded(px(10.))
                .bg(self.surface_color())
                .border_1()
                .border_color(self.border_color())
                .shadow(vec![BoxShadow {
                    color: rgba(0x00000030).into(),
                    offset: point(px(0.), px(6.)),
                    blur_radius: px(20.),
                    spread_radius: px(0.),
                    inset: false,
                }])
                .occlude()
                .overflow_hidden()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                    this.code_language_block = None;
                    this.query
                        .update(cx, |editor, cx| editor.cancel_composition(cx));
                    this.focus_editor(window, cx);
                    cx.notify();
                }))
                .child(
                    div()
                        .h(px(40.))
                        .flex_shrink_0()
                        .px_3()
                        .pt(px(5.))
                        .border_b_1()
                        .border_color(self.border_color())
                        .child(self.query.clone()),
                )
                .child(list),
        )
    }
}

// Fence aliases match the canonical menu entry without rewriting the stored language.
fn canonical_language(language: &str) -> &str {
    match language {
        "text" | "txt" | "plaintext" | "plain" => "",
        "rs" => "rust",
        "js" | "mjs" | "cjs" | "node" | "nodejs" => "javascript",
        "ts" => "typescript",
        "py" => "python",
        "rb" => "ruby",
        "golang" => "go",
        "kt" | "kts" => "kotlin",
        "c++" | "cxx" | "hpp" => "cpp",
        "c#" | "csharp" => "cs",
        "sh" | "shell" | "zsh" => "bash",
        "yml" => "yaml",
        "md" => "markdown",
        other => other,
    }
}
