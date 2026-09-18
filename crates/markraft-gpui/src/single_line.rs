//! A literal one-line input.
//!
//! A single-line editor runs on its own schema — `doc > paragraph > text`, with
//! no marks at all — rather than on the host's schema with a filter that keeps
//! one block. The schema is the simpler of the two options the design allowed:
//! nothing can create a second block or a mark, so no correction, change filter
//! or command chain has to undo one, and the commands the view binds fall
//! through on their own because the schema offers them nothing to do.

use gpui::{Pixels, px};
use markraft_doc::projection::Projection;
use markraft_doc::{Node, NodeTypeSpec, Schema, SchemaSpec};
use std::sync::LazyLock;
use std::{borrow::Cow, ops::Range};

static SCHEMA: LazyLock<Schema> = LazyLock::new(|| {
    Schema::new(
        SchemaSpec::new()
            .node(NodeTypeSpec::new("doc", "paragraph"))
            .node(NodeTypeSpec::new("paragraph", "text*").group("block"))
            .node(NodeTypeSpec::text("text").group("inline")),
    )
    .expect("the single-line schema spec is valid")
});

/// The schema a single-line editor runs on.
pub(crate) fn schema() -> &'static Schema {
    &SCHEMA
}

/// `text` with every line ending turned into a space, when `single_line`.
pub(crate) fn text(text: &str, single_line: bool) -> Cow<'_, str> {
    if single_line && text.contains(['\r', '\n']) {
        Cow::Owned(text.replace("\r\n", "\n").replace(['\r', '\n'], " "))
    } else {
        Cow::Borrowed(text)
    }
}

/// `doc` flattened onto [`schema`]: its plain text, in one unmarked paragraph.
pub(crate) fn document(doc: &Node, from: &Schema) -> Node {
    let flat = text(Projection::of(doc, from).plain_text(), true).into_owned();
    document_from_text(&flat)
}

/// A single-line document holding `value`.
pub(crate) fn document_from_text(value: &str) -> Node {
    let schema = schema();
    let content = if value.is_empty() {
        Vec::new()
    } else {
        vec![schema.text(value)]
    };
    let paragraph = schema
        .node("paragraph", content)
        .expect("text is valid paragraph content");
    schema
        .doc([paragraph])
        .expect("one paragraph is a valid single-line document")
}

/// An input method's selected range, re-measured over the text the field will
/// actually hold.
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
    use markraft_markdown::{commonmark_schema, from_markdown};

    #[test]
    fn single_line_preserves_literal_markers_and_maps_composition_offsets() {
        assert_eq!(text("# a\r\n[] b\rc\nd", true), "# a [] b c d");
        assert_eq!(text("# a\n", false), "# a\n");
        assert_eq!(selected_range("😀\r\n中", Some(4..5)), Some(3..4));
    }

    #[test]
    fn a_rich_document_flattens_to_one_unmarked_paragraph() {
        let rich = commonmark_schema();
        let doc = from_markdown(&rich, "# Title\n\n**bold**").expect("valid Markdown");
        let flattened = document(&doc, &rich);
        let projection = Projection::of(&flattened, schema());
        assert_eq!(projection.plain_text(), "Title bold");
        assert_eq!(projection.line_count(), 1);
        assert!(
            projection.lines()[0]
                .runs
                .iter()
                .all(|run| run.marks.is_empty())
        );
        // The schema itself is what keeps a second block out.
        assert!(schema().node_id("heading").is_none());
        assert!(schema().mark_types().is_empty());
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
