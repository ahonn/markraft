//! One vocabulary for the chrome. Every floating surface takes its radius, border and
//! shadow from here, every keycap is the same part, and every list row is the same
//! height, so the panels, the menus and the tooltips read as one set of components.

use super::*;

/// Rectangular floating surfaces: the panel card, the two menus and the file status
/// card. The link pill is a capsule around one line of controls and keeps the radius
/// its height gives it.
pub(super) const POPOVER_RADIUS: Pixels = px(12.);
/// One list row: the format menu, the language menu. A ⌘K row is taller, a Browse row
/// carries a title and a line of metadata, and the Settings window's menus have their
/// own metrics, so each has a height of its own.
pub(super) const ROW_HEIGHT: Pixels = px(32.);
pub(super) const ROW_RADIUS: Pixels = px(6.);

/// A keycap, as the editor's completion menu draws it.
const KEYCAP_WIDTH: Pixels = px(17.);
const KEYCAP_HEIGHT: Pixels = px(18.);
const KEYCAP_RADIUS: Pixels = px(5.);

/// Secondary text. Dark enough to stay legible on both surface colors, the light
/// panel included.
pub(super) fn muted(dark: bool) -> Hsla {
    if dark { rgb(0x93959d) } else { rgb(0x66696f) }.into()
}

/// The hairline around a surface or a control.
pub(super) fn border_color(dark: bool) -> Hsla {
    if dark { rgb(0x383a40) } else { rgb(0xdcdcdc) }.into()
}

/// The one shadow a floating surface casts, so a menu and a panel sit at the same
/// height above the note.
pub(super) fn popover_shadow() -> Vec<BoxShadow> {
    vec![BoxShadow {
        color: rgba(0x00000030).into(),
        offset: point(px(0.), px(6.)),
        blur_radius: px(24.),
        spread_radius: px(0.),
        inset: false,
    }]
}

/// The keys of a shortcut hint. It takes `dark` rather than the app, so the tooltip
/// view, which owns no palette, draws the same caps as the panels.
pub(super) fn keycaps(hint: &str, dark: bool) -> Div {
    div()
        .flex()
        .gap(px(3.))
        .children(hint.chars().map(move |key| {
            div()
                .w(KEYCAP_WIDTH)
                .h(KEYCAP_HEIGHT)
                .flex()
                .items_center()
                .justify_center()
                .rounded(KEYCAP_RADIUS)
                .bg(if dark {
                    rgba(0xffffff05)
                } else {
                    rgba(0xffffff20)
                })
                .border_1()
                .border_color(border_color(dark))
                .text_size(px(11.))
                .text_color(muted(dark))
                .child(key.to_string())
        }))
}

/// Reduced motion keeps a spring's end states and drops the travel between them.
pub(in crate::app) fn playback(reduce_motion: bool) -> SpringPlayback {
    if reduce_motion {
        SpringPlayback::Completed
    } else {
        SpringPlayback::Running
    }
}

/// A scrollbar's thumb, as the editor draws the note's.
pub(super) fn scrollbar_color(dark: bool) -> Hsla {
    if dark {
        rgba(0xffffff4d)
    } else {
        rgba(0x00000047)
    }
    .into()
}

/// The right padding of a list that scrolls `handle` and pads its rows by `padding`:
/// room for the thumb while the list overflows, so the thumb covers no row.
pub(super) fn thumb_lane(padding: Pixels, handle: &ScrollHandle) -> Pixels {
    padding.max(Scrollbar::lane(handle))
}

impl WorkspaceView {
    /// Fill the rest of a panel with `contents`, which scrolls `handle`, and show how
    /// much of it is on screen.
    pub(super) fn scroll_area(
        &self,
        id: &'static str,
        contents: Stateful<Div>,
        handle: &ScrollHandle,
    ) -> Div {
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(contents.flex_1().min_h_0())
            .child(self.scrollbar(id, handle))
    }

    /// The scrollbar of a scroller that tracks `handle`. It goes after that scroller,
    /// in the same parent.
    pub(super) fn scrollbar(&self, id: &'static str, handle: &ScrollHandle) -> Scrollbar {
        Scrollbar::new(id, handle, scrollbar_color(self.dark))
    }
}
