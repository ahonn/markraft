use super::*;

const WIDTH: Pixels = px(300.);
const LABEL_WIDTH: Pixels = px(44.);
/// How far in from either edge the title's band starts, as the toolbar lays it out.
const TITLE_INSET: Pixels = px(112.);

impl NotesApp {
    /// The card under the title, laid out as the one a macOS document window opens from
    /// its own title: what the file is called, and where it is.
    pub(super) fn rename_card(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        // The name field is the shared query, so anything else that opens has taken it.
        if self.panel != Panel::Editor
            || self.link_popover.is_some()
            || self.code_language_block.is_some()
            || self.format_menu.is_some()
            || self.file_status_popover
            || self
                .rename
                .as_ref()
                .is_some_and(|rename| rename.id != self.library.active_id)
        {
            self.rename = None;
        }
        // Like the link field, it closes as soon as focus goes anywhere else. Tab moves
        // it to the panel handle, which still belongs to the card.
        if self.rename.is_some()
            && !self.query.focus_handle(cx).is_focused(window)
            && !self.panel_focus.is_focused(window)
        {
            self.rename = None;
        }
        let rename = self.rename.as_ref()?;
        let note = self.library.active_note();
        let path = note.path.as_ref()?;
        let extension = path
            .extension()
            .map(|extension| format!(".{}", extension.to_string_lossy()))
            .unwrap_or_default();
        let folder = path.parent().map(|folder| {
            match self
                .path
                .as_deref()
                .and_then(|root| Some((root, folder.strip_prefix(root).ok()?)))
            {
                Some((root, relative)) => folder_label(root, relative),
                None => folder.display().to_string(),
            }
        });
        let width = WIDTH.min(window.bounds().size.width - px(16.));
        let left = (window.bounds().size.width - width) / 2.;
        let label = |text: &'static str| {
            div()
                .w(LABEL_WIDTH)
                .flex_none()
                .text_right()
                .text_color(self.muted())
                .child(text)
        };
        let links = rename.links;
        let checked = rename.update_links;
        let accent = notes_style(self.dark).marker;
        Some(
            div()
                .id("rename-card")
                .absolute()
                .top(TOOLBAR_HEIGHT - px(10.))
                .left(left)
                .w(width)
                .p(px(12.))
                .flex()
                .flex_col()
                .gap(px(8.))
                .text_size(px(12.))
                .rounded(POPOVER_RADIUS)
                .bg(self.surface_color())
                .border_1()
                .border_color(self.border_color())
                .shadow(popover_shadow())
                .occlude()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_mouse_down_out(cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    // The title closes the card itself; closing it here as well would
                    // let that same click open it again.
                    let edge = window.bounds().size.width - TITLE_INSET;
                    let on_title = event.position.y < TOOLBAR_HEIGHT - px(10.)
                        && (TITLE_INSET..edge).contains(&event.position.x);
                    if !on_title {
                        this.close_rename(window, cx);
                    }
                }))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(label("Name"))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .h(px(24.))
                                .px(px(6.))
                                .flex()
                                .items_center()
                                .rounded(px(6.))
                                .border_1()
                                .border_color(self.border_color())
                                .text_size(px(13.))
                                .child(div().flex_1().min_w_0().w_0().child(self.query_field(cx))),
                        )
                        .child(div().flex_none().text_color(self.muted()).child(extension)),
                )
                .when_some(folder, |s, folder| {
                    s.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(label("Where"))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(self.control_text())
                                    .child(folder),
                            ),
                    )
                })
                .when_some(rename.error.clone(), |s, error| {
                    s.child(
                        div()
                            .pl(LABEL_WIDTH + px(8.))
                            .line_height(px(16.))
                            .text_color(self.danger())
                            .child(error),
                    )
                })
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(8.))
                        .pl(LABEL_WIDTH + px(8.))
                        .child(if links == 0 {
                            div().id("rename-no-links")
                        } else {
                            self.ring(
                                "rename-links",
                                px(6.),
                                div()
                                    .id("rename-links")
                                    .role(Role::CheckBox)
                                    .aria_label("Update links to this note")
                                    .aria_toggled(if checked {
                                        accesskit::Toggled::True
                                    } else {
                                        accesskit::Toggled::False
                                    })
                                    .relative()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.))
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.intent(Intent::RenameLinks, window, cx);
                                    }))
                                    .child(
                                        div()
                                            .size(px(14.))
                                            .flex_none()
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .rounded(px(4.))
                                            .border_1()
                                            .border_color(if checked {
                                                accent
                                            } else {
                                                self.border_color()
                                            })
                                            .when(checked, |s| {
                                                s.bg(accent).child(sized_icon(
                                                    Icon::Check,
                                                    rgb(0xffffff).into(),
                                                    10.,
                                                ))
                                            }),
                                    )
                                    .child(div().text_color(self.control_text()).child(
                                        if links == 1 {
                                            "Update 1 link".to_owned()
                                        } else {
                                            format!("Update {links} links")
                                        },
                                    )),
                            )
                        })
                        .child(self.button("rename-apply", "Rename", Intent::ApplyRename, cx)),
                ),
        )
    }
}
