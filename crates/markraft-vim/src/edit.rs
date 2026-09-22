//! The edits vim commands make, as change sets over the document.
//!
//! Each function builds the whole of a command's effect, so a command is one
//! transaction and one undo step, and every one of them can be driven straight from an
//! [`EditorState`] in a test. Linewise operators retain the containers around selected
//! blocks in the register while deleting only those blocks from the document.

use crate::motion::{self, Span};
use markraft_core::commands::delete_range_changes;
use markraft_core::projection::Projection;
use markraft_core::{
    Change, ChangeSet, EditorState, Fit, Fragment, Node, Selection, Slice, TrackMode,
    TransactionSpec,
};
use std::ops::Range;

/// What a yank or delete put in the unnamed register.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Register {
    /// The cut content, so marks, links and block structure survive a round trip.
    pub slice: Slice,
    /// The same content as plain text, for other applications.
    pub text: String,
    /// Whole lines, which `p` and `P` put below and above the cursor's line.
    pub linewise: bool,
    /// How deep in the tree the linewise nodes were taken from, so a paste puts
    /// them back beside a node of the same kind: 0 is a top-level block.
    pub depth: usize,
}

/// The enclosing sibling range for `lines`, and the depth of its parent.
/// Partial containers at its edges are filtered by [`linewise_content`].
///
/// The model's own [`block_range`](markraft_core::ResolvedPos::block_range) finds the
/// smallest run of siblings covering the lines, which is what keeps a range spanning
/// several nesting levels well formed. It is then widened outwards while the parent
/// holds nothing but that run, so `dd` on the only paragraph of a list item takes the
/// item, and on the only item of a list takes the list — which it must, because the
/// schema does not allow an empty list.
pub(crate) fn linewise_unit(
    state: &EditorState,
    projection: &Projection,
    lines: Range<usize>,
) -> Option<(Range<usize>, usize)> {
    let doc = state.doc();
    let last_line = motion::last_line(projection);
    let first = projection.line(lines.start.min(last_line))?;
    let last = projection.line(lines.end.saturating_sub(1).min(last_line))?;
    // Node boundaries rather than text positions, so a block-level leaf — which holds
    // no text for a position to sit inside — is covered like any other line.
    let start = first.ancestors.last()?.before;
    let own = last.ancestors.last()?;
    let end = own.before + doc.node_at(own.before)?.node_size();
    let from = doc.resolve(start).ok()?;
    let to = doc.resolve(end).ok()?;
    let range = from.block_range(state.schema(), &to, None)?;
    let mut depth = range.depth();
    let (mut start, mut end) = (range.start(), range.end());
    while depth > 0 && start == from.start(depth) && end == from.end(depth) {
        start = from.before(depth);
        end = from.after(depth);
        depth -= 1;
    }
    Some((start..end, depth))
}

/// The selected lines as closed nodes, plus the exact nodes to remove. A
/// partially selected container is kept around the yank, but only its selected
/// descendants are deleted from the source document.
fn linewise_content(
    state: &EditorState,
    projection: &Projection,
    lines: Range<usize>,
) -> Option<(Slice, Vec<Range<usize>>, usize)> {
    let (range, depth) = linewise_unit(state, projection, lines.clone())?;
    let selected: std::collections::HashSet<usize> = projection
        .lines()
        .get(lines.start..lines.end.min(projection.line_count()))?
        .iter()
        .filter_map(|line| line.ancestors.last().map(|own| own.before))
        .collect();

    fn collect(
        node: &Node,
        before: usize,
        selected: &std::collections::HashSet<usize>,
    ) -> Option<(Node, Vec<Range<usize>>, bool)> {
        let whole = before..before + node.node_size();
        if selected.contains(&before) {
            return Some((node.clone(), vec![whole], true));
        }
        let mut children = Vec::new();
        let mut cuts = Vec::new();
        let mut all = true;
        let mut pos = before + 1;
        for child in node.children() {
            if let Some((part, ranges, complete)) = collect(child, pos, selected) {
                children.push(part);
                cuts.extend(ranges);
                all &= complete;
            } else {
                all = false;
            }
            pos += child.node_size();
        }
        if children.is_empty() {
            None
        } else if all {
            Some((node.clone(), vec![whole], true))
        } else {
            Some((node.copy(Fragment::from_nodes(children)), cuts, false))
        }
    }

    let mut nodes = Vec::new();
    let mut cuts = Vec::new();
    let mut pos = range.start;
    while pos < range.end {
        let node = state.doc().node_at(pos)?;
        if let Some((selected_node, ranges, _)) = collect(&node, pos, &selected) {
            nodes.push(selected_node);
            cuts.extend(ranges);
        }
        pos += node.node_size();
    }
    // Adjacent deletions must be fitted together: fitting each independently
    // can introduce filler content before the full line range has been removed.
    let mut merged: Vec<Range<usize>> = Vec::new();
    for cut in cuts {
        if let Some(previous) = merged.last_mut()
            && previous.end == cut.start
        {
            previous.end = cut.end;
        } else {
            merged.push(cut);
        }
    }
    Some((
        Slice::from_fragment(Fragment::from_nodes(nodes)),
        merged,
        depth,
    ))
}

/// The register a line range would yank.
pub(crate) fn linewise_register(
    state: &EditorState,
    projection: &Projection,
    lines: Range<usize>,
    plain: &dyn Fn(&Slice) -> String,
) -> Option<Register> {
    let (slice, _, depth) = linewise_content(state, projection, lines)?;
    Some(Register {
        text: plain(&slice),
        slice,
        linewise: true,
        depth,
    })
}

/// The register a charwise range would yank.
pub(crate) fn charwise_register(
    state: &EditorState,
    range: Range<usize>,
    plain: &dyn Fn(&Slice) -> String,
) -> Register {
    let slice = state
        .doc()
        .slice_with_schema(state.schema(), range.start, range.end)
        .unwrap_or_else(|_| Slice::empty());
    Register {
        text: plain(&slice),
        slice,
        linewise: false,
        depth: 0,
    }
}

/// Build a change set and the document it produces, or `None` when the schema
/// would reject the result.
fn resolve(state: &EditorState, changes: Vec<Change>) -> Option<(ChangeSet, Node)> {
    let set = ChangeSet::create(state.schema(), state.doc(), changes).ok()?;
    if set.is_empty() {
        return None;
    }
    let doc = set.apply(state.doc()).ok()?;
    doc.check(state.schema()).ok()?;
    Some((set, doc))
}

fn spec(set: ChangeSet, selection: Selection) -> TransactionSpec {
    TransactionSpec::new()
        .change_set(set)
        .selection(selection)
        .user_event("input.vim")
        .scroll_into_view()
}

/// Delete a charwise range and leave the cursor where it started.
pub(crate) fn delete_charwise(state: &EditorState, range: Range<usize>) -> Option<TransactionSpec> {
    let changes = delete_range_changes(state.schema(), state.doc(), range.start, range.end);
    let (set, doc) = resolve(state, changes)?;
    let caret = set
        .map_pos(range.start, -1, TrackMode::Simple)
        .unwrap_or(range.start)
        .min(doc.content_size());
    Some(spec(set, Selection::near(state.schema(), &doc, caret, 1)))
}

/// Delete whole lines. The schema keeps the document valid, so deleting everything
/// leaves the smallest document it allows.
pub(crate) fn delete_linewise(
    state: &EditorState,
    projection: &Projection,
    lines: Range<usize>,
) -> Option<TransactionSpec> {
    let (_, ranges, _) = linewise_content(state, projection, lines)?;
    let start = ranges.first()?.start;
    let changes = ranges
        .into_iter()
        .map(|range| Change::delete(range.start, range.end).with_fit(Fit::Auto))
        .collect();
    let (set, doc) = resolve(state, changes)?;
    let caret = set
        .map_pos(start, 1, TrackMode::Simple)
        .unwrap_or(start)
        .min(doc.content_size());
    let after = Projection::of(&doc, state.schema());
    let line = motion::line_from(&after, caret);
    Some(spec(
        set,
        Selection::cursor(motion::first_non_blank(&after, line)),
    ))
}

/// `cc`: empty the lines but keep the first one, so the new text inherits the list,
/// quote or code line that was changed rather than becoming a paragraph.
pub(crate) fn change_linewise(
    state: &EditorState,
    projection: &Projection,
    lines: Range<usize>,
) -> Option<TransactionSpec> {
    let first = projection.line(lines.start)?;
    let mut changes = Vec::new();
    if lines.end > lines.start + 1 {
        let (_, ranges, _) = linewise_content(state, projection, lines.start + 1..lines.end)?;
        changes.extend(
            ranges
                .into_iter()
                .map(|range| Change::delete(range.start, range.end).with_fit(Fit::Auto)),
        );
    }
    changes.extend(delete_range_changes(
        state.schema(),
        state.doc(),
        first.from,
        first.to,
    ));
    let (set, doc) = resolve(state, changes)?;
    let caret = set
        .map_pos(first.from, -1, TrackMode::Simple)
        .unwrap_or(first.from);
    Some(spec(
        set,
        Selection::near(state.schema(), &doc, caret.min(doc.content_size()), 1),
    ))
}

/// `p` and `P`. A linewise register becomes whole nodes below or above the cursor's
/// line, at the depth they were taken from; a charwise one is pasted inline, after the
/// cursor's grapheme for `p` and at it for `P`. The cursor lands where vim leaves it: on
/// the first non-blank of the first pasted line, or on the last pasted grapheme.
pub(crate) fn paste(
    state: &EditorState,
    projection: &Projection,
    cursor: usize,
    register: &Register,
    after: bool,
    literal: bool,
) -> Option<TransactionSpec> {
    if register.slice.is_empty() {
        return None;
    }
    // A verbatim block holds characters, not structure: what goes in is the
    // register's prose, without the marks or the characters that spell them.
    let content = if literal {
        let text = register.text.trim_end_matches('\n');
        if text.is_empty() {
            return None;
        }
        Slice::from_fragment(Fragment::from_node(state.schema().text(text)))
    } else {
        register.slice.clone()
    };
    if register.linewise {
        let at = linewise_paste_position(state.doc(), projection, cursor, register, after)?;
        let (set, doc) = resolve(
            state,
            vec![Change::replace(at, at, content).with_fit(Fit::Auto)],
        )?;
        let start = set.map_pos(at, -1, TrackMode::Simple).unwrap_or(at);
        let after_doc = Projection::of(&doc, state.schema());
        let line = motion::line_from(&after_doc, start.min(doc.content_size()));
        return Some(spec(
            set,
            Selection::cursor(motion::first_non_blank(&after_doc, line)),
        ));
    }
    let at = if after {
        motion::next_in_line(projection, cursor)
    } else {
        cursor
    };
    let (set, doc) = resolve(
        state,
        vec![Change::replace(at, at, content).with_fit(Fit::Auto)],
    )?;
    let end = set
        .map_pos(at, 1, TrackMode::Simple)
        .unwrap_or(at)
        .min(doc.content_size());
    // The caret sits after the pasted text; vim leaves it on its last grapheme.
    let after_doc = Projection::of(&doc, state.schema());
    Some(spec(
        set,
        Selection::cursor(motion::previous_in_line(&after_doc, end)),
    ))
}

/// Where a linewise paste inserts: beside the cursor's ancestor at the depth the
/// register's nodes were cut from, or at the deepest one it has.
fn linewise_paste_position(
    doc: &Node,
    projection: &Projection,
    cursor: usize,
    register: &Register,
    after: bool,
) -> Option<usize> {
    let line = projection.line(motion::line_of(projection, cursor))?;
    // Projection paths include the line's own node, including leaves that a
    // resolved position cannot enter.
    let level = register.depth.min(line.ancestors.len().checked_sub(1)?);
    let before = line.ancestors[level].before;
    Some(if after {
        before + doc.node_at(before)?.node_size()
    } else {
        before
    })
}

/// `x`: the `count` graphemes at the cursor, never reaching past the end of the line so
/// that it cannot join two of them. A line with no text — a horizontal rule, or an empty
/// paragraph — yields an empty range, which makes the command a no-op.
pub(crate) fn delete_chars_range(
    projection: &Projection,
    cursor: usize,
    count: usize,
) -> Range<usize> {
    let mut end = cursor;
    for _ in 0..count.clamp(1, motion::MAX_COUNT) {
        let next = motion::next_in_line(projection, end);
        if next == end {
            break;
        }
        end = next;
    }
    cursor..end
}

/// `D` and `C`: from the cursor to the end of its line.
pub(crate) fn to_line_end(projection: &Projection, cursor: usize) -> Range<usize> {
    let line = &projection.lines()[motion::line_of(projection, cursor)];
    cursor..line.to
}

/// The inclusive charwise range a Visual selection covers: from the anchor grapheme to
/// the cursor's, whichever way round they are.
pub(crate) fn visual_range(projection: &Projection, anchor: usize, cursor: usize) -> Range<usize> {
    motion::charwise_range(projection, anchor, cursor, Span::Inclusive)
}

#[cfg(test)]
mod tests {
    use super::*;
    use markraft_commonmark::{
        commonmark_extensions, commonmark_schema, from_markdown, to_markdown,
    };
    use markraft_core::projection::{projection_of, slice_to_plain_text};
    use markraft_core::{EditorStateConfig, Extension};

    fn state_of(markdown: &str) -> EditorState {
        let schema = commonmark_schema();
        let doc = from_markdown(&schema, markdown).expect("valid Markdown");
        EditorState::create(EditorStateConfig::new(schema.clone()).doc(doc).extensions(
            Extension::all([
                markraft_core::projection::projection(),
                markraft_core::history(Default::default()),
                commonmark_extensions(&schema),
            ]),
        ))
        .expect("a valid state")
    }

    /// The Markdown a spec leaves behind.
    fn applied(state: &EditorState, spec: TransactionSpec) -> String {
        let tr = state.update([spec]).expect("the edit applies");
        to_markdown(state.schema(), tr.state().doc())
    }

    /// The node a linewise unit covers, as Markdown.
    fn unit_of(state: &EditorState, lines: Range<usize>) -> (String, usize) {
        let projection = projection_of(state);
        let (range, depth) = linewise_unit(state, &projection, lines).expect("a linewise unit");
        let slice = state
            .doc()
            .slice(range.start, range.end)
            .expect("a valid slice");
        (
            markraft_commonmark::to_markdown_fragment(state.schema(), &slice),
            depth,
        )
    }

    #[test]
    fn a_linewise_unit_widens_to_the_outermost_node_the_lines_fill() {
        // A paragraph that is the only block of an item, in an item that is not the
        // only one, gives the item.
        let state = state_of("- a\n- b");
        assert_eq!(unit_of(&state, 0..1), ("- a".to_owned(), 1));
        // The only item of a list gives the list, because a list may not be empty.
        let state = state_of("- only\n\nafter");
        assert_eq!(unit_of(&state, 0..1), ("- only".to_owned(), 0));
        // A plain paragraph is its own unit.
        let state = state_of("one\n\ntwo");
        assert_eq!(unit_of(&state, 0..1), ("one".to_owned(), 0));
        // A range spanning nesting levels is widened to whole siblings of the
        // common parent.
        let state = state_of("# head\n\n- a\n- b");
        assert_eq!(unit_of(&state, 0..2).1, 0);
    }

    #[test]
    fn a_linewise_yank_keeps_marks_and_structure() {
        let state = state_of("- **a**\n  - b");
        let projection = projection_of(&state);
        let plain = |slice: &Slice| slice_to_plain_text(state.schema(), slice);
        let register = linewise_register(&state, &projection, 1..2, &plain).expect("a register");
        assert!(register.linewise);
        assert_eq!(register.text, "b");
        assert_eq!(
            markraft_commonmark::to_markdown_fragment(state.schema(), &register.slice),
            "- b"
        );
    }

    #[test]
    fn a_linewise_paste_puts_nodes_below_and_above() {
        let state = state_of("- one\n- two");
        let projection = projection_of(&state);
        let plain = |slice: &Slice| slice_to_plain_text(state.schema(), slice);
        let register = linewise_register(&state, &projection, 0..1, &plain).expect("a register");
        let cursor = projection.lines()[1].from;
        let below = paste(&state, &projection, cursor, &register, true, false).expect("a paste");
        assert_eq!(applied(&state, below), "- one\n- two\n- one");
        let above = paste(&state, &projection, cursor, &register, false, false).expect("a paste");
        assert_eq!(applied(&state, above), "- one\n- one\n- two");
    }

    #[test]
    fn a_charwise_paste_lands_inline_and_keeps_its_marks() {
        let state = state_of("ab");
        let projection = projection_of(&state);
        let source = state_of("**XY**");
        let source_projection = projection_of(&source);
        let plain = |slice: &Slice| slice_to_plain_text(source.schema(), slice);
        let register = charwise_register(
            &source,
            motion::line_start(&source_projection, 0)..motion::line_end(&source_projection, 0),
            &plain,
        );
        let spec = paste(&state, &projection, 1, &register, true, false).expect("a paste");
        assert_eq!(applied(&state, spec), "a**XY**b");
        let spec = paste(&state, &projection, 2, &register, false, false).expect("a paste");
        assert_eq!(applied(&state, spec), "a**XY**b");
    }

    #[test]
    fn deleting_characters_stops_at_the_end_of_the_line() {
        let state = state_of("ab\n\ncd");
        let projection = projection_of(&state);
        assert_eq!(delete_chars_range(&projection, 2, 9), 2..3);
        // A horizontal rule holds no text, so `x` finds nothing to remove.
        let state = state_of("***");
        let projection = projection_of(&state);
        let rule = projection.lines()[0].from;
        assert_eq!(delete_chars_range(&projection, rule, 1), rule..rule);
    }

    #[test]
    fn a_linewise_change_empties_the_line_but_keeps_its_kind() {
        let state = state_of("- one\n- two\n- three");
        let projection = projection_of(&state);
        let spec = change_linewise(&state, &projection, 0..2).expect("a change");
        assert_eq!(applied(&state, spec), "- \n- three");
    }
}
