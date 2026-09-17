use gpui::{Hsla, Pixels, px, rgb, rgba};
use markraft_core::BlockKind;

/// Editor presentation, independent of document semantics and host window policy.
#[derive(Clone, Debug)]
pub struct EditorStyle {
    pub padding: Pixels,
    pub body_size: Pixels,
    pub heading_sizes: [Pixels; 3],
    pub line_height_ratio: f32,
    pub paragraph_gap: Pixels,
    pub list_gap: Pixels,
    pub heading_top_gap: Pixels,
    pub heading_bottom_gap: Pixels,
    pub list_indent: Pixels,
    pub background: Hsla,
    pub text: Hsla,
    pub muted_text: Hsla,
    pub marker: Hsla,
    pub code_background: Hsla,
    pub code_radius: Pixels,
    pub draw_markers: bool,
    /// Height of host chrome drawn over the editor's bottom edge. Content gains this
    /// much extra bottom padding, the caret is revealed above it, and the scrollbar
    /// track ends before it.
    pub bottom_overlay: Pixels,
    pub scrollbar: Hsla,
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
            heading_top_gap: px(0.),
            heading_bottom_gap: px(10.),
            list_indent: px(28.),
            background: rgb(0xfcfbf8).into(),
            text: rgb(0x24282e).into(),
            muted_text: rgb(0x24282e).into(),
            marker: rgb(0x74766e).into(),
            code_background: rgb(0xedece7).into(),
            code_radius: px(0.),
            draw_markers: false,
            bottom_overlay: px(0.),
            scrollbar: rgba(0x00000047).into(),
        }
    }
}

impl EditorStyle {
    /// Compact, fixed-scale typography for a small floating note.
    pub fn notes() -> Self {
        Self {
            padding: px(24.),
            body_size: px(14.),
            heading_sizes: [px(24.), px(20.), px(17.)],
            line_height_ratio: 1.5,
            paragraph_gap: px(6.),
            list_gap: px(6.),
            heading_top_gap: px(14.),
            heading_bottom_gap: px(10.),
            list_indent: px(22.),
            background: rgb(0xefefef).into(),
            text: rgb(0x1c1d21).into(),
            muted_text: rgb(0x92959b).into(),
            marker: rgb(0x2f7cf6).into(),
            code_background: rgb(0xdfe0e3).into(),
            code_radius: px(3.),
            draw_markers: true,
            bottom_overlay: px(0.),
            scrollbar: rgba(0x00000047).into(),
        }
    }

    pub fn notes_dark() -> Self {
        Self {
            background: rgb(0x242528).into(),
            text: rgb(0xe6e7e9).into(),
            muted_text: rgb(0x93979e).into(),
            marker: rgb(0x4c9bff).into(),
            code_background: rgb(0x34363b).into(),
            scrollbar: rgba(0xffffff4d).into(),
            ..Self::notes()
        }
    }

    pub(crate) fn font_size(&self, kind: &BlockKind) -> Pixels {
        match kind {
            BlockKind::Heading(1) => self.heading_sizes[0],
            BlockKind::Heading(2) => self.heading_sizes[1],
            BlockKind::Heading(_) => self.heading_sizes[2],
            _ => self.body_size,
        }
    }
}
