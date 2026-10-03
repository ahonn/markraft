//! Editing commands: pure functions from a state to a described edit.
//!
//! A [`Command`] takes an [`EditorState`] and returns the
//! [`TransactionSpec`] that performs its edit, or `None` when it does not apply
//! to that state. Nothing is dispatched: running a command is
//! [`run_command`], which is `state.update([spec])`. That keeps commands
//! testable, composable and free of any view.
//!
//! ```
//! # use markraft_core::*;
//! # use markraft_core::commands::*;
//! # let schema = Schema::new(SchemaSpec::new()
//! #     .node(NodeTypeSpec::new("doc", "block+"))
//! #     .node(NodeTypeSpec::new("paragraph", "inline*").group("block"))
//! #     .node(NodeTypeSpec::text("text").group("inline"))).unwrap();
//! let state = EditorState::create(EditorStateConfig::new(schema)).unwrap();
//! let typing = insert_text("hi");
//! let tr = run_command(&state, &typing).unwrap().unwrap();
//! assert_eq!(tr.new_doc().content_size(), 4);
//! ```
//!
//! # Composition
//!
//! [`chain`] runs a list of commands in order and takes the first that applies,
//! which is how a key binding like Enter is expressed: split a list item, else
//! lift an empty block, else split the block.
//!
//! # Annotations
//!
//! Every command annotates its spec with the [`user_event`](crate::protocol::user_event)
//! that describes it — one of the names in
//! [`protocol::event`](crate::protocol::event) — so the undo history groups
//! edits by what the user did, and asks the view to scroll where a user would
//! expect the caret to be visible.
//!
//! # Validity
//!
//! A command that changes the document builds and applies its change set before
//! returning, and reports `None` rather than a spec that would produce a
//! document [`Node::check`] rejects. Commands are therefore safe to chain
//! blindly, at the cost of one application and one validation per attempt.
//!
//! # Tables
//!
//! The table commands have no cell selection. Moving past the last cell grows
//! the table: [`goto_next_cell`] (Tab) and [`goto_cell_below`] (Enter) add a
//! row there.

mod blocks;
mod general;
mod input_rules;
mod list;
mod marks;
mod motion;
pub mod structure;
mod table;
mod text;

pub use blocks::{
    create_paragraph_near, exit_code, lift, lift_empty_block, new_line_in_code, set_block_type,
    split_block, split_block_keep_marks, wrap_in,
};
pub use general::{
    delete_selection, join_backward, join_down, join_forward, join_textblock_backward,
    join_textblock_forward, join_up, select_all, select_node_backward, select_node_forward,
    select_parent_node, select_textblock_end, select_textblock_start,
};
pub use input_rules::{
    InputRule, InputRuleHandler, InputRuleMatch, InputRuleMatcher, InputRuleUndo, input_rule,
    input_rule_field, input_rules, undo_input_rule,
};
pub use list::{lift_list_item, sink_list_item, split_list_item, wrap_in_list};
pub use marks::{mark_applies, range_has_mark, remove_mark, set_mark, toggle_mark};
pub use motion::{
    Direction, delete_by, delete_by_grapheme, delete_by_word, move_by, move_by_grapheme,
    move_by_word,
};
pub use table::{
    CellPos, ColumnAlignment, TableTypes, add_column_after, add_column_before, add_row_after,
    add_row_before, cell_at, column_alignments, delete_column, delete_empty_table, delete_row,
    delete_table, exit_table_below, goto_cell_above, goto_cell_below, goto_next_cell,
    goto_prev_cell, insert_row_below, insert_table, set_column_alignment, spans_cells,
    table_invariant,
};
pub use text::{
    delete_range, delete_range_changes, insert_hard_break, insert_node, insert_text,
    replace_selection, replace_selection_changes, replace_selection_with_event,
};

use std::sync::Arc;

use crate::change::{Change, ChangeSet};
use crate::node::Node;
use crate::pos::ResolvedPos;
use crate::schema::Schema;
use crate::state::{EditorState, StateError, Transaction, TransactionSpec};

/// A pure editing command.
///
/// Returns the edit to perform, or `None` when the command does not apply to
/// the given state. A command never mutates anything and never dispatches.
pub type Command = Arc<dyn Fn(&EditorState) -> Option<TransactionSpec> + Send + Sync>;

/// Wrap a closure as a [`Command`].
pub fn command(
    f: impl Fn(&EditorState) -> Option<TransactionSpec> + Send + Sync + 'static,
) -> Command {
    Arc::new(f)
}

/// A command that tries each of `commands` in order and takes the first that
/// applies.
pub fn chain(commands: impl IntoIterator<Item = Command>) -> Command {
    let commands: Vec<Command> = commands.into_iter().collect();
    Arc::new(move |state| commands.iter().find_map(|command| command(state)))
}

/// Run a command against a state.
///
/// `None` means the command did not apply. `Some(Err(..))` means it applied but
/// a configured filter or extender made the resulting transaction impossible to
/// build.
pub fn run_command(
    state: &EditorState,
    command: &Command,
) -> Option<Result<Transaction, StateError>> {
    command(state).map(|spec| state.update([spec]))
}

/// Run a command and the transactions its appenders produce.
///
/// The counterpart of [`run_command`] for a view layer, which has to see every
/// step. See [`EditorState::update_with_appended`].
pub fn run_command_with_appended(
    state: &EditorState,
    command: &Command,
) -> Option<Result<Vec<Transaction>, StateError>> {
    command(state).map(|spec| state.update_with_appended([spec]))
}

/// Build a change set from `changes` and check that it produces a document the
/// schema accepts.
///
/// Returns the set and the resulting document, or `None` when the changes
/// cannot be expressed or would break the schema.
pub(crate) fn resolve_changes(
    state: &EditorState,
    changes: Vec<Change>,
) -> Option<(ChangeSet, Node)> {
    let set = ChangeSet::create(state.schema(), state.doc(), changes).ok()?;
    if set.is_empty() {
        return None;
    }
    let doc = set.apply(state.doc()).ok()?;
    doc.check_from(state.doc(), state.schema()).ok()?;
    Some((set, doc))
}

/// Like [`resolve_changes`], but for an edit that has to be expressed as
/// several rounds because one round's changes would overlap.
///
/// Each round is resolved against the document the previous ones produce and
/// the sets are composed, so the result is still one change set.
pub(crate) fn resolve_rounds(
    state: &EditorState,
    rounds: Vec<Vec<Change>>,
) -> Option<(ChangeSet, Node)> {
    let schema = state.schema();
    let mut doc = state.doc().clone();
    let mut acc: Option<ChangeSet> = None;
    for changes in rounds {
        if changes.is_empty() {
            continue;
        }
        let set = ChangeSet::create(schema, &doc, changes).ok()?;
        doc = set.apply(&doc).ok()?;
        acc = Some(match acc {
            Some(previous) => previous.compose(&set).ok()?,
            None => set,
        });
    }
    let set = acc?;
    if set.is_empty() {
        return None;
    }
    // Only the final document has to be valid — a round may pass through a
    // shape the schema rejects — so it is checked against the one document
    // known to be valid, which it shares every untouched subtree with.
    doc.check_from(state.doc(), schema).ok()?;
    Some((set, doc))
}

/// A spec performing `changes`, or `None` when they cannot be expressed or
/// would leave a document the schema rejects.
///
/// The escape hatch for a host whose edit has no command of its own — setting a
/// link's href, ticking a task item's box — so that such an edit is still
/// validated and annotated the way every catalogue command is.
pub fn changes_spec(
    state: &EditorState,
    changes: Vec<Change>,
    event: &str,
) -> Option<TransactionSpec> {
    let (set, _) = resolve_changes(state, changes)?;
    Some(
        TransactionSpec::new()
            .change_set(set)
            .user_event(event)
            .scroll_into_view(),
    )
}

/// The resolved cursor of an empty text selection.
pub(crate) fn cursor(state: &EditorState) -> Option<ResolvedPos> {
    let selection = state.selection();
    if !selection.is_cursor() {
        return None;
    }
    state.doc().resolve(selection.head(state.doc())).ok()
}

/// The cursor, when it sits at the start of a textblock.
pub(crate) fn at_block_start(state: &EditorState) -> Option<ResolvedPos> {
    block_boundary(state, false)
}

/// The cursor, when it sits at the end of a textblock.
pub(crate) fn at_block_end(state: &EditorState) -> Option<ResolvedPos> {
    block_boundary(state, true)
}

fn block_boundary(state: &EditorState, end: bool) -> Option<ResolvedPos> {
    let resolved = cursor(state)?;
    let depth = (1..=resolved.depth())
        .rev()
        .find(|&d| resolved.node(d).is_textblock(state.schema()))?;
    let boundary = if end {
        resolved.end(depth)
    } else {
        resolved.start(depth)
    };
    let hidden = resolved.depth() - depth;
    let at_boundary = if end {
        resolved.pos().checked_add(hidden) == Some(boundary)
    } else {
        resolved.pos().checked_sub(hidden) == Some(boundary)
    };
    at_boundary
        .then(|| state.doc().resolve(boundary).ok())
        .flatten()
}

/// The position of the boundary before `pos`'s ancestors, where content that
/// backspace would join sits.
///
/// Walks out of every ancestor `pos` is at the start of, stopping at an
/// isolating node.
pub(crate) fn find_cut_before(schema: &Schema, resolved: &ResolvedPos) -> Option<usize> {
    if schema.node_type(resolved.parent().type_id()).is_isolating() {
        return None;
    }
    for d in (0..resolved.depth()).rev() {
        if resolved.index(d) > 0 {
            return Some(resolved.before(d + 1));
        }
        if schema.node_type(resolved.node(d).type_id()).is_isolating() {
            break;
        }
    }
    None
}

/// The counterpart of [`find_cut_before`] for forward deletion.
pub(crate) fn find_cut_after(schema: &Schema, resolved: &ResolvedPos) -> Option<usize> {
    if schema.node_type(resolved.parent().type_id()).is_isolating() {
        return None;
    }
    for d in (0..resolved.depth()).rev() {
        let node = resolved.node(d);
        if resolved.index_after(d) < node.child_count() {
            return Some(resolved.after(d + 1));
        }
        if schema.node_type(node.type_id()).is_isolating() {
            break;
        }
    }
    None
}

/// The block range the current selection covers, if any.
pub(crate) fn selection_block_range(
    state: &EditorState,
    pred: Option<&dyn Fn(&Node) -> bool>,
) -> Option<crate::pos::NodeRange> {
    let doc = state.doc();
    let selection = state.selection();
    let from = doc.resolve(selection.from(doc)).ok()?;
    let to = doc.resolve(selection.to(doc)).ok()?;
    from.block_range(state.schema(), &to, pred)
}
