//! Pop-up buttons: one value out of a short list, drawn the way AppKit's pop-up button
//! is. The button shows the value and a pair of chevrons; the menu opens over it with
//! the value under the pointer and a checkmark beside it.
//!
//! `gpui-base` supplies the behaviour — [`Select`] for the keyboard, focus and
//! accessibility, [`Popup`] for placing the menu above the page — and this module the
//! look and the one piece of state, which menu is open and which row is lit.

use super::controls::{Palette, metrics::*};
use super::{Change, SettingsView};
use crate::app::ui::icons::{Icon, sized_icon};
use gpui::{prelude::*, *};
use gpui_base::actions::{SelectDown, SelectUp};
use gpui_base::{Popup, Select};
use std::rc::Rc;

/// Every pop-up button the pages hold. Each has its own place in the focus order.
const IDS: [&str; 16] = [
    "summon",
    "language",
    "line-width",
    "ordered-delimiter",
    "hard-break",
    "image-name",
    "notes-folder",
    "new-note-folder",
    "image-folder",
    "font",
    "line-height",
    "tab-key",
    "new-note-name",
    "bullet-marker",
    "code-fence",
    "emphasis-marker",
];

pub(super) struct Selects {
    open: Option<&'static str>,
    /// The row the keyboard or the pointer is on while a menu is open.
    lit: usize,
    /// Where the keyboard goes while a menu is open.
    menu: FocusHandle,
    triggers: Vec<FocusHandle>,
}

impl Selects {
    pub(super) fn new(cx: &mut App) -> Self {
        Self {
            open: None,
            lit: 0,
            menu: cx.focus_handle(),
            triggers: IDS.iter().map(|_| cx.focus_handle()).collect(),
        }
    }

    pub(super) fn close(&mut self) {
        self.open = None;
    }

    fn trigger(&self, id: &str) -> &FocusHandle {
        let index = IDS
            .iter()
            .position(|known| *known == id)
            .expect("every pop-up button is listed in IDS");
        &self.triggers[index]
    }
}

/// A row of a pop-up button's menu.
pub(super) enum MenuRow {
    Item(MenuItem),
    /// A hairline between groups of items.
    Separator,
}

pub(super) struct MenuItem {
    label: SharedString,
    /// What choosing it asks for; nothing for the value already chosen.
    change: Option<Change>,
    /// The value the button shows, ticked in its menu.
    checked: bool,
}

impl MenuItem {
    /// A value to choose, ticked while it is the one chosen.
    pub(super) fn choice(
        label: impl Into<SharedString>,
        change: Option<Change>,
        checked: bool,
    ) -> MenuRow {
        MenuRow::Item(Self {
            label: label.into(),
            change,
            checked,
        })
    }

    /// Something to do from the menu — Choose Folder…, Show in Finder — that no value
    /// stands for.
    pub(super) fn action(label: impl Into<SharedString>, change: Change) -> MenuRow {
        MenuRow::Item(Self {
            label: label.into(),
            change: Some(change),
            checked: false,
        })
    }
}

impl MenuRow {
    fn height(&self) -> f32 {
        match self {
            MenuRow::Item(_) => SELECT_ROW_HEIGHT,
            MenuRow::Separator => SELECT_SEPARATOR_HEIGHT,
        }
    }

    fn item(&self) -> Option<&MenuItem> {
        match self {
            MenuRow::Item(item) => Some(item),
            MenuRow::Separator => None,
        }
    }
}

/// The next item from `from` in `down`'s direction, past the separators, or `from`
/// itself at either end. `items` says which rows are items.
fn step(items: &[bool], from: usize, down: bool) -> usize {
    let mut at = from;
    loop {
        let next = if down {
            at + 1
        } else {
            match at.checked_sub(1) {
                Some(next) => next,
                None => return from,
            }
        };
        match items.get(next) {
            Some(true) => return next,
            Some(false) => at = next,
            None => return from,
        }
    }
}

impl SettingsView {
    /// A pop-up button for `value`, one of `options`; choosing one sends `change`.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn select<T: Clone + PartialEq + 'static>(
        &self,
        id: &'static str,
        label: String,
        options: &[(String, T)],
        value: T,
        change: fn(T) -> Change,
        p: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let face = options
            .iter()
            .find(|(_, v)| *v == value)
            .unwrap_or(&options[0])
            .0
            .clone();
        let rows = options
            .iter()
            .map(|(name, v)| MenuItem::choice(name.clone(), Some(change(v.clone())), *v == value))
            .collect();
        self.pop_up(id, label, face.into(), rows, p, cx)
    }

    /// A pop-up button showing `face`, whose menu holds `rows`.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn pop_up(
        &self,
        id: &'static str,
        label: String,
        face: SharedString,
        rows: Vec<MenuRow>,
        p: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = self.selects.open == Some(id);
        let chosen = rows
            .iter()
            .position(|row| row.item().is_some_and(|item| item.checked))
            .or_else(|| rows.iter().position(|row| row.item().is_some()))
            .unwrap_or(0);
        let trigger_focus = self.selects.trigger(id).clone();
        let this = cx.entity().downgrade();

        let trigger = div()
            .id(SharedString::from(format!("{id}-button")))
            .flex()
            .items_center()
            .w(px(SELECT_WIDTH))
            .h(px(SELECT_HEIGHT))
            .pl(px(BUTTON_PAD_X))
            .pr(px(SELECT_PAD_RIGHT))
            .rounded(px(SELECT_RADIUS))
            // AppKit's pop-up button does not light up under the pointer; it darkens
            // while its menu is open.
            .bg(if open { p.pressed } else { p.fill })
            .text_size(px(TEXT_SIZE))
            .text_color(p.text)
            .on_click(cx.listener(move |view, _, window, cx| {
                if view.selects.open == Some(id) {
                    view.close_select(window, cx);
                } else {
                    view.open_select(id, chosen, window, cx);
                }
            }))
            .child(div().flex_1().min_w_0().truncate().child(face.clone()))
            .child(sized_icon(Icon::UpDown, p.text, SELECT_CHEVRON));

        // What each row asks for, for the keyboard's Return as well as the pointer.
        let changes: Rc<Vec<Option<Change>>> = Rc::new(
            rows.iter()
                .map(|row| row.item().and_then(|item| item.change.clone()))
                .collect(),
        );
        let menu = open.then(|| self.select_menu(id, rows, chosen, changes.clone(), p, cx));
        let on_open = this.clone();
        let on_confirm = this.clone();
        Select::new(id)
            .open(open)
            .focus_handle(&trigger_focus)
            .content_focus_handle(&self.selects.menu)
            .accessibility_label(label)
            .accessibility_value(face)
            .on_open_change(move |open, window, cx| {
                let _ = on_open.update(cx, |view, cx| {
                    if open {
                        view.open_select(id, chosen, window, cx);
                    } else {
                        view.selects.close();
                        cx.notify();
                    }
                });
            })
            .on_confirm(move |window, cx| {
                let _ = on_confirm.update(cx, |view, cx| {
                    let change = changes.get(view.selects.lit).cloned().flatten();
                    view.choose(change, window, cx);
                });
            })
            .child(
                Popup::new(SharedString::from(format!("{id}-popup")), trigger)
                    .anchor(Anchor::TopLeft)
                    .when_some(menu, |popup, menu| popup.content(menu)),
            )
            .into_any_element()
    }

    /// The open menu, lifted so that the value it holds sits over the button.
    fn select_menu(
        &self,
        id: &'static str,
        rows: Vec<MenuRow>,
        chosen: usize,
        changes: Rc<Vec<Option<Change>>>,
        p: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let lit = self.selects.lit;
        let above: f32 = rows[..chosen].iter().map(MenuRow::height).sum();
        let height = SELECT_MENU_PAD * 2. + 2. + rows.iter().map(MenuRow::height).sum::<f32>();
        let lift = SELECT_HEIGHT / 2. + 1. + SELECT_MENU_PAD + above + SELECT_ROW_HEIGHT / 2.;
        // Which rows the keyboard can land on: the items, not the separators.
        let kinds: Rc<Vec<bool>> = Rc::new(rows.iter().map(|row| row.item().is_some()).collect());
        let (down, up) = (kinds.clone(), kinds);
        let menu = div()
            .id(SharedString::from(format!("{id}-menu")))
            .track_focus(&self.selects.menu)
            .role(Role::ListBox)
            .occlude()
            // The host sits under the button, so the menu is lifted by the button's
            // height and then far enough that the row it holds is centred on the button,
            // the way AppKit opens a pop-up button's menu.
            .absolute()
            .top(px(-lift))
            // The labels line up with the button's, past the checkmark column.
            .left(px(-(SELECT_MENU_PAD
                + SELECT_CHECK_WIDTH
                + SELECT_ROW_PAD_X
                - BUTTON_PAD_X)))
            .p(px(SELECT_MENU_PAD))
            // As wide as the button from its label on, and the checkmark column besides.
            .min_w(px(SELECT_WIDTH
                + SELECT_MENU_PAD * 2.
                + SELECT_CHECK_WIDTH
                + SELECT_ROW_PAD_X
                - BUTTON_PAD_X))
            .flex()
            .flex_col()
            .rounded(px(SELECT_MENU_RADIUS))
            .bg(p.menu)
            .border_1()
            .border_color(p.border)
            .shadow_lg()
            .text_size(px(TEXT_SIZE))
            .text_color(p.text)
            .on_action(cx.listener(move |view, _: &SelectDown, _, cx| {
                view.selects.lit = step(&down, view.selects.lit, true);
                cx.notify();
            }))
            .on_action(cx.listener(move |view, _: &SelectUp, _, cx| {
                view.selects.lit = step(&up, view.selects.lit, false);
                cx.notify();
            }))
            .on_mouse_down_out(cx.listener(|view, _, window, cx| view.close_select(window, cx)))
            .children(rows.into_iter().enumerate().map(|(row, entry)| {
                let Some(item) = entry.item() else {
                    return div()
                        .h(px(SELECT_SEPARATOR_HEIGHT))
                        .flex()
                        .items_center()
                        .px(px(SELECT_ROW_PAD_X))
                        .child(div().w_full().h(px(1.)).bg(p.border))
                        .into_any_element();
                };
                let lit = row == lit;
                let ink = if lit { white() } else { p.text };
                let changes = changes.clone();
                div()
                    .id(SharedString::from(format!("{id}-option-{row}")))
                    .role(Role::ListBoxOption)
                    .aria_selected(item.checked)
                    .when(lit, |item| item.aria_active_descendant())
                    .flex()
                    .items_center()
                    .h(px(SELECT_ROW_HEIGHT))
                    .pr(px(SELECT_ROW_PAD_X))
                    .rounded(px(SELECT_ROW_RADIUS))
                    .when(lit, |item| item.bg(p.accent).text_color(white()))
                    .cursor_pointer()
                    .on_hover(cx.listener(move |view, hovered: &bool, _, cx| {
                        if *hovered && view.selects.lit != row {
                            view.selects.lit = row;
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |view, _, window, cx| {
                        view.choose(changes.get(row).cloned().flatten(), window, cx)
                    }))
                    .child(
                        div()
                            .flex_shrink_0()
                            .w(px(SELECT_CHECK_WIDTH + SELECT_ROW_PAD_X))
                            .flex()
                            .justify_center()
                            .when(item.checked, |cell| {
                                cell.child(sized_icon(Icon::Check, ink, SELECT_CHECK))
                            }),
                    )
                    .child(item.label.clone())
                    .into_any_element()
            }));
        // The positioner places what it holds by that element's own bounds, so the
        // menu is shifted inside a host rather than moved itself. The host is as tall
        // as the menu reaches below the button: near the window's foot the positioner
        // then lifts it, menu and all, rather than letting the window cut it off.
        div()
            .h(px((height - lift).max(0.)))
            .child(menu)
            .into_any_element()
    }

    fn open_select(
        &mut self,
        id: &'static str,
        chosen: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selects.open = Some(id);
        self.selects.lit = chosen;
        window.focus(&self.selects.menu, cx);
        cx.notify();
    }

    fn close_select(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.selects.open.take() {
            window.focus(&self.selects.trigger(id).clone(), cx);
            cx.notify();
        }
    }

    /// A row was chosen: what it asks for goes to the app, and the menu closes.
    fn choose(&mut self, change: Option<Change>, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(change) = change {
            self.link.send(change, cx);
        }
        self.close_select(window, cx);
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::step;

    #[test]
    fn the_keyboard_walks_the_items_past_separators_and_stops_at_the_ends() {
        let items = [true, false, true, true];
        assert_eq!(step(&items, 0, true), 2);
        assert_eq!(step(&items, 2, false), 0);
        assert_eq!(step(&items, 3, true), 3);
        assert_eq!(step(&items, 0, false), 0);
    }
}
