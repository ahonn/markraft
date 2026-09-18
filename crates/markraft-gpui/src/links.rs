//! The link mark, as a host's link editor sees it.
//!
//! A link is an ordinary mark with an `href` attribute, so setting one is a
//! mark change over a range and unlinking is the same change inverted. The one
//! thing the mark model does not answer directly is "which stretch is this one
//! link", which a caret inside a link needs, so that is computed here.

use markraft_core::{Attrs, EditorState, Mark, MarkTypeId, Node, Selection, TransactionSpec};
use std::ops::Range;

/// The link mark covering `pos`, as a position range and its href.
///
/// The range is the whole run of inline content carrying the *same* link mark,
/// which is what a link editor has to replace.
pub(crate) fn link_at(doc: &Node, ty: MarkTypeId, pos: usize) -> Option<(Range<usize>, Mark)> {
    let resolved = doc.resolve(pos).ok()?;
    for depth in (1..=resolved.depth()).rev() {
        if let Some(mark) = resolved.node(depth).marks().get(ty) {
            return Some((resolved.before(depth)..resolved.after(depth), mark.clone()));
        }
    }
    let parent = resolved.parent();
    let start = resolved.pos() - resolved.parent_offset();
    let mut found: Option<(Range<usize>, Mark)> = None;
    let mut offset = 0usize;
    for child in parent.children() {
        let range = start + offset..start + offset + child.node_size();
        offset += child.node_size();
        let Some(mark) = child.marks().get(ty) else {
            if found.is_some() && range.start > pos {
                break;
            }
            found = None;
            continue;
        };
        match &mut found {
            Some((span, existing)) if existing == mark && span.end == range.start => {
                span.end = range.end;
            }
            _ => {
                if found.as_ref().is_some_and(|(span, _)| span.end > pos) {
                    break;
                }
                found = Some((range, mark.clone()));
            }
        }
    }
    found.filter(|(span, _)| span.start <= pos && pos <= span.end)
}

/// The href every part of the selection shares, or the one the caret sits in.
pub(crate) fn active_link(state: &EditorState, ty: MarkTypeId) -> Option<String> {
    let doc = state.doc();
    let selection = state.selection();
    let (from, to) = (selection.from(doc), selection.to(doc));
    if from == to {
        return link_at(doc, ty, from).map(|(_, mark)| href(&mark));
    }
    let mut common: Option<String> = None;
    let mut all = true;
    let projection = markraft_core::projection::projection_of(state);
    for run in projection.lines().iter().flat_map(|line| &line.runs) {
        if run.from.max(from) >= run.to.min(to) {
            continue;
        }
        match run.marks.get(ty) {
            Some(mark) => {
                let url = href(mark);
                if common.get_or_insert(url.clone()) != &url {
                    all = false;
                }
            }
            None => all = false,
        }
    }
    all.then_some(common).flatten()
}

fn href(mark: &Mark) -> String {
    mark.attrs
        .get("href")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_owned()
}

/// Link the selection to `url`, or unlink it with `None`.
///
/// A caret edits the link it touches; elsewhere the URL is inserted as the
/// linked text itself, which is what pasting a bare URL onto nothing does.
pub(crate) fn set_link(
    state: &EditorState,
    ty: MarkTypeId,
    url: Option<&str>,
) -> Option<TransactionSpec> {
    let doc = state.doc();
    let selection = state.selection();
    let (from, to) = (selection.from(doc), selection.to(doc));
    let (from, to) = if from == to {
        match link_at(doc, ty, from) {
            Some((span, _)) => (span.start, span.end),
            None => {
                let url = url?;
                return insert_linked(state, ty, url);
            }
        }
    } else {
        (from, to)
    };
    let command = match url {
        Some(url) => markraft_core::commands::set_mark(ty, Attrs::from_pairs([("href", url)])),
        None => markraft_core::commands::remove_mark(ty),
    };
    let selected = state
        .update([TransactionSpec::new().selection(Selection::text(from, to))])
        .ok()?;
    let spec = command(selected.state())?;
    let changed = selected.state().update([spec]).ok()?;
    Some(
        TransactionSpec::new()
            .change_set(changed.changes().clone())
            .selection(selection.map(state.schema(), changed.new_doc(), &changed.changes().desc()))
            .user_event("format.link")
            .scroll_into_view(),
    )
}

/// Insert `url` as its own linked text at the caret.
fn insert_linked(state: &EditorState, ty: MarkTypeId, url: &str) -> Option<TransactionSpec> {
    let schema = state.schema();
    let marks = markraft_core::MarkSet::from_marks(
        schema,
        [Mark::with_attrs(ty, Attrs::from_pairs([("href", url)]))],
    );
    let slice = markraft_core::Slice::from_fragment(markraft_core::Fragment::from_node(
        schema.text_marked(url, marks),
    ));
    markraft_core::commands::replace_selection(slice)(state)
        .map(|spec| spec.user_event("format.link"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use markraft_core::projection::projection_of;

    #[test]
    fn changing_a_nested_links_destination_preserves_its_single_scope() {
        let state = crate::typeahead::tests::state_of("[a *b **c*** d](https://old.test)");
        let ty = state.schema().mark_id("link").unwrap();
        let pos = projection_of(&state).line_offset_to_pos(0, 3).unwrap();
        let state = state
            .update([TransactionSpec::new().selection(Selection::cursor(pos))])
            .unwrap()
            .state()
            .clone();
        let serializer = markraft_commonmark::HtmlSerializer::commonmark(state.schema());
        let expected = serializer
            .serialize(state.doc())
            .replace("https://old.test", "https://new.test");
        let changed = state
            .update([set_link(&state, ty, Some("https://new.test")).unwrap()])
            .unwrap();
        assert_eq!(serializer.serialize(changed.new_doc()), expected);
    }

    #[test]
    fn nested_links_are_detected_and_partially_unlinked() {
        let state = crate::typeahead::tests::state_of("[**foo **bar****](https://example.com)");
        let ty = state.schema().mark_id("link").unwrap();
        let projection = projection_of(&state);
        let from = projection.line_offset_to_pos(0, 4).unwrap();
        let to = projection.line_offset_to_pos(0, 7).unwrap();
        let state = state
            .update([TransactionSpec::new().selection(Selection::text(from, to))])
            .unwrap()
            .state()
            .clone();
        assert_eq!(
            active_link(&state, ty).as_deref(),
            Some("https://example.com")
        );
        assert!(link_at(state.doc(), ty, from).is_some());
        let spec = set_link(&state, ty, None).unwrap();
        let changed = state.update([spec]).unwrap();
        let projection = projection_of(changed.state());
        assert_eq!(projection.plain_text(), "foo bar");
        for run in &projection.lines()[0].runs {
            assert_eq!(run.marks.contains_type(ty), run.char_from < 4);
        }
    }
}
