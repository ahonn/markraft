//! The system clipboard, in three flavours.
//!
//! A copy writes the selected [`Slice`] three times: as HTML, which is what a
//! word processor takes; as the document kind's own markup, which is the item's
//! text and what every plain-text reader sees; and as JSON in the item's
//! metadata, which is what another Markraft window reads back so that a round
//! trip is exact. A paste prefers that JSON, then the HTML flavour another
//! application left, then the text read as markup.
//!
//! Only the JSON is this crate's own: it needs the schema and nothing else. The
//! other two flavours come from the host's [`Codecs`], so the view never learns
//! which document kind it is editing.

use gpui::{App, ClipboardItem};
use markraft_core::{Codecs, Schema, Slice};

#[derive(Clone, Copy)]
pub(crate) enum PasteMode {
    Formatted,
    Plain,
    Markdown,
}

const METADATA_PREFIX: &str = "markraft-fragment-v1:";

/// The string flavour of `slice`: the kind's markup, or its plain text when the
/// kind has no markup of its own.
pub(crate) fn markup(codecs: &dyn Codecs, slice: &Slice) -> String {
    codecs
        .to_markup(slice)
        .unwrap_or_else(|| codecs.to_text(slice))
}

pub(crate) fn write(schema: &Schema, codecs: &dyn Codecs, slice: &Slice, cx: &mut App) {
    let metadata = format!("{METADATA_PREFIX}{}", slice.to_json(schema));
    cx.write_to_clipboard(ClipboardItem::new_string_with_metadata(
        markup(codecs, slice),
        metadata,
    ));
    // The rich flavour goes on the same item, after it has been written.
    if let Some(html) = codecs.to_html(slice) {
        platform::write_html(&html);
    }
}

/// The slice `item` holds, read according to `mode`.
///
/// `None` means there is nothing this editor can paste as structure; the caller
/// falls back to inserting the item's text literally.
pub(crate) fn read_fragment(
    schema: &Schema,
    codecs: &dyn Codecs,
    item: &ClipboardItem,
    mode: PasteMode,
) -> Option<Slice> {
    let text = item.text();
    if matches!(mode, PasteMode::Markdown) {
        return text.and_then(|text| codecs.from_markup(&text));
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
        && let Some(slice) = codecs.from_html(&html)
    {
        return Some(slice);
    }
    text.and_then(|text| codecs.from_markup(&text))
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
    use markraft_commonmark::{CommonMarkCodecs, commonmark_schema};

    #[test]
    fn a_slice_survives_the_metadata_round_trip() {
        let schema = commonmark_schema();
        let codecs = CommonMarkCodecs::new(schema.clone());
        let slice = codecs
            .from_markup("**bold** and `code`")
            .expect("a fragment");
        let json = slice.to_json(&schema);
        let back = Slice::from_json(&schema, &json).expect("the same slice");
        assert_eq!(back, slice);
        assert_eq!(markup(&codecs, &slice), "**bold** and `code`");
    }

    #[test]
    fn the_rich_flavour_is_html_another_application_can_read() {
        let schema = commonmark_schema();
        let codecs = CommonMarkCodecs::new(schema.clone());
        let slice = codecs
            .from_markup("**bold** and `code`")
            .expect("a fragment");
        let html = codecs.to_html(&slice).expect("an HTML flavour");
        assert_eq!(html, "<p><strong>bold</strong> and <code>code</code></p>");
        // And it is what the paste path reads when there is no metadata.
        assert_eq!(codecs.from_html(&html).as_ref(), Some(&slice));
    }
}
