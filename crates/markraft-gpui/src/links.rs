//! The link mark, as a host's link editor sees it.
//!
//! A link is an ordinary mark with an `href` attribute, so setting one is a
//! mark change over a range and unlinking is the same change inverted. The one
//! thing the mark model does not answer directly is "which stretch is this one
//! link", which a caret inside a link needs, so that is computed here.

use markraft_doc::{
    Attrs, Change, EditorState, Mark, MarkTypeId, Node, Selection, TransactionSpec,
};
use std::ops::Range;

/// The link mark covering `pos`, as a position range and its href.
///
/// The range is the whole run of inline content carrying the *same* link mark,
/// which is what a link editor has to replace.
pub(crate) fn link_at(doc: &Node, ty: MarkTypeId, pos: usize) -> Option<(Range<usize>, Mark)> {
    let resolved = doc.resolve(pos).ok()?;
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
    doc.nodes_between(from, to, &mut |node, pos, _, _| {
        if node.text().is_none() && !node.is_leaf() {
            return true;
        }
        let end = pos + node.node_size();
        if pos.max(from) >= end.min(to) {
            return true;
        }
        match node.marks().get(ty) {
            Some(mark) => {
                let url = href(mark);
                if common.get_or_insert(url.clone()) != &url {
                    all = false;
                }
            }
            None => all = false,
        }
        true
    });
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
                return Some(insert_linked(state, ty, url));
            }
        }
    } else {
        (from, to)
    };
    let change = match url {
        Some(url) => Change::add_mark(
            from,
            to,
            Mark::with_attrs(ty, Attrs::from_pairs([("href", url)])),
        ),
        None => Change::remove_mark_type(from, to, ty),
    };
    markraft_doc::commands::changes_spec(state, vec![change], "format.link")
}

/// Insert `url` as its own linked text at the caret.
fn insert_linked(state: &EditorState, ty: MarkTypeId, url: &str) -> TransactionSpec {
    let schema = state.schema();
    let doc = state.doc();
    let range = state.selection().replacement_range(doc);
    let marks = markraft_doc::MarkSet::from_marks(
        schema,
        [Mark::with_attrs(ty, Attrs::from_pairs([("href", url)]))],
    );
    let slice = markraft_doc::Slice::from_fragment(markraft_doc::Fragment::from_node(
        schema.text_marked(url, marks),
    ));
    TransactionSpec::new()
        .changes([Change::replace(range.from, range.to, slice).with_fit(markraft_doc::Fit::Auto)])
        .selection(Selection::cursor(range.from + url.chars().count()))
        .user_event("format.link")
        .scroll_into_view()
}
