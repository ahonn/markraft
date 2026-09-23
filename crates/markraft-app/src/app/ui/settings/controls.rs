//! The Settings window's page grammar, as a classic macOS settings pane draws it: a
//! column of right-aligned labels, the controls beside them, a line of secondary text
//! where a control needs one, and hairlines between groups. Behaviour and semantics
//! come from `gpui-base`; every colour comes from the note's own palette.

use gpui::{prelude::*, *};
use gpui_base::{Button, Checkbox, Radio, RadioGroup};

use super::super::icons::{Icon, sized_icon};

pub(super) mod metrics {
    /// The band the traffic lights sit in, with the page's name centred in it.
    pub const TITLE_HEIGHT: f32 = 30.;
    pub const TITLE_SIZE: f32 = 13.;
    /// The row of pages under the title, the way a macOS settings window has it.
    pub const TOOLBAR_PAD_BOTTOM: f32 = 6.;
    pub const TOOLBAR_GAP: f32 = 2.;
    pub const TOOLBAR_ITEM_MIN_WIDTH: f32 = 64.;
    pub const TOOLBAR_ITEM_PAD_X: f32 = 8.;
    pub const TOOLBAR_ITEM_PAD_Y: f32 = 5.;
    pub const TOOLBAR_ITEM_RADIUS: f32 = 6.;
    pub const TOOLBAR_ICON: f32 = 20.;
    pub const TOOLBAR_LABEL_GAP: f32 = 3.;
    pub const TOOLBAR_LABEL_SIZE: f32 = 11.;

    pub const PAGE_PAD_X: f32 = 24.;
    pub const PAGE_TOP: f32 = 18.;
    pub const PAGE_BOTTOM: f32 = 22.;

    /// The label column: wide enough for the longest label, which right-aligns to it.
    pub const LABEL_WIDTH: f32 = 150.;
    pub const LABEL_GAP: f32 = 10.;
    /// One line of a form: every control and every label sits on a line this tall,
    /// so a label reads level with the first control beside it.
    pub const LINE_HEIGHT: f32 = 24.;
    pub const ROW_GAP: f32 = 6.;
    pub const LINE_GAP: f32 = 2.;
    pub const TEXT_SIZE: f32 = 13.;
    pub const HELP_SIZE: f32 = 11.;
    pub const DIVIDER_MARGIN: f32 = 12.;

    pub const CONTROL_HEIGHT: f32 = 24.;
    pub const CONTROL_RADIUS: f32 = 6.;
    pub const CONTROL_GAP: f32 = 6.;
    pub const BUTTON_PAD_X: f32 = 10.;
    pub const BUTTON_TEXT_SIZE: f32 = 12.5;

    pub const CHECKBOX_SIZE: f32 = 14.;
    pub const CHECKBOX_RADIUS: f32 = 4.;
    pub const CHECKBOX_GAP: f32 = 7.;
    pub const CHECKMARK_SIZE: f32 = 10.;

    pub const SEGMENTED_PAD: f32 = 2.;
    pub const SEGMENTED_RADIUS: f32 = 7.;
    pub const SEGMENTED_OPTION_RADIUS: f32 = SEGMENTED_RADIUS - SEGMENTED_PAD;
    pub const SEGMENTED_OPTION_MIN_WIDTH: f32 = 44.;
    pub const SEGMENTED_OPTION_PAD_X: f32 = 8.;
    pub const SEGMENTED_TEXT_SIZE: f32 = 12.;

    pub const KEYCAP_PAD_X: f32 = 8.;
    pub const KEYCAP_GAP: f32 = 5.;
    /// The recorder while it listens: wider than the chords it holds, because what it
    /// says then is the longest thing in it.
    pub const RECORDER_WIDTH: f32 = 150.;
    pub const STEPPER_VALUE_WIDTH: f32 = 44.;
}

use metrics::*;

/// The window's colours, derived from the note's own text and ground: every tint is
/// the text colour at a fixed strength.
#[derive(Clone, Copy)]
pub(super) struct Palette {
    pub surface: Hsla,
    /// The title band and the toolbar under it.
    pub toolbar: Hsla,
    pub text: Hsla,
    pub subtitle: Hsla,
    pub border: Hsla,
    pub hover: Hsla,
    pub selected: Hsla,
    /// A keycap's fill, and a button's.
    pub fill: Hsla,
    pub pressed: Hsla,
    pub accent: Hsla,
    pub danger: Hsla,
}

impl Palette {
    pub fn new(dark: bool) -> Self {
        let style = crate::app::notes_style(dark);
        let text = style.text;
        let surface = style.background;
        Self {
            surface,
            toolbar: surface.blend(text.alpha(if dark { 0.03 } else { 0.035 })),
            text,
            subtitle: text.alpha(0.58),
            border: text.alpha(if dark { 0.12 } else { 0.1 }),
            hover: text.alpha(0.06),
            selected: text.alpha(if dark { 0.12 } else { 0.09 }),
            fill: text.alpha(if dark { 0.1 } else { 0.07 }),
            pressed: text.alpha(if dark { 0.16 } else { 0.12 }),
            accent: style.marker,
            danger: if dark { rgb(0xf18a8a) } else { rgb(0xc44d4d) }.into(),
        }
    }
}

/// One labelled row: the label right-aligned in its column, and the lines beside it.
/// A row without a label continues the one above, the way a second checkbox under
/// "Window:" does.
pub(super) fn row(label: Option<&'static str>, lines: Vec<AnyElement>, p: Palette) -> Div {
    div()
        .flex()
        .items_start()
        .gap(px(LABEL_GAP))
        .child(
            div()
                .flex_shrink_0()
                .w(px(LABEL_WIDTH))
                .h(px(LINE_HEIGHT))
                .flex()
                .items_center()
                .justify_end()
                .text_size(px(TEXT_SIZE))
                .text_color(p.text)
                .children(label.map(|label| format!("{label}:"))),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(LINE_GAP))
                .children(lines),
        )
}

/// One line beside a label: its controls, level with the label.
pub(super) fn line(controls: Vec<AnyElement>) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(px(CONTROL_GAP))
        .min_h(px(LINE_HEIGHT))
        .children(controls)
        .into_any_element()
}

/// The secondary line that explains the control above it.
pub(super) fn help(text: impl Into<SharedString>, p: Palette) -> AnyElement {
    div()
        .pb(px(2.))
        .text_size(px(HELP_SIZE))
        .text_color(p.subtitle)
        .child(text.into())
        .into_any_element()
}

/// Why the value that was just asked for was refused, under the control that asked.
pub(super) fn error(text: impl Into<SharedString>, p: Palette) -> AnyElement {
    div()
        .text_size(px(HELP_SIZE))
        .text_color(p.danger)
        .child(text.into())
        .into_any_element()
}

/// The hairline between groups of rows.
pub(super) fn divider(p: Palette) -> Div {
    div().my(px(DIVIDER_MARGIN)).h(px(1.)).bg(p.border)
}

/// A value that reads on one line, cut short rather than wrapped: a path.
pub(super) fn value(text: impl Into<SharedString>, secondary: bool, p: Palette) -> AnyElement {
    div()
        .min_w_0()
        .truncate()
        .text_size(px(if secondary { HELP_SIZE } else { TEXT_SIZE }))
        .text_color(if secondary { p.subtitle } else { p.text })
        .child(text.into())
        .into_any_element()
}

pub(super) fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    p: Palette,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Button {
    Button::new(id)
        .flex_shrink_0()
        .h(px(CONTROL_HEIGHT))
        .px(px(BUTTON_PAD_X))
        .rounded(px(CONTROL_RADIUS))
        .bg(p.fill)
        .text_size(px(BUTTON_TEXT_SIZE))
        .text_color(p.text)
        .cursor_pointer()
        .hover(move |style| style.bg(p.pressed))
        .active(move |style| style.bg(p.text.alpha(0.18)))
        .child(label.into())
        .on_click(on_click)
}

/// A labelled checkbox, the classic pane's control for anything on or off.
pub(super) fn checkbox(
    id: &'static str,
    label: &'static str,
    checked: bool,
    disabled: bool,
    p: Palette,
    on_change: impl Fn(bool, &mut Window, &mut App) + 'static,
) -> Checkbox {
    Checkbox::new(id)
        .checked(checked)
        .disabled(disabled)
        .accessibility_label(label)
        .flex()
        .items_center()
        .gap(px(CHECKBOX_GAP))
        .h(px(LINE_HEIGHT))
        .text_size(px(TEXT_SIZE))
        .text_color(p.text)
        .when(disabled, |checkbox| checkbox.opacity(0.45))
        .when(!disabled, |checkbox| checkbox.cursor_pointer())
        .on_change(move |state, _, window, cx| {
            on_change(state == gpui_base::CheckboxState::Checked, window, cx)
        })
        .child(
            div()
                .flex()
                .items_center()
                .justify_center()
                .flex_shrink_0()
                .size(px(CHECKBOX_SIZE))
                .rounded(px(CHECKBOX_RADIUS))
                .when(checked, |mark| {
                    mark.bg(p.accent).child(sized_icon(
                        Icon::Check,
                        rgb(0xffffff).into(),
                        CHECKMARK_SIZE,
                    ))
                })
                .when(!checked, |mark| {
                    mark.bg(p.surface)
                        .border_1()
                        .border_color(p.text.alpha(0.3))
                }),
        )
        .child(label)
}

/// A few choices shown at once. A radio group underneath, because only one of them
/// can be the value.
pub(super) fn segmented<T: Copy + PartialEq + 'static>(
    id: &'static str,
    label: &'static str,
    options: &[(&'static str, T)],
    selected: T,
    p: Palette,
    on_change: impl Fn(T, &mut Window, &mut App) + 'static,
) -> RadioGroup {
    let on_change = std::rc::Rc::new(on_change);
    let count = options.len();
    RadioGroup::new(id)
        .aria_label(label)
        .flex()
        .items_center()
        .h(px(CONTROL_HEIGHT))
        .p(px(SEGMENTED_PAD))
        .gap(px(SEGMENTED_PAD))
        .rounded(px(SEGMENTED_RADIUS))
        .bg(p.fill)
        .children(options.iter().enumerate().map(|(index, &(name, value))| {
            let chosen = value == selected;
            let on_change = on_change.clone();
            Radio::new(SharedString::from(format!("{id}-{index}")))
                .checked(chosen)
                .accessibility_label(name)
                .set_position(index + 1, count)
                .flex()
                .items_center()
                .justify_center()
                .h_full()
                .min_w(px(SEGMENTED_OPTION_MIN_WIDTH))
                .px(px(SEGMENTED_OPTION_PAD_X))
                .rounded(px(SEGMENTED_OPTION_RADIUS))
                .text_size(px(SEGMENTED_TEXT_SIZE))
                .cursor_pointer()
                .when(chosen, |option| {
                    option
                        .bg(p.surface)
                        .text_color(p.text)
                        .shadow(vec![BoxShadow {
                            color: hsla(0., 0., 0., 0.12),
                            offset: point(px(0.), px(0.5)),
                            blur_radius: px(1.5),
                            spread_radius: px(0.),
                            inset: false,
                        }])
                })
                .when(!chosen, |option| {
                    option
                        .text_color(p.subtitle)
                        .hover(move |style| style.text_color(p.text))
                })
                .on_change(move |_, _, window, cx| on_change(value, window, cx))
                .child(name)
        }))
}

/// A number with a button either side: the value in the middle, read as it is set.
pub(super) fn stepper(
    id: &'static str,
    shown: impl Into<SharedString>,
    (can_decrease, can_increase): (bool, bool),
    p: Palette,
    on_step: impl Fn(i32, &mut Window, &mut App) + 'static,
) -> Div {
    let on_step = std::rc::Rc::new(on_step);
    let step = |delta: i32, glyph: &'static str, enabled: bool| {
        let on_step = on_step.clone();
        Button::new(SharedString::from(format!("{id}-{delta}")))
            .disabled(!enabled)
            .accessibility_label(if delta < 0 { "Smaller" } else { "Larger" })
            .size(px(CONTROL_HEIGHT))
            .rounded(px(CONTROL_RADIUS))
            .bg(p.fill)
            .text_size(px(TEXT_SIZE + 1.))
            .text_color(if enabled { p.text } else { p.subtitle })
            .when(enabled, |button| {
                button
                    .cursor_pointer()
                    .hover(move |style| style.bg(p.pressed))
            })
            .child(glyph)
            .on_click(move |_, window, cx| on_step(delta, window, cx))
    };
    div()
        .flex()
        .items_center()
        .gap(px(4.))
        .child(step(-1, "−", can_decrease))
        .child(
            div()
                .w(px(STEPPER_VALUE_WIDTH))
                .flex()
                .justify_center()
                .text_size(px(TEXT_SIZE))
                .child(shown.into()),
        )
        .child(step(1, "+", can_increase))
}

/// What a shortcut field shows: listening, the chord it holds, or none.
pub(super) enum ChordFace {
    Recording,
    Bound(Vec<String>),
    Unbound,
}

pub(super) fn chord_face(face: ChordFace, p: Palette) -> Div {
    let pill = || {
        div()
            .flex()
            .items_center()
            .flex_shrink_0()
            .h(px(CONTROL_HEIGHT))
            .px(px(KEYCAP_PAD_X))
            .rounded(px(CONTROL_RADIUS))
            .text_size(px(TEXT_SIZE))
    };
    match face {
        ChordFace::Recording => pill()
            .justify_center()
            .w(px(RECORDER_WIDTH))
            .bg(p.text.alpha(0.06))
            .border_1()
            .border_color(p.subtitle)
            .text_color(p.subtitle)
            .child("Press shortcut…"),
        ChordFace::Bound(keys) => pill()
            .gap(px(KEYCAP_GAP))
            .bg(p.fill)
            .text_color(p.text)
            .children(keys),
        ChordFace::Unbound => pill().bg(p.fill).text_color(p.subtitle).child("None"),
    }
}
