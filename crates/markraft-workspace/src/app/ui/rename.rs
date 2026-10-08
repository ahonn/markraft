use super::*;

const WIDTH: Pixels = px(280.);
const HEIGHT: Pixels = px(40.);
/// How far in from either edge the title's band starts, as the toolbar lays it out.
const TITLE_INSET: Pixels = px(112.);

impl MarkraftApp {
    /// The pill under the title that names the note's file: the link editor's shape,
    /// holding the name where that one holds an address.
    pub(super) fn rename_pill(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        let rename = self.interaction.rename()?;
        let extension = self
            .library
            .active_note()
            .path
            .as_ref()?
            .extension()
            .map(|extension| format!(".{}", extension.to_string_lossy()))
            .unwrap_or_default();
        let width = WIDTH.min(window.bounds().size.width - px(16.));
        let left = (window.bounds().size.width - width) / 2.;
        let separator = div().w(px(1.)).h(px(16.)).mx_1().bg(self.border_color());
        Some(
            div()
                .id("rename-pill")
                .absolute()
                .top(TOOLBAR_HEIGHT - px(10.))
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
                .occlude()
                // Clicks inside the pill must not reach the editor underneath.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_mouse_down_out(cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    // The title closes the pill itself; closing it here as well would
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
                        .flex_1()
                        .min_w_0()
                        .w_0()
                        .pt(px(1.))
                        .child(self.query_field(cx)),
                )
                .child(
                    div()
                        .flex_none()
                        .pl(px(4.))
                        .text_size(px(13.))
                        .text_color(self.muted())
                        .child(extension),
                )
                .child(separator)
                // Only a note something links to has links to bring along.
                .when(rename.links > 0, |s| {
                    s.child(self.format_button(
                        "rename-links",
                        self.i18n.text("surfaces.rename.update-links"),
                        Icon::Link,
                        Intent::RenameLinks,
                        Some(rename.update_links),
                        cx,
                    ))
                })
                .child(self.format_button(
                    "rename-apply",
                    self.i18n.text("surfaces.rename.apply"),
                    Icon::Check,
                    Intent::ApplyRename,
                    None,
                    cx,
                )),
        )
    }
}
