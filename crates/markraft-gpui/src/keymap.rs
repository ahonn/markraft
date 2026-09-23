//! What each bound action does, as a chain of catalogue commands.
//!
//! The chains mirror ProseMirror's base and list keymaps, with the three
//! departures the editor's own tests describe: Backspace at the start of a list
//! item outdents before it lifts or joins, Enter in an empty list item leaves
//! the list, and Backspace in an empty verbatim block turns it into a
//! paragraph.

use crate::types::DocTypes;
use markraft_core::commands::structure::markup_of;
use markraft_core::commands::{
    Command, Direction, add_row_after, chain, changes_spec, command, create_paragraph_near,
    delete_by_grapheme, delete_by_word, delete_empty_table, delete_selection, exit_code,
    goto_cell_below, goto_next_cell, goto_prev_cell, guard_cell_boundary, guard_cell_range,
    guard_cell_split, join_backward, join_forward, lift, lift_empty_block, lift_list_item,
    move_by_grapheme, move_by_word, new_line_in_code, select_node_backward, select_node_forward,
    set_block_type, sink_list_item, split_block_keep_marks, split_list_item, undo_input_rule,
    wrap_in, wrap_in_list,
};
use markraft_core::projection::projection_of;
use markraft_core::{
    AttrValue, Attrs, Change, EditorState, Markup, NodeTypeId, Selection, Slice, Token,
    TransactionSpec,
};

/// Run `command` only where `pred` holds.
fn when(pred: impl Fn(&EditorState) -> bool + Send + Sync + 'static, command: Command) -> Command {
    markraft_core::commands::command(move |state| pred(state).then(|| command(state)).flatten())
}

/// One command running `steps` in order, as a single edit.
///
/// Each step is computed against the document the previous ones produce and the
/// change sets are composed, so the whole is one transaction and one undo entry.
fn composed(steps: Vec<Command>) -> Command {
    command(move |state| {
        let mut current = state.clone();
        let mut set: Option<markraft_core::ChangeSet> = None;
        let mut selection = None;
        let mut event = None;
        for (index, step) in steps.iter().enumerate() {
            let Some(spec) = step(&current) else {
                // Follow-up normalization is optional, but it must never run
                // when the primary edit did not apply.
                if index == 0 {
                    return None;
                }
                continue;
            };
            let tr = current.update([spec]).ok()?;
            let next = tr.changes().clone();
            set = Some(match set {
                Some(previous) => previous.compose(&next).ok()?,
                None => next,
            });
            selection = Some(tr.new_selection());
            event = event.or_else(|| tr.user_event_name().map(str::to_owned));
            current = tr.state().clone();
        }
        let mut spec = TransactionSpec::new()
            .change_set(set?)
            .user_event(event.as_deref().unwrap_or("input"))
            .scroll_into_view();
        if let Some(selection) = selection {
            spec = spec.selection(selection);
        }
        Some(spec)
    })
}

/// Split a task item, leaving the new one unticked: a fresh task is something
/// still to do, whatever the item it was split from said.
fn split_task_item(types: &DocTypes, item: NodeTypeId) -> Command {
    let types = types.clone();
    composed(vec![
        split_list_item(item),
        command(move |state| {
            let (ty, attrs, before) = types.item_at_cursor(state)?;
            if Some(ty) != types.task_item || !DocTypes::task_checked(&attrs) {
                return None;
            }
            let node = state.doc().node_at(before)?;
            let cleared = node.with_attrs(attrs.with("checked", AttrValue::Bool(false)));
            let slice =
                markraft_core::Slice::from_fragment(markraft_core::Fragment::from_node(cleared));
            markraft_core::commands::changes_spec(
                state,
                vec![Change::replace(before, before + node.node_size(), slice)],
                "format.block",
            )
            .map(|spec| spec.selection(state.selection().clone()))
        }),
    ])
}

/// Every command in `list` that exists, in order.
fn some(list: impl IntoIterator<Item = Option<Command>>) -> Command {
    chain(list.into_iter().flatten())
}

/// The same command for both item types a list may hold.
fn per_item(types: &DocTypes, make: impl Fn(NodeTypeId) -> Command) -> Vec<Option<Command>> {
    vec![types.list_item.map(&make), types.task_item.map(&make)]
}

impl DocTypes {
    /// Whether the cursor sits at the start of the first block of a list item.
    fn at_item_start(&self, state: &EditorState) -> bool {
        let doc = state.doc();
        let selection = state.selection();
        if !selection.is_cursor() {
            return false;
        }
        let projection = projection_of(state);
        let Some(index) = projection.line_at(selection.head(doc)) else {
            return false;
        };
        let line = &projection.lines()[index];
        line.pos_to_offset(selection.head(doc)) == Some(0)
            && line.ancestors.last().is_some_and(|own| own.index == 0)
            && line
                .ancestors
                .iter()
                .nth_back(1)
                .is_some_and(|parent| self.is_item(parent.node_type))
    }

    /// The type and attributes of the textblock the cursor sits in.
    fn block_at_cursor(&self, state: &EditorState) -> Option<(NodeTypeId, Attrs)> {
        let doc = state.doc();
        let resolved = doc.resolve(state.selection().head(doc)).ok()?;
        let parent = resolved.parent();
        parent
            .is_textblock(state.schema())
            .then(|| (parent.type_id(), parent.attrs().clone()))
    }

    /// The innermost list type the cursor sits in.
    fn list_at_cursor(&self, state: &EditorState) -> Option<NodeTypeId> {
        let doc = state.doc();
        let resolved = doc.resolve(state.selection().head(doc)).ok()?;
        (0..=resolved.depth())
            .rev()
            .map(|depth| resolved.node(depth).type_id())
            .find(|ty| self.is_list(*ty))
    }

    /// Whether the cursor sits in a table cell.
    fn in_table_cell(&self, state: &EditorState) -> bool {
        self.table_types()
            .and_then(|types| markraft_core::commands::cell_at(types, state))
            .is_some()
    }

    /// The innermost list item the cursor sits in, with the position before it.
    fn item_at_cursor(&self, state: &EditorState) -> Option<(NodeTypeId, Attrs, usize)> {
        let doc = state.doc();
        let resolved = doc.resolve(state.selection().head(doc)).ok()?;
        (1..=resolved.depth()).rev().find_map(|depth| {
            let node = resolved.node(depth);
            self.is_item(node.type_id())
                .then(|| (node.type_id(), node.attrs().clone(), resolved.before(depth)))
        })
    }
}

/// Enter.
pub(crate) fn enter(types: &DocTypes) -> Command {
    enter_with(types, None)
}

/// Enter, with each command that splits a textblock at the caret wrapped by
/// the document kind's [`SplitWrap`](crate::SplitWrap).
pub(crate) fn enter_with(types: &DocTypes, wrap: Option<&crate::SplitWrap>) -> Command {
    let splits = |command: Command| match wrap {
        Some(wrap) => wrap(command),
        None => command,
    };
    let mut list: Vec<Option<Command>> = vec![
        // Inside a table Enter moves down a row and appends one at the bottom.
        // The guard behind it is the invariant written down: a cell that split
        // would leave its row one cell wider than the rest.
        types.table_types().map(goto_cell_below),
        types.table_types().map(guard_cell_split),
        types.list_item.map(split_list_item).map(splits),
        types
            .task_item
            .map(|item| split_task_item(types, item))
            .map(splits),
    ];
    list.extend(per_item(types, lift_list_item));
    list.extend([
        Some(new_line_in_code()),
        Some(create_paragraph_near()),
        Some(lift_empty_block()),
        Some(splits(split_block_keep_marks())),
    ]);
    some(list)
}

/// Backspace at the start of an empty verbatim block: turn it into a paragraph.
///
/// A verbatim block keeps Enter for itself, so the key that would otherwise
/// delete nothing is the way out of an empty one. A raw block has no chrome of
/// its own, so an empty one is invisible as well as inescapable.
fn clear_empty_verbatim(types: &DocTypes) -> Command {
    let types = types.clone();
    command(move |state| {
        let paragraph = types.paragraph?;
        if !state.selection().is_cursor() || !types.in_verbatim_block_at(state) {
            return None;
        }
        let doc = state.doc();
        let resolved = doc.resolve(state.selection().head(doc)).ok()?;
        // Empty content leaves the cursor nowhere but the block's start.
        if resolved.parent().content_size() != 0 {
            return None;
        }
        set_block_type(paragraph, Attrs::empty())(state)
    })
}

/// Backspace.
pub(crate) fn backspace(types: &DocTypes) -> Command {
    let outdent = {
        let types = types.clone();
        let inner = some(per_item(&types, lift_list_item));
        when(move |state| types.at_item_start(state), inner)
    };
    some([
        // An edit reaching from one cell into another is refused outright; the
        // boundary guard then stops the chain before anything joins two cells.
        types.table_types().map(guard_cell_range),
        Some(undo_input_rule()),
        Some(delete_selection()),
        Some(demote_heading_at_start(types)),
        Some(lift_quote_at_start(types)),
        Some(delete_by_grapheme(Direction::Backward)),
        types.table_types().map(delete_empty_table),
        types.table_types().map(guard_cell_boundary),
        Some(outdent),
        Some(clear_empty_verbatim(types)),
        Some(join_backward()),
        Some(select_node_backward()),
    ])
}

/// Whether the cursor sits at the start of its textblock.
fn at_textblock_start(state: &EditorState) -> bool {
    let doc = state.doc();
    let selection = state.selection();
    if !selection.is_cursor() {
        return false;
    }
    let Ok(resolved) = doc.resolve(selection.head(doc)) else {
        return false;
    };
    let depth = (1..=resolved.depth())
        .rev()
        .find(|&d| resolved.node(d).is_textblock(state.schema()));
    let Some(depth) = depth else {
        return false;
    };
    let start = resolved.start(depth);
    let hidden = resolved.depth() - depth;
    resolved.pos().checked_sub(hidden) == Some(start)
}

/// At the start of a heading, Backspace lowers the level or turns it into a
/// paragraph — the editable ATX-prefix behaviour.
fn demote_heading_at_start(types: &DocTypes) -> Command {
    let types = types.clone();
    command(move |state| {
        if !at_textblock_start(state) {
            return None;
        }
        let heading = types.heading?;
        let paragraph = types.paragraph?;
        let (ty, attrs) = types.block_at_cursor(state)?;
        if ty != heading {
            return None;
        }
        let level = attrs.get("level").and_then(|v| v.as_int()).unwrap_or(1);
        if level <= 1 {
            return set_block_type(paragraph, Attrs::empty())(state);
        }
        set_block_type(heading, Attrs::from_pairs([("level", level - 1)]))(state)
    })
}

/// At the start of a quoted block, Backspace lifts one quote level.
fn lift_quote_at_start(types: &DocTypes) -> Command {
    let types = types.clone();
    command(move |state| {
        if !at_textblock_start(state) {
            return None;
        }
        let blockquote = types.blockquote?;
        let doc = state.doc();
        let head = state.selection().head(doc);
        let resolved = doc.resolve(head).ok()?;
        let in_quote =
            (1..=resolved.depth()).any(|depth| resolved.node(depth).type_id() == blockquote);
        if !in_quote {
            return None;
        }
        markraft_core::commands::lift()(state)
    })
}

/// Forward delete.
pub(crate) fn delete_forward(types: &DocTypes) -> Command {
    some([
        types.table_types().map(guard_cell_range),
        Some(delete_selection()),
        Some(delete_by_grapheme(Direction::Forward)),
        types.table_types().map(guard_cell_boundary),
        Some(join_forward()),
        Some(select_node_forward()),
    ])
}

pub(crate) fn delete_word(types: &DocTypes, dir: Direction) -> Command {
    some([
        types.table_types().map(guard_cell_range),
        Some(delete_selection()),
        Some(delete_by_word(dir)),
        types.table_types().map(guard_cell_boundary),
        Some(match dir {
            Direction::Backward => join_backward(),
            Direction::Forward => join_forward(),
        }),
    ])
}

pub(crate) fn move_grapheme(dir: Direction, extend: bool) -> Command {
    move_by_grapheme(dir, extend)
}

pub(crate) fn move_word(dir: Direction, extend: bool) -> Command {
    move_by_word(dir, extend)
}

/// ⌘↑ / ⌘↓ and their shifted forms.
pub(crate) fn move_document_edge(end: bool, extend: bool) -> Command {
    command(move |state| {
        let doc = state.doc();
        let target = if end { doc.content_size() } else { 0 };
        let head = Selection::near(state.schema(), doc, target, if end { -1 } else { 1 });
        let selection = if extend {
            Selection::text(state.selection().anchor(doc), head.head(doc))
        } else {
            head
        };
        if selection == *state.selection() {
            return None;
        }
        Some(
            TransactionSpec::new()
                .selection(selection)
                .user_event(if extend { "select" } else { "move" })
                .scroll_into_view(),
        )
    })
}

/// Tab: step to the next table cell, sink a list item, or indent inside a
/// verbatim block.
///
/// A cell holds inline content, so no verbatim block can sit in one and the two
/// never compete.
pub(crate) fn indent(types: &DocTypes) -> Command {
    let verbatim = {
        let types = types.clone();
        when(
            move |state| types.in_verbatim_block_at(state) && state.selection().is_cursor(),
            markraft_core::commands::insert_text("\t"),
        )
    };
    let mut list = vec![types.table_types().map(goto_next_cell)];
    list.extend(per_item(types, sink_list_item));
    list.push(Some(verbatim));
    some(list)
}

/// Shift-Tab: step to the previous table cell, lift a list item, or lift a
/// block out of its wrapper.
///
/// [`goto_prev_cell`] does not apply in the first cell, and lifting a cell out
/// of its row would leave that row short, so inside a table the rest of the
/// chain is skipped rather than run.
pub(crate) fn outdent(types: &DocTypes) -> Command {
    let mut tail = per_item(types, lift_list_item);
    tail.push(Some(lift()));
    let outside = {
        let types = types.clone();
        when(move |state| !types.in_table_cell(state), some(tail))
    };
    some([types.table_types().map(goto_prev_cell), Some(outside)])
}

/// Undo or redo, through the history extension.
pub(crate) fn history(undo: bool) -> Command {
    command(move |state| {
        if undo {
            markraft_core::undo(state)
        } else {
            markraft_core::redo(state)
        }
    })
}

/// Set a textblock type, or return to a paragraph when it is already that type.
pub(crate) fn toggle_block(types: &DocTypes, ty: NodeTypeId, attrs: Attrs) -> Command {
    let types = types.clone();
    command(move |state| {
        let paragraph = types.paragraph?;
        let current = types.block_at_cursor(state);
        let same = current
            .as_ref()
            .is_some_and(|(at, has)| *at == ty && *has == attrs);
        let target = if same { paragraph } else { ty };
        let attrs = if same { Attrs::empty() } else { attrs.clone() };
        set_block_type(target, attrs)(state)
    })
}

/// Wrap in a block quote, or lift out of the one the cursor sits in.
pub(crate) fn toggle_quote(types: &DocTypes) -> Command {
    let types = types.clone();
    command(move |state| {
        let blockquote = types.blockquote?;
        toggle_wrap_in(blockquote, Attrs::empty())(state)
    })
}

/// Wrap the selection in `ty` with `attrs`, or, where it already sits in one:
/// retype that node when its attributes differ, and lift out of it when they
/// do not.
///
/// The retyping case is what lets one wrapper have variants — a quote that is
/// a callout, say — without a second node type: asking for a variant the
/// cursor is not in changes the one it is in, and asking for the variant it is
/// already in takes the wrapper away, which is what a toggle means.
pub(crate) fn toggle_wrap_in(ty: NodeTypeId, attrs: Attrs) -> Command {
    command(move |state| {
        let doc = state.doc();
        let schema = state.schema();
        let resolved = doc.resolve(state.selection().head(doc)).ok()?;
        let depth = (0..=resolved.depth())
            .rev()
            .find(|d| resolved.node(*d).type_id() == ty);
        let Some(depth) = depth else {
            return wrap_in(ty, attrs.clone())(state);
        };
        let node = resolved.node(depth);
        let wanted = markup_of(schema, ty, &attrs);
        if node.attrs() == &wanted.attrs {
            return lift()(state);
        }
        let before = resolved.before(depth);
        let after = before + node.node_size() - 1;
        let markup = Markup {
            ty,
            attrs: wanted.attrs,
            marks: node.marks().clone(),
        };
        let changes = vec![
            Change::replace(
                before,
                before + 1,
                Slice::from_tokens(&[Token::Open(markup.clone())]),
            ),
            Change::replace(
                after,
                after + 1,
                Slice::from_tokens(&[Token::Close(markup)]),
            ),
        ];
        changes_spec(state, changes, "settype")
    })
}

/// Wrap in a list of `ty`, or leave the list when the cursor is already in one
/// of that type holding items of `item`.
pub(crate) fn toggle_list(types: &DocTypes, ty: NodeTypeId, item: NodeTypeId) -> Command {
    let types = types.clone();
    command(move |state| {
        let current = types.list_at_cursor(state);
        let current_item = types.item_at_cursor(state).map(|(ty, _, _)| ty);
        if current == Some(ty) && current_item == Some(item) {
            return lift_list_item(item)(state);
        }
        if current == Some(ty) && current_item.is_some() {
            // The same list type but the other kind of item: change the items
            // rather than nesting a second list.
            return convert_items(&types, item)(state);
        }
        composed(vec![
            wrap_in_list(ty, Attrs::empty()),
            convert_items(&types, item),
        ])(state)
    })
}

/// Turn the selected items into `item`, preserving their content and selection.
fn convert_items(types: &DocTypes, item: NodeTypeId) -> Command {
    let types = types.clone();
    command(move |state| {
        let doc = state.doc();
        let selection = state.selection();
        let from = selection.from(doc);
        let to = selection.to(doc);
        let projection = projection_of(state);
        let mut positions = std::collections::BTreeSet::new();
        for line in projection.lines() {
            if line.to < from || line.from > to || (from != to && line.from == to) {
                continue;
            }
            if let Some(ancestor) = line
                .ancestors
                .iter()
                .rev()
                .find(|a| types.is_item(a.node_type))
            {
                positions.insert(ancestor.before);
            }
        }
        let mut changes = Vec::new();
        for before in positions {
            let node = doc.node_at(before)?;
            if node.type_id() == item {
                continue;
            }
            let replaced = state
                .schema()
                .create(
                    item,
                    state.schema().node_type(item).default_attrs().clone(),
                    node.marks().clone(),
                    node.content().clone(),
                )
                .ok()?;
            // Replace only the boundary tokens. Nested selected items then
            // produce disjoint changes and retain the original content mapping.
            let markup = replaced.markup().clone();
            changes.push(Change::replace(
                before,
                before + 1,
                markraft_core::Slice::from_tokens(&[markraft_core::Token::Open(markup.clone())]),
            ));
            let end = before + node.node_size();
            changes.push(Change::replace(
                end - 1,
                end,
                markraft_core::Slice::from_tokens(&[markraft_core::Token::Close(markup)]),
            ));
        }
        if changes.is_empty() {
            return None;
        }
        markraft_core::commands::changes_spec(state, changes, "format.block")
            .map(|spec| spec.selection(selection.clone()))
    })
}

/// ⌘⏎: add a row below in a table, tick or clear the task box the cursor sits
/// in, else leave a code block.
pub(crate) fn toggle_task(types: &DocTypes) -> Command {
    let types = types.clone();
    command(move |state| {
        if let Some(table) = types.table_types().filter(|_| types.in_table_cell(state)) {
            return add_row_after(table)(state);
        }
        let task = types.task_item?;
        let Some((ty, attrs, before)) = types.item_at_cursor(state) else {
            return exit_code()(state);
        };
        if ty != task {
            return exit_code()(state);
        }
        let node = state.doc().node_at(before)?;
        let checked = DocTypes::task_checked(&attrs);
        let updated = node.with_attrs(attrs.with("checked", AttrValue::Bool(!checked)));
        let slice =
            markraft_core::Slice::from_fragment(markraft_core::Fragment::from_node(updated));
        // The node keeps its size, so the caret keeps its position.
        markraft_core::commands::changes_spec(
            state,
            vec![Change::replace(before, before + node.node_size(), slice)],
            "format.block",
        )
        .map(|spec| spec.selection(state.selection().clone()))
    })
}

/// Insert literal text, turning its line endings into block breaks.
///
/// A line ending is a block break everywhere but in a verbatim block, where it
/// is the character it looks like. The multi-block form is an open slice, so its
/// first and last paragraphs merge with the block the caret sits in exactly as
/// a paste of the same shape would.
pub(crate) fn insert_plain(types: &DocTypes, text: &str) -> Command {
    let guard = types.table_types().map(guard_cell_range);
    let types = types.clone();
    let text = text.to_owned();
    let insert = command(move |state| {
        if text == "#"
            && let Some(spec) = promote_heading_at_start(&types)(state)
        {
            return Some(spec);
        }
        if !text.contains('\n') || types.in_verbatim_block_at(state) {
            return markraft_core::commands::insert_text(&text)(state);
        }
        let schema = state.schema();
        let paragraph = types.paragraph?;
        let attrs = schema.node_type(paragraph).default_attrs().clone();
        let nodes: Option<Vec<_>> = text
            .split('\n')
            .map(|part| {
                let content = if part.is_empty() {
                    markraft_core::Fragment::empty()
                } else {
                    markraft_core::Fragment::from_node(schema.text(part))
                };
                schema
                    .create(
                        paragraph,
                        attrs.clone(),
                        markraft_core::MarkSet::empty(),
                        content,
                    )
                    .ok()
            })
            .collect();
        let slice = markraft_core::Slice::new(markraft_core::Fragment::from_nodes(nodes?), 1, 1);
        markraft_core::commands::replace_selection(slice)(state)
    });
    // Typing over a selection that reaches out of one cell would merge the
    // cells it spans, so that edit is refused before anything else is tried.
    some([guard, Some(insert)])
}

/// At the start of a heading, typing `#` raises the level (up to 6).
fn promote_heading_at_start(types: &DocTypes) -> Command {
    let types = types.clone();
    command(move |state| {
        if !at_textblock_start(state) {
            return None;
        }
        let heading = types.heading?;
        let (ty, attrs) = types.block_at_cursor(state)?;
        if ty != heading {
            return None;
        }
        let level = attrs.get("level").and_then(|v| v.as_int()).unwrap_or(1);
        if level >= 6 {
            return None;
        }
        set_block_type(heading, Attrs::from_pairs([("level", level + 1)]))(state)
    })
}

/// ⌘A: the verbatim block the cursor sits in first, then the whole document.
pub(crate) fn select_all(types: &DocTypes) -> Command {
    let types = types.clone();
    command(move |state| {
        let doc = state.doc();
        if types.in_verbatim_block_at(state) {
            let resolved = doc.resolve(state.selection().head(doc)).ok()?;
            let (from, to) = (
                resolved.start(resolved.depth()),
                resolved.end(resolved.depth()),
            );
            let selection = Selection::text(from, to);
            if selection != *state.selection() && !(from == 0 && to == doc.content_size()) {
                return Some(
                    TransactionSpec::new()
                        .selection(selection)
                        .user_event("select"),
                );
            }
        }
        markraft_core::commands::select_all()(state)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typeahead::tests::{at, state_of, types_of};
    use markraft_commonmark::{schema as md, to_markdown};
    use markraft_core::commands::{delete_range, run_command};
    use markraft_core::projection::projection_of;

    /// Run `command` and give back the Markdown it leaves, or `None` when the
    /// command does not apply.
    fn after(state: &EditorState, command: &Command) -> Option<String> {
        let tr = run_command(state, command)?.expect("a transaction");
        Some(to_markdown(state.schema(), tr.state().doc()))
    }

    /// The state a command leaves, or `None` when it does not apply.
    fn applied(state: &EditorState, command: &Command) -> Option<EditorState> {
        Some(
            run_command(state, command)?
                .expect("a transaction")
                .state()
                .clone(),
        )
    }

    /// The row and column the caret sits at, or `None` outside a table.
    fn cell_of(state: &EditorState) -> Option<(usize, usize)> {
        let types = types_of(state).table_types()?;
        markraft_core::commands::cell_at(types, state).map(|at| (at.row, at.column))
    }

    /// A two-by-two table, and the Markdown it reads back as.
    fn table_state() -> (EditorState, String) {
        let state = state_of("| a | b |\n| - | - |\n| c | d |");
        let markdown = to_markdown(state.schema(), state.doc());
        (state, markdown)
    }

    /// The caret at the start of the line holding `needle`.
    fn caret_in(state: &EditorState, needle: &str) -> usize {
        let projection = projection_of(state);
        let line = projection
            .lines()
            .iter()
            .find(|line| {
                projection
                    .line_text(projection.line_at(line.from).expect("a line"))
                    .is_some_and(|text| text == needle)
            })
            .expect("a line holding the text");
        line.from
    }

    #[test]
    fn enter_splits_a_list_item_and_leaves_the_list_from_an_empty_one() {
        let state = state_of("- one");
        let state = at(&state, caret_in(&state, "one") + 3);
        let split = after(&state, &enter(&types_of(&state))).expect("the split applies");
        assert_eq!(split, "- one\n- ");
        // Enter again, in the now empty item, leaves the list: the empty block
        // becomes a paragraph. CommonMark has no empty-paragraph spelling, so
        // the file just ends after the list's blank separator.
        let state = state_of("- one\n-\n");
        let state = at(&state, projection_of(&state).lines()[1].to);
        let lifted = after(&state, &enter(&types_of(&state))).expect("the lift applies");
        assert_eq!(lifted, "- one");
    }

    /// With the document kind's split wrapper, Enter inside a style keeps it on
    /// both sides — in a list item as in a paragraph.
    #[test]
    fn enter_with_a_kinds_split_keeps_the_style_open_at_the_caret() {
        let wrap: crate::SplitWrap = std::sync::Arc::new(markraft_commonmark::keeping_styles);
        for (source, line, expected) in [
            ("**abcd**", "**abcd**", "**ab**\n\n**cd**"),
            ("- **abcd**", "**abcd**", "- **ab**\n- **cd**"),
        ] {
            let state = state_of(source);
            let state = at(&state, caret_in(&state, line) + 4);
            let enter = enter_with(&types_of(&state), Some(&wrap));
            assert_eq!(after(&state, &enter).as_deref(), Some(expected), "{source}");
        }
    }

    #[test]
    fn backspace_at_a_list_items_start_outdents_before_it_leaves_the_list() {
        // A nested item outdents one level first.
        let state = state_of("- one\n  - two");
        let state = at(&state, caret_in(&state, "two"));
        let outdented = after(&state, &backspace(&types_of(&state))).expect("the outdent applies");
        assert_eq!(outdented, "- one\n- two");
        // A top-level item then leaves the list altogether.
        let state = state_of("- one\n- two");
        let state = at(&state, caret_in(&state, "two"));
        let lifted = after(&state, &backspace(&types_of(&state))).expect("the lift applies");
        assert_eq!(lifted, "- one\n\ntwo");
    }

    #[test]
    fn backspace_inside_a_block_deletes_one_grapheme_and_joins_at_its_start() {
        let state = state_of("ab");
        let state = at(&state, 3);
        assert_eq!(
            after(&state, &backspace(&types_of(&state))).as_deref(),
            Some("a")
        );
        // A grapheme cluster goes as a whole.
        let state = state_of("a👩‍👩‍👧");
        let end = projection_of(&state).lines()[0].to;
        let state = at(&state, end);
        assert_eq!(
            after(&state, &backspace(&types_of(&state))).as_deref(),
            Some("a")
        );
        // At a paragraph's start the two blocks join instead.
        let state = state_of("one\n\ntwo");
        let state = at(&state, caret_in(&state, "two"));
        assert_eq!(
            after(&state, &backspace(&types_of(&state))).as_deref(),
            Some("onetwo")
        );
    }

    #[test]
    fn tab_and_shift_tab_sink_and_lift_a_list_item() {
        let state = state_of("- one\n- two");
        let state = at(&state, caret_in(&state, "two"));
        let sunk = after(&state, &indent(&types_of(&state))).expect("the sink applies");
        assert_eq!(sunk, "- one\n  - two");
        let state = state_of("- one\n  - two");
        let state = at(&state, caret_in(&state, "two"));
        let lifted = after(&state, &outdent(&types_of(&state))).expect("the lift applies");
        assert_eq!(lifted, "- one\n- two");
    }

    /// Tab walks a table in row-major order and grows it rather than falling
    /// out of it, which is what it does in Typora and Bear.
    #[test]
    fn tab_walks_the_cells_and_appends_a_row_past_the_last_one() {
        let (state, markdown) = table_state();
        let types = types_of(&state);
        let lines = projection_of(&state);
        let (first, last) = (lines.lines()[0].from, lines.lines()[3].to);
        let stepped = applied(&at(&state, first), &indent(&types)).expect("Tab steps right");
        assert_eq!(to_markdown(state.schema(), stepped.doc()), markdown);
        assert_eq!(cell_of(&stepped), Some((0, 1)));
        let grown = applied(&at(&state, last), &indent(&types)).expect("Tab grows the table");
        assert_eq!(projection_of(&grown).lines().len(), 6, "a row was appended");
        assert_eq!(cell_of(&grown), Some((2, 0)));
        // ⇧Tab steps back, and stops rather than lifting the first cell out of
        // its row, which would leave that row one cell short.
        let back =
            applied(&at(&state, lines.lines()[1].from), &outdent(&types)).expect("⇧Tab steps left");
        assert_eq!(cell_of(&back), Some((0, 0)));
        let stopped = applied(&at(&state, first), &outdent(&types));
        assert!(stopped.is_none(), "⇧Tab in the first cell does nothing");
    }

    /// A cell that split would leave its row one cell wider than the rest, so
    /// Enter moves down a row instead, appending one at the bottom.
    #[test]
    fn enter_moves_down_a_row_and_never_splits_a_cell() {
        let (state, markdown) = table_state();
        let types = types_of(&state);
        let lines = projection_of(&state);
        let inside = lines.lines()[0].to;
        let moved = applied(&at(&state, inside), &enter(&types)).expect("Enter applies");
        assert_eq!(
            to_markdown(state.schema(), moved.doc()),
            markdown,
            "nothing was split"
        );
        assert_eq!(cell_of(&moved), Some((1, 0)));
        let grown = applied(&at(&state, lines.lines()[3].to), &enter(&types)).expect("Enter");
        assert_eq!(projection_of(&grown).lines().len(), 6, "a row was appended");
        assert_eq!(cell_of(&grown), Some((2, 1)));
        // ⌘⏎ adds a row under the caret's own row rather than at the bottom,
        // and leaves the caret in the cell it was typing in.
        let added = applied(&at(&state, inside), &toggle_task(&types)).expect("⌘⏎ adds a row");
        assert_eq!(projection_of(&added).lines().len(), 6);
        assert_eq!(cell_of(&added), Some((0, 0)));
    }

    /// Joining across a cell boundary would merge two cells and leave their
    /// rows short, so a deletion that reaches one stops there.
    #[test]
    fn backspace_stops_at_a_cell_boundary_but_still_deletes_inside_one() {
        let (state, markdown) = table_state();
        let types = types_of(&state);
        let lines = projection_of(&state);
        let (start, end) = (lines.lines()[1].from, lines.lines()[1].to);
        let stopped = applied(&at(&state, start), &backspace(&types)).expect("the guard applies");
        assert_eq!(to_markdown(state.schema(), stopped.doc()), markdown);
        assert_eq!(cell_of(&stopped), Some((0, 1)), "and the caret stays put");
        // Forward delete stops at the other edge of the same cell.
        let stopped =
            applied(&at(&state, end), &delete_forward(&types)).expect("the guard applies");
        assert_eq!(to_markdown(state.schema(), stopped.doc()), markdown);
        // Inside the cell both still take a character.
        let deleted = applied(&at(&state, end), &backspace(&types)).expect("a grapheme goes");
        assert_ne!(to_markdown(state.schema(), deleted.doc()), markdown);
        let deleted = applied(&at(&state, start), &delete_forward(&types)).expect("one goes");
        assert_ne!(to_markdown(state.schema(), deleted.doc()), markdown);
        // A selection reaching out of the cell is refused outright.
        let across = state
            .update([TransactionSpec::new()
                .selection(Selection::text(lines.lines()[0].from, lines.lines()[1].to))])
            .expect("a selection")
            .state()
            .clone();
        let refused = applied(&across, &backspace(&types)).expect("the guard applies");
        assert_eq!(to_markdown(state.schema(), refused.doc()), markdown);
        let typed = applied(&across, &insert_plain(&types, "x")).expect("the guard applies");
        assert_eq!(to_markdown(state.schema(), typed.doc()), markdown);
    }

    /// Backspace at the start of a table nobody has typed in yet takes the
    /// table, which is the one case where it means the grid and not a letter.
    #[test]
    fn backspace_at_the_start_of_an_empty_table_takes_it() {
        let state = state_of("|   |   |\n| - | - |");
        let types = types_of(&state);
        let start = projection_of(&state).lines()[0].from;
        let taken = applied(&at(&state, start), &backspace(&types)).expect("the table goes");
        assert_eq!(to_markdown(state.schema(), taken.doc()), "");
    }

    #[test]
    fn a_block_type_toggles_back_to_a_paragraph() {
        let state = state_of("text");
        let types = types_of(&state);
        let heading = state.schema().node_id(md::HEADING).unwrap();
        let level = Attrs::from_pairs([("level", 1i64)]);
        let command = toggle_block(&types, heading, level.clone());
        assert_eq!(after(&state, &command).as_deref(), Some("# text"));
        let state = state_of("# text");
        assert_eq!(after(&state, &command).as_deref(), Some("text"));
        // A different level sets rather than clears.
        let two = toggle_block(&types, heading, Attrs::from_pairs([("level", 2i64)]));
        assert_eq!(after(&state, &two).as_deref(), Some("## text"));
    }

    #[test]
    fn backspace_at_heading_start_demotes_and_hash_promotes() {
        let state = state_of("## title");
        let types = types_of(&state);
        let start = projection_of(&state).lines()[0].from;
        let demoted = applied(&at(&state, start), &backspace(&types)).expect("demotes");
        assert_eq!(to_markdown(state.schema(), demoted.doc()), "# title");
        let head = demoted.selection().head(demoted.doc());
        let promoted = applied(&at(&demoted, head), &insert_plain(&types, "#")).expect("promotes");
        assert_eq!(to_markdown(state.schema(), promoted.doc()), "## title");
        let h1 = state_of("# title");
        let cleared = applied(
            &at(&h1, projection_of(&h1).lines()[0].from),
            &backspace(&types_of(&h1)),
        )
        .expect("clears to paragraph");
        assert_eq!(to_markdown(state.schema(), cleared.doc()), "title");
    }

    #[test]
    fn backspace_at_quote_start_lifts() {
        let state = state_of("> quoted");
        let types = types_of(&state);
        let start = projection_of(&state).lines()[0].from;
        let lifted = applied(&at(&state, start), &backspace(&types)).expect("lifts");
        assert_eq!(to_markdown(state.schema(), lifted.doc()), "quoted");
    }

    #[test]
    fn a_list_toggles_off_and_switches_item_kind_in_place() {
        let state = state_of("text");
        let types = types_of(&state);
        let bullet = state.schema().node_id(md::BULLET_LIST).unwrap();
        let item = state.schema().node_id(md::LIST_ITEM).unwrap();
        let task = state.schema().node_id(md::TASK_ITEM).unwrap();
        let bullets = toggle_list(&types, bullet, item);
        assert_eq!(after(&state, &bullets).as_deref(), Some("- text"));
        let state = state_of("- text");
        assert_eq!(after(&state, &bullets).as_deref(), Some("text"));
        // The same list with the other item kind converts in place.
        let tasks = toggle_list(&types, bullet, task);
        assert_eq!(after(&state, &tasks).as_deref(), Some("- [ ] text"));
    }

    #[test]
    fn a_paragraph_can_be_wrapped_directly_in_a_task_list() {
        let state = state_of("text");
        let types = types_of(&state);
        let command = toggle_list(&types, types.bullet_list.unwrap(), types.task_item.unwrap());
        assert_eq!(after(&state, &command).as_deref(), Some("- [ ] text"));
        let state = state_of("one\n\ntwo");
        let state = state
            .update([TransactionSpec::new().selection(Selection::All)])
            .unwrap()
            .state()
            .clone();
        assert_eq!(
            after(&state, &command).as_deref(),
            Some("- [ ] one\n- [ ] two")
        );
    }

    #[test]
    fn enter_in_an_empty_completed_task_leaves_the_list() {
        let state = state_of("- [x] ");
        let state = at(&state, projection_of(&state).lines()[0].from);
        assert_eq!(
            after(&state, &enter(&types_of(&state))).as_deref(),
            Some("")
        );
    }

    #[test]
    fn the_task_box_toggles_and_a_code_block_is_left_instead() {
        let state = state_of("- [ ] text");
        let types = types_of(&state);
        let state = at(&state, caret_in(&state, "text"));
        assert_eq!(
            after(&state, &toggle_task(&types)).as_deref(),
            Some("- [x] text")
        );
        // Outside a task item the same key leaves a code block: a new, empty
        // block after it. Empty paragraphs have no Markdown spelling.
        let state = state_of("```\ncode\n```");
        let end = projection_of(&state).lines()[0].to;
        let state = at(&state, end);
        assert_eq!(
            after(&state, &toggle_task(&types_of(&state))).as_deref(),
            Some("```\ncode\n```")
        );
    }

    /// The caret at `offset` characters into the document's first line.
    fn offset_in_first_line(state: &EditorState, offset: usize) -> EditorState {
        let line = projection_of(state).lines()[0].clone();
        at(
            state,
            line.offset_to_pos(offset).expect("an offset in the line"),
        )
    }

    /// A raw block holds its source as text, so Enter writes the line ending it
    /// looks like rather than splitting the block in two.
    #[test]
    fn enter_in_a_raw_block_writes_a_newline_and_keeps_the_block() {
        let state = state_of("<div>\nab\n</div>");
        let raw = state.doc().child(0).type_id();
        let state = offset_in_first_line(&state, "<div>\na".chars().count());
        let split = applied(&state, &enter(&types_of(&state))).expect("Enter applies");
        assert_eq!(
            to_markdown(state.schema(), split.doc()),
            "<div>\na\nb\n</div>"
        );
        assert_eq!(split.doc().child_count(), 1, "still one block");
        assert_eq!(split.doc().child(0).type_id(), raw, "and still the raw one");
    }

    /// Typing past the last character of a raw block stays in it: there is no
    /// chrome to fall out of, so the text simply grows.
    #[test]
    fn typing_at_the_end_of_a_raw_block_stays_inside_it() {
        let state = state_of("<div>\nab\n</div>");
        let raw = state.doc().child(0).type_id();
        let state = at(&state, projection_of(&state).lines()[0].to);
        let typed = applied(&state, &insert_plain(&types_of(&state), "\nc"))
            .expect("the insertion applies");
        assert_eq!(
            to_markdown(state.schema(), typed.doc()),
            "<div>\nab\n</div>\nc"
        );
        assert_eq!(typed.doc().child_count(), 1, "the newline stayed literal");
        assert_eq!(typed.doc().child(0).type_id(), raw);
    }

    /// An emptied raw block draws nothing at all, so Backspace — the key that
    /// would otherwise delete nothing — is the way out of one.
    #[test]
    fn backspace_in_an_empty_raw_block_leaves_a_paragraph() {
        let state = state_of("<div>");
        let types = types_of(&state);
        let line = projection_of(&state).lines()[0].clone();
        let emptied = applied(&state, &delete_range(line.from, line.to)).expect("the text goes");
        let emptied = at(&emptied, projection_of(&emptied).lines()[0].from);
        let cleared = applied(&emptied, &backspace(&types)).expect("Backspace applies");
        assert_eq!(cleared.doc().child_count(), 1);
        assert_eq!(Some(cleared.doc().child(0).type_id()), types.paragraph);
        // A raw block with text in it still loses one character at a time.
        let state = at(&state, line.to);
        let deleted = applied(&state, &backspace(&types)).expect("a grapheme goes");
        assert_eq!(to_markdown(state.schema(), deleted.doc()), "<div");
    }

    #[test]
    fn literal_text_splits_into_blocks_but_stays_literal_in_code() {
        let state = state_of("");
        let types = types_of(&state);
        assert_eq!(
            after(&state, &insert_plain(&types, "one\ntwo")).as_deref(),
            Some("one\n\ntwo")
        );
        let state = state_of("```\n\n```");
        let state = at(&state, projection_of(&state).lines()[0].to);
        assert_eq!(
            after(&state, &insert_plain(&types_of(&state), "a\nb")).as_deref(),
            Some("```\na\nb\n```")
        );
    }
}
