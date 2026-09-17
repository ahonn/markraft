use gpui::{Pixels, px};
use markraft_core::{Document, Span};
use std::{borrow::Cow, ops::Range};

pub(crate) fn text(text: &str, single_line: bool) -> Cow<'_, str> {
    if single_line && text.contains(['\r', '\n']) {
        Cow::Owned(text.replace("\r\n", "\n").replace(['\r', '\n'], " "))
    } else {
        Cow::Borrowed(text)
    }
}

pub(crate) fn document(document: Document) -> Document {
    let mut result = Document::default();
    result.blocks[0].spans.push(Span {
        text: text(&document.plain_text(), true).into_owned(),
        marks: Default::default(),
    });
    result.normalize();
    result
}

pub(crate) fn selected_range(source: &str, selected: Option<Range<usize>>) -> Option<Range<usize>> {
    let offset = |requested| {
        let mut units = 0;
        let mut bytes = 0;
        for character in source.chars() {
            if units + character.len_utf16() > requested {
                break;
            }
            units += character.len_utf16();
            bytes += character.len_utf8();
        }
        text(&source[..bytes], true).encode_utf16().count()
    };
    selected.map(|range| offset(range.start)..offset(range.end))
}

/// The field paints in a fixed viewport. Keeping its own offset avoids relying
/// on a parent's flex/block layout to give the text a scrollable layout width.
pub(crate) fn scroll_offset(
    previous: Pixels,
    caret: Pixels,
    content: Pixels,
    viewport: Pixels,
) -> Pixels {
    let viewport = viewport.max(px(1.));
    let margin = px(12.).min(viewport / 4.);
    let caret_width = px(2.);
    let maximum = (content + margin + caret_width - viewport).max(px(0.));
    let mut offset = previous.max(px(0.)).min(maximum);
    if caret < offset + margin {
        offset = caret - margin;
    } else if caret + caret_width > offset + viewport - margin {
        offset = caret + caret_width + margin - viewport;
    }
    offset.max(px(0.)).min(maximum)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn single_line_preserves_literal_markers_and_maps_composition_offsets() {
        assert_eq!(text("# a\r\n[] b\rc\nd", true), "# a [] b c d");
        assert_eq!(text("# a\n", false), "# a\n");
        assert_eq!(selected_range("😀\r\n中", Some(4..5)), Some(3..4));
        let flattened = document(Document::from_markdown("# Title\n**bold**"));
        assert_eq!(flattened.plain_text(), "Title bold");
        assert_eq!(flattened.blocks.len(), 1);
        assert_eq!(flattened.blocks[0].kind, Default::default());
        assert!(
            flattened.blocks[0]
                .spans
                .iter()
                .all(|span| span.marks == Default::default())
        );
    }

    #[test]
    fn horizontal_offset_reveals_both_ends_and_recovers_after_resize_or_deletion() {
        let end = scroll_offset(px(0.), px(680.), px(680.), px(432.));
        assert!(end > px(0.));
        assert!(px(680.) - end + px(2.) <= px(432. - 12.));
        assert_eq!(scroll_offset(end, px(0.), px(680.), px(432.)), px(0.));
        assert_eq!(scroll_offset(px(0.), px(680.), px(680.), px(432.)), end);
        let narrow = scroll_offset(end, px(680.), px(680.), px(240.));
        assert!(narrow > end);
        assert_eq!(scroll_offset(narrow, px(20.), px(20.), px(240.)), px(0.));
        assert_eq!(scroll_offset(end, px(680.), px(680.), px(800.)), px(0.));
        assert_eq!(scroll_offset(px(100.), px(0.), px(0.), px(0.)), px(0.));
    }
}
