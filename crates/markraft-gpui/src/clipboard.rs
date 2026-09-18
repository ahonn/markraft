use gpui::{App, ClipboardItem};
use markraft_core::Document;

#[derive(Clone, Copy)]
pub(crate) enum PasteMode {
    Formatted,
    Plain,
    Markdown,
}

const METADATA_PREFIX: &str = "markraft-fragment-v1:";

pub(crate) fn write(fragment: Document, text: String, cx: &mut App) {
    let metadata = format!(
        "{METADATA_PREFIX}{}",
        fragment.to_json().unwrap_or_default()
    );
    cx.write_to_clipboard(ClipboardItem::new_string_with_metadata(text, metadata));
    platform::write_html(&fragment.to_html());
}

pub(crate) fn read_fragment(item: &ClipboardItem, mode: PasteMode) -> Option<Document> {
    let text = item.text();
    if matches!(mode, PasteMode::Markdown) {
        return text.map(|text| Document::from_markdown_fragment(&text));
    }
    if let Some(fragment) = item
        .metadata()
        .and_then(|metadata| metadata.strip_prefix(METADATA_PREFIX))
        .and_then(|json| Document::from_json(json).ok())
    {
        return Some(fragment);
    }
    if let Some(html) = platform::read_html() {
        let document = Document::from_html(&html);
        if !document.plain_text().is_empty() {
            return Some(document);
        }
    }
    text.map(|text| Document::from_markdown_fragment(&text))
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
