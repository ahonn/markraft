use crate::platform::symbols;
use gpui::{Hsla, IntoElement, RenderImage, Rgba, Styled, canvas, px};
use std::{cell::RefCell, collections::HashMap, sync::Arc};

macro_rules! define_icons {
    ($($variant:ident => $symbol:literal),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
        pub(super) enum Icon { $($variant),+ }

        impl Icon {
            fn symbol(self) -> &'static str {
                match self { $(Self::$variant => $symbol),+ }
            }

            #[cfg(test)]
            const ALL: &[Self] = &[$(Self::$variant),+];
        }
    };
}

// Semantic names stay independent of AppKit. Prefer symbols present on macOS 13,
// the minimum supported version; the native-render test checks the host catalog.
define_icons! {
    Plus => "plus",
    Notes => "doc.on.doc",
    Command => "command",
    Text => "textformat",
    Close => "xmark.circle.fill",
    ChevronDown => "chevron.down",
    Check => "checkmark",
    Pin => "pin",
    Trash => "trash",
    Copy => "doc.on.clipboard",
    Export => "square.and.arrow.up",
    Settings => "gearshape",
    Bold => "bold",
    Italic => "italic",
    Code => "chevron.left.forwardslash.chevron.right",
    Strikethrough => "strikethrough",
    Underline => "underline",
    Link => "link",
    Edit => "pencil",
    Open => "folder",
    Heading => "textformat.size",
    Quote => "text.quote",
    CodeBlock => "curlybraces.square",
    Paragraph => "paragraphsign",
    Ordered => "list.number",
    Bullet => "list.bullet",
    Task => "checklist",
    Divider => "minus",
    Restore => "arrow.uturn.backward",
    Table => "tablecells",
    RowAdd => "rectangle.stack.badge.plus",
    RowDelete => "rectangle.stack.badge.minus",
    ColumnAdd => "rectangle.badge.plus",
    ColumnDelete => "rectangle.badge.minus",
    AlignLeft => "text.alignleft",
    AlignCenter => "text.aligncenter",
    AlignRight => "text.alignright",
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
struct Key {
    icon: Icon,
    extent: u32,
    scale: u32,
    color: u32,
}

type SymbolCache = HashMap<Key, Option<Arc<RenderImage>>>;
thread_local! {
    // AppKit rendering stays on GPUI's UI thread; only pixel buffers enter GPUI.
    static CACHE: RefCell<SymbolCache> = RefCell::new(HashMap::new());
}

fn load(kind: Icon, color: Hsla, extent: f32, scale: f32) -> Option<Arc<RenderImage>> {
    let color = Rgba::from(color);
    let key = Key {
        icon: kind,
        extent: extent.to_bits(),
        scale: scale.to_bits(),
        color: color.into(),
    };
    CACHE.with_borrow_mut(|cache| {
        if let Some(image) = cache.get(&key) {
            return image.clone();
        }
        let image = symbols::render(kind.symbol(), extent, scale, color).or_else(|| {
            eprintln!("Markraft: could not render SF Symbol {}", kind.symbol());
            symbols::render("questionmark", extent, scale, color)
        });
        // Bound textures retained across repeated theme/display changes. Current
        // frame elements retain their Arcs when the cache is recycled.
        if cache.len() >= 512 {
            cache.clear();
        }
        cache.insert(key, image.clone());
        image
    })
}

pub(super) fn icon(kind: Icon, color: Hsla) -> impl IntoElement {
    sized_icon(kind, color, 16.)
}

pub(super) fn sized_icon(kind: Icon, color: Hsla, extent: f32) -> impl IntoElement {
    canvas(
        move |_, window, _| load(kind, color, extent, window.scale_factor()),
        |bounds, image, window, _| {
            if let Some(image) = image {
                let _ = window.paint_image(bounds, bounds, Default::default(), image, 0, false);
            }
        },
    )
    .size(px(extent))
    .flex_shrink_0()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::rgb;

    #[test]
    fn all_symbols_render_at_standard_and_retina_scales() {
        for &kind in Icon::ALL {
            for (extent, scale) in [(12., 1.), (16., 2.), (20., 2.)] {
                let image = symbols::render(kind.symbol(), extent, scale, rgb(0x333333))
                    .unwrap_or_else(|| panic!("missing SF Symbol: {}", kind.symbol()));
                let pixels = image.as_bytes(0).unwrap();
                let expected_side = (extent * scale) as i32;
                assert_eq!(image.size(0).width.0, expected_side);
                assert_eq!(image.size(0).height.0, expected_side);
                assert!(
                    pixels.chunks_exact(4).any(|p| p[3] > 0),
                    "empty {kind:?} at {extent} x {scale}"
                );
                assert!(pixels.chunks_exact(4).any(|p| p[3] == 0), "opaque {kind:?}");
            }
        }
    }

    #[test]
    fn command_symbol_keeps_all_four_loops() {
        let image = symbols::render("command", 20., 2., rgb(0x000000)).unwrap();
        let side = image.size(0).width.0 as usize;
        let mut quadrants = [0_u64; 4];
        for (index, pixel) in image.as_bytes(0).unwrap().chunks_exact(4).enumerate() {
            let quadrant =
                usize::from(index % side >= side / 2) + 2 * usize::from(index / side >= side / 2);
            quadrants[quadrant] += u64::from(pixel[3]);
        }
        // Cropping to NSImage.alignmentRect instead of its full image bounds can
        // clip the lower loops. This symmetric glyph must keep balanced ink.
        let min = *quadrants.iter().min().unwrap();
        let max = *quadrants.iter().max().unwrap();
        assert!(
            min > 0 && min * 100 >= max * 85,
            "unbalanced symbol: {quadrants:?}"
        );
    }

    #[test]
    fn tint_and_cache_follow_theme_and_display_scale() {
        let dark = load(Icon::Command, rgb(0xffffff).into(), 16., 2.).unwrap();
        let same = load(Icon::Command, rgb(0xffffff).into(), 16., 2.).unwrap();
        assert!(Arc::ptr_eq(&dark, &same));
        let light = load(Icon::Command, rgb(0x000000).into(), 16., 2.).unwrap();
        assert_ne!(dark.as_bytes(0), light.as_bytes(0));
        let standard = load(Icon::Command, rgb(0xffffff).into(), 16., 1.).unwrap();
        assert_ne!(dark.size(0), standard.size(0));
        // GPUI stores BGRA; use a non-neutral tint to catch swapped channels.
        let red = symbols::render("plus", 16., 2., rgb(0xff0000)).unwrap();
        assert!(
            red.as_bytes(0)
                .unwrap()
                .chunks_exact(4)
                .any(|p| p[2] > 200 && p[0] < 10 && p[3] > 200)
        );
    }
}
