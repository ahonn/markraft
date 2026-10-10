//! The list an extension shows in its popup, styled from [`EditorStyle`] so it matches
//! the editor it hangs over.

use crate::{EditorStyle, Scrollbar, TypeaheadItem};
use gpui::{prelude::*, *};
use std::rc::Rc;

const ROW_HEIGHT: Pixels = px(36.);

/// What the popup does with the row under the pointer.
pub(crate) type RowHandler = Rc<dyn Fn(usize, &mut Window, &mut App)>;

pub(crate) struct CompletionList<'a> {
    pub style: &'a EditorStyle,
    pub items: &'a [TypeaheadItem],
    /// The provider's own leading element for a row, drawn instead of its glyph.
    pub leading: &'a dyn Fn(&TypeaheadItem, Hsla) -> Option<AnyElement>,
    pub selected: usize,
    pub width: Pixels,
    pub max_height: Pixels,
    pub scroll: ScrollHandle,
    pub hover: RowHandler,
    pub activate: RowHandler,
}

impl CompletionList<'_> {
    pub(crate) fn render(self) -> AnyElement {
        let style = self.style;
        let mut list = div()
            .id("typeahead-list")
            .track_scroll(&self.scroll)
            .max_h(self.max_height)
            .overflow_y_scroll()
            .p(px(6.))
            .pr(px(6.).max(Scrollbar::lane(&self.scroll)));
        for (index, item) in self.items.iter().enumerate() {
            let hover = self.hover.clone();
            let activate = self.activate.clone();
            list = list.child(
                div()
                    .id(("typeahead-item", index))
                    .role(Role::Button)
                    .aria_label(item.label.clone())
                    .aria_selected(index == self.selected)
                    .h(ROW_HEIGHT)
                    .flex_shrink_0()
                    .px_2()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .rounded(px(8.))
                    .text_size(px(13.))
                    .cursor_pointer()
                    .when(index == self.selected, |row| row.bg(style.popup_selected))
                    .when(index != self.selected, |row| {
                        row.hover(|row| row.bg(style.popup_hover))
                    })
                    .on_mouse_move(move |_, window, cx| hover(index, window, cx))
                    .on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        activate(index, window, cx);
                    })
                    .map(|row| match (self.leading)(item, style.text) {
                        Some(leading) => row.child(leading),
                        None if item.glyph.is_empty() => row,
                        None => {
                            row.child(div().w(px(16.)).flex_shrink_0().child(item.glyph.clone()))
                        }
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(item.label.clone()),
                    )
                    .when(!item.detail.is_empty(), |row| {
                        row.child(
                            div()
                                .flex_shrink_0()
                                .max_w(px(96.))
                                .truncate()
                                .text_size(px(12.))
                                .text_color(style.muted_text)
                                .child(item.detail.clone()),
                        )
                    })
                    .when(!item.hint.is_empty(), |row| {
                        row.child(keycaps(&item.hint, style))
                    }),
            );
        }
        div()
            .id("typeahead-popup")
            .w(self.width)
            .flex()
            .flex_col()
            .rounded(px(10.))
            .bg(style.popup_background)
            .border_1()
            .border_color(style.popup_border)
            .shadow(vec![BoxShadow {
                color: rgba(0x00000030).into(),
                offset: point(px(0.), px(6.)),
                blur_radius: px(20.),
                spread_radius: px(0.),
                inset: false,
            }])
            .text_color(style.text)
            .font_family(".SystemUIFont")
            // The popup occludes the editor: a click in it must neither move the caret
            // nor blur the editor, which keeps the menu's key bindings alive.
            .occlude()
            .overflow_hidden()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(list)
            .child(Scrollbar::new(
                "typeahead-scrollbar",
                &self.scroll,
                style.scrollbar,
            ))
            .into_any_element()
    }
}

/// A shortcut hint drawn one key per cap.
fn keycaps(hint: &str, style: &EditorStyle) -> Div {
    div()
        .flex()
        .flex_shrink_0()
        .gap(px(3.))
        .children(hint.chars().map(|key| {
            div()
                .w(px(17.))
                .h(px(18.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(5.))
                .border_1()
                .border_color(style.popup_border)
                .text_size(px(11.))
                .text_color(style.muted_text)
                .child(key.to_string())
        }))
}
