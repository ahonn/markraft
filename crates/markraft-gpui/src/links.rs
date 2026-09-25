//! The link mark, as a host's link editor sees it.
//!
//! Which link the caret or the selection is in is read from the marks. Setting
//! and removing a link is the document kind's business where it spells links in
//! the text — it answers [`DocumentKind::set_link`](crate::DocumentKind::set_link)
//! — and [`set_link`] is what the view does itself for a kind that does not:
//! it sets or removes the mark.

use markraft_core::protocol::event;
use markraft_core::{
    Attrs, Change, EditorState, Fragment, Mark, MarkSet, MarkTypeId, Node, Selection, Slice,
    TransactionSpec,
};
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
    let runs = projection
        .lines()
        .iter()
        .flat_map(|line| line.runs().iter().map(move |run| (line, run)));
    for (line, run) in runs {
        if line.abs(run.start).max(from) >= line.abs(run.end).min(to) {
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

/// Link the selection to `url`, or unlink it with `None`, by setting the link
/// mark itself — what the view does for a document kind whose
/// [`set_link`](crate::DocumentKind::set_link) answers nothing.
///
/// A caret edits the link it touches, and so does a selection inside one link;
/// elsewhere the URL is inserted as the linked text itself, which is what
/// pasting a bare URL onto nothing does.
pub(crate) fn set_link(
    state: &EditorState,
    ty: MarkTypeId,
    url: Option<&str>,
) -> Option<TransactionSpec> {
    let doc = state.doc();
    let selection = state.selection();
    let (from, to) = (selection.from(doc), selection.to(doc));
    let (from, to) = match link_at(doc, ty, from) {
        Some((span, _)) if from >= span.start && to <= span.end => (span.start, span.end),
        _ if from == to => return insert_linked(state, ty, url?),
        _ => (from, to),
    };
    let change = match url {
        Some(url) => {
            let title = link_at(doc, ty, from)
                .and_then(|(_, mark)| {
                    mark.attrs
                        .get("title")
                        .and_then(|value| value.as_str())
                        .map(str::to_owned)
                })
                .unwrap_or_default();
            let attrs = state
                .schema()
                .build_mark_attrs(ty, &Attrs::from_pairs([("href", url), ("title", &title)]))
                .ok()?;
            Change::add_mark(from, to, Mark::with_attrs(ty, attrs))
        }
        None => Change::remove_mark_type(from, to, ty),
    };
    Some(
        TransactionSpec::new()
            .changes([change])
            .selection(Selection::text(from, to))
            .user_event(event::FORMAT_LINK)
            .scroll_into_view(),
    )
}

/// Insert `url` as its own linked text at the caret (autolink form — no brackets).
fn insert_linked(state: &EditorState, ty: MarkTypeId, url: &str) -> Option<TransactionSpec> {
    let schema = state.schema();
    let marks = MarkSet::from_marks(
        schema,
        [Mark::with_attrs(ty, Attrs::from_pairs([("href", url)]))],
    );
    let slice = Slice::from_fragment(Fragment::from_node(schema.text_marked(url, marks)));
    markraft_core::commands::replace_selection(slice)(state)
        .map(|spec| spec.user_event(event::FORMAT_LINK))
}

#[cfg(test)]
mod tests {
    use super::*;
    use markraft_core::projection::projection_of;

    /// The document kind's own link command, as the host injects it.
    fn kind_link(state: &EditorState, url: Option<&str>) -> TransactionSpec {
        let formatter = markraft_commonmark::Formatter::new(Default::default());
        let command = match url {
            Some(url) => formatter.set_link(url, ""),
            None => formatter.unlink(),
        };
        command(state)
            .expect("the kind can write it")
            .expect("the command applies")
    }

    fn selecting(state: EditorState, from: usize, to: usize) -> EditorState {
        let projection = projection_of(&state);
        let from = projection.line_offset_to_pos(0, from).unwrap();
        let to = projection.line_offset_to_pos(0, to).unwrap();
        state
            .update([TransactionSpec::new().selection(Selection::text(from, to))])
            .unwrap()
            .state()
            .clone()
    }

    #[test]
    fn changing_a_nested_links_destination_preserves_its_single_scope() {
        let state = crate::typeahead::tests::state_of("[a *b **c*** d](https://old.test)");
        let state = selecting(state, 3, 3);
        let serializer =
            markraft_commonmark::HtmlSerializer::commonmark(state.schema(), &Default::default());
        let expected = serializer
            .serialize(state.doc())
            .replace("https://old.test", "https://new.test");
        let changed = state
            .update([kind_link(&state, Some("https://new.test"))])
            .unwrap();
        assert_eq!(serializer.serialize(changed.new_doc()), expected);
    }

    #[test]
    fn a_link_around_the_selection_is_found_and_unlinked_whole() {
        let state = crate::typeahead::tests::state_of("[foo **bar**](https://example.com)");
        let ty = state.schema().mark_id("link").unwrap();
        let bar = projection_of(&state).plain_text().find("bar").expect("bar");
        let state = selecting(state, bar, bar + 3);
        assert_eq!(
            active_link(&state, ty).as_deref(),
            Some("https://example.com")
        );
        let from = state.selection().from(state.doc());
        assert!(link_at(state.doc(), ty, from).is_some());
        let changed = state.update([kind_link(&state, None)]).unwrap();
        assert_eq!(
            markraft_commonmark::to_markdown(changed.state().schema(), changed.new_doc()),
            "foo **bar**"
        );
        assert_eq!(active_link(changed.state(), ty), None);
    }

    /// Without a kind's own command the view sets the mark itself.
    #[test]
    fn the_views_own_link_sets_and_removes_the_mark() {
        let schema = markraft_commonmark::commonmark_schema();
        let ty = schema.mark_id("link").unwrap();
        let doc = markraft_commonmark::from_markdown(&schema, "word").unwrap();
        let state = EditorState::create(
            markraft_core::EditorStateConfig::new(schema.clone())
                .doc(doc)
                .selection(Selection::text(1, 5))
                .extensions(markraft_core::projection::projection()),
        )
        .unwrap();
        let linked = state
            .update([set_link(&state, ty, Some("https://e.test")).unwrap()])
            .unwrap()
            .state()
            .clone();
        assert_eq!(active_link(&linked, ty).as_deref(), Some("https://e.test"));
        let unlinked = linked
            .update([set_link(&linked, ty, None).unwrap()])
            .unwrap()
            .state()
            .clone();
        assert_eq!(active_link(&unlinked, ty), None);
    }
}
