use gpui::{Hsla, Pixels, px, rgb, rgba};

/// Editor presentation, independent of document semantics and host window policy.
#[derive(Clone, Debug)]
pub struct EditorStyle {
    pub padding: Pixels,
    pub body_size: Pixels,
    pub heading_sizes: [Pixels; 3],
    pub line_height_ratio: f32,
    pub paragraph_gap: Pixels,
    pub list_gap: Pixels,
    /// Space above a heading of level 1, 2, and 3 or deeper, except at the top.
    pub heading_top_gaps: [Pixels; 3],
    pub heading_bottom_gap: Pixels,
    pub list_indent: Pixels,
    pub background: Hsla,
    pub text: Hsla,
    pub muted_text: Hsla,
    pub marker: Hsla,
    pub link: Hsla,
    pub code_background: Hsla,
    /// Inline code is a pill: its own, slightly stronger fill and quieter text.
    pub inline_code_background: Hsla,
    pub inline_code_text: Hsla,
    pub code_radius: Pixels,
    /// Quote bars and horizontal rules.
    pub rule: Hsla,
    pub quote_indent: Pixels,
    pub draw_markers: bool,
    /// Heights of host chrome drawn over the editor's top and bottom edges. Content
    /// gains this much extra padding, the caret is revealed clear of it, and the
    /// scrollbar track stays between the two.
    pub top_overlay: Pixels,
    pub bottom_overlay: Pixels,
    pub scrollbar: Hsla,
    /// Popups an extension anchors in the text, such as a typeahead menu.
    pub popup_background: Hsla,
    pub popup_border: Hsla,
    pub popup_selected: Hsla,
}

impl Default for EditorStyle {
    fn default() -> Self {
        Self {
            padding: px(32.),
            body_size: px(17.),
            heading_sizes: [px(30.), px(25.), px(21.)],
            line_height_ratio: 1.5,
            paragraph_gap: px(10.),
            list_gap: px(10.),
            heading_top_gaps: [px(0.); 3],
            heading_bottom_gap: px(10.),
            list_indent: px(28.),
            background: rgb(0xfcfbf8).into(),
            text: rgb(0x24282e).into(),
            muted_text: rgb(0x24282e).into(),
            marker: rgb(0x74766e).into(),
            link: rgb(0x2a6fdb).into(),
            code_background: rgb(0xedece7).into(),
            inline_code_background: rgb(0xedece7).into(),
            inline_code_text: rgb(0x24282e).into(),
            code_radius: px(0.),
            rule: rgb(0xd9d7d0).into(),
            quote_indent: px(18.),
            draw_markers: false,
            top_overlay: px(0.),
            bottom_overlay: px(0.),
            scrollbar: rgba(0x00000047).into(),
            popup_background: rgb(0xfcfbf8).into(),
            popup_border: rgb(0xd9d7d0).into(),
            popup_selected: rgb(0xedece7).into(),
        }
    }
}

impl EditorStyle {
    /// Compact, fixed-scale typography for a small floating note.
    pub fn notes() -> Self {
        Self {
            padding: px(24.),
            body_size: px(14.),
            heading_sizes: [px(22.), px(18.), px(16.)],
            line_height_ratio: 1.5,
            paragraph_gap: px(7.),
            list_gap: px(7.),
            heading_top_gaps: [px(19.), px(8.), px(7.)],
            heading_bottom_gap: px(6.),
            list_indent: px(22.),
            background: rgb(0xefefef).into(),
            text: rgb(0x1c1d21).into(),
            muted_text: rgb(0x92959b).into(),
            marker: rgb(0x2f7cf6).into(),
            link: rgb(0x2f7cf6).into(),
            code_background: rgb(0xe3e3e4).into(),
            inline_code_background: rgb(0xdbdbdd).into(),
            inline_code_text: rgb(0x55575c).into(),
            code_radius: px(6.),
            rule: rgb(0xc4c5c9).into(),
            quote_indent: px(12.),
            draw_markers: true,
            top_overlay: px(0.),
            bottom_overlay: px(0.),
            scrollbar: rgba(0x00000047).into(),
            // The host's popover palette, so an in-editor popup matches its pickers.
            popup_background: rgb(0xf8f8f8).into(),
            popup_border: rgb(0xdcdcdc).into(),
            popup_selected: rgb(0xe2e2e2).into(),
        }
    }

    pub fn notes_dark() -> Self {
        Self {
            background: rgb(0x242528).into(),
            text: rgb(0xe6e7e9).into(),
            muted_text: rgb(0x93979e).into(),
            marker: rgb(0x4c9bff).into(),
            link: rgb(0x4c9bff).into(),
            code_background: rgb(0x34363b).into(),
            inline_code_background: rgb(0x3c3e44).into(),
            inline_code_text: rgb(0xb9bcc2).into(),
            rule: rgb(0x46484e).into(),
            scrollbar: rgba(0xffffff4d).into(),
            popup_background: rgb(0x2e2f33).into(),
            popup_border: rgb(0x383a40).into(),
            popup_selected: rgb(0x414246).into(),
            ..Self::notes()
        }
    }

    /// The size a line's text is drawn at, from its heading level and whether it
    /// is code.
    pub(crate) fn font_size(&self, heading: Option<u8>, code: bool) -> Pixels {
        match (heading, code) {
            (Some(1), _) => self.heading_sizes[0],
            (Some(2), _) => self.heading_sizes[1],
            (Some(_), _) => self.heading_sizes[2],
            (None, true) => self.body_size - px(2.),
            (None, false) => self.body_size,
        }
    }

    /// The space above a heading of `level`, except at the top of the document.
    pub(crate) fn heading_top_gap(&self, level: u8) -> Pixels {
        self.heading_top_gaps[usize::from(level).clamp(1, 3) - 1]
    }
}
