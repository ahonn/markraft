//! Restore a Settings window using display-local coordinates on macOS.
use super::{HEIGHT, WIDTH};
use crate::storage::SettingsWindowPlacement;
use gpui::{Bounds, DisplayId, Pixels, PlatformDisplay, Point, Size, point, px, size};
use std::rc::Rc;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct SettingsDisplay {
    pub id: DisplayId,
    pub uuid: Option<String>,
    pub visible: Bounds<Pixels>,
}

impl SettingsDisplay {
    pub fn from_display(display: &Rc<dyn PlatformDisplay>) -> Self {
        Self {
            id: display.id(),
            uuid: display.uuid().ok().map(|uuid| uuid.to_string()),
            visible: display.visible_bounds(),
        }
    }
}

/// Display IDs can change between launches; saved coordinates belong to the UUID.
pub(super) fn select_display<'a>(
    displays: &'a [SettingsDisplay],
    saved: Option<&SettingsWindowPlacement>,
    fallback: Option<DisplayId>,
) -> Option<&'a SettingsDisplay> {
    saved
        .and_then(|saved| saved.display_uuid.as_ref())
        .and_then(|uuid| {
            displays
                .iter()
                .find(|display| display.uuid.as_ref() == Some(uuid))
        })
        .or_else(|| displays.iter().find(|display| Some(display.id) == fallback))
        .or_else(|| displays.first())
}

pub(super) fn initial_bounds(
    display: Option<&SettingsDisplay>,
    saved: Option<&SettingsWindowPlacement>,
    near: Bounds<Pixels>,
) -> Bounds<Pixels> {
    let saved = saved.filter(|saved| saved.is_valid());
    let extent = size(px(WIDTH), px(saved.map_or(HEIGHT, |saved| saved.height)));
    let Some(display) = display else {
        return Bounds::new(Point::default(), extent);
    };
    let screen = display.visible;
    let extent = size(extent.width, extent.height.min(screen.size.height));
    let origin = match saved {
        Some(saved) => constrain(
            point(px(saved.origin[0]), px(saved.origin[1])),
            screen,
            extent,
        ),
        None => beside_note(screen, near, extent),
    };
    Bounds::new(origin, extent)
}

fn constrain(origin: Point<Pixels>, screen: Bounds<Pixels>, extent: Size<Pixels>) -> Point<Pixels> {
    point(
        origin
            .x
            .min(screen.right() - extent.width)
            .max(screen.left()),
        origin
            .y
            .min(screen.bottom() - extent.height)
            .max(screen.top()),
    )
}

/// Open beside the note so a floating note does not cover Settings; otherwise center it.
fn beside_note(
    screen: Bounds<Pixels>,
    near: Bounds<Pixels>,
    extent: Size<Pixels>,
) -> Point<Pixels> {
    let margin = px(16.);
    for x in [near.right() + margin, near.left() - margin - extent.width] {
        if x >= screen.left() && x + extent.width <= screen.right() {
            return constrain(point(x, near.top()), screen, extent);
        }
    }
    constrain(screen.center() - (extent / 2.).into(), screen, extent)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(id: u64, uuid: &str, visible: [f32; 4]) -> SettingsDisplay {
        SettingsDisplay {
            id: DisplayId::new(id),
            uuid: Some(uuid.into()),
            visible: Bounds::new(
                point(px(visible[0]), px(visible[1])),
                size(px(visible[2]), px(visible[3])),
            ),
        }
    }

    fn note(x: f32, y: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(480.), px(320.)))
    }

    fn saved(uuid: &str, origin: [f32; 2], height: f32) -> SettingsWindowPlacement {
        SettingsWindowPlacement {
            origin,
            height,
            display_uuid: Some(uuid.into()),
        }
    }

    #[test]
    fn saved_display_uses_uuid_and_missing_display_falls_back_to_the_note() {
        let displays = [
            screen(20, "primary", [0., 25., 1440., 875.]),
            screen(99, "external", [80., 25., 1840., 1055.]),
        ];
        let saved = saved("external", [300., 150.], 280.);
        assert_eq!(
            select_display(&displays, Some(&saved), Some(DisplayId::new(20)))
                .unwrap()
                .id,
            DisplayId::new(99)
        );
        assert_eq!(
            select_display(&displays[..1], Some(&saved), Some(DisplayId::new(20)))
                .unwrap()
                .id,
            DisplayId::new(20)
        );
    }

    #[test]
    fn invalid_saved_geometry_falls_back_to_initial_placement() {
        let display = screen(1, "primary", [0., 25., 1440., 875.]);
        let near = note(100., 200.);
        let expected = initial_bounds(Some(&display), None, near);
        for saved in [
            saved("primary", [100., 200.], -1.),
            saved("primary", [100., 200.], 0.),
            saved("primary", [f32::NAN, 200.], 300.),
        ] {
            assert_eq!(initial_bounds(Some(&display), Some(&saved), near), expected);
        }
    }

    #[test]
    fn restored_short_page_keeps_its_position_near_the_bottom() {
        let display = screen(1, "primary", [0., 25., 1440., 875.]);
        let saved = saved("primary", [300., 620.], 260.);
        let bounds = initial_bounds(Some(&display), Some(&saved), note(0., 0.));
        assert_eq!(bounds.origin, point(px(300.), px(620.)));
        assert_eq!(bounds.size, size(px(WIDTH), px(260.)));
    }

    #[test]
    fn restoration_clamps_to_visible_bounds_after_a_display_disconnects() {
        let display = screen(1, "primary", [80., 30., 1200., 720.]);
        let distant = saved("disconnected", [1800., 900.], 900.);
        let bounds = initial_bounds(Some(&display), Some(&distant), note(0., 0.));
        assert_eq!(bounds.origin, point(px(760.), px(30.)));
        assert_eq!(bounds.size.height, px(720.));
        let saved = saved("primary", [-500., -200.], 300.);
        let bounds = initial_bounds(Some(&display), Some(&saved), note(0., 0.));
        assert_eq!(bounds.origin, point(px(80.), px(30.)));
    }

    #[test]
    fn initial_placement_uses_both_sides_and_avoids_the_dock_and_menu_bar() {
        let display = screen(1, "primary", [80., 25., 1360., 810.]);
        assert_eq!(
            initial_bounds(Some(&display), None, note(100., 200.)).origin,
            point(px(596.), px(200.))
        );
        assert_eq!(
            initial_bounds(Some(&display), None, note(900., 200.)).origin,
            point(px(364.), px(200.))
        );
        assert_eq!(
            initial_bounds(Some(&display), None, note(100., 0.)).top(),
            px(25.)
        );
        assert_eq!(
            initial_bounds(Some(&display), None, note(100., 800.)).bottom(),
            display.visible.bottom()
        );
        let wide = Bounds::new(point(px(300.), px(200.)), size(px(900.), px(320.)));
        assert_eq!(
            initial_bounds(Some(&display), None, wide).origin,
            point(px(500.), px(220.))
        );
    }
}
