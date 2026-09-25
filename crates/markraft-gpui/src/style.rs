use gpui::{Hsla, Pixels, SharedString, px, rgb, rgba};

/// The system UI face, which a note's text is set in unless the host picks
/// another.
const DEFAULT_FONT_FAMILY: &str = ".SystemUIFont";

/// Editor presentation, independent of document semantics and host window policy.
#[derive(Clone, Debug)]
pub struct EditorStyle {
    pub padding: Pixels,
    /// The widest the text column may be: a readable line length. Where the
    /// view is wider, the column — prose, code blocks, tables, the placeholder
    /// and everything aligned to the text — is this wide and centred in it,
    /// with [`EditorStyle::padding`] still kept outside. `None` lets the text
    /// fill the view.
    pub max_line_width: Option<Pixels>,
    /// The family the note's own text is set in: its paragraphs, headings,
    /// list and quote text, table cells and placeholder. Code keeps its
    /// monospaced face, and the editor's chrome — pills, labels — the system
    /// one.
    pub font_family: SharedString,
    pub body_size: Pixels,
    pub heading_sizes: [Pixels; 6],
    pub line_height_ratio: f32,
    pub paragraph_gap: Pixels,
    /// The row a table keeps above its grid for the host's table toolbar,
    /// focused or not, so the toolbar never covers the block above and the
    /// caret coming or going never moves what follows. The host that draws
    /// the toolbar says how tall it is; zero for one that draws none.
    pub table_toolbar_room: Pixels,
    pub list_gap: Pixels,
    /// Space above each of the six heading levels, except at the top.
    pub heading_top_gaps: [Pixels; 6],
    pub heading_bottom_gap: Pixels,
    pub list_indent: Pixels,
    pub background: Hsla,
    pub text: Hsla,
    pub muted_text: Hsla,
    pub marker: Hsla,
    pub link: Hsla,
    /// A wiki link whose target the host cannot open. It still reads as a link,
    /// because that is what it is, but not as one worth clicking.
    pub broken_link: Hsla,
    /// The selection fill, painted under the text: the first while the editor
    /// holds focus, the second while it does not. Both are translucent, so the
    /// text has to stay readable over what they composite to.
    pub selection: Hsla,
    pub selection_inactive: Hsla,
    pub code_background: Hsla,
    /// Inline code is a pill: its own, slightly stronger fill and quieter text.
    pub inline_code_background: Hsla,
    pub inline_code_text: Hsla,
    pub code_radius: Pixels,
    /// The fill behind highlighted text. Body text is drawn over it, so it
    /// answers to the body text's floor.
    pub highlight: Hsla,
    /// The fill behind a table's header row, which is its first row.
    ///
    /// A band rather than a tint of the text: the grid lines are drawn in
    /// [`EditorStyle::rule`] over it, so a light theme keeps the band close to
    /// the background and a dark one darkens it, which is the direction that
    /// keeps a 3:1 line against both.
    pub table_header_background: Hsla,
    /// Quote bars, horizontal rules and table grid lines.
    pub rule: Hsla,
    /// One accent per callout tone, in the order
    /// `callout::Tone` declares them: note, summary, success,
    /// caution, danger, example, quote. Each draws both the quote's own bar and
    /// its header label, so each answers to the text floor rather than the
    /// graphic one.
    pub callout_tones: [Hsla; 7],
    pub quote_indent: Pixels,
    /// Heights of host chrome drawn over the editor's top and bottom edges. Content
    /// gains this much extra padding, the caret is revealed clear of it, and the
    /// scrollbar track stays between the two.
    pub top_overlay: Pixels,
    pub bottom_overlay: Pixels,
    pub scrollbar: Hsla,
    /// Popups an extension anchors in the text, such as a typeahead menu.
    pub popup_background: Hsla,
    pub popup_border: Hsla,
    /// The row the keyboard is on. Stronger than `popup_hover`, so a pointer
    /// crossing the list never looks like it moved the selection.
    pub popup_selected: Hsla,
    /// The row the pointer is over.
    pub popup_hover: Hsla,
}

impl Default for EditorStyle {
    fn default() -> Self {
        Self {
            padding: px(32.),
            max_line_width: None,
            font_family: DEFAULT_FONT_FAMILY.into(),
            body_size: px(17.),
            heading_sizes: [px(30.), px(25.), px(21.), px(19.), px(18.), px(17.)],
            line_height_ratio: 1.5,
            paragraph_gap: px(10.),
            table_toolbar_room: px(28.),
            list_gap: px(10.),
            heading_top_gaps: [px(0.); 6],
            heading_bottom_gap: px(10.),
            list_indent: px(28.),
            background: rgb(0xfcfbf8).into(),
            text: rgb(0x24282e).into(),
            muted_text: rgb(0x6a6c64).into(),
            marker: rgb(0x74766e).into(),
            link: rgb(0x2a6fdb).into(),
            broken_link: rgb(0x8f9298).into(),
            selection: rgba(0xb9d5efb0).into(),
            selection_inactive: rgba(0xd4d9de90).into(),
            code_background: rgb(0xedece7).into(),
            inline_code_background: rgb(0xedece7).into(),
            inline_code_text: rgb(0x24282e).into(),
            code_radius: px(0.),
            highlight: rgb(0xf8e5a0).into(),
            table_header_background: rgb(0xf4f2ec).into(),
            rule: rgb(0xd9d7d0).into(),
            callout_tones: [
                rgb(0x1f63d6).into(),
                rgb(0x00696f).into(),
                rgb(0x1c7a3e).into(),
                rgb(0x8a5200).into(),
                rgb(0xb3261e).into(),
                rgb(0x6a3ab2).into(),
                rgb(0x55575c).into(),
            ],
            quote_indent: px(18.),
            top_overlay: px(0.),
            bottom_overlay: px(0.),
            scrollbar: rgba(0x00000047).into(),
            popup_background: rgb(0xfcfbf8).into(),
            popup_border: rgb(0xd9d7d0).into(),
            popup_selected: rgb(0xedece7).into(),
            popup_hover: rgb(0xf4f3ef).into(),
        }
    }
}

impl EditorStyle {
    /// Compact, fixed-scale typography for a small floating note.
    pub fn notes() -> Self {
        Self {
            padding: px(24.),
            max_line_width: None,
            font_family: DEFAULT_FONT_FAMILY.into(),
            body_size: px(14.),
            // Ratios of 2.25, 1.75, 1.5, 1.25, 1 and 1 em held down at the top,
            // so a floating note's title does not crowd its own window. Rather
            // than drawing the last two levels at the body size and telling H6
            // apart by colour, every level is a size step down from the one
            // above it, ending at the body size in bold.
            heading_sizes: [px(26.), px(21.), px(18.), px(16.), px(15.), px(14.)],
            line_height_ratio: 1.5,
            // Paragraphs part further than the items of a list, which are one
            // block's lines.
            paragraph_gap: px(10.),
            table_toolbar_room: px(28.),
            list_gap: px(6.),
            // A heading belongs to the text under it, so the space above it has
            // to beat the gap below it at every level.
            heading_top_gaps: [px(22.), px(20.), px(16.), px(14.), px(12.), px(12.)],
            heading_bottom_gap: px(8.),
            list_indent: px(22.),
            background: rgb(0xefefef).into(),
            text: rgb(0x1c1d21).into(),
            muted_text: rgb(0x686b71).into(),
            marker: rgb(0x1f63d6).into(),
            link: rgb(0x1f63d6).into(),
            broken_link: rgb(0x8b8e95).into(),
            selection: rgba(0xb9d5efb0).into(),
            selection_inactive: rgba(0xd4d9de90).into(),
            code_background: rgb(0xe3e3e4).into(),
            inline_code_background: rgb(0xdbdbdd).into(),
            inline_code_text: rgb(0x55575c).into(),
            code_radius: px(6.),
            highlight: rgb(0xf5df8e).into(),
            table_header_background: rgb(0xe6e6e7).into(),
            rule: rgb(0x86888d).into(),
            callout_tones: [
                rgb(0x1f63d6).into(),
                rgb(0x00696f).into(),
                rgb(0x1c7a3e).into(),
                rgb(0x8a5200).into(),
                rgb(0xb3261e).into(),
                rgb(0x6a3ab2).into(),
                rgb(0x55575c).into(),
            ],
            quote_indent: px(16.),
            top_overlay: px(0.),
            bottom_overlay: px(0.),
            scrollbar: rgba(0x00000047).into(),
            // The host's popover palette, so an in-editor popup matches its pickers.
            popup_background: rgb(0xf8f8f8).into(),
            popup_border: rgb(0xdcdcdc).into(),
            popup_selected: rgb(0xe2e2e2).into(),
            popup_hover: rgb(0xededed).into(),
        }
    }

    pub fn notes_dark() -> Self {
        Self {
            background: rgb(0x242528).into(),
            text: rgb(0xe6e7e9).into(),
            muted_text: rgb(0x93979e).into(),
            marker: rgb(0x4c9bff).into(),
            link: rgb(0x4c9bff).into(),
            broken_link: rgb(0x7f848c).into(),
            selection: rgba(0x3a6fb08c).into(),
            selection_inactive: rgba(0x5a5e6690).into(),
            code_background: rgb(0x34363b).into(),
            inline_code_background: rgb(0x3c3e44).into(),
            inline_code_text: rgb(0xb9bcc2).into(),
            highlight: rgb(0x5b4a17).into(),
            table_header_background: rgb(0x1d1e21).into(),
            rule: rgb(0x6e717a).into(),
            callout_tones: [
                rgb(0x4c9bff).into(),
                rgb(0x4fd1d9).into(),
                rgb(0x5ed17a).into(),
                rgb(0xe8b04b).into(),
                rgb(0xff7b72).into(),
                rgb(0xc6a0f6).into(),
                rgb(0x93979e).into(),
            ],
            scrollbar: rgba(0xffffff4d).into(),
            popup_background: rgb(0x2e2f33).into(),
            popup_border: rgb(0x383a40).into(),
            popup_selected: rgb(0x414246).into(),
            popup_hover: rgb(0x38393d).into(),
            ..Self::notes()
        }
    }

    /// The size a line's text is drawn at, from its heading level and whether it
    /// is code.
    pub(crate) fn font_size(&self, heading: Option<u8>, code: bool) -> Pixels {
        match (heading, code) {
            (Some(level), _) => self.heading_sizes[usize::from(level).clamp(1, 6) - 1],
            (None, true) => self.body_size - px(2.),
            (None, false) => self.body_size,
        }
    }

    /// The space above a heading of `level`, except at the top of the document.
    pub(crate) fn heading_top_gap(&self, level: u8) -> Pixels {
        self.heading_top_gaps[usize::from(level).clamp(1, 6) - 1]
    }

    /// The accent a callout of `tone` is drawn in.
    pub(crate) fn callout_tone(&self, tone: crate::callout::Tone) -> Hsla {
        self.callout_tones[tone.index()]
    }
}

#[cfg(test)]
mod tests {
    use super::EditorStyle;
    use gpui::{Hsla, Rgba};

    /// WCAG relative luminance of an opaque colour.
    fn luminance(color: Hsla) -> f32 {
        let channel = |c: f32| {
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        let rgb: Rgba = color.into();
        0.2126 * channel(rgb.r) + 0.7152 * channel(rgb.g) + 0.0722 * channel(rgb.b)
    }

    /// WCAG contrast ratio between two opaque colours.
    fn contrast(a: Hsla, b: Hsla) -> f32 {
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    /// What a translucent fill composites to over an opaque background, which is
    /// what the text drawn on top of it actually contrasts against.
    fn over(fill: Hsla, background: Hsla) -> Hsla {
        let (fill, background): (Rgba, Rgba) = (fill.into(), background.into());
        let blend = |f: f32, b: f32| f * fill.a + b * (1. - fill.a);
        Rgba {
            r: blend(fill.r, background.r),
            g: blend(fill.g, background.g),
            b: blend(fill.b, background.b),
            a: 1.,
        }
        .into()
    }

    fn assert_at_least(ratio: f32, floor: f32, what: &str) {
        assert!(ratio >= floor, "{what} is {ratio:.2}:1, below {floor}:1");
    }

    /// Every pairing a note draws text or a line in, so a palette edit cannot
    /// quietly drop one below its floor.
    fn assert_readable(style: &EditorStyle, theme: &str) {
        let body = style.text;
        for (what, color, background, floor) in [
            ("text", body, style.background, 4.5),
            ("muted text", style.muted_text, style.background, 4.5),
            ("marker", style.marker, style.background, 4.5),
            ("link", style.link, style.background, 4.5),
            ("code text", body, style.code_background, 4.5),
            (
                "inline code text",
                style.inline_code_text,
                style.inline_code_background,
                4.5,
            ),
            (
                "table header text",
                body,
                style.table_header_background,
                4.5,
            ),
            ("highlighted text", body, style.highlight, 4.5),
            // A rule is a graphic, not text, so it answers to the 3:1 floor.
            ("rule", style.rule, style.background, 3.0),
            (
                "callout note",
                style.callout_tones[0],
                style.background,
                4.5,
            ),
            (
                "callout summary",
                style.callout_tones[1],
                style.background,
                4.5,
            ),
            (
                "callout success",
                style.callout_tones[2],
                style.background,
                4.5,
            ),
            (
                "callout caution",
                style.callout_tones[3],
                style.background,
                4.5,
            ),
            (
                "callout danger",
                style.callout_tones[4],
                style.background,
                4.5,
            ),
            (
                "callout example",
                style.callout_tones[5],
                style.background,
                4.5,
            ),
            (
                "callout quote",
                style.callout_tones[6],
                style.background,
                4.5,
            ),
            (
                "text on the selection",
                body,
                over(style.selection, style.background),
                4.5,
            ),
            (
                "text on the inactive selection",
                body,
                over(style.selection_inactive, style.background),
                4.5,
            ),
        ] {
            assert_at_least(
                contrast(color, background),
                floor,
                &format!("{theme}: {what}"),
            );
        }
    }

    #[test]
    fn the_light_note_palette_is_readable() {
        assert_readable(&EditorStyle::notes(), "notes");
    }

    #[test]
    fn the_dark_note_palette_is_readable() {
        assert_readable(&EditorStyle::notes_dark(), "notes_dark");
    }

    /// A table's grid is drawn in `rule`, over the background and over the
    /// header band alike, so the band may not cost the lines their 3:1 floor.
    /// Dark is the theme where a lighter band would: its `rule` sits only just
    /// above the floor against the background, so the band goes the other way.
    #[test]
    fn table_grid_lines_stay_legible_over_the_header_band() {
        let style = EditorStyle::notes_dark();
        assert_at_least(
            contrast(style.rule, style.background),
            3.0,
            "notes_dark: grid lines on the background",
        );
        assert_at_least(
            contrast(style.rule, style.table_header_background),
            3.0,
            "notes_dark: grid lines on the header band",
        );
        assert_ne!(
            style.table_header_background, style.background,
            "the header band has to be visible as a band"
        );
    }

    #[test]
    fn a_note_heading_is_bound_to_the_text_under_it() {
        for style in [EditorStyle::notes(), EditorStyle::notes_dark()] {
            for level in 1..=6u8 {
                assert!(
                    style.heading_top_gap(level) > style.heading_bottom_gap,
                    "h{level} is not bound to its body"
                );
            }
            assert!(style.heading_sizes.windows(2).all(|pair| pair[0] > pair[1]));
            assert!(style.heading_sizes[5] >= style.body_size);
        }
    }

    /// The default preset's muted text once matched its body text, which left
    /// placeholders and list markers indistinguishable from content.
    #[test]
    fn the_default_palette_has_readable_muted_text() {
        let style = EditorStyle::default();
        assert_ne!(style.muted_text, style.text);
        assert_at_least(
            contrast(style.muted_text, style.background),
            4.5,
            "default: muted text",
        );
    }
}
