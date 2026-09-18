//! The system clipboard, in three flavours.
//!
//! A copy writes the selected [`Slice`] three times: as HTML, which is what a
//! word processor takes; as Markdown, which is the item's text and what every
//! plain-text reader sees; and as JSON in the item's metadata, which is what
//! another Markraft window reads back so that a round trip is exact. A paste
//! prefers that JSON, then the HTML flavour another application left, then the
//! text read as Markdown.

use gpui::{App, ClipboardItem};
use markraft_doc::{Schema, Slice};
use markraft_markdown::{HtmlParser, HtmlSerializer, MarkdownParser, commonmark_serializer};

#[derive(Clone, Copy)]
pub(crate) enum PasteMode {
    Formatted,
    Plain,
    Markdown,
}

const METADATA_PREFIX: &str = "markraft-fragment-v1:";

/// The Markdown flavour of `slice`, which is also what a plain paste inserts.
pub(crate) fn markdown(schema: &Schema, slice: &Slice) -> String {
    commonmark_serializer(schema).serialize_fragment(slice)
}

pub(crate) fn write(schema: &Schema, slice: &Slice, cx: &mut App) {
    let metadata = format!("{METADATA_PREFIX}{}", slice.to_json(schema));
    cx.write_to_clipboard(ClipboardItem::new_string_with_metadata(
        markdown(schema, slice),
        metadata,
    ));
    // The rich flavour goes on the same item, after it has been written.
    platform::write_html(&HtmlSerializer::commonmark(schema).serialize_fragment(slice));
}

/// The slice `item` holds, read according to `mode`.
///
/// `None` means there is nothing this editor can paste as structure; the caller
/// falls back to inserting the item's text literally.
pub(crate) fn read_fragment(
    schema: &Schema,
    item: &ClipboardItem,
    mode: PasteMode,
) -> Option<Slice> {
    let text = item.text();
    if matches!(mode, PasteMode::Markdown) {
        return text.and_then(|text| parse_markdown(schema, &text));
    }
    if let Some(slice) = item
        .metadata()
        .and_then(|metadata| metadata.strip_prefix(METADATA_PREFIX).map(str::to_owned))
        .and_then(|json| serde_json::from_str(&json).ok())
        .and_then(|value| Slice::from_json(schema, &value).ok())
    {
        return Some(slice);
    }
    if let Some(html) = platform::read_html()
        && let Ok(slice) = HtmlParser::commonmark(schema.clone()).parse_fragment(&html)
        && !slice.is_empty()
    {
        return Some(slice);
    }
    text.and_then(|text| parse_markdown(schema, &text))
}

fn parse_markdown(schema: &Schema, source: &str) -> Option<Slice> {
    MarkdownParser::commonmark(schema.clone())
        .parse_fragment(source)
        .ok()
        .filter(|slice| !slice.is_empty())
}

#[cfg(target_os = "macos")]
mod platform {
    use objc2_app_kit::{NSPasteboard, NSPasteboardTypeHTML};
    use objc2_foundation::{NSArray, NSString};

    pub(super) fn read_html() -> Option<String> {
        let pasteboard = NSPasteboard::generalPasteboard();
        // AppKit's immutable HTML type is process-global; no owner is retained.
        pasteboard
            .stringForType(unsafe { NSPasteboardTypeHTML })
            .map(|value| value.to_string())
    }

    pub(super) fn write_html(html: &str) {
        let pasteboard = NSPasteboard::generalPasteboard();
        // Add the HTML representation to GPUI's existing text + metadata item.
        // A nil owner is valid because the data is supplied synchronously below.
        unsafe {
            pasteboard.addTypes_owner(&NSArray::from_slice(&[NSPasteboardTypeHTML]), None);
            pasteboard.setString_forType(&NSString::from_str(html), NSPasteboardTypeHTML);
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    pub(super) fn read_html() -> Option<String> {
        None
    }

    pub(super) fn write_html(_: &str) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use markraft_markdown::commonmark_schema;

    #[test]
    fn a_slice_survives_the_metadata_round_trip() {
        let schema = commonmark_schema();
        let slice = MarkdownParser::commonmark(schema.clone())
            .parse_fragment("**bold** and `code`")
            .expect("a fragment");
        let json = slice.to_json(&schema);
        let back = Slice::from_json(&schema, &json).expect("the same slice");
        assert_eq!(back, slice);
        assert_eq!(markdown(&schema, &slice), "**bold** and `code`");
    }

    #[test]
    fn the_rich_flavour_is_html_another_application_can_read() {
        let schema = commonmark_schema();
        let slice = MarkdownParser::commonmark(schema.clone())
            .parse_fragment("**bold** and `code`")
            .expect("a fragment");
        let html = HtmlSerializer::commonmark(&schema).serialize_fragment(&slice);
        assert_eq!(html, "<p><strong>bold</strong> and <code>code</code></p>");
        // And it is what the paste path reads when there is no metadata.
        let back = HtmlParser::commonmark(schema.clone())
            .parse_fragment(&html)
            .expect("the HTML reparses");
        assert_eq!(back, slice);
    }
}
