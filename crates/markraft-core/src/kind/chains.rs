//! What each bound action does, as a chain of catalogue commands: the key
//! chains a view binds for a document kind described by [`DocTypes`].
//!
//! This is the one place in the crate that consults a role table. The
//! [`commands`](crate::commands) catalogue below it never does; the chains
//! are built on that catalogue and pick from it by the roles a kind fills.
//!
//! Besides the usual base and list bindings, the chains settle these cases:
//! Backspace at the start of a list item joins it to the item before — or,
//! first in a nested list, to the item the list is in — and in a top-level
//! list's first item outdents before it lifts; Backspace at the start of a
//! block after a list or a quote, and Delete at the end of any textblock, join
//! the two textblocks' text wherever they sit; Enter in an empty list item
//! leaves the list; and Backspace in an empty verbatim block turns it into a
//! paragraph — or, for a raw block after another block, deletes it.

use std::ops::Range;

use super::conceal::{markup_safe, word_boundary};
use super::{DocTypes, DocumentKind};
use crate::change::TrackMode;
use crate::commands::structure::markup_of;
use crate::commands::{
    Command, Direction, chain, changes_spec, command, create_paragraph_near, delete_by,
    delete_by_grapheme, delete_empty_table, delete_range_changes, delete_selection, exit_code,
    goto_cell_below, goto_next_cell, goto_prev_cell, insert_hard_break, join_backward,
    join_forward, join_textblock_backward, join_textblock_forward, lift, lift_empty_block,
    lift_list_item, move_by, move_by_grapheme, new_line_in_code, resolve_changes,
    select_node_backward, select_node_forward, set_block_type, sink_list_item,
    split_block_keep_marks, split_list_item, undo_input_rule, wrap_in, wrap_in_list,
};
use crate::projection::{Projection, projection_of};
use crate::protocol::event;
use crate::{
    AttrValue, Attrs, Change, EditorState, MarkTypeId, Markup, NodeTypeId, Selection, Slice, Token,
    TransactionSpec,
};

/// Run `command` only where `pred` holds.
fn when(pred: impl Fn(&EditorState) -> bool + Send + Sync + 'static, command: Command) -> Command {
    crate::commands::command(move |state| pred(state).then(|| command(state)).flatten())
}

/// One command running `steps` in order, as a single edit.
///
/// Each step is computed against the document the previous ones produce and the
/// change sets are composed, so the whole is one transaction and one undo entry.
fn composed(steps: Vec<Command>) -> Command {
    command(move |state| {
        let mut current = state.clone();
        let mut set: Option<crate::ChangeSet> = None;
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
            let slice = crate::Slice::from_fragment(crate::Fragment::from_node(cleared));
            crate::commands::changes_spec(
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
            && line.ancestors().last().is_some_and(|own| own.index == 0)
            && line
                .ancestors()
                .iter()
                .nth_back(1)
                .is_some_and(|parent| self.is_item(parent.node_type))
    }

    /// The type and attributes of the textblock the cursor sits in.
    fn block_at_cursor(&self, state: &EditorState) -> Option<(NodeTypeId, Attrs)> {
        let resolved = state.resolved_head()?;
        let parent = resolved.parent();
        parent
            .is_textblock(state.schema())
            .then(|| (parent.type_id(), parent.attrs().clone()))
    }

    /// The innermost list type the cursor sits in.
    fn list_at_cursor(&self, state: &EditorState) -> Option<NodeTypeId> {
        let resolved = state.resolved_head()?;
        (0..=resolved.depth())
            .rev()
            .map(|depth| resolved.node(depth).type_id())
            .find(|ty| self.is_list(*ty))
    }

    /// Whether the cursor sits in a table cell.
    fn in_table_cell(&self, state: &EditorState) -> bool {
        self.table_types()
            .and_then(|types| crate::commands::cell_at(types, state))
            .is_some()
    }

    /// The innermost list item the cursor sits in, with the position before it.
    fn item_at_cursor(&self, state: &EditorState) -> Option<(NodeTypeId, Attrs, usize)> {
        let resolved = state.resolved_head()?;
        (1..=resolved.depth()).rev().find_map(|depth| {
            let node = resolved.node(depth);
            self.is_item(node.type_id())
                .then(|| (node.type_id(), node.attrs().clone(), resolved.before(depth)))
        })
    }
}

/// Enter, for a kind with nothing of its own to say about it.
pub fn enter(types: &DocTypes) -> Command {
    enter_with(types, &super::PlainKind)
}

/// Enter, with each command that splits a textblock at the caret wrapped by
/// the document kind ([`DocumentKind::wrap_split`]), and the kind's own Enter
/// rule ([`DocumentKind::enter_rule`]) tried before any of them.
pub fn enter_with(types: &DocTypes, kind: &dyn DocumentKind) -> Command {
    let splits = |command: Command| kind.wrap_split(command);
    let mut list: Vec<Option<Command>> = vec![
        // Inside a table Enter moves down a row and appends one at the bottom;
        // a cell is never split, which core's table invariant holds.
        types.table_types().map(goto_cell_below),
        kind.enter_rule(),
        // A verbatim block keeps Return for its own newlines, in a list item
        // as anywhere: splitting the item there would cut the block in two.
        Some(new_line_keeping_indent()),
        types.list_item.map(split_list_item).map(splits),
        types
            .task_item
            .map(|item| split_task_item(types, item))
            .map(splits),
    ];
    list.extend(per_item(types, lift_list_item));
    list.extend([
        Some(create_paragraph_near()),
        Some(lift_empty_block()),
        Some(splits(split_block_keep_marks())),
    ]);
    some(list)
}

/// Shift-Return: a new line inside the block rather than a new block.
///
/// In a verbatim block that is a newline, as Return is. Elsewhere it is a hard
/// break, spelled — where the kind keeps its markup in the text — with what
/// [`DocumentKind::break_spelling`] answers, or the trailing `\` the
/// serialiser writes where it answers nothing, since a line ending alone is a
/// soft break a reader sees as a space. The kind is asked each time the
/// command is built, which a view does per key press, so a host preference
/// takes effect at once. A GFM table row is one line, so a cell takes none.
pub fn line_break(types: &DocTypes, kind: &dyn DocumentKind) -> Command {
    let hard_break = types.hard_break.map(|node| {
        let types = types.clone();
        let insert = if types.syntax.is_some() {
            let marker = kind.break_spelling().unwrap_or("\\");
            command(move |state| {
                composed(vec![
                    crate::commands::insert_text(marker),
                    insert_hard_break(node),
                ])(state)
            })
        } else {
            insert_hard_break(node)
        };
        when(move |state| !types.in_table_cell(state), insert)
    });
    some([
        Some(new_line_keeping_indent()),
        cell_break(types),
        hard_break,
    ])
}

/// Shift-Return in a table cell: a GFM row is one line, so the cell's line
/// break is the HTML `<br />`, drawn as a break.
fn cell_break(types: &DocTypes) -> Option<Command> {
    let raw = types.raw_inline?;
    let types = types.clone();
    Some(command(move |state| {
        if !types.in_table_cell(state) {
            return None;
        }
        let tag = state
            .schema()
            .create(
                raw,
                crate::attrs! { "source" => "<br />" },
                crate::MarkSet::empty(),
                crate::Fragment::empty(),
            )
            .ok()?;
        crate::commands::replace_selection(Slice::from_fragment(crate::Fragment::from_node(tag)))(
            state,
        )
        .map(|spec| spec.user_event(event::INPUT_TYPE))
    }))
}

/// A new line in a verbatim block, starting with the spaces and tabs the line
/// it splits starts with — no more than lie before the caret — so code goes on
/// at the depth it was at. The spaces and tabs right after a caret go, as a
/// code editor drops them: split inside an indent, the new line
/// starts at its text.
fn new_line_keeping_indent() -> Command {
    command(|state| {
        let doc = state.doc();
        let schema = state.schema();
        let from = doc.resolve(state.selection().from(doc)).ok()?;
        let block = from.parent();
        let before = block.text_between(schema, 0, from.parent_offset(), None, None);
        let line = before.rsplit('\n').next().unwrap_or_default();
        let indent: String = line
            .chars()
            .take_while(|c| matches!(c, ' ' | '\t'))
            .collect();
        // `new_line_in_code` decides where a newline is text rather than a block
        // break; the indent only follows where it applies.
        let newline = new_line_in_code()(state)?;
        let after = block.text_between(
            schema,
            from.parent_offset(),
            block.content_size(),
            None,
            None,
        );
        let blanks = after
            .chars()
            .take_while(|c| matches!(c, ' ' | '\t'))
            .count();
        if indent.is_empty() && (blanks == 0 || !state.selection().is_cursor()) {
            return Some(newline);
        }
        let over_blanks;
        let target = if blanks > 0 && state.selection().is_cursor() {
            let caret = from.pos();
            over_blanks = state
                .update([TransactionSpec::new().selection(Selection::text(caret, caret + blanks))])
                .ok()?
                .state()
                .clone();
            &over_blanks
        } else {
            state
        };
        crate::commands::insert_text(&format!("\n{indent}"))(target)
    })
}

/// Backspace at the start of an empty verbatim block: turn it into a paragraph.
///
/// A verbatim block keeps Enter for itself, so the key that would otherwise
/// delete nothing is the way out of an empty one. A raw block has no chrome of
/// its own, so an empty one is invisible as well as inescapable: it goes the
/// way an empty paragraph does, joining the block before it, and only becomes
/// a paragraph when there is nothing to join.
fn clear_empty_verbatim(types: &DocTypes) -> Command {
    let types = types.clone();
    command(move |state| {
        let paragraph = types.paragraph?;
        if !state.selection().is_cursor() || !types.in_verbatim_block_at(state) {
            return None;
        }
        let resolved = state.resolved_head()?;
        // Empty content leaves the cursor nowhere but the block's start.
        if resolved.parent().content_size() != 0 {
            return None;
        }
        if Some(resolved.parent().type_id()) == types.raw_block
            && let Some(joined) = join_backward()(state)
        {
            return Some(joined);
        }
        set_block_type(paragraph, Attrs::empty())(state)
    })
}

/// Backspace at the start of a list item that has an item before it: join the
/// item to that one, its blocks carrying on the item before,
/// rather than lifting it out of the list. An empty item joins the same way,
/// leaving an empty paragraph in the item before for what is typed next. The
/// first item of a list nested in an item joins that item, the rest of the
/// nested list staying where it was. The first item of a top-level list still
/// outdents or leaves the list.
fn join_item_backward(types: &DocTypes) -> Command {
    let types = types.clone();
    command(move |state| {
        if !types.at_item_start(state) {
            return None;
        }
        let resolved = state.resolved_head()?;
        let depth = resolved.depth();
        if depth < 2 {
            return None;
        }
        let item = resolved.node(depth - 1);
        if !types.is_item(item.type_id()) {
            return None;
        }
        if resolved.index(depth - 2) > 0 {
            return join_backward()(state);
        }
        if depth < 3 || !types.is_item(resolved.node(depth - 3).type_id()) {
            return None;
        }
        // Lifted out, the item is the one after the item its list was in,
        // and joins it as any other item does.
        composed(vec![lift_list_item(item.type_id()), join_backward()])(state)
    })
}

/// Whether the textblock a text selection nearest `pos` in direction `dir`
/// would land in takes text joined into it: not a verbatim block, which keeps
/// its text to itself, and not a table cell, whose row holds one line.
fn prose_textblock_near(types: &DocTypes, state: &EditorState, pos: usize, dir: i32) -> bool {
    let doc = state.doc();
    Selection::find_from(state.schema(), doc, pos, dir, true)
        .and_then(|found| doc.resolve(found.head(doc)).ok())
        .is_some_and(|resolved| {
            let ty = resolved.parent().type_id();
            resolved.parent().is_textblock(state.schema())
                && !types.is_verbatim(ty)
                && types.table_cell != Some(ty)
        })
}

/// Backspace at the start of a textblock right after a list or a quote: its
/// text carries on the last textblock inside them, rather
/// than the block moving into the list as an item or into the quote.
fn join_text_after_wrapper(types: &DocTypes) -> Command {
    let types = types.clone();
    let join = join_textblock_backward();
    command(move |state| {
        if !at_textblock_start(state) || types.in_verbatim_block_at(state) {
            return None;
        }
        let (resolved, depth) = caret_textblock(state)?;
        let index = resolved.index(depth - 1);
        let before = resolved
            .node(depth - 1)
            .maybe_child(index.checked_sub(1)?)?;
        let wrapper = Some(before.type_id()) == types.blockquote || types.is_list(before.type_id());
        if !wrapper || !prose_textblock_near(&types, state, resolved.before(depth), -1) {
            return None;
        }
        join(state)
    })
}

/// `command`, unless the caret ends a textblock that a code block or a table
/// follows. Delete there does nothing: joining would pull the
/// block's text into the paragraph, or the paragraph's into a cell.
fn stop_before_kept_text(types: &DocTypes, command: Command) -> Command {
    let types = types.clone();
    crate::commands::command(move |state| {
        let (resolved, depth) = caret_textblock(state)?;
        let at_end = state.selection().is_cursor() && resolved.pos() == resolved.end(depth);
        if at_end && !prose_textblock_near(&types, state, resolved.after(depth), 1) {
            return None;
        }
        command(state)
    })
}

/// Delete at the end of a textblock: the text of the next textblock carries
/// on this one wherever it sits — the next item, a nested list's first item,
/// the first item of a list after a paragraph, a paragraph after a list.
/// A verbatim block on either side keeps its text to itself.
fn join_text_forward(types: &DocTypes) -> Command {
    let types = types.clone();
    let join = join_textblock_forward();
    command(move |state| {
        if types.in_verbatim_block_at(state) {
            return None;
        }
        let (resolved, depth) = caret_textblock(state)?;
        if !prose_textblock_near(&types, state, resolved.after(depth), 1) {
            return None;
        }
        join(state)
    })
}

/// Backspace at the start of a textblock right after a leaf block — a divider
/// — or Delete at the end of one right before it: take the leaf at once,
/// rather than selecting it for a second press. Delete then carries the next
/// textblock's text on this one.
fn take_leaf_block(types: &DocTypes, dir: Direction) -> Command {
    let types = types.clone();
    command(move |state| {
        if !state.selection().is_cursor() || types.in_verbatim_block_at(state) {
            return None;
        }
        let schema = state.schema();
        let (resolved, depth) = caret_textblock(state)?;
        let index = resolved.index(depth - 1);
        let (at_edge, index) = match dir {
            Direction::Backward => (at_textblock_start(state), index.checked_sub(1)?),
            Direction::Forward => (resolved.pos() == resolved.end(depth), index + 1),
        };
        let leaf = resolved.node(depth - 1).maybe_child(index)?;
        if !at_edge || !leaf.is_leaf() || leaf.is_inline(schema) {
            return None;
        }
        let from = match dir {
            Direction::Backward => resolved.before(depth) - leaf.node_size(),
            Direction::Forward => resolved.after(depth),
        };
        let take = crate::commands::delete_range(from, from + leaf.node_size());
        match dir {
            Direction::Backward => take(state),
            Direction::Forward => composed(vec![take, join_text_forward(&types)])(state),
        }
    })
}

/// Backspace at the start of a paragraph right after a table: its text carries
/// on the table's last cell, and the caret stays at the seam.
/// A paragraph holding a line break stays where it is, since a cell's row holds
/// one line.
fn join_into_table_after(types: &DocTypes) -> Command {
    let types = types.clone();
    command(move |state| {
        let table = types.table?;
        if !at_textblock_start(state) || types.in_verbatim_block_at(state) {
            return None;
        }
        let schema = state.schema();
        let (resolved, depth) = caret_textblock(state)?;
        let paragraph = resolved.node(depth);
        let before = resolved
            .node(depth - 1)
            .maybe_child(resolved.index(depth - 1).checked_sub(1)?)?;
        let breaks = paragraph.content().iter().any(|child| {
            Some(child.type_id()) == types.hard_break
                || child.text().is_some_and(|text| text.contains('\n'))
        });
        if before.type_id() != table || breaks {
            return None;
        }
        // The end of the last cell's content: one token back for each node the
        // walk down the table's last children closes.
        let mut cell = before;
        let mut end = resolved.before(depth) - 1;
        while !cell.is_textblock(schema) {
            cell = cell.last_child()?;
            end -= 1;
        }
        if types.table_cell != Some(cell.type_id()) {
            return None;
        }
        let spec = changes_spec(
            state,
            vec![
                Change::insert(end, Slice::from_fragment(paragraph.content().clone())),
                Change::delete(resolved.before(depth), resolved.after(depth)),
            ],
            "delete.backward",
        )?;
        Some(spec.selection(Selection::cursor(end)))
    })
}

/// Backspace.
pub fn backspace(types: &DocTypes) -> Command {
    let outdent = {
        let types = types.clone();
        let inner = some(per_item(&types, lift_list_item));
        when(move |state| types.at_item_start(state), inner)
    };
    some([
        // A shortcut inside the text — an emoji code, a link — is taken back to
        // what was typed. One that made the block — `- `, `# `, `> ` — leaves
        // the caret at the block's start, where Backspace takes the format off
        // rather than giving the characters back.
        Some(when(|state| !at_textblock_start(state), undo_input_rule())),
        Some(delete_selection()),
        // At a block's start, the container it opens goes before the block's
        // own format: a quote is lifted, an item joined or outdented, and
        // only a heading opening nothing becomes a paragraph.
        Some(lift_quote_at_start(types)),
        Some(delete_by_grapheme(Direction::Backward)),
        types.table_types().map(delete_empty_table),
        Some(join_item_backward(types)),
        Some(outdent),
        Some(clear_heading_at_start(types)),
        Some(clear_empty_verbatim(types)),
        Some(join_text_after_wrapper(types)),
        Some(take_leaf_block(types, Direction::Backward)),
        Some(join_into_table_after(types)),
        Some(join_backward()),
        Some(select_node_backward()),
    ])
}

/// The caret's position resolved, and the depth of the textblock it is in:
/// the innermost, so a caret inside an inline node still names its block.
fn caret_textblock(state: &EditorState) -> Option<(crate::ResolvedPos, usize)> {
    let resolved = state.resolved_head()?;
    let depth = (1..=resolved.depth())
        .rev()
        .find(|&d| resolved.node(d).is_textblock(state.schema()))?;
    Some((resolved, depth))
}

/// Whether the cursor sits at the start of its textblock.
fn at_textblock_start(state: &EditorState) -> bool {
    if !state.selection().is_cursor() {
        return false;
    }
    let Some((resolved, depth)) = caret_textblock(state) else {
        return false;
    };
    let start = resolved.start(depth);
    let hidden = resolved.depth() - depth;
    resolved.pos().checked_sub(hidden) == Some(start)
}

/// At the start of a heading, Backspace turns it into a paragraph, whatever
/// its level. Typing `#` there raises the level again.
///
/// This runs after the steps that take a container the block opens — a quote
/// lifted, an item joined or outdented — so a heading that opens one keeps
/// its level and loses the container, as a paragraph there would.
fn clear_heading_at_start(types: &DocTypes) -> Command {
    let types = types.clone();
    command(move |state| {
        if !at_textblock_start(state) {
            return None;
        }
        let heading = types.heading?;
        let paragraph = types.paragraph?;
        let (ty, _) = types.block_at_cursor(state)?;
        if ty != heading {
            return None;
        }
        set_block_type(paragraph, Attrs::empty())(state)
    })
}

/// At the start of a quoted block, Backspace lifts one quote level. A block
/// with another before it in the same quote is left to the joins after this,
/// so its text carries on the block before.
fn lift_quote_at_start(types: &DocTypes) -> Command {
    let types = types.clone();
    command(move |state| {
        if !at_textblock_start(state) {
            return None;
        }
        let blockquote = types.blockquote?;
        let (resolved, textblock) = caret_textblock(state)?;
        let in_quote =
            (1..=resolved.depth()).any(|depth| resolved.node(depth).type_id() == blockquote);
        if !in_quote {
            return None;
        }
        if resolved.node(textblock - 1).type_id() == blockquote && resolved.index(textblock - 1) > 0
        {
            return None;
        }
        crate::commands::lift()(state)
    })
}

/// Forward delete.
pub fn delete_forward(types: &DocTypes) -> Command {
    some([
        Some(delete_selection()),
        Some(delete_by_grapheme(Direction::Forward)),
        Some(join_text_forward(types)),
        Some(take_leaf_block(types, Direction::Forward)),
        Some(stop_before_kept_text(types, join_forward())),
        Some(select_node_forward()),
    ])
}

/// ⌃A and ⌃E: to the start or the end of the caret's textblock, as the
/// Emacs keys of every macOS text view go to the paragraph's.
pub fn textblock_edge(end: bool) -> Command {
    if end {
        crate::commands::select_textblock_end()
    } else {
        crate::commands::select_textblock_start()
    }
}

/// ⌃K: delete to the end of the caret's textblock, or — at its end already —
/// join the next one, as Delete does there.
pub fn delete_to_textblock_end(types: &DocTypes) -> Command {
    let within = command(move |state| {
        if !state.selection().is_cursor() {
            return None;
        }
        let resolved = state.resolved_head()?;
        if !resolved.parent().is_textblock(state.schema()) {
            return None;
        }
        let end = resolved.end(resolved.depth());
        (resolved.pos() < end)
            .then(|| delete_within_textblock(resolved.pos(), end)(state))
            .flatten()
    });
    chain([within, delete_forward(types)])
}

/// Delete `from..to`, a stretch of one textblock.
pub fn delete_within_textblock(from: usize, to: usize) -> Command {
    crate::commands::delete_range(from, to)
}

/// ⌥⌫ and ⌥⌦: delete to the word boundary a reader sees. See
/// [`super::conceal::word_boundary`].
pub fn delete_word(types: &DocTypes, dir: Direction) -> Command {
    let syntax = types.syntax;
    some([
        Some(delete_selection()),
        Some(delete_by(move |projection, pos| {
            word_boundary(syntax, projection, pos, dir)
        })),
        Some(match dir {
            Direction::Backward => join_backward(),
            Direction::Forward => join_forward(),
        }),
    ])
}

/// ← and →, or ⇧← and ⇧→ with `extend`: one grapheme, across markup a caret
/// conceals.
pub fn move_grapheme(dir: Direction, extend: bool) -> Command {
    match (dir, extend) {
        (Direction::Forward, false) => chain([move_by_grapheme(dir, extend), exit_leaf_below()]),
        _ => move_by_grapheme(dir, extend),
    }
}

/// → or ↓ on a selected leaf block — a divider — with nothing after it in its
/// parent: a new paragraph after it to carry on in, as ↓ out of a table's last
/// row makes one. Without it the caret could not get past the block.
pub fn exit_leaf_below() -> Command {
    command(|state| {
        let doc = state.doc();
        let Selection::Node { pos } = *state.selection() else {
            return None;
        };
        let resolved = doc.resolve(pos).ok()?;
        let leaf = resolved.node_after()?;
        let last = resolved.index(resolved.depth()) + 1 == resolved.parent().child_count();
        if !last || !leaf.is_leaf() || leaf.is_inline(state.schema()) {
            return None;
        }
        create_paragraph_near()(state)
    })
}

/// ⌥← and ⌥→, shifted or not: move to the word boundary a reader sees. See
/// [`super::conceal::word_boundary`].
pub fn move_word(types: &DocTypes, dir: Direction, extend: bool) -> Command {
    let syntax = types.syntax;
    move_by(dir, extend, move |projection, pos| {
        word_boundary(syntax, projection, pos, dir)
    })
}

/// ⌘↑ / ⌘↓ and their shifted forms.
pub fn move_document_edge(end: bool, extend: bool) -> Command {
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

/// Tab: step to the next table cell, sink a list item, or insert `text` at a
/// caret in a verbatim block.
///
/// A cell holds inline content, so no verbatim block can sit in one and the two
/// never compete.
pub fn indent(types: &DocTypes, text: &str) -> Command {
    let lines = shift_code_lines(types, text, false);
    let verbatim = {
        let types = types.clone();
        when(
            move |state| types.in_verbatim_block_at(state) && state.selection().is_cursor(),
            crate::commands::insert_text(text),
        )
    };
    let mut list = vec![types.table_types().map(goto_next_cell)];
    list.extend(per_item(types, sink_list_item));
    list.push(Some(lines));
    list.push(Some(verbatim));
    some(list)
}

/// Tab over a selection in a code block, and Shift-Tab there with or without
/// one: indent or outdent every line the selection covers, as a code editor
/// does, the selection staying on them. A line the selection only reaches the
/// start of is not one it covers. Outdenting takes `text` — what Tab inserts
/// — from a line that starts with it, or else a tab, or else up to as many
/// spaces as one level is wide.
fn shift_code_lines(types: &DocTypes, text: &str, outdent: bool) -> Command {
    let types = types.clone();
    let text = text.to_owned();
    command(move |state| {
        let doc = state.doc();
        let schema = state.schema();
        let selection = state.selection();
        if !matches!(selection, Selection::Text { .. })
            || (!outdent && selection.is_empty(doc))
            || !types.in_verbatim_block_at(state)
        {
            return None;
        }
        let (from, to) = (selection.from(doc), selection.to(doc));
        let start = doc.resolve(from).ok()?;
        let block = start.parent();
        let content = start.start(start.depth());
        if !block.is_textblock(schema) || to > start.end(start.depth()) {
            return None;
        }
        let source: Vec<char> = block
            .children()
            .map(|child| child.text().unwrap_or("\u{fffc}"))
            .collect::<String>()
            .chars()
            .collect();
        let (from, to) = (from - content, to - content);
        let starts = std::iter::once(0).chain(
            (0..source.len())
                .filter(|&i| source[i] == '\n')
                .map(|i| i + 1),
        );
        let covered: Vec<usize> = starts
            .filter(|&line| {
                let end = source[line..]
                    .iter()
                    .position(|&c| c == '\n')
                    .map_or(source.len(), |at| line + at);
                end >= from && (line <= from || line < to)
            })
            .collect();
        let width = if text.chars().all(|c| c == ' ') && !text.is_empty() {
            text.chars().count()
        } else {
            4
        };
        let changes: Vec<Change> = covered
            .into_iter()
            .filter_map(|line| {
                let at = content + line;
                if !outdent {
                    return Some(Change::insert(
                        at,
                        Slice::from_fragment(crate::Fragment::from_node(schema.text(&text))),
                    ));
                }
                let rest = &source[line..];
                let starts_with = |s: &str| {
                    let s: Vec<char> = s.chars().collect();
                    !s.is_empty() && rest.starts_with(&s)
                };
                let taken = if starts_with(&text) {
                    text.chars().count()
                } else if rest.first() == Some(&'\t') {
                    1
                } else {
                    rest.iter().take(width).take_while(|&&c| c == ' ').count()
                };
                (taken > 0).then(|| Change::delete(at, at + taken))
            })
            .collect();
        if changes.is_empty() {
            return None;
        }
        changes_spec(state, changes, if outdent { "delete" } else { "input" })
    })
}

/// Shift-Tab: step to the previous table cell, lift a list item, or lift a
/// block out of its wrapper.
///
/// [`goto_prev_cell`] does not apply in the first cell, and lifting a cell out
/// of its row would leave that row short, so inside a table the rest of the
/// chain is skipped rather than run.
pub fn outdent(types: &DocTypes, text: &str) -> Command {
    let lines = shift_code_lines(types, text, true);
    let mut tail = vec![Some(lines)];
    tail.extend(per_item(types, lift_list_item));
    tail.push(Some(lift()));
    let outside = {
        let types = types.clone();
        when(move |state| !types.in_table_cell(state), some(tail))
    };
    some([types.table_types().map(goto_prev_cell), Some(outside)])
}

/// Undo or redo, through the history extension.
pub fn history(undo: bool) -> Command {
    command(move |state| {
        if undo {
            crate::history::undo(state)
        } else {
            crate::history::redo(state)
        }
    })
}

/// ⌥⌘C: with the caret in a paragraph that has text, a new
/// empty code block of `ty` with `attrs` at the caret — after the paragraph at
/// its end, before it at its start, and splitting it anywhere else — with the
/// caret in the block. On an empty line, over a selection, or in a code block
/// already, the block type toggles as any other's does; a selection then
/// collapses to its start, so typing does not replace the code.
pub fn code_block(types: &DocTypes, ty: NodeTypeId, attrs: Attrs) -> Command {
    let types = types.clone();
    let toggle = toggle_block(&types, ty, attrs.clone());
    command(move |state| {
        let doc = state.doc();
        let schema = state.schema();
        let selection = state.selection();
        let resolved = doc.resolve(selection.head(doc)).ok()?;
        let parent = resolved.parent();
        let in_prose = Some(parent.type_id()) == types.paragraph && parent.content_size() > 0;
        if !selection.is_cursor() {
            let start = selection.from(doc);
            return toggle(state).map(|spec| spec.selection(Selection::cursor(start)));
        }
        if !in_prose {
            return toggle(state);
        }
        let code = schema
            .create(
                ty,
                attrs.clone(),
                crate::MarkSet::empty(),
                crate::Fragment::empty(),
            )
            .ok()?;
        let depth = resolved.depth();
        let offset = resolved.parent_offset();
        let (at, tokens, caret) = if offset == parent.content_size() {
            let at = resolved.after(depth);
            (at, vec![Token::Node(code)], at + 1)
        } else if offset == 0 {
            let at = resolved.before(depth);
            (at, vec![Token::Node(code)], at + 1)
        } else {
            let markup = parent.markup().clone();
            let at = resolved.pos();
            let tokens = vec![
                Token::Close(markup.clone()),
                Token::Node(code),
                Token::Open(markup),
            ];
            (at, tokens, at + 2)
        };
        let change = Change::insert(at, Slice::from_tokens(&tokens)).with_fit(crate::Fit::Auto);
        let spec = changes_spec(state, vec![change], "format.block")?;
        Some(spec.selection(Selection::cursor(caret)).scroll_into_view())
    })
}

/// Set a textblock type, or return to a paragraph when it is already that type.
pub fn toggle_block(types: &DocTypes, ty: NodeTypeId, attrs: Attrs) -> Command {
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
pub fn toggle_quote(types: &DocTypes) -> Command {
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
pub fn toggle_wrap_in(ty: NodeTypeId, attrs: Attrs) -> Command {
    command(move |state| {
        let schema = state.schema();
        let resolved = state.resolved_head()?;
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

/// Wrap in a list of `ty` carrying `list_attrs`, or leave the list when the
/// cursor is already in one of that type holding items of `item`.
///
/// In a list of another kind — ordered where `ty` is a bullet list, or holding
/// the other kind of item — the whole list the cursor is in becomes the kind
/// asked for, rather than a new list nesting in its item.
pub fn toggle_list(
    types: &DocTypes,
    ty: NodeTypeId,
    list_attrs: Attrs,
    item: NodeTypeId,
) -> Command {
    let types = types.clone();
    command(move |state| {
        let current = types.list_at_cursor(state);
        let current_item = types.item_at_cursor(state).map(|(ty, _, _)| ty);
        if current == Some(ty) && current_item == Some(item) {
            return lift_list_item(item)(state);
        }
        if current.is_some() && current_item.is_some() {
            return convert_list(&types, ty, &list_attrs, item)(state);
        }
        composed(vec![
            wrap_in_list(ty, list_attrs.clone()),
            convert_items(&types, item),
        ])(state)
    })
}

/// Turn the innermost list the cursor is in into a list of `ty` holding items
/// of `item`, the items keeping their content and the lists nested in them
/// keeping their kind.
///
/// A list that changes type takes `list_attrs` — its marker is the new kind's,
/// not a spelling of the old one's — and keeps only its tightness, which is
/// how its items are spaced rather than how it is marked. One already of `ty`
/// keeps its attributes whole, and an item already of `item` keeps its own.
fn convert_list(types: &DocTypes, ty: NodeTypeId, list_attrs: &Attrs, item: NodeTypeId) -> Command {
    let types = types.clone();
    let list_attrs = list_attrs.clone();
    command(move |state| {
        let schema = state.schema();
        let resolved = state.resolved_head()?;
        let depth = (1..=resolved.depth())
            .rev()
            .find(|depth| types.is_list(resolved.node(*depth).type_id()))?;
        let list = resolved.node(depth);
        let before = resolved.before(depth);
        // The open and close tokens of the node at `at`, of `size`, as `markup`.
        let retype = |at: usize, size: usize, markup: Markup| {
            [
                Change::replace(
                    at,
                    at + 1,
                    Slice::from_tokens(&[Token::Open(markup.clone())]),
                ),
                Change::replace(
                    at + size - 1,
                    at + size,
                    Slice::from_tokens(&[Token::Close(markup)]),
                ),
            ]
        };
        let mut changes = Vec::new();
        let mut close = None;
        if list.type_id() != ty {
            let mut attrs = list_attrs.clone();
            if let Some(tight) = list.attrs().get("tight")
                && attrs.get("tight").is_none()
                && schema.node_type(ty).default_attrs().get("tight").is_some()
            {
                attrs = attrs.with("tight", tight.clone());
            }
            let [open, closing] = retype(before, list.node_size(), markup_of(schema, ty, &attrs));
            changes.push(open);
            close = Some(closing);
        }
        let mut at = before + 1;
        for child in list.children() {
            if child.type_id() != item {
                // Built rather than retyped in place, so an item whose content
                // the new kind cannot hold refuses the whole conversion.
                let replaced = schema
                    .create(
                        item,
                        schema.node_type(item).default_attrs().clone(),
                        child.marks().clone(),
                        child.content().clone(),
                    )
                    .ok()?;
                changes.extend(retype(at, child.node_size(), replaced.markup().clone()));
            }
            at += child.node_size();
        }
        changes.extend(close);
        if changes.is_empty() {
            return None;
        }
        changes_spec(state, changes, "format.block")
            .map(|spec| spec.selection(state.selection().clone()))
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
            if line.to() < from || line.from() > to || (from != to && line.from() == to) {
                continue;
            }
            if let Some(index) = line
                .ancestors()
                .iter()
                .rposition(|a| types.is_item(a.node_type))
            {
                positions.insert(line.ancestor_before(index));
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
                crate::Slice::from_tokens(&[crate::Token::Open(markup.clone())]),
            ));
            let end = before + node.node_size();
            changes.push(Change::replace(
                end - 1,
                end,
                crate::Slice::from_tokens(&[crate::Token::Close(markup)]),
            ));
        }
        if changes.is_empty() {
            return None;
        }
        crate::commands::changes_spec(state, changes, "format.block")
            .map(|spec| spec.selection(selection.clone()))
    })
}

/// ⌘⏎: add a row below in a table, tick or clear the task box the cursor sits
/// in, else leave a code block.
pub fn toggle_task(types: &DocTypes) -> Command {
    let types = types.clone();
    command(move |state| {
        if let Some(table) = types.table_types().filter(|_| types.in_table_cell(state)) {
            return crate::commands::insert_row_below(table)(state);
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
        let slice = crate::Slice::from_fragment(crate::Fragment::from_node(updated));
        // The node keeps its size, so the caret keeps its position.
        crate::commands::changes_spec(
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
pub fn insert_plain(types: &DocTypes, text: &str) -> Command {
    let types = types.clone();
    let text = text.to_owned();
    command(move |state| {
        if text == "#"
            && let Some(spec) = promote_heading_at_start(&types)(state)
        {
            return Some(spec);
        }
        if !text.contains('\n') || types.in_verbatim_block_at(state) {
            return crate::commands::insert_text(&text)(state);
        }
        let schema = state.schema();
        let paragraph = types.paragraph?;
        let attrs = schema.node_type(paragraph).default_attrs().clone();
        let nodes: Option<Vec<_>> = text
            .split('\n')
            .map(|part| {
                let content = if part.is_empty() {
                    crate::Fragment::empty()
                } else {
                    crate::Fragment::from_node(schema.text(part))
                };
                schema
                    .create(paragraph, attrs.clone(), crate::MarkSet::empty(), content)
                    .ok()
            })
            .collect();
        let slice = crate::Slice::new(crate::Fragment::from_nodes(nodes?), 1, 1);
        crate::commands::replace_selection(slice)(state)
    })
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
pub fn select_all(types: &DocTypes) -> Command {
    let types = types.clone();
    command(move |state| {
        let doc = state.doc();
        if types.in_verbatim_block_at(state) {
            let resolved = state.resolved_head()?;
            let (from, to) = (
                resolved.start(resolved.depth()),
                resolved.end(resolved.depth()),
            );
            let selection = Selection::text(from, to);
            if selection != *state.selection() && !(from == 0 && to == doc.content_size()) {
                return Some(
                    TransactionSpec::new()
                        .selection(selection)
                        .user_event(event::SELECT),
                );
            }
        }
        crate::commands::select_all()(state)
    })
}

/// Delete `range`, keeping markup whole.
///
/// A span whose text the range takes entirely goes with its spelling, and a
/// span the range only reaches into keeps every run of its spelling: deleting
/// `bold` from `x **bold** y` leaves `x y`, and deleting from its `o` to the
/// end leaves `x **b**`, where a plain deletion would leave `x ** y` and
/// `x **b` — asterisks that no longer pair, read back as text. See
/// [`markup_safe`]. With `keep_emptied` the spelling of a span the range
/// empties stays, so the text typed next goes inside it. The caret is left
/// where the deletion started, and the transaction carries `event` as its
/// user event.
pub fn delete_keeping_markup(
    state: &EditorState,
    projection: &Projection,
    syntax: Option<MarkTypeId>,
    range: Range<usize>,
    keep_emptied: bool,
    event: &str,
) -> Option<TransactionSpec> {
    let ranges = markup_safe(projection, syntax, range.clone(), keep_emptied);
    if ranges.is_empty() {
        return None;
    }
    let changes = ranges
        .iter()
        .flat_map(|part| delete_range_changes(state.schema(), state.doc(), part.start, part.end))
        .collect();
    let (set, doc) = resolve_changes(state, changes)?;
    let start = ranges[0].start.min(range.start);
    let caret = set
        .map_pos(start, -1, TrackMode::Simple)
        .unwrap_or(start)
        .min(doc.content_size());
    Some(
        TransactionSpec::new()
            .change_set(set)
            .selection(Selection::near(state.schema(), &doc, caret, 1))
            .user_event(event)
            .scroll_into_view(),
    )
}
