use super::*;

const WIDTH: Pixels = px(280.);
const HEIGHT: Pixels = px(40.);
const GAP: Pixels = px(6.);

impl NotesApp {
    /// The pill floating above the linked or selected text.
    pub(super) fn link_pill(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        if self.panel != Panel::Editor {
            self.link_popover = None;
        }
        // The edit field closes as soon as focus goes anywhere else.
        if self.link_popover == Some(LinkPopover::Edit)
            && !self.query.focus_handle(cx).is_focused(window)
        {
            self.link_popover = None;
        }
        let editor = self.editor().read(cx);
        let url = editor.active_link();
        if self.link_popover == Some(LinkPopover::View) && url.is_none() {
            self.link_popover = None;
        }
        let mode = self.link_popover?;
        let anchor = editor.anchor_bounds()?;
        let viewport = window.bounds().size;
        let width = WIDTH.min(viewport.width - px(16.));
        let left = (anchor.center().x - width / 2.)
            .min(viewport.width - width - px(8.))
            .max(px(8.));
        let above = anchor.top() - HEIGHT - GAP;
        // Near the top it drops below the text rather than covering the window controls.
        let top = if above < px(36.) {
            anchor.bottom() + GAP
        } else {
            above
        };
        let separator = || div().w(px(1.)).h(px(16.)).mx_1().bg(self.border_color());
        let contents = match mode {
            LinkPopover::Edit => div()
                .flex()
                .items_center()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .w_0()
                        .pt(px(1.))
                        .child(self.query.clone()),
                )
                .child(separator())
                .child(self.format_button(
                    "link-apply",
                    "Apply · ↩",
                    Icon::Check,
                    Intent::ApplyLink,
                    false,
                    cx,
                ))
                .child(self.format_button(
                    "link-remove",
                    "Unlink",
                    Icon::Trash,
                    Intent::Unlink,
                    false,
                    cx,
                )),
            LinkPopover::View => div()
                .flex()
                .items_center()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .id("link-url")
                        .flex_1()
                        .min_w_0()
                        .w_0()
                        .truncate()
                        .text_size(px(13.))
                        .text_color(self.muted())
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, window, cx| {
                            cx.stop_propagation();
                            this.intent(Intent::OpenLink, window, cx);
                        }))
                        .child(url.unwrap_or_default()),
                )
                .child(self.format_button(
                    "link-edit",
                    "Edit link",
                    Icon::Edit,
                    Intent::EditLink,
                    false,
                    cx,
                ))
                .child(self.format_button(
                    "link-copy",
                    "Copy link",
                    Icon::Copy,
                    Intent::CopyLink,
                    false,
                    cx,
                ))
                .child(self.format_button(
                    "link-open",
                    "Open link",
                    Icon::Open,
                    Intent::OpenLink,
                    false,
                    cx,
                ))
                .child(self.format_button(
                    "link-unlink",
                    "Unlink",
                    Icon::Trash,
                    Intent::Unlink,
                    false,
                    cx,
                )),
        };
        Some(
            div()
                .id("link-pill")
                .absolute()
                .top(top)
                .left(left)
                .w(width)
                .h(HEIGHT)
                .pl(px(16.))
                .pr(px(8.))
                .flex()
                .items_center()
                .rounded(HEIGHT / 2.)
                .bg(self.surface_color())
                .border_1()
                .border_color(if self.dark {
                    rgba(0xffffff18)
                } else {
                    rgba(0xffffffcc)
                })
                .shadow(vec![BoxShadow {
                    color: rgba(0x00000030).into(),
                    offset: point(px(0.), px(6.)),
                    blur_radius: px(20.),
                    spread_radius: px(0.),
                    inset: false,
                }])
                // Clicks inside the pill must not reach the editor underneath.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(contents),
        )
    }
}
