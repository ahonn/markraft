//! Pop-up buttons: one value out of a short list, drawn the way AppKit's pop-up button
//! is. The button shows the value and a pair of chevrons; the menu hangs from the
//! button, or stands on it where the window has no room below, with a checkmark
//! beside the value and scrolling for longer lists.
//!
//! `gpui-base` supplies the behaviour — [`Select`] for the keyboard, focus and
//! accessibility, [`Popup`] for placing the menu above the page — and this module the
//! look and the one piece of state, which menu is open and which row is lit.

use super::controls::{Palette, metrics::*};
use super::{Change, SettingsView};
use crate::app::ui::icons::{Icon, sized_icon};
use gpui::{prelude::*, *};
use gpui_base::actions::{SelectDown, SelectUp};
use gpui_base::{ElementExt as _, Popup, Select};
use std::cell::Cell;
use std::rc::Rc;

/// Every pop-up button the pages hold. Each has its own place in the focus order.
const IDS: [&str; 18] = [
    "summon",
    "language",
    "line-width",
    "ordered-delimiter",
    "hard-break",
    "image-name",
    "notes-folder",
    "new-note-folder",
    "image-folder",
    "daily-folder",
    "daily-template",
    "font",
    "line-height",
    "tab-key",
    "new-note-name",
    "bullet-marker",
    "code-fence",
    "emphasis-marker",
];

/// What `Popup` keeps between a menu and the window's edge.
const POPUP_MARGIN: Pixels = px(8.);

pub(super) struct Selects {
    open: Option<&'static str>,
    /// The row the keyboard or the pointer is on while a menu is open.
    lit: usize,
    /// Where the keyboard goes while a menu is open.
    menu: FocusHandle,
    scroll: ScrollHandle,
    triggers: Vec<FocusHandle>,
    /// Where each button was last drawn.
    spots: Vec<Rc<Cell<Spot>>>,
}

/// Where a pop-up button was drawn and how tall its window was: what says which
/// side of the button has room for the menu.
#[derive(Clone, Copy, Default)]
struct Spot {
    button: Bounds<Pixels>,
    window: Pixels,
}

/// An open menu before it is placed: the surface that is painted and, inside its
/// padding, the list that scrolls when the rows are taller than the room there is.
struct Menu {
    surface: Div,
    list: Stateful<Div>,
    /// The rows' heights added up.
    rows_height: f32,
    /// Where the lit row ends, from the top of the list.
    lit_bottom: f32,
}

impl Selects {
    pub(super) fn new(cx: &mut App) -> Self {
        Self {
            open: None,
            lit: 0,
            menu: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            triggers: IDS.iter().map(|_| cx.focus_handle()).collect(),
            spots: IDS.iter().map(|_| Rc::default()).collect(),
        }
    }

    pub(super) fn close(&mut self) {
        self.open = None;
    }

    fn highlight(&mut self, row: usize) {
        self.lit = row;
        self.scroll.scroll_to_item(row);
    }

    fn index(id: &str) -> usize {
        IDS.iter()
            .position(|known| *known == id)
            .expect("every pop-up button is listed in IDS")
    }

    fn trigger(&self, id: &str) -> &FocusHandle {
        &self.triggers[Self::index(id)]
    }

    /// The button and, while it is open, its menu. The popup places the surface, so
    /// which row is lit never moves the menu.
    fn popup(&self, id: &'static str, trigger: Stateful<Div>, menu: Option<Menu>) -> Popup {
        let spot = self.spots[Self::index(id)].clone();
        let drawn = spot.get();
        let trigger = trigger.on_prepaint(move |button, window, _| {
            spot.set(Spot {
                button,
                window: window.viewport_size().height,
            })
        });
        let popup = Popup::new(SharedString::from(format!("{id}-popup")), trigger);
        let Some(menu) = menu else {
            return popup;
        };
        // The padding and the border, above the list and below it.
        let frame = px(SELECT_MENU_PAD * 2. + 2.);
        let wanted = (px(menu.rows_height) + frame).min(px(SELECT_MENU_MAX_HEIGHT));
        let (anchor, height) = place(drawn, wanted);
        let viewport = (height - frame).max(px(0.));
        // GPUI handles scroll_to_item before initializing overflow on the first
        // layout. Seed the offset from our fixed row heights to reveal the value
        // immediately, and again should the room for the menu change under it;
        // subsequent keyboard navigation uses the measured bounds.
        if (self.scroll.bounds().size.height - viewport).abs() > px(0.5) {
            self.scroll
                .set_offset(point(px(0.), -(px(menu.lit_bottom) - viewport).max(px(0.))));
        }
        popup.anchor(anchor).content(
            menu.surface.h(height).child(
                menu.list
                    .h(viewport)
                    .track_scroll(&self.scroll)
                    .overflow_y_scroll(),
            ),
        )
    }
}

/// Which corner of the button a menu `wanted` tall hangs from, and how tall it may
/// be there: under the button where it fits, over it where it fits only there, and
/// otherwise on the side with more room, cut to that room.
fn place(spot: Spot, wanted: Pixels) -> (Anchor, Pixels) {
    let below = spot.window - POPUP_MARGIN - spot.button.bottom();
    let above = spot.button.top() - POPUP_MARGIN;
    let (anchor, room) = if wanted <= below || below >= above {
        (Anchor::TopLeft, below)
    } else {
        (Anchor::BottomLeft, above)
    };
    (anchor, wanted.min(room.floor()).max(px(0.)))
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
        let menu = open.then(|| self.select_menu(id, rows, changes.clone(), p, cx));
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
            .child(self.selects.popup(id, trigger, menu))
            .into_any_element()
    }

    /// The open menu: its surface, and the rows in a list of their own.
    fn select_menu(
        &self,
        id: &'static str,
        rows: Vec<MenuRow>,
        changes: Rc<Vec<Option<Change>>>,
        p: Palette,
        cx: &mut Context<Self>,
    ) -> Menu {
        let lit = self.selects.lit;
        let rows_height = rows.iter().map(MenuRow::height).sum();
        let lit_bottom = rows.iter().take(lit + 1).map(MenuRow::height).sum();
        // Which rows the keyboard can land on: the items, not the separators.
        let kinds: Rc<Vec<bool>> = Rc::new(rows.iter().map(|row| row.item().is_some()).collect());
        let (down, up) = (kinds.clone(), kinds);
        let surface = div()
            .occlude()
            .p(px(SELECT_MENU_PAD))
            // No narrower than the button it hangs from.
            .min_w(px(SELECT_WIDTH))
            .flex()
            .flex_col()
            .rounded(px(SELECT_MENU_RADIUS))
            .bg(p.menu)
            .border_1()
            .border_color(p.border)
            .shadow_lg()
            .text_size(px(TEXT_SIZE))
            .text_color(p.text)
            .on_mouse_down_out(cx.listener(|view, _, window, cx| view.close_select(window, cx)));
        let list = div()
            .id(SharedString::from(format!("{id}-menu")))
            .track_focus(&self.selects.menu)
            .role(Role::ListBox)
            .flex()
            .flex_col()
            .on_action(cx.listener(move |view, _: &SelectDown, _, cx| {
                view.selects.highlight(step(&down, view.selects.lit, true));
                cx.notify();
            }))
            .on_action(cx.listener(move |view, _: &SelectUp, _, cx| {
                view.selects.highlight(step(&up, view.selects.lit, false));
                cx.notify();
            }))
            .children(rows.into_iter().enumerate().map(|(row, entry)| {
                let Some(item) = entry.item() else {
                    return div()
                        .flex_shrink_0()
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
                    .flex_shrink_0()
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
        Menu {
            surface,
            list,
            rows_height,
            lit_bottom,
        }
    }

    fn open_select(
        &mut self,
        id: &'static str,
        chosen: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selects.open = Some(id);
        self.selects.scroll = ScrollHandle::new();
        self.selects.highlight(chosen);
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
    use super::super::controls::metrics::{SELECT_HEIGHT, SELECT_MENU_PAD, SELECT_ROW_HEIGHT};
    use super::{Menu, Selects, Spot, place, step};
    use gpui::{
        Anchor, Bounds, Context, InteractiveElement, IntoElement, ParentElement, Render,
        ScrollDelta, ScrollWheelEvent, Styled, TestAppContext, VisualTestContext, Window, div,
        point, px, size,
    };

    /// A button 120 down the window, and its menu open.
    struct MenuHarness {
        selects: Selects,
        rows: usize,
    }

    impl Render for MenuHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().pt(px(120.)).pl(px(30.)).child(
                self.selects.popup(
                    "language",
                    div()
                        .id("test-button")
                        .debug_selector(|| "button".into())
                        .w(px(200.))
                        .h(px(SELECT_HEIGHT)),
                    Some(Menu {
                        surface: div()
                            .debug_selector(|| "menu".into())
                            .w(px(200.))
                            .flex()
                            .flex_col()
                            .p(px(SELECT_MENU_PAD))
                            .border_1(),
                        list: div()
                            .id("test-menu")
                            .flex()
                            .flex_col()
                            .children((0..self.rows).map(|row| {
                                div()
                                    .flex_shrink_0()
                                    .h(px(SELECT_ROW_HEIGHT))
                                    .child(row.to_string())
                            })),
                        rows_height: self.rows as f32 * SELECT_ROW_HEIGHT,
                        lit_bottom: (self.selects.lit + 1) as f32 * SELECT_ROW_HEIGHT,
                    }),
                ),
            )
        }
    }

    fn draw(cx: &mut VisualTestContext) {
        // Capture the anchor, then lay out the menu.
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }

    fn spot(top: f32, window: f32) -> Spot {
        Spot {
            button: Bounds::new(point(px(30.), px(top)), size(px(180.), px(24.))),
            window: px(window),
        }
    }

    #[test]
    fn a_menu_opens_on_the_side_of_its_button_that_has_room() {
        // Under the button while it fits there, even where there is more room above.
        assert_eq!(place(spot(300., 500.), px(84.)), (Anchor::TopLeft, px(84.)));
        assert_eq!(
            place(spot(300., 500.), px(168.)),
            (Anchor::TopLeft, px(168.))
        );
        // Above it once the window ends too soon below.
        assert_eq!(
            place(spot(300., 400.), px(84.)),
            (Anchor::BottomLeft, px(84.))
        );
        // Fitting neither side, the side with more room, as tall as that room.
        assert_eq!(
            place(spot(300., 400.), px(304.)),
            (Anchor::BottomLeft, px(292.))
        );
        assert_eq!(
            place(spot(100., 400.), px(304.)),
            (Anchor::TopLeft, px(268.))
        );
    }

    #[gpui::test]
    fn a_menu_stays_where_it_opened_while_the_selection_moves(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(|_, cx| MenuHarness {
            selects: Selects::new(cx),
            rows: 10,
        });
        cx.simulate_resize(size(px(500.), px(420.)));
        draw(cx);
        let button = cx.debug_bounds("button").unwrap();
        let before = cx.debug_bounds("menu").unwrap();
        assert_eq!(before.top(), button.bottom());
        assert_eq!(before.left(), button.left());
        assert_eq!(before.size.height, px(252.));
        view.update(cx, |view, cx| {
            view.selects.highlight(8);
            cx.notify();
        });
        draw(cx);
        assert_eq!(cx.debug_bounds("menu").unwrap(), before);
        assert_eq!(
            view.update(cx, |view, _| view.selects.scroll.offset().y),
            px(0.)
        );
    }

    #[gpui::test]
    fn a_menu_with_no_room_below_stands_on_its_button(cx: &mut TestAppContext) {
        let (_, cx) = cx.add_window_view(|_, cx| MenuHarness {
            selects: Selects::new(cx),
            rows: 3,
        });
        cx.simulate_resize(size(px(500.), px(220.)));
        draw(cx);
        let button = cx.debug_bounds("button").unwrap();
        let menu = cx.debug_bounds("menu").unwrap();
        assert_eq!(menu.bottom(), button.top());
        assert_eq!(menu.left(), button.left());
        assert_eq!(menu.size.height, px(84.));
    }

    #[gpui::test]
    fn constrained_menu_scrolls_and_keeps_keyboard_selection_visible(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(|_, cx| {
            let mut selects = Selects::new(cx);
            selects.highlight(19);
            MenuHarness { selects, rows: 20 }
        });
        cx.simulate_resize(size(px(500.), px(220.)));
        draw(cx);
        // More room above the button than below it, and not enough for the rows.
        let bounds = cx.debug_bounds("menu").unwrap();
        assert_eq!(bounds.top(), px(8.));
        assert_eq!(bounds.bottom(), cx.debug_bounds("button").unwrap().top());
        let scroll = view.update(cx, |view, _| view.selects.scroll.clone());
        assert!(
            scroll.offset().y < px(0.),
            "bounds {:?}, max {:?}, last {:?}",
            scroll.bounds(),
            scroll.max_offset(),
            scroll.bounds_for_item(19)
        );
        // The rows scroll inside the menu's padding, not under its edge.
        let inset = px(SELECT_MENU_PAD + 1.);
        assert_eq!(scroll.bounds().top(), bounds.top() + inset);
        assert_eq!(scroll.bounds().bottom(), bounds.bottom() - inset);
        assert_eq!(scroll.offset().y, -scroll.max_offset().y);
        let last = scroll.bounds_for_item(19).unwrap();
        assert_eq!(last.bottom() + scroll.offset().y, scroll.bounds().bottom());
        assert_eq!(last.size.height, px(SELECT_ROW_HEIGHT));

        view.update(cx, |view, cx| {
            view.selects.highlight(0);
            cx.notify();
        });
        draw(cx);
        let first = scroll.bounds_for_item(0).unwrap();
        assert!(first.top() + scroll.offset().y >= scroll.bounds().top());
        assert!(first.bottom() + scroll.offset().y <= scroll.bounds().bottom());
        assert_eq!(cx.debug_bounds("menu").unwrap(), bounds);

        let before_wheel = scroll.offset().y;
        cx.simulate_event(ScrollWheelEvent {
            position: bounds.center(),
            delta: ScrollDelta::Pixels(point(px(0.), px(-80.))),
            ..Default::default()
        });
        draw(cx);
        assert!(
            scroll.offset().y < before_wheel,
            "the wheel must scroll the menu"
        );
        assert_eq!(cx.debug_bounds("menu").unwrap(), bounds);
    }

    #[test]
    fn the_keyboard_walks_the_items_past_separators_and_stops_at_the_ends() {
        let items = [true, false, true, true];
        assert_eq!(step(&items, 0, true), 2);
        assert_eq!(step(&items, 2, false), 0);
        assert_eq!(step(&items, 3, true), 3);
        assert_eq!(step(&items, 0, false), 0);
    }
}
