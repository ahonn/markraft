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
// the minimum supported version; the native-render test only proves this Mac has
// them. The version a symbol arrived in is in the system's own catalogue:
// `plutil -p /System/Library/CoreServices/CoreGlyphs.bundle/Contents/Resources/\
//  name_availability.plist` maps each name to a release year, and `year_to_release`
// in the same file maps that year to a macOS version.
define_icons! {
    Plus => "plus",
    // Daily notes: today's, and the days either side.
    Calendar => "calendar",
    PreviousDay => "chevron.backward",
    NextDay => "chevron.forward",
    // vim's `:u`, `:red`, `:q` and `:qa`, offered to a `:` query.
    Undo => "arrow.uturn.backward",
    Redo => "arrow.uturn.forward",
    Hide => "eye.slash",
    Quit => "power",
    // Browse notes (⌘P): quick-open weight, same family as Plus / More.
    Notes => "magnifyingglass",
    // Actions (⌘K): macOS “more” rather than the ⌘ key glyph.
    Command => "ellipsis.circle",
    // Format toolbar: brush when closed; circled x when open (no paintbrush.circle in SF Symbols).
    Text => "paintbrush",
    Close => "xmark.circle",
    ChevronDown => "chevron.down",
    // Clearing a field: the shortcut recorder's chord.
    ClearField => "xmark.circle.fill",
    // A pop-up button's pair of chevrons.
    UpDown => "chevron.up.chevron.down",
    Check => "checkmark",
    Pin => "pin",
    Trash => "trash",
    // Taking a link off its text, which stays: erased, not thrown away.
    Unlink => "eraser",
    Copy => "doc.on.clipboard",
    Export => "square.and.arrow.up",
    Print => "printer",
    Send => "paperplane",
    Document => "doc.text",
    Settings => "gearshape",
    // The Settings pages for the editor and for the syntax it writes: typing, and the
    // plain text a note is kept as.
    Typing => "character.cursor.ibeam",
    Markdown => "doc.plaintext",
    About => "info.circle",
    Bold => "bold",
    Italic => "italic",
    Code => "chevron.left.forwardslash.chevron.right",
    Strikethrough => "strikethrough",
    Underline => "underline",
    Link => "link",
    Edit => "pencil",
    Open => "folder",
    // Leaving Markraft: a link in the browser, a file in another editor.
    External => "arrow.up.right.square",
    Lock => "lock",
    Alert => "exclamationmark.triangle",
    // Two versions of one file that have to be told apart.
    Conflict => "arrow.triangle.branch",
    Save => "arrow.down.doc",
    Reset => "arrow.counterclockwise",
    Count => "number",
    Heading => "textformat.size",
    Quote => "text.quote",
    CodeBlock => "curlybraces.square",
    Paragraph => "paragraphsign",
    Ordered => "list.number",
    Bullet => "list.bullet",
    Task => "checklist",
    Divider => "minus",
    Table => "tablecells",
    RowAbove => "arrow.up.to.line",
    RowBelow => "arrow.down.to.line",
    ColumnLeft => "arrow.left.to.line",
    ColumnRight => "arrow.right.to.line",
    // Taking one away is said by the destructive ink; the split says which one.
    RowDelete => "square.split.1x2",
    ColumnDelete => "square.split.2x1",
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
            log::warn!("could not render SF Symbol {}", kind.symbol());
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
#[cfg_attr(coverage_nightly, coverage(off))]
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
