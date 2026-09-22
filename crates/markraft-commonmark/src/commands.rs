//! Method-B mark commands: edit delimiter characters, then let normalize
//! re-derive the marks.

use markraft_core::commands::{Command, command, mark_applies, range_has_mark};
use markraft_core::{Attrs, Change, Fragment, Mark, MarkTypeId, Selection, Slice, TransactionSpec};

use crate::inline::{is_syntax, style_delimiters, syntax_text};
use crate::schema as md;

/// Toggle a Method-B style mark by inserting or stripping its delimiters.
///
/// With a non-empty selection the delimiters wrap the selected text when the
/// mark is absent, and the flanking syntax leaves for that mark are removed
/// when it is present. With a cursor a delimiter pair is inserted and the
/// caret placed between them — the Typora-like empty-span behaviour.
///
/// Non-Method-B marks fall through to the core [`toggle_mark`](markraft_core::commands::toggle_mark).
pub fn toggle_style_mark(mark_type: MarkTypeId, attrs: Attrs) -> Command {
    command(move |state| {
        let schema = state.schema();
        let name = schema.try_mark_type(mark_type)?.name();
        let Some((open, close)) =
            style_delimiters(name).or_else(|| (name == md::CODE).then_some(("`", "`")))
        else {
            return markraft_core::commands::toggle_mark(mark_type, attrs.clone())(state);
        };
        let doc = state.doc();
        let selection = state.selection();
        let ranges = selection.ranges(doc);
        if !mark_applies(schema, doc, &ranges, mark_type) {
            return None;
        }

        let mark = Mark::with_attrs(mark_type, schema.build_mark_attrs(mark_type, &attrs).ok()?);
        // A code span's fence is as long as its content needs — `` `a`b` `` is
        // spelled with two backticks — so matching one against a fixed string
        // would miss every fence but the shortest.
        let fenced = name == md::CODE;
        let matches = move |delim: &'static str| {
            move |text: &str| {
                if fenced {
                    !text.is_empty() && text.chars().all(|c| c == '`')
                } else {
                    text == delim
                }
            }
        };
        let (is_open, is_close) = (matches(open), matches(close));

        if selection.is_cursor() {
            // An empty pair: the delimiters go in as syntax leaves rather than as
            // ordinary text, because the serializer escapes ordinary text and the
            // characters would stop reading as delimiters the moment they were
            // written out. The style mark comes later, from normalize, once there
            // is content between them for it to cover.
            let pos = selection.head(doc);
            let inserted = Fragment::from_nodes([
                syntax_text(schema, open).ok()?,
                syntax_text(schema, close).ok()?,
            ]);
            let between = pos + open.chars().count();
            return Some(
                TransactionSpec::new()
                    .changes([Change::insert(pos, Slice::from_fragment(inserted))])
                    .selection(Selection::cursor(between))
                    .user_event("mark.add"),
            );
        }

        let add = !ranges
            .iter()
            .any(|range| range_has_mark(doc, range.from, range.to, mark_type));

        if add {
            // The whole Method-B shape in one step: the mark over the selection
            // and a delimiter leaf on each side carrying it too, which is what
            // parse builds and what serialize expects. Every position here refers
            // to the starting document, so the order of the changes is free.
            let leaf = |delim: &str| -> Option<Slice> {
                let leaf = syntax_text(schema, delim).ok()?;
                let leaf = leaf.mark(leaf.marks().add(schema, mark.clone()));
                Some(Slice::from_fragment(Fragment::from_node(leaf)))
            };
            let mut changes = Vec::new();
            for range in ranges.iter() {
                changes.push(Change::insert(range.to, leaf(close)?));
                changes.push(Change::insert(range.from, leaf(open)?));
                changes.push(Change::add_mark(range.from, range.to, mark.clone()));
            }
            let first = ranges.first()?;
            let from = first.from + open.chars().count();
            let to = first.to + open.chars().count();
            return Some(
                TransactionSpec::new()
                    .changes(changes)
                    .selection(Selection::text(from, to))
                    .user_event("mark.add"),
            );
        }

        // Remove: take out this mark's own delimiter leaves and the mark over
        // what stood between them. Deleting the leaves alone would leave the
        // style mark on the text and normalize would write them back; replacing
        // the span with its plain text would throw away every *other* mark
        // inside it. Positions all refer to the starting document.
        let mut changes = Vec::new();
        for range in ranges.iter() {
            let open_at = left_syntax_at(schema, doc, range.from, mark_type, &is_open)
                .or_else(|| syntax_inside(schema, doc, range, mark_type, &is_open, true));
            let close_at = right_syntax_at(schema, doc, range.to, mark_type, &is_close)
                .or_else(|| syntax_inside(schema, doc, range, mark_type, &is_close, false));
            let (Some((open_from, open_to)), Some((close_from, close_to))) = (open_at, close_at)
            else {
                return markraft_core::commands::toggle_mark(mark_type, attrs.clone())(state);
            };
            if close_from < open_to {
                return markraft_core::commands::toggle_mark(mark_type, attrs.clone())(state);
            }
            changes.push(Change::delete(open_from, open_to));
            changes.push(Change::remove_mark_type(open_to, close_from, mark_type));
            changes.push(Change::delete(close_from, close_to));
        }
        Some(
            TransactionSpec::new()
                .changes(changes)
                .user_event("mark.remove"),
        )
    })
}

/// The first (or last) delimiter leaf for `mark_type` inside `range`.
fn syntax_inside(
    schema: &markraft_core::Schema,
    doc: &markraft_core::Node,
    range: &markraft_core::SelectionRange,
    mark_type: MarkTypeId,
    is_delim: &dyn Fn(&str) -> bool,
    first: bool,
) -> Option<(usize, usize)> {
    let mut found = None;
    doc.nodes_between(range.from, range.to, &mut |node, at, _, _| {
        if is_delimiter_leaf(schema, node, mark_type, is_delim) && !(first && found.is_some()) {
            found = Some((at, at + node.node_size()));
        }
        true
    });
    found
}

/// The delimiter leaf ending at `pos`, if that is what stands there.
fn left_syntax_at(
    schema: &markraft_core::Schema,
    doc: &markraft_core::Node,
    pos: usize,
    mark_type: MarkTypeId,
    is_delim: &dyn Fn(&str) -> bool,
) -> Option<(usize, usize)> {
    let node = doc.resolve(pos).ok()?.node_before()?;
    is_delimiter_leaf(schema, &node, mark_type, is_delim).then(|| (pos - node.node_size(), pos))
}

/// The delimiter leaf starting at `pos`, if that is what stands there.
fn right_syntax_at(
    schema: &markraft_core::Schema,
    doc: &markraft_core::Node,
    pos: usize,
    mark_type: MarkTypeId,
    is_delim: &dyn Fn(&str) -> bool,
) -> Option<(usize, usize)> {
    let node = doc.resolve(pos).ok()?.node_after()?;
    is_delimiter_leaf(schema, &node, mark_type, is_delim).then(|| (pos, pos + node.node_size()))
}

fn is_delimiter_leaf(
    schema: &markraft_core::Schema,
    node: &markraft_core::Node,
    mark_type: MarkTypeId,
    is_delim: &dyn Fn(&str) -> bool,
) -> bool {
    is_syntax(schema, node)
        && node.marks().contains_type(mark_type)
        && node.text().is_some_and(is_delim)
}
