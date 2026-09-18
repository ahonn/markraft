//! What each bound action does, as a chain of catalogue commands.
//!
//! The chains mirror ProseMirror's base and list keymaps, with the two
//! departures the editor's own tests describe: Backspace at the start of a list
//! item outdents before it lifts or joins, and Enter in an empty list item
//! leaves the list.

use crate::types::DocTypes;
use markraft_core::commands::{
    Command, Direction, chain, command, create_paragraph_near, delete_by_grapheme, delete_by_word,
    delete_selection, exit_code, join_backward, join_forward, lift, lift_empty_block,
    lift_list_item, move_by_grapheme, move_by_word, new_line_in_code, select_node_backward,
    select_node_forward, set_block_type, sink_list_item, split_block_keep_marks, split_list_item,
    toggle_mark, undo_input_rule, wrap_in, wrap_in_list,
};
use markraft_core::projection::projection_of;
use markraft_core::{
    AttrValue, Attrs, Change, EditorState, NodeTypeId, Selection, TransactionSpec,
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

    /// Whether the cursor sits inside a code block.
    fn in_code_block(&self, state: &EditorState) -> bool {
        let doc = state.doc();
        doc.resolve(state.selection().head(doc))
            .is_ok_and(|resolved| Some(resolved.parent().type_id()) == self.code_block)
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
    let mut list: Vec<Option<Command>> = vec![
        types.list_item.map(split_list_item),
        types.task_item.map(|item| split_task_item(types, item)),
    ];
    list.extend(per_item(types, lift_list_item));
    list.extend([
        Some(new_line_in_code()),
        Some(create_paragraph_near()),
        Some(lift_empty_block()),
        Some(split_block_keep_marks()),
    ]);
    some(list)
}

/// Backspace.
pub(crate) fn backspace(types: &DocTypes) -> Command {
    let outdent = {
        let types = types.clone();
        let inner = some(per_item(&types, lift_list_item));
        when(move |state| types.at_item_start(state), inner)
    };
    chain([
        undo_input_rule(),
        delete_selection(),
        delete_by_grapheme(Direction::Backward),
        outdent,
        join_backward(),
        select_node_backward(),
    ])
}

/// Forward delete.
pub(crate) fn delete_forward() -> Command {
    chain([
        delete_selection(),
        delete_by_grapheme(Direction::Forward),
        join_forward(),
        select_node_forward(),
    ])
}

pub(crate) fn delete_word(dir: Direction) -> Command {
    chain([
        delete_selection(),
        delete_by_word(dir),
        match dir {
            Direction::Backward => join_backward(),
            Direction::Forward => join_forward(),
        },
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

/// Tab: sink a list item, or indent inside a code block.
pub(crate) fn indent(types: &DocTypes) -> Command {
    let code = {
        let types = types.clone();
        when(
            move |state| types.in_code_block(state) && state.selection().is_cursor(),
            markraft_core::commands::insert_text("\t"),
        )
    };
    let mut list = per_item(types, sink_list_item);
    list.push(Some(code));
    some(list)
}

/// Shift-Tab: lift a list item, or lift a block out of its wrapper.
pub(crate) fn outdent(types: &DocTypes) -> Command {
    let mut list = per_item(types, lift_list_item);
    list.push(Some(lift()));
    some(list)
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

/// Toggle a mark over the selection.
pub(crate) fn mark(ty: Option<markraft_core::MarkTypeId>) -> Command {
    match ty {
        Some(ty) => toggle_mark(ty, Attrs::empty()),
        None => command(|_| None),
    }
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
        let doc = state.doc();
        let inside = doc
            .resolve(state.selection().head(doc))
            .is_ok_and(|resolved| {
                (0..=resolved.depth()).any(|d| resolved.node(d).type_id() == blockquote)
            });
        if inside {
            lift()(state)
        } else {
            wrap_in(blockquote, Attrs::empty())(state)
        }
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

/// ⌘⏎: tick or clear the task box the cursor sits in, else leave a code block.
pub(crate) fn toggle_task(types: &DocTypes) -> Command {
    let types = types.clone();
    command(move |state| {
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
/// A line ending is a block break everywhere but in a code block, where it is
/// the character it looks like. The multi-block form is an open slice, so its
/// first and last paragraphs merge with the block the caret sits in exactly as
/// a paste of the same shape would.
pub(crate) fn insert_plain(types: &DocTypes, text: &str) -> Command {
    let types = types.clone();
    let text = text.to_owned();
    command(move |state| {
        if !text.contains('\n') || types.in_code_block(state) {
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
    })
}

/// ⌘A: the code block the cursor sits in first, then the whole document.
pub(crate) fn select_all(types: &DocTypes) -> Command {
    let types = types.clone();
    command(move |state| {
        let doc = state.doc();
        if types.in_code_block(state) {
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
    use markraft_core::commands::run_command;
    use markraft_core::projection::projection_of;

    /// Run `command` and give back the Markdown it leaves, or `None` when the
    /// command does not apply.
    fn after(state: &EditorState, command: &Command) -> Option<String> {
        let tr = run_command(state, command)?.expect("a transaction");
        Some(to_markdown(state.schema(), tr.state().doc()))
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
        // becomes a paragraph, which the codec writes as a `<br>` line.
        let state = state_of("- one\n-\n");
        let state = at(&state, projection_of(&state).lines()[1].to);
        let lifted = after(&state, &enter(&types_of(&state))).expect("the lift applies");
        assert_eq!(lifted, "- one\n\n<br>");
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
        // block after it, which the codec writes as a `<br>` line.
        let state = state_of("```\ncode\n```");
        let end = projection_of(&state).lines()[0].to;
        let state = at(&state, end);
        assert_eq!(
            after(&state, &toggle_task(&types_of(&state))).as_deref(),
            Some("```\ncode\n```\n\n<br>")
        );
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
