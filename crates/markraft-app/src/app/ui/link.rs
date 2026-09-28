use super::*;

const WIDTH: Pixels = px(280.);
const HEIGHT: Pixels = px(40.);
const GAP: Pixels = px(6.);

impl MarkraftApp {
    /// The pill floating above the linked or selected text.
    pub(super) fn link_pill(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        let mode = self.interaction.link()?;
        let editor = self.editor().read(cx);
        let url = editor.active_link();
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
                        .child(self.query_field(cx)),
                )
                .child(separator())
                .child(self.format_button(
                    "link-apply",
                    self.i18n.text("surfaces.link.apply"),
                    Icon::Check,
                    Intent::ApplyLink,
                    None,
                    cx,
                ))
                .child(self.format_button(
                    "link-remove",
                    self.i18n.text("surfaces.link.unlink"),
                    Icon::Unlink,
                    Intent::Unlink,
                    None,
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
                    self.i18n.text("surfaces.link.edit"),
                    Icon::Edit,
                    Intent::EditLink,
                    None,
                    cx,
                ))
                .child(self.format_button(
                    "link-copy",
                    self.i18n.text("surfaces.link.copy"),
                    Icon::Copy,
                    Intent::CopyLink,
                    None,
                    cx,
                ))
                .child(self.format_button(
                    "link-open",
                    self.i18n.text("surfaces.link.open"),
                    Icon::External,
                    Intent::OpenLink,
                    None,
                    cx,
                ))
                .child(self.format_button(
                    "link-unlink",
                    self.i18n.text("surfaces.link.unlink"),
                    Icon::Unlink,
                    Intent::Unlink,
                    None,
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
                .border_color(self.border_color())
                .shadow(popover_shadow())
                // Clicks inside the pill must not reach the editor underneath.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(contents),
        )
    }
}
