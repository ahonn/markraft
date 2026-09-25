//! The system clipboard, in three flavours.
//!
//! A copy writes the selected [`Slice`] three times: as HTML, which is what a
//! word processor takes; as the document kind's own markup, which is the item's
//! text and what every plain-text reader sees; and as JSON in the item's
//! metadata, which is what another Markraft window reads back so that a round
//! trip is exact. The JSON holds what the kind says a copy carries
//! ([`Codecs::copied`]), which for a kind that spells its marks in the text is
//! the selection with the spelling it needs to keep them. A paste prefers that
//! JSON, then the HTML flavour another application left, then the text read as
//! markup. A plain paste reads the text as [`Codecs::from_text`] does.
//!
//! Only the JSON is this crate's own: it needs the schema and nothing else. The
//! other two flavours come from the host's [`Codecs`], so the view never learns
//! which document kind it is editing.

use crate::types::DocTypes;
use gpui::{App, ClipboardItem};
use markraft_core::{EditorState, Fragment, Schema, Selection, Slice, kind::Codecs};

/// A selection from the start of a list item's text into a later item of the
/// same list, as whole items in their list, as Typora copies it: pasted, it is
/// the list it was, where the open slice the selection spells would make its
/// first item a paragraph. `None` for every other selection.
pub(crate) fn whole_items(state: &EditorState, types: &DocTypes) -> Option<Slice> {
    let doc = state.doc();
    let schema = state.schema();
    let selection = state.selection();
    if !matches!(selection, Selection::Text { .. }) {
        return None;
    }
    let (from, to) = (selection.from(doc), selection.to(doc));
    let start = doc.resolve(from).ok()?;
    let text = (1..=start.depth())
        .rev()
        .find(|&depth| start.node(depth).is_textblock(schema))?;
    // Inline containers the caret sits in open no text of their own.
    let hidden = start.depth() - text;
    if text < 2 || start.pos().checked_sub(hidden) != Some(start.start(text)) {
        return None;
    }
    let item = text - 1;
    let list = item - 1;
    if !types.is_item(start.node(item).type_id())
        || !types.is_list(start.node(list).type_id())
        || start.index(item) != 0
        || to <= start.after(item)
        || to >= start.end(list)
    {
        return None;
    }
    let items = doc.slice(start.before(item), to).ok()?;
    Some(Slice::new(
        Fragment::from_node(start.node(list).copy(items.content().clone())),
        0,
        items.open_end() + 1,
    ))
}

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
    let metadata = metadata(schema, codecs, slice);
    cx.write_to_clipboard(ClipboardItem::new_string_with_metadata(
        markup(codecs, slice),
        metadata,
    ));
    // The rich flavour goes on the same item, after it has been written.
    if let Some(html) = codecs.to_html(slice) {
        platform::write_html(&html);
    }
}

/// The metadata flavour of `slice`: what the kind says a copy of it carries.
fn metadata(schema: &Schema, codecs: &dyn Codecs, slice: &Slice) -> String {
    format!("{METADATA_PREFIX}{}", codecs.copied(slice).to_json(schema))
}

/// The slice a metadata flavour holds.
fn from_metadata(schema: &Schema, metadata: &str) -> Option<Slice> {
    let json = metadata.strip_prefix(METADATA_PREFIX)?;
    let value = serde_json::from_str(json).ok()?;
    Slice::from_json(schema, &value).ok()
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
        .and_then(|metadata| from_metadata(schema, metadata))
    {
        return Some(slice);
    }
    if let Some(html) = platform::read_html(text.as_deref())
        && let Some(slice) = codecs.from_html(&html)
    {
        return Some(slice);
    }
    text.and_then(|text| codecs.from_markup(&text))
}

#[cfg(target_os = "macos")]
mod platform {
    use objc2_app_kit::{NSPasteboard, NSPasteboardTypeHTML, NSPasteboardTypeString};
    use objc2_foundation::{NSArray, NSString};

    /// The HTML flavour on the pasteboard, when it belongs to the item being
    /// pasted: the pasteboard's plain text is the item's `text`. An item that
    /// did not come from the pasteboard — a test's, one a host built — is not
    /// read through HTML someone else put there.
    pub(super) fn read_html(text: Option<&str>) -> Option<String> {
        let pasteboard = NSPasteboard::generalPasteboard();
        // AppKit's immutable type names are process-global; no owner is retained.
        let plain = pasteboard
            .stringForType(unsafe { NSPasteboardTypeString })
            .map(|value| value.to_string());
        if plain.as_deref() != text {
            return None;
        }
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
    pub(super) fn read_html(_: Option<&str>) -> Option<String> {
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
        let codecs = CommonMarkCodecs::new(schema.clone(), Default::default());
        let slice = codecs
            .from_markup("**bold** and `code`")
            .expect("a fragment");
        let json = slice.to_json(&schema);
        let back = Slice::from_json(&schema, &json).expect("the same slice");
        assert_eq!(back, slice);
        assert_eq!(markup(&codecs, &slice), "**bold** and `code`");
    }

    /// Copying the inside of a styled span and pasting it in the same kind
    /// keeps the style: the copy carries the delimiters the selection left out.
    #[test]
    fn a_copied_span_pastes_with_its_style() {
        use markraft_core::commands::{replace_selection, run_command};
        let state = crate::typeahead::tests::state_of("x **bold** y\n\nz");
        let schema = state.schema().clone();
        let codecs = CommonMarkCodecs::new(schema.clone(), Default::default());
        // `bold`, without the `**` either side.
        let (from, to) = (1 + 4, 1 + 8);
        let slice = state.doc().slice(from, to).expect("a slice");
        assert_eq!(
            markup(&codecs, &slice),
            "bold",
            "Markdown is the text verbatim"
        );
        let pasted = from_metadata(&schema, &metadata(&schema, &codecs, &slice)).expect("JSON");
        let end = state.doc().content_size() - 1;
        let at_end = state
            .update([markraft_core::TransactionSpec::new()
                .selection(markraft_core::Selection::cursor(end))])
            .unwrap()
            .state()
            .clone();
        let after = run_command(&at_end, &replace_selection(pasted))
            .expect("the paste runs")
            .expect("the paste applies")
            .state()
            .clone();
        assert_eq!(
            markraft_commonmark::to_markdown(&schema, after.doc()),
            "x **bold** y\n\nz**bold**"
        );
        // A span copied whole, delimiters and all, is carried as it is.
        let whole = state.doc().slice(1 + 2, 1 + 10).expect("a slice");
        assert_eq!(codecs.copied(&whole), whole);
    }

    /// Items copied from the start of one into another come as the list they
    /// were in, and paste as that list; a selection that starts inside an
    /// item's text, or stays in one item, copies as it always has.
    #[test]
    fn items_copied_from_an_items_start_paste_as_a_list() {
        use markraft_core::commands::{replace_selection, run_command};
        let state = crate::typeahead::tests::state_of("x\n\n- a\n- bc\n- d");
        let schema = state.schema().clone();
        let types = crate::typeahead::tests::types_of(&state);
        let codecs = CommonMarkCodecs::new(schema.clone(), Default::default());
        let select = |from: usize, to: usize| {
            state
                .update(
                    [markraft_core::TransactionSpec::new().selection(Selection::text(from, to))],
                )
                .unwrap()
                .state()
                .clone()
        };
        let text = markraft_core::projection::projection_of(&state);
        let (a, b) = (text.lines()[1].from(), text.lines()[2].from());
        let selected = select(a, b + 1);
        let slice = whole_items(&selected, &types).expect("whole items");
        assert_eq!(markup(&codecs, &slice), "- a\n- b");
        let at_x = crate::typeahead::tests::at(&state, 2);
        let pasted = run_command(&at_x, &replace_selection(slice))
            .expect("the paste runs")
            .expect("the paste applies")
            .state()
            .clone();
        // Pasted after `x`, the items join the list that follows, as two
        // neighbouring lists of one kind read in Markdown.
        assert_eq!(
            markraft_commonmark::to_markdown(&schema, pasted.doc()),
            "x\n\n- a\n- b\n- a\n- bc\n- d"
        );
        assert_eq!(
            whole_items(&select(a + 1, b + 1), &types),
            None,
            "inside the text"
        );
        assert_eq!(whole_items(&select(a, a + 1), &types), None, "one item");
    }

    /// A plain paste keeps every character literal.
    #[test]
    fn plain_text_stays_the_characters_it_is() {
        let codecs = CommonMarkCodecs::new(commonmark_schema(), Default::default());
        let slice = codecs.from_text("*a* _b_\n# c");
        assert_eq!(codecs.to_text(&slice), "*a* _b_\n# c");
        assert_eq!(
            codecs.to_markup(&slice).as_deref(),
            Some("\\*a\\* \\_b\\_\n\n\\# c")
        );
    }

    #[test]
    fn the_rich_flavour_is_html_another_application_can_read() {
        let schema = commonmark_schema();
        let codecs = CommonMarkCodecs::new(schema.clone(), Default::default());
        let slice = codecs
            .from_markup("**bold** and `code`")
            .expect("a fragment");
        let html = codecs.to_html(&slice).expect("an HTML flavour");
        assert_eq!(html, "<p><strong>bold</strong> and <code>code</code></p>");
        // And it is what the paste path reads when there is no metadata.
        assert_eq!(codecs.from_html(&html).as_ref(), Some(&slice));
    }
}
