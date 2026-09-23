//! The Settings window's page grammar: a titled card of rows, each row a label and
//! description on the leading edge and one control on the trailing one. The numbers
//! follow cmdspace's settings pages, so the two windows read as one family; behaviour
//! and semantics come from `gpui-base`, and every colour from the note's own palette.

use crate::app::ui::tokens::playback;
use gpui::{prelude::*, *};
use gpui_base::{Button, Radio, RadioGroup, Switch, SwitchThumb, SwitchTrack};

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

    pub const PAGE_PADDING: f32 = 20.;
    pub const PAGE_TOP: f32 = 20.;
    pub const PAGE_BOTTOM: f32 = 24.;
    pub const PAGE_MAX_WIDTH: f32 = 560.;

    pub const SECTION_GAP: f32 = 24.;
    pub const SECTION_TITLE_SIZE: f32 = 13.;
    pub const SECTION_TITLE_BOTTOM: f32 = 8.;
    pub const CARD_RADIUS: f32 = 8.;
    pub const CARD_PAD_X: f32 = 12.;
    /// The title over a card and the note under it start where the rows' text does.
    pub const SECTION_TEXT_INSET: f32 = CARD_PAD_X;
    pub const NOTE_GAP: f32 = 8.;

    /// Every boxed control on a page, border included.
    pub const CONTROL_HEIGHT: f32 = 26.;
    pub const CONTROL_RADIUS: f32 = 6.;
    pub const ROW_PAD_Y: f32 = 8.;
    /// One control, padded above and below.
    pub const ROW_MIN_HEIGHT: f32 = CONTROL_HEIGHT + ROW_PAD_Y * 2.;
    pub const ROW_GAP: f32 = 12.;
    pub const ROW_TEXT_GAP: f32 = 2.;
    pub const ROW_CONTROL_GAP: f32 = 6.;
    pub const TRAILING_GAP: f32 = 6.;
    pub const LABEL_SIZE: f32 = 13.;
    /// Everything under a label reads as the one secondary voice.
    pub const DESCRIPTION_SIZE: f32 = 11.5;
    pub const DISABLED_OPACITY: f32 = 0.45;

    pub const BUTTON_PAD_X: f32 = 10.;
    pub const BUTTON_TEXT_SIZE: f32 = 12.5;

    pub const SEGMENTED_PAD: f32 = 2.;
    pub const SEGMENTED_RADIUS: f32 = 7.;
    pub const SEGMENTED_OPTION_RADIUS: f32 = SEGMENTED_RADIUS - SEGMENTED_PAD;
    pub const SEGMENTED_OPTION_MIN_WIDTH: f32 = 44.;
    pub const SEGMENTED_OPTION_PAD_X: f32 = 8.;
    pub const SEGMENTED_TEXT_SIZE: f32 = 12.;

    pub const SWITCH_WIDTH: f32 = 30.;
    pub const SWITCH_HEIGHT: f32 = 18.;
    pub const SWITCH_THUMB: f32 = 14.;

    /// A bound chord, as one pill inside the control height.
    pub const KEYCAP_HEIGHT: f32 = 24.;
    pub const KEYCAP_PAD_X: f32 = 8.;
    pub const KEYCAP_GAP: f32 = 5.;
    /// The recorder while it listens: wider than the chords it holds, because what it
    /// says then is the longest thing in it.
    pub const RECORDER_WIDTH: f32 = 150.;
}

use metrics::*;

/// The window's colours, derived from the note's own text and ground the way
/// cmdspace derives its chrome: every tint is the text colour at a fixed strength.
#[derive(Clone, Copy)]
pub(super) struct Palette {
    pub surface: Hsla,
    /// The title band and the toolbar under it.
    pub toolbar: Hsla,
    pub text: Hsla,
    pub subtitle: Hsla,
    /// The ground of a card of rows.
    pub card: Hsla,
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
            card: text.alpha(if dark { 0.055 } else { 0.04 }),
            border: text.alpha(0.08),
            hover: text.alpha(0.06),
            selected: text.alpha(if dark { 0.12 } else { 0.09 }),
            fill: text.alpha(if dark { 0.1 } else { 0.07 }),
            pressed: text.alpha(if dark { 0.16 } else { 0.12 }),
            accent: style.marker,
            danger: if dark { rgb(0xf18a8a) } else { rgb(0xc44d4d) }.into(),
        }
    }
}

/// A titled card of rows. The rules between rows say where one setting ends, so the
/// card is a fill and nothing else.
pub(super) fn section(
    title: &'static str,
    rows: Vec<AnyElement>,
    note: Option<SharedString>,
    p: Palette,
) -> Div {
    div()
        .flex()
        .flex_col()
        .flex_shrink_0()
        .pb(px(SECTION_GAP))
        .child(
            div()
                .pb(px(SECTION_TITLE_BOTTOM))
                .pl(px(SECTION_TEXT_INSET))
                .text_size(px(SECTION_TITLE_SIZE))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(p.subtitle)
                .child(title),
        )
        .child(
            div()
                .id(title)
                .role(Role::Group)
                .aria_label(title)
                .flex()
                .flex_col()
                .px(px(CARD_PAD_X))
                .rounded(px(CARD_RADIUS))
                .bg(p.card)
                .children(rows.into_iter().enumerate().map(|(position, row)| {
                    div()
                        .when(position > 0, |row| row.border_t_1().border_color(p.border))
                        .child(row)
                })),
        )
        .children(note.map(|note| {
            div()
                .pt(px(NOTE_GAP))
                .pl(px(SECTION_TEXT_INSET))
                .text_size(px(DESCRIPTION_SIZE))
                .text_color(p.subtitle)
                .child(note)
        }))
}

/// One setting, one row.
pub(super) struct Row {
    label: SharedString,
    description: Option<SharedString>,
    trailing: Vec<AnyElement>,
    hint: Option<SharedString>,
    error: Option<SharedString>,
    disabled: bool,
    /// The description is a path or another value that reads on one line, cut short
    /// rather than wrapped.
    single_line: bool,
}

impl Row {
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            description: None,
            trailing: Vec::new(),
            hint: None,
            error: None,
            disabled: false,
            single_line: false,
        }
    }
    pub fn single_line(mut self) -> Self {
        self.single_line = true;
        self
    }
    pub fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }
    /// The controls on the trailing edge, in reading order: a reset goes before the
    /// control it resets, so the two read as one.
    pub fn trailing(mut self, control: impl IntoElement) -> Self {
        self.trailing.push(control.into_any_element());
        self
    }
    pub fn hint(mut self, hint: Option<impl Into<SharedString>>) -> Self {
        self.hint = hint.map(Into::into);
        self
    }
    /// Why the value that was just asked for was refused. Inline rather than in the
    /// note's window: the row that is wrong is the row that has to say so.
    pub fn error(mut self, error: Option<impl Into<SharedString>>) -> Self {
        self.error = error.map(Into::into);
        self
    }
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn render(self, p: Palette) -> AnyElement {
        let disabled = self.disabled;
        let single_line = self.single_line;
        div()
            .flex()
            .flex_col()
            .justify_center()
            .w_full()
            .min_h(px(ROW_MIN_HEIGHT))
            .py(px(ROW_PAD_Y))
            .gap(px(ROW_CONTROL_GAP))
            .child(
                div()
                    .flex()
                    .items_center()
                    .w_full()
                    .gap(px(ROW_GAP))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(ROW_TEXT_GAP))
                            .when(disabled, |text| text.opacity(DISABLED_OPACITY))
                            .child(
                                div()
                                    .text_size(px(LABEL_SIZE))
                                    .text_color(p.text)
                                    .child(self.label),
                            )
                            .children(self.description.map(|description| {
                                div()
                                    .text_size(px(DESCRIPTION_SIZE))
                                    .text_color(p.subtitle)
                                    .when(single_line, |line| line.truncate())
                                    .child(description)
                            })),
                    )
                    .when(!self.trailing.is_empty(), |row| {
                        row.child(
                            div()
                                .flex_shrink_0()
                                .flex()
                                .items_center()
                                .justify_end()
                                .gap(px(TRAILING_GAP))
                                .children(self.trailing),
                        )
                    }),
            )
            .children(self.hint.map(|hint| {
                div()
                    .text_size(px(DESCRIPTION_SIZE))
                    .text_color(p.subtitle)
                    .child(hint)
            }))
            .children(self.error.map(|error| {
                div()
                    .text_size(px(DESCRIPTION_SIZE))
                    .text_color(p.danger)
                    .child(error)
            }))
            .into_any_element()
    }
}

pub(super) fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    p: Palette,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Button {
    Button::new(id)
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

/// Critically damped, settling in about 150 ms, like the note's own controls.
const SWITCH_SPRING: SpringConfig = SpringConfig::new(3700., 121.7, 1.);

pub(super) fn switch(
    id: &'static str,
    label: &'static str,
    checked: bool,
    p: Palette,
    cx: &App,
    on_change: impl Fn(bool, &mut Window, &mut App) + 'static,
) -> Switch {
    let travel = SWITCH_WIDTH - SWITCH_THUMB - 4.;
    let off = p.text.alpha(0.2);
    let on = p.accent;
    Switch::new(id)
        .checked(checked)
        .accessibility_label(label)
        .rounded_full()
        .cursor_pointer()
        .on_change(move |next, _, window, cx| on_change(next, window, cx))
        .child(
            SwitchTrack::new(SharedString::from(format!("{id}-track")))
                .checked(checked)
                .w(px(SWITCH_WIDTH))
                .h(px(SWITCH_HEIGHT))
                .p(px(2.))
                .rounded_full()
                .with_spring(
                    SharedString::from(format!("{id}-spring")),
                    SpringAnimation::new(SWITCH_SPRING)
                        .to(checked)
                        .playback(playback(cx.reduce_motion())),
                    move |track, phase| {
                        track.bg(phase.interpolate_clamped(off, on)).child(
                            SwitchThumb::new(checked)
                                .size(px(SWITCH_THUMB))
                                .rounded_full()
                                .bg(rgb(0xffffff))
                                .ml(phase.interpolate_clamped(px(0.), px(travel))),
                        )
                    },
                ),
        )
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

/// What the shortcut field shows: listening, the chord it holds, or none.
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
            .h(px(KEYCAP_HEIGHT))
            .px(px(KEYCAP_PAD_X))
            .rounded(px(CONTROL_RADIUS))
            .text_size(px(LABEL_SIZE))
    };
    match face {
        ChordFace::Recording => div()
            .flex()
            .items_center()
            .justify_center()
            .w(px(RECORDER_WIDTH))
            .h(px(CONTROL_HEIGHT))
            .rounded(px(CONTROL_RADIUS))
            .bg(p.text.alpha(0.06))
            .border_1()
            .border_color(p.subtitle)
            .text_size(px(LABEL_SIZE))
            .text_color(p.subtitle)
            .child("Press shortcut…"),
        ChordFace::Bound(keys) => pill()
            .gap(px(KEYCAP_GAP))
            .bg(p.fill)
            .text_color(p.text)
            .children(keys),
        ChordFace::Unbound => pill().text_color(p.subtitle).child("None"),
    }
}
