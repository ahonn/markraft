//! What each bound action does, as a chain of catalogue commands.
//!
//! The chains mirror ProseMirror's base and list keymaps, with the departures
//! the editor's own tests describe, each of them what Typora does: Backspace
//! at the start of a list item joins it to the item before — or, first in a
//! nested list, to the item the list is in — and in a top-level list's first
//! item outdents before it lifts; Backspace at the start of a block after a
//! list or a quote, and Delete at the end of any textblock, join the two
//! textblocks' text wherever they sit; Enter in an empty list item leaves the
//! list; and Backspace in an empty verbatim block turns it into a paragraph —
//! or, for a raw block after another block, deletes it.

use crate::types::DocTypes;
use markraft_core::commands::structure::markup_of;
use markraft_core::commands::{
    Command, Direction, chain, changes_spec, command, create_paragraph_near, delete_by,
    delete_by_grapheme, delete_empty_table, delete_selection, exit_code, goto_cell_below,
    goto_next_cell, goto_prev_cell, guard_cell_boundary, guard_cell_range, guard_cell_split,
    insert_hard_break, join_backward, join_forward, join_textblock_backward,
    join_textblock_forward, lift, lift_empty_block, lift_list_item, move_by, move_by_grapheme,
    new_line_in_code, select_node_backward, select_node_forward, set_block_type, sink_list_item,
    split_block_keep_marks, split_list_item, undo_input_rule, wrap_in, wrap_in_list,
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
            && line.ancestors().last().is_some_and(|own| own.index == 0)
            && line
                .ancestors()
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
    enter_with(types, None, None)
}

/// Enter, with each command that splits a textblock at the caret wrapped by
/// the document kind's [`SplitWrap`](crate::SplitWrap), and the kind's
/// [`enter_rule`](crate::Setup::enter_rule) tried before any of them.
pub(crate) fn enter_with(
    types: &DocTypes,
    wrap: Option<&crate::SplitWrap>,
    rule: Option<&Command>,
) -> Command {
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
        rule.cloned(),
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
/// `spelling` answers, or the trailing `\` the serialiser writes where there
/// is none, since a line ending alone is a soft break a reader sees as a space.
/// `spelling` is asked each time the command runs, so a host preference takes
/// effect at once. A GFM table row is one line, so a cell takes none.
pub(crate) fn line_break(types: &DocTypes, spelling: Option<&crate::BreakSpelling>) -> Command {
    let hard_break = types.hard_break.map(|node| {
        let types = types.clone();
        let insert = if types.syntax.is_some() {
            let spelling = spelling.cloned();
            command(move |state| {
                let marker = spelling.as_ref().map_or("\\", |spelling| spelling());
                composed(vec![
                    markraft_core::commands::insert_text(marker),
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
/// break is the HTML `<br />`, as Typora writes it, drawn as a break.
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
                markraft_core::attrs! { "source" => "<br />" },
                markraft_core::MarkSet::empty(),
                markraft_core::Fragment::empty(),
            )
            .ok()?;
        markraft_core::commands::replace_selection(Slice::from_fragment(
            markraft_core::Fragment::from_node(tag),
        ))(state)
        .map(|spec| spec.user_event("input.type"))
    }))
}

/// A new line in a verbatim block, starting with the spaces and tabs the line
/// it splits starts with — no more than lie before the caret — so code goes on
/// at the depth it was at.
fn new_line_keeping_indent() -> Command {
    command(|state| {
        let doc = state.doc();
        let from = doc.resolve(state.selection().from(doc)).ok()?;
        let block = from.parent();
        let before = block.text_between(state.schema(), 0, from.parent_offset(), None, None);
        let line = before.rsplit('\n').next().unwrap_or_default();
        let indent: String = line
            .chars()
            .take_while(|c| matches!(c, ' ' | '\t'))
            .collect();
        // `new_line_in_code` decides where a newline is text rather than a block
        // break; the indent only follows where it applies.
        let newline = new_line_in_code()(state)?;
        if indent.is_empty() {
            return Some(newline);
        }
        markraft_core::commands::insert_text(&format!("\n{indent}"))(state)
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
        let doc = state.doc();
        let resolved = doc.resolve(state.selection().head(doc)).ok()?;
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
/// item to that one, its blocks carrying on the item before as Typora does,
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
        let doc = state.doc();
        let resolved = doc.resolve(state.selection().head(doc)).ok()?;
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
/// text carries on the last textblock inside them, as Typora does, rather
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
/// follows. Delete there does nothing, as in Typora: joining would pull the
/// block's text into the paragraph, or the paragraph's into a cell.
fn stop_before_kept_text(types: &DocTypes, command: Command) -> Command {
    let types = types.clone();
    markraft_core::commands::command(move |state| {
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
/// the first item of a list after a paragraph, a paragraph after a list — as
/// Typora does. A verbatim block on either side keeps its text to itself.
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
/// — or Delete at the end of one right before it: take the leaf at once, as
/// Typora does, rather than selecting it for a second press. Delete then
/// carries the next textblock's text on this one, as it does in Typora.
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
        let take = markraft_core::commands::delete_range(from, from + leaf.node_size());
        match dir {
            Direction::Backward => take(state),
            Direction::Forward => composed(vec![take, join_text_forward(&types)])(state),
        }
    })
}

/// Backspace at the start of a paragraph right after a table: its text carries
/// on the table's last cell, as Typora does, and the caret stays at the seam.
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
        // A shortcut inside the text — an emoji code, a link — is taken back to
        // what was typed. One that made the block — `- `, `# `, `> ` — leaves
        // the caret at the block's start, where Backspace takes the format off
        // as Typora does, rather than giving the characters back.
        Some(when(|state| !at_textblock_start(state), undo_input_rule())),
        Some(delete_selection()),
        // At a block's start, the container it opens goes before the block's
        // own format: a quote is lifted, an item joined or outdented, and
        // only a heading opening nothing becomes a paragraph.
        Some(lift_quote_at_start(types)),
        Some(delete_by_grapheme(Direction::Backward)),
        types.table_types().map(delete_empty_table),
        types.table_types().map(guard_cell_boundary),
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
fn caret_textblock(state: &EditorState) -> Option<(markraft_core::ResolvedPos, usize)> {
    let doc = state.doc();
    let resolved = doc.resolve(state.selection().head(doc)).ok()?;
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
/// its level, as Typora does. Typing `#` there raises the level again.
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
/// so its text carries on the block before, as Typora does.
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
        Some(join_text_forward(types)),
        Some(take_leaf_block(types, Direction::Forward)),
        Some(stop_before_kept_text(types, join_forward())),
        Some(select_node_forward()),
    ])
}

/// ⌃A and ⌃E: to the start or the end of the caret's textblock, as the
/// Emacs keys of every macOS text view go to the paragraph's.
pub(crate) fn textblock_edge(end: bool) -> Command {
    if end {
        markraft_core::commands::select_textblock_end()
    } else {
        markraft_core::commands::select_textblock_start()
    }
}

/// ⌃K: delete to the end of the caret's textblock, or — at its end already —
/// join the next one, as Delete does there.
pub(crate) fn delete_to_textblock_end(types: &DocTypes) -> Command {
    let within = {
        let types = types.clone();
        command(move |state| {
            let doc = state.doc();
            if !state.selection().is_cursor() {
                return None;
            }
            let resolved = doc.resolve(state.selection().head(doc)).ok()?;
            if !resolved.parent().is_textblock(state.schema()) {
                return None;
            }
            let end = resolved.end(resolved.depth());
            (resolved.pos() < end)
                .then(|| delete_within_textblock(&types, resolved.pos(), end)(state))
                .flatten()
        })
    };
    chain([within, delete_forward(types)])
}

/// Delete `from..to`, a stretch of one textblock, with the guards Backspace
/// keeps: nothing in a table cell reaches past it.
pub(crate) fn delete_within_textblock(types: &DocTypes, from: usize, to: usize) -> Command {
    some([
        types.table_types().map(guard_cell_range),
        Some(markraft_core::commands::delete_range(from, to)),
    ])
}

/// ⌥⌫ and ⌥⌦: delete to the word boundary a reader sees. See
/// [`crate::conceal::word_boundary`].
pub(crate) fn delete_word(types: &DocTypes, dir: Direction) -> Command {
    let syntax = types.syntax;
    some([
        types.table_types().map(guard_cell_range),
        Some(delete_selection()),
        Some(delete_by(move |projection, pos| {
            crate::conceal::word_boundary(syntax, projection, pos, dir)
        })),
        types.table_types().map(guard_cell_boundary),
        Some(match dir {
            Direction::Backward => join_backward(),
            Direction::Forward => join_forward(),
        }),
    ])
}

pub(crate) fn move_grapheme(dir: Direction, extend: bool) -> Command {
    match (dir, extend) {
        (Direction::Forward, false) => chain([move_by_grapheme(dir, extend), exit_leaf_below()]),
        _ => move_by_grapheme(dir, extend),
    }
}

/// → or ↓ on a selected leaf block — a divider — with nothing after it in its
/// parent: a new paragraph after it to carry on in, as ↓ out of a table's last
/// row makes one. Without it the caret could not get past the block.
pub(crate) fn exit_leaf_below() -> Command {
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
/// [`crate::conceal::word_boundary`].
pub(crate) fn move_word(types: &DocTypes, dir: Direction, extend: bool) -> Command {
    let syntax = types.syntax;
    move_by(dir, extend, move |projection, pos| {
        crate::conceal::word_boundary(syntax, projection, pos, dir)
    })
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

/// Tab: step to the next table cell, sink a list item, or insert `text` at a
/// caret in a verbatim block.
///
/// A cell holds inline content, so no verbatim block can sit in one and the two
/// never compete.
pub(crate) fn indent(types: &DocTypes, text: &str) -> Command {
    let lines = shift_code_lines(types, text, false);
    let verbatim = {
        let types = types.clone();
        when(
            move |state| types.in_verbatim_block_at(state) && state.selection().is_cursor(),
            markraft_core::commands::insert_text(text),
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
                        Slice::from_fragment(markraft_core::Fragment::from_node(
                            schema.text(&text),
                        )),
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
pub(crate) fn outdent(types: &DocTypes, text: &str) -> Command {
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
pub(crate) fn history(undo: bool) -> Command {
    command(move |state| {
        if undo {
            markraft_core::history::undo(state)
        } else {
            markraft_core::history::redo(state)
        }
    })
}

/// ⌥⌘C, as Typora does it: with the caret in a paragraph that has text, a new
/// empty code block of `ty` with `attrs` at the caret — after the paragraph at
/// its end, before it at its start, and splitting it anywhere else — with the
/// caret in the block. On an empty line, over a selection, or in a code block
/// already, the block type toggles as any other's does; a selection then
/// collapses to its start, as in Typora, so typing does not replace the code.
pub(crate) fn code_block(types: &DocTypes, ty: NodeTypeId, attrs: Attrs) -> Command {
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
                markraft_core::MarkSet::empty(),
                markraft_core::Fragment::empty(),
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
        let change =
            Change::insert(at, Slice::from_tokens(&tokens)).with_fit(markraft_core::Fit::Auto);
        let spec = changes_spec(state, vec![change], "format.block")?;
        Some(spec.selection(Selection::cursor(caret)).scroll_into_view())
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

/// Wrap in a list of `ty` carrying `list_attrs`, or leave the list when the
/// cursor is already in one of that type holding items of `item`.
///
/// In a list of another kind — ordered where `ty` is a bullet list, or holding
/// the other kind of item — the whole list the cursor is in becomes the kind
/// asked for, as Typora converts it, rather than a new list nesting in its item.
pub(crate) fn toggle_list(
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
        let doc = state.doc();
        let schema = state.schema();
        let resolved = doc.resolve(state.selection().head(doc)).ok()?;
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
            return markraft_core::commands::insert_row_below(table)(state);
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
    use crate::typeahead::tests::{at, run, state_of, types_of};
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
                    .line_text(projection.line_at(line.from()).expect("a line"))
                    .is_some_and(|text| text == needle)
            })
            .expect("a line holding the text");
        line.from()
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
        let state = at(&state, projection_of(&state).lines()[1].to());
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
            let enter = enter_with(&types_of(&state), Some(&wrap), None);
            assert_eq!(after(&state, &enter).as_deref(), Some(expected), "{source}");
        }
    }

    /// Backspace at the start of a nested list's first item joins it to the
    /// item the list is in, as a paragraph of that item, as Typora does; in a
    /// top-level list's first item it leaves the list altogether.
    #[test]
    fn backspace_at_a_first_items_start_joins_the_item_above_or_leaves_the_list() {
        let state = state_of("- one\n  - two\n  - three");
        let state = at(&state, caret_in(&state, "two"));
        let joined = after(&state, &backspace(&types_of(&state))).expect("the join applies");
        assert_eq!(joined, "- one\n\n  two\n\n  - three");
        let state = state_of("- one\n- two");
        let state = at(&state, caret_in(&state, "one"));
        let lifted = after(&state, &backspace(&types_of(&state))).expect("the lift applies");
        assert_eq!(lifted, "one\n\n- two");
    }

    #[test]
    fn backspace_at_a_later_items_start_joins_it_to_the_item_before() {
        for (source, line, expected) in [
            ("- one\n- two", "two", "- one\n\n  two"),
            ("- one\n- two\n- three", "two", "- one\n\n  two\n\n- three"),
            ("1. one\n2. two", "two", "1. one\n\n   two"),
            ("- one\n  - a\n  - b", "b", "- one\n  - a\n\n    b"),
        ] {
            let state = state_of(source);
            let state = at(&state, caret_in(&state, line));
            let joined = after(&state, &backspace(&types_of(&state))).expect("the join applies");
            assert_eq!(joined, expected, "{source:?}");
        }
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
        let end = projection_of(&state).lines()[0].to();
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
        let sunk = after(&state, &indent(&types_of(&state), "\t")).expect("the sink applies");
        assert_eq!(sunk, "- one\n  - two");
        let state = state_of("- one\n  - two");
        let state = at(&state, caret_in(&state, "two"));
        let lifted = after(&state, &outdent(&types_of(&state), "\t")).expect("the lift applies");
        assert_eq!(lifted, "- one\n- two");
    }

    /// Over a selection in a code block, Tab indents every line it covers and
    /// Shift-Tab outdents them; a line the selection only reaches the start of
    /// stays as it is. Shift-Tab at a caret outdents its own line.
    #[test]
    fn tab_and_shift_tab_shift_the_lines_of_a_code_selection() {
        let state = state_of("```\na\n\tb\n    c\nd\n```");
        let types = types_of(&state);
        let first = projection_of(&state).lines()[0].from();
        let select = |from: usize, to: usize| {
            state
                .update([
                    TransactionSpec::new().selection(Selection::text(first + from, first + to))
                ])
                .unwrap()
                .state()
                .clone()
        };
        // `a` to the start of `d`: three lines.
        let indented = after(&select(0, 11), &indent(&types, "\t"));
        assert_eq!(
            indented.as_deref(),
            Some("```\n\ta\n\t\tb\n\t    c\nd\n```")
        );
        let outdented = after(&select(0, 11), &outdent(&types, "\t"));
        assert_eq!(outdented.as_deref(), Some("```\na\nb\nc\nd\n```"));
        let spaces = after(&select(0, 11), &indent(&types, "  "));
        assert_eq!(spaces.as_deref(), Some("```\n  a\n  \tb\n      c\nd\n```"));
        let caret = after(&at(&state, first + 3), &outdent(&types, "\t"));
        assert_eq!(caret.as_deref(), Some("```\na\nb\n    c\nd\n```"));
    }

    #[test]
    fn tab_in_a_code_block_inserts_the_indent_text_it_is_given() {
        let state = state_of("```\nab\n```");
        let state = at(&state, caret_in(&state, "ab") + 1);
        let types = types_of(&state);
        assert_eq!(
            after(&state, &indent(&types, "\t")).as_deref(),
            Some("```\na\tb\n```")
        );
        assert_eq!(
            after(&state, &indent(&types, "    ")).as_deref(),
            Some("```\na    b\n```")
        );
    }

    /// Return in a code block starts the new line at the depth of the line it
    /// splits, in the spaces or tabs that line uses, and no deeper than the
    /// caret: splitting inside the indent carries only what lies before it.
    #[test]
    fn return_in_a_code_block_keeps_the_lines_indent() {
        for (source, needle, offset, typed) in [
            (
                "```\nfn a() {\n    x\n```",
                "    x",
                5,
                "```\nfn a() {\n    x\n    y\n```",
            ),
            ("```\n\tx\n```", "\tx", 2, "```\n\tx\n\ty\n```"),
            ("```\n  \t x\n```", "  \t x", 5, "```\n  \t x\n  \t y\n```"),
            ("```\n    x\n```", "    x", 2, "```\n  \n  y  x\n```"),
            ("```\nx\n```", "x", 1, "```\nx\ny\n```"),
        ] {
            let state = state_of(source);
            let mut start = None;
            state.doc().descendants(&mut |node, pos, _, _| {
                if let Some(at) = node.text().and_then(|text| text.find(needle)) {
                    start = start.or(Some(pos + at));
                }
                true
            });
            let state = at(&state, start.expect("the line") + offset);
            let entered = applied(&state, &enter(&types_of(&state))).expect("a new line");
            let entered = run(&entered, &markraft_core::commands::insert_text("y"));
            assert_eq!(
                to_markdown(state.schema(), entered.doc()),
                typed,
                "{source:?}"
            );
        }
    }

    /// Return at the end of a paragraph that more blocks of its item follow
    /// splits the item, as Typora does: the new item takes those blocks. Until
    /// something is typed it opens on an empty line, which the source holds as
    /// the marker alone on its line.
    #[test]
    fn return_before_more_blocks_of_an_item_splits_it() {
        for (source, new) in [
            ("- first\n\n  para2\n- next\n", "- new\n\n  para2"),
            (
                "- [ ] first\n\n  para2\n- [ ] next\n",
                "- [ ] new\n\n  para2",
            ),
            ("- first\n  - sub\n- next\n", "- new\n  - sub"),
        ] {
            let state = state_of(source);
            let state = at(&state, caret_in(&state, "first") + 5);
            let entered = applied(&state, &enter(&types_of(&state))).expect("a split");
            let baseline = markraft_commonmark::SourceDocument::parse(state.schema(), source)
                .expect("the source parses");
            assert!(
                baseline.render(state.schema(), entered.doc()).is_ok(),
                "{source:?}: {}",
                state.schema().describe(entered.doc())
            );
            let typed = run(&entered, &markraft_core::commands::insert_text("new"));
            let markdown = to_markdown(state.schema(), typed.doc());
            assert!(markdown.contains(new), "{source:?}: {markdown:?}");
            assert!(
                baseline.render(state.schema(), typed.doc()).is_ok(),
                "{source:?}: {markdown:?}"
            );
        }
        // A code block keeps Return for its own newlines, wherever it is in
        // the item: no split cuts it in two or moves what follows it.
        for (source, offset, expected) in [
            (
                "- ```\n  ab\n  ```\n\n  para\n",
                2,
                "- ```\n  ab\n  \n  ```\n\n  para",
            ),
            ("- ```\n  ab\n  ```\n", 1, "- ```\n  a\n  b\n  ```"),
            (
                "- [ ] \n  ```\n  ab\n  ```\n",
                2,
                "- [ ] \n  ```\n  ab\n  \n  ```",
            ),
        ] {
            let state = state_of(source);
            let state = at(&state, caret_in(&state, "ab") + offset);
            assert_eq!(
                after(&state, &enter(&types_of(&state))).as_deref(),
                Some(expected),
                "{source:?}"
            );
        }
        // Where nothing follows in the item, Return still splits it.
        let state = state_of("- first\n\n  para2\n- next\n");
        let state = at(&state, caret_in(&state, "para2") + 5);
        assert_eq!(
            after(&state, &enter(&types_of(&state))).as_deref(),
            Some("- first\n\n  para2\n\n- \n\n- next")
        );
    }

    /// A list shortcut in a list of another kind turns the whole list into the
    /// kind asked for, as Typora does, rather than nesting a new list in the
    /// item. The new list is marked with the attributes the shortcut gives.
    #[test]
    fn a_list_shortcut_converts_the_whole_list_it_is_in() {
        let state = state_of("1) a\n2) b\n");
        let state = at(&state, caret_in(&state, "b"));
        let types = types_of(&state);
        let (bullet, ordered) = (types.bullet_list.unwrap(), types.ordered_list.unwrap());
        let (item, task) = (types.list_item.unwrap(), types.task_item.unwrap());
        let stars = toggle_list(
            &types,
            bullet,
            markraft_core::attrs! {"bullet_char" => "*"},
            item,
        );
        assert_eq!(after(&state, &stars).as_deref(), Some("* a\n* b"));
        let tasks = toggle_list(&types, bullet, Attrs::empty(), task);
        assert_eq!(after(&state, &tasks).as_deref(), Some("- [ ] a\n- [ ] b"));

        let state = state_of("* a\n* b\n  - nested\n");
        let state = at(&state, caret_in(&state, "b"));
        let numbers = toggle_list(&types, ordered, Attrs::empty(), item);
        assert_eq!(
            after(&state, &numbers).as_deref(),
            Some("1. a\n2. b\n   - nested"),
            "a nested list keeps its kind"
        );
        let tasks = toggle_list(&types, bullet, Attrs::empty(), task);
        assert_eq!(
            after(&state, &tasks).as_deref(),
            Some("* [ ] a\n* [ ] b\n  - nested"),
            "the same list keeps its marker when only its items change"
        );

        let state = state_of("- [x] a\n- [ ] b\n");
        let state = at(&state, caret_in(&state, "b"));
        let numbers = toggle_list(&types, ordered, Attrs::empty(), task);
        assert_eq!(
            after(&state, &numbers).as_deref(),
            Some("1. [x] a\n2. [ ] b"),
            "a task keeps its box"
        );
        let loose = state_of("1. a\n\n2. b\n");
        let loose = at(&loose, caret_in(&loose, "b"));
        assert_eq!(
            after(&loose, &toggle_list(&types, bullet, Attrs::empty(), item)).as_deref(),
            Some("- a\n\n- b"),
            "the spacing between items stays"
        );
    }

    /// Tab walks a table in row-major order and grows it rather than falling
    /// out of it, which is what it does in Typora and Bear.
    #[test]
    fn tab_walks_the_cells_and_appends_a_row_past_the_last_one() {
        let (state, markdown) = table_state();
        let types = types_of(&state);
        let lines = projection_of(&state);
        let (first, last) = (lines.lines()[0].from(), lines.lines()[3].to());
        let stepped = applied(&at(&state, first), &indent(&types, "\t")).expect("Tab steps right");
        assert_eq!(to_markdown(state.schema(), stepped.doc()), markdown);
        assert_eq!(cell_of(&stepped), Some((0, 1)));
        let grown = applied(&at(&state, last), &indent(&types, "\t")).expect("Tab grows the table");
        assert_eq!(projection_of(&grown).lines().len(), 6, "a row was appended");
        assert_eq!(cell_of(&grown), Some((2, 0)));
        // ⇧Tab steps back, and stops rather than lifting the first cell out of
        // its row, which would leave that row one cell short.
        let back = applied(&at(&state, lines.lines()[1].from()), &outdent(&types, "\t"))
            .expect("⇧Tab steps left");
        assert_eq!(cell_of(&back), Some((0, 0)));
        let stopped = applied(&at(&state, first), &outdent(&types, "\t"));
        assert!(stopped.is_none(), "⇧Tab in the first cell does nothing");
    }

    /// A cell that split would leave its row one cell wider than the rest, so
    /// Enter moves down a row instead, appending one at the bottom.
    #[test]
    fn enter_moves_down_a_row_and_never_splits_a_cell() {
        let (state, markdown) = table_state();
        let types = types_of(&state);
        let lines = projection_of(&state);
        let inside = lines.lines()[0].to();
        let moved = applied(&at(&state, inside), &enter(&types)).expect("Enter applies");
        assert_eq!(
            to_markdown(state.schema(), moved.doc()),
            markdown,
            "nothing was split"
        );
        assert_eq!(cell_of(&moved), Some((1, 0)));
        let grown = applied(&at(&state, lines.lines()[3].to()), &enter(&types)).expect("Enter");
        assert_eq!(projection_of(&grown).lines().len(), 6, "a row was appended");
        assert_eq!(cell_of(&grown), Some((2, 1)));
        // ⌘⏎ adds a row under the caret's own row rather than at the bottom,
        // and moves into its first cell to fill it in, as Typora does.
        let added = applied(&at(&state, inside), &toggle_task(&types)).expect("⌘⏎ adds a row");
        assert_eq!(projection_of(&added).lines().len(), 6);
        assert_eq!(cell_of(&added), Some((1, 0)));
    }

    /// Joining across a cell boundary would merge two cells and leave their
    /// rows short, so a deletion that reaches one stops there.
    #[test]
    fn backspace_stops_at_a_cell_boundary_but_still_deletes_inside_one() {
        let (state, markdown) = table_state();
        let types = types_of(&state);
        let lines = projection_of(&state);
        let (start, end) = (lines.lines()[1].from(), lines.lines()[1].to());
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
            .update([TransactionSpec::new().selection(Selection::text(
                lines.lines()[0].from(),
                lines.lines()[1].to(),
            ))])
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
        let start = projection_of(&state).lines()[0].from();
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
    fn backspace_at_heading_start_makes_a_paragraph_and_hash_promotes() {
        for heading in ["# title", "## title", "###### title"] {
            let state = state_of(heading);
            let types = types_of(&state);
            let start = projection_of(&state).lines()[0].from();
            let cleared = applied(&at(&state, start), &backspace(&types)).expect("clears");
            assert_eq!(
                to_markdown(state.schema(), cleared.doc()),
                "title",
                "{heading:?}"
            );
        }
        let state = state_of("# title");
        let types = types_of(&state);
        let start = projection_of(&state).lines()[0].from();
        let promoted = applied(&at(&state, start), &insert_plain(&types, "#")).expect("promotes");
        assert_eq!(to_markdown(state.schema(), promoted.doc()), "## title");
    }

    /// Typora 1.14.10: a heading that opens an item or a quote keeps its
    /// level and loses the container, as a paragraph there would; anywhere
    /// else it becomes a paragraph.
    #[test]
    fn backspace_at_a_heading_opening_a_container_takes_the_container() {
        for (source, line, expected) in [
            ("- # 1", 0, "# 1"),
            ("> # 1", 0, "# 1"),
            ("- [ ] # 1", 0, "# 1"),
            // Typora leaves the list loose; a heading needs no blank line
            // to stay apart, so the two read the same.
            ("- [ ] 0\n- [ ] # 1", 1, "- [ ] 0\n  # 1"),
            ("> 0\n>\n> # 1", 1, "> 0\n>\n> 1"),
            ("- 0\n\n  # 1", 1, "- 0\n\n  1"),
        ] {
            let state = state_of(source);
            let types = types_of(&state);
            let start = projection_of(&state).lines()[line].from();
            let after = applied(&at(&state, start), &backspace(&types)).expect("applies");
            assert_eq!(
                to_markdown(state.schema(), after.doc()),
                expected,
                "{source:?}"
            );
        }
    }

    #[test]
    fn backspace_at_quote_start_lifts() {
        let state = state_of("> quoted");
        let types = types_of(&state);
        let start = projection_of(&state).lines()[0].from();
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
        let bullets = toggle_list(&types, bullet, Attrs::empty(), item);
        assert_eq!(after(&state, &bullets).as_deref(), Some("- text"));
        let state = state_of("- text");
        assert_eq!(after(&state, &bullets).as_deref(), Some("text"));
        // The same list with the other item kind converts in place.
        let tasks = toggle_list(&types, bullet, Attrs::empty(), task);
        assert_eq!(after(&state, &tasks).as_deref(), Some("- [ ] text"));
    }

    #[test]
    fn a_new_list_carries_the_attributes_it_is_given() {
        let state = state_of("text");
        let types = types_of(&state);
        let bullet = state.schema().node_id(md::BULLET_LIST).unwrap();
        let item = state.schema().node_id(md::LIST_ITEM).unwrap();
        let stars = toggle_list(
            &types,
            bullet,
            markraft_core::attrs! {"bullet_char" => "*"},
            item,
        );
        assert_eq!(after(&state, &stars).as_deref(), Some("* text"));
    }

    #[test]
    fn a_paragraph_can_be_wrapped_directly_in_a_task_list() {
        let state = state_of("text");
        let types = types_of(&state);
        let command = toggle_list(
            &types,
            types.bullet_list.unwrap(),
            Attrs::empty(),
            types.task_item.unwrap(),
        );
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
        let state = at(&state, projection_of(&state).lines()[0].from());
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
        let end = projection_of(&state).lines()[0].to();
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
        let state = at(&state, projection_of(&state).lines()[0].to());
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
        let emptied =
            applied(&state, &delete_range(line.from(), line.to())).expect("the text goes");
        let emptied = at(&emptied, projection_of(&emptied).lines()[0].from());
        let cleared = applied(&emptied, &backspace(&types)).expect("Backspace applies");
        assert_eq!(cleared.doc().child_count(), 1);
        assert_eq!(Some(cleared.doc().child(0).type_id()), types.paragraph);
        // A raw block with text in it still loses one character at a time.
        let state = at(&state, line.to());
        let deleted = applied(&state, &backspace(&types)).expect("a grapheme goes");
        assert_eq!(to_markdown(state.schema(), deleted.doc()), "<div");
    }

    /// An emptied raw block after another block goes in one press, as an
    /// empty paragraph would, leaving the caret where the block before ends.
    /// An empty code block is still only turned into a paragraph.
    #[test]
    fn backspace_in_an_empty_raw_block_after_a_block_deletes_it() {
        let state = state_of("a\n\n[r]: https://x.y");
        let types = types_of(&state);
        let line = projection_of(&state).lines()[1].clone();
        let emptied =
            applied(&state, &delete_range(line.from(), line.to())).expect("the text goes");
        let emptied = at(&emptied, projection_of(&emptied).lines()[1].from());
        let joined = applied(&emptied, &backspace(&types)).expect("Backspace applies");
        assert_eq!(to_markdown(state.schema(), joined.doc()), "a");
        assert_eq!(joined.doc().child_count(), 1);
        assert_eq!(Some(joined.doc().child(0).type_id()), types.paragraph);
        let end = projection_of(&joined).lines()[0].to();
        assert_eq!(joined.selection().head(joined.doc()), end);

        let state = state_of("a\n\n```\n```");
        let state = at(&state, projection_of(&state).lines()[1].from());
        let cleared = applied(&state, &backspace(&types_of(&state))).expect("Backspace applies");
        assert_eq!(cleared.doc().child_count(), 2);
        assert_eq!(Some(cleared.doc().child(1).type_id()), types.paragraph);
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
        let state = at(&state, projection_of(&state).lines()[0].to());
        assert_eq!(
            after(&state, &insert_plain(&types_of(&state), "a\nb")).as_deref(),
            Some("```\na\nb\n```")
        );
    }

    /// Return in the last item of a list, then Backspace twice: the first
    /// joins the empty item to the one before as an empty paragraph, as Typora
    /// does, the second joins that paragraph back and leaves the caret where it
    /// started — not in the list that follows.
    #[test]
    fn backspace_twice_from_a_new_last_item_returns_to_the_item_before() {
        let state = state_of("1. eight\n9. nine\n\n- bullet a");
        let types = types_of(&state);
        let end_of_nine = caret_in(&state, "nine") + 4;
        let state = applied(&at(&state, end_of_nine), &enter(&types)).expect("Return applies");
        let joined = applied(&state, &backspace(&types)).expect("Backspace joins the item");
        assert_eq!(
            joined.doc().child(0).child_count(),
            2,
            "the empty item joined"
        );
        let back = applied(&joined, &backspace(&types)).expect("Backspace joins the paragraph");
        assert_eq!(back.doc().child_count(), 2);
        assert_eq!(back.selection().head(back.doc()), end_of_nine);
        let typed = applied(&back, &markraft_core::commands::insert_text("5")).expect("typing");
        assert_eq!(
            to_markdown(typed.schema(), typed.doc()),
            "1. eight\n2. nine5\n\n- bullet a"
        );
    }

    /// Return at the end of `eight` in the middle of an ordered list, then
    /// Backspace: the new empty item joins `eight` as an empty paragraph, and
    /// what is typed next is a paragraph of that item, as Typora does. The
    /// empty paragraph writes nothing, so the list is saved as it was until then.
    #[test]
    fn backspace_in_an_empty_middle_item_joins_the_item_before() {
        let source = "7. seven\n8. eight\n9. nine\n";
        let state = state_of(source);
        let types = types_of(&state);
        let end_of_eight = caret_in(&state, "eight") + 5;
        let state = applied(&at(&state, end_of_eight), &enter(&types)).expect("Return applies");
        let joined = applied(&state, &backspace(&types)).expect("Backspace joins the item");
        assert_eq!(
            joined.doc().child(0).child_count(),
            3,
            "the empty item joined"
        );
        let saved =
            markraft_commonmark::SourceDocument::parse(joined.schema(), source).expect("parses");
        assert_eq!(
            saved.render(joined.schema(), joined.doc()).as_deref(),
            Ok(source)
        );
        let typed = applied(&joined, &markraft_core::commands::insert_text("5")).expect("typing");
        assert_eq!(
            to_markdown(typed.schema(), typed.doc()),
            "7. seven\n\n8. eight\n\n   5\n\n9. nine"
        );
    }

    #[test]
    fn backspace_in_an_empty_middle_bullet_or_task_item_joins_the_item_before() {
        for (source, line, expected) in [
            (
                "- one\n- two\n- three",
                "two",
                "- one\n\n- two\n\n  5\n\n- three",
            ),
            (
                "- [ ] one\n- [ ] two\n- [ ] three",
                "two",
                "- [ ] one\n\n- [ ] two\n\n  5\n\n- [ ] three",
            ),
        ] {
            let state = state_of(source);
            let types = types_of(&state);
            let end = caret_in(&state, line) + line.len();
            let state = applied(&at(&state, end), &enter(&types)).expect("Return applies");
            let joined = applied(&state, &backspace(&types)).expect("Backspace joins the item");
            let typed =
                applied(&joined, &markraft_core::commands::insert_text("5")).expect("typing");
            assert_eq!(
                to_markdown(typed.schema(), typed.doc()),
                expected,
                "{source}"
            );
        }
    }

    /// Delete at the end of a textblock and Backspace at the start of one right
    /// after a list or a quote join the two textblocks' text wherever they sit,
    /// as Typora does. A code block keeps its text to itself.
    #[test]
    fn delete_and_backspace_join_text_across_lists_and_quotes() {
        let forward = [
            ("- one\n- two", "one", "- onetwo"),
            ("zero\n\n- one", "zero", "zeroone"),
            ("- one\n\ntwo", "one", "- onetwo"),
            ("- one\n  - two", "one", "- onetwo"),
            ("> one\n\ntwo", "one", "> onetwo"),
        ];
        for (source, line, expected) in forward {
            let state = state_of(source);
            let state = at(&state, caret_in(&state, line) + line.len());
            let joined = after(&state, &delete_forward(&types_of(&state)));
            assert_eq!(joined.as_deref(), Some(expected), "Delete in {source:?}");
        }
        let backward = [
            ("- one\n\ntwo", "two", "- onetwo"),
            ("> one\n\ntwo", "two", "> onetwo"),
            ("- one\n  - two\n\nthree", "three", "- one\n  - twothree"),
        ];
        for (source, line, expected) in backward {
            let state = state_of(source);
            let state = at(&state, caret_in(&state, line));
            let joined = after(&state, &backspace(&types_of(&state)));
            assert_eq!(joined.as_deref(), Some(expected), "Backspace in {source:?}");
        }
        // Before a code block or a table, Delete does nothing.
        for source in ["one\n\n```\ncode\n```", "one\n\n| a |\n| - |\n| b |"] {
            let state = state_of(source);
            let state = at(&state, caret_in(&state, "one") + 3);
            assert_eq!(
                after(&state, &delete_forward(&types_of(&state))),
                None,
                "{source:?}"
            );
        }
    }

    /// Around a divider, Backspace and Delete take it at once, as Typora does,
    /// rather than first selecting it: Backspace leaves the caret where it was,
    /// and Delete carries the text after the divider on the line it ends.
    #[test]
    fn backspace_and_delete_take_a_divider_at_once() {
        let state = state_of("one\n\n---\n\ntwo");
        let types = types_of(&state);
        let back = applied(&at(&state, caret_in(&state, "two")), &backspace(&types))
            .expect("Backspace takes the divider");
        assert_eq!(to_markdown(back.schema(), back.doc()), "one\n\ntwo");
        assert_eq!(
            back.selection().head(back.doc()),
            caret_in(&back, "two"),
            "the caret stays at the start of the line"
        );
        let forward = applied(
            &at(&state, caret_in(&state, "one") + 3),
            &delete_forward(&types),
        )
        .expect("Delete takes the divider");
        assert_eq!(to_markdown(forward.schema(), forward.doc()), "onetwo");
        assert_eq!(
            forward.selection().head(forward.doc()),
            caret_in(&forward, "onetwo") + 3
        );
        // With nothing to join after it, Delete still takes the divider.
        let last = state_of("one\n\n---");
        let gone = after(
            &at(&last, caret_in(&last, "one") + 3),
            &delete_forward(&types_of(&last)),
        );
        assert_eq!(gone.as_deref(), Some("one"));
    }

    /// → leaves a selected divider for the line after it, and makes one when
    /// the divider ends the note; ← goes back to the line before.
    #[test]
    fn arrows_leave_a_selected_divider() {
        for (source, expected) in [
            ("one\n\n---\n\ntwo", "one\n\n---\n\ntwo"),
            ("one\n\n---", "one\n\n---\n\n"),
        ] {
            let state = state_of(source);
            let rule = projection_of(&state).lines()[1].from();
            let selected = state
                .update([TransactionSpec::new().selection(Selection::node(rule))])
                .expect("the divider is selectable")
                .state()
                .clone();
            let right = applied(&selected, &move_grapheme(Direction::Forward, false))
                .expect("→ leaves the divider");
            assert!(right.selection().is_cursor(), "{source:?}");
            assert_eq!(
                to_markdown(right.schema(), right.doc()),
                expected.trim_end()
            );
            assert!(
                right.selection().head(right.doc()) > rule,
                "{source:?}: the caret is past the divider"
            );
            let left = applied(&selected, &move_grapheme(Direction::Backward, false))
                .expect("← leaves the divider");
            assert_eq!(
                left.selection(),
                &Selection::cursor(caret_in(&state, "one") + 3)
            );
        }
    }

    /// ← from the start of the line after a divider and → from the end of the
    /// line before it select the divider, as ↑ and ↓ do; with Shift, the
    /// selection grows over it to the text beyond. No caret is left beside it,
    /// where there is no line to type in.
    #[test]
    fn arrows_onto_a_divider_select_it() {
        let state = state_of("one\n\n---\n\ntwo");
        let rule = projection_of(&state).lines()[1].from();
        let end_of_one = caret_in(&state, "one") + 3;
        let start_of_two = caret_in(&state, "two");
        for (from, dir) in [
            (start_of_two, Direction::Backward),
            (end_of_one, Direction::Forward),
        ] {
            let moved =
                applied(&at(&state, from), &move_grapheme(dir, false)).expect("the arrow moves");
            assert_eq!(moved.selection(), &Selection::node(rule), "{dir:?}");
        }
        let grown = applied(
            &at(&state, end_of_one),
            &move_grapheme(Direction::Forward, true),
        )
        .expect("Shift-→ extends");
        assert_eq!(
            grown.selection(),
            &Selection::text(end_of_one, start_of_two)
        );
    }

    /// Backspace at the start of a paragraph right after a table carries its
    /// text into the table's last cell, as Typora does.
    #[test]
    fn backspace_after_a_table_joins_its_last_cell() {
        let state = state_of("| a | b |\n| - | - |\n| c | d |\n\ntwo");
        let state = at(&state, caret_in(&state, "two"));
        let joined = applied(&state, &backspace(&types_of(&state))).expect("the join applies");
        let markdown = to_markdown(joined.schema(), joined.doc());
        assert!(markdown.contains("| dtwo"), "{markdown}");
        assert_eq!(joined.doc().child_count(), 1, "the paragraph is gone");
        assert_eq!(
            cell_of(&joined),
            Some((1, 1)),
            "the caret is in the last cell"
        );
    }

    /// Backspace at the start of a quote's later paragraph joins the paragraph
    /// before it in the same quote, as Typora does; only the quote's first
    /// block lifts out of it.
    #[test]
    fn backspace_in_a_quotes_later_paragraph_joins_the_one_before() {
        let state = state_of("> one\n>\n> two");
        let state = at(&state, caret_in(&state, "two"));
        let joined = after(&state, &backspace(&types_of(&state)));
        assert_eq!(joined.as_deref(), Some("> onetwo"));
        let first = state_of("> one\n>\n> two");
        let lifted = after(
            &at(&first, caret_in(&first, "one")),
            &backspace(&types_of(&first)),
        );
        assert_eq!(lifted.as_deref(), Some("one\n\n> two"));
    }

    /// The first item of a list has nothing before it to return to: an empty
    /// one leaves the list as before.
    #[test]
    fn backspace_in_an_empty_first_item_still_leaves_the_list() {
        let state = state_of("- \n- two");
        let types = types_of(&state);
        let state = at(&state, projection_of(&state).lines()[0].from());
        let lifted = applied(&state, &backspace(&types)).expect("Backspace lifts the item");
        assert_eq!(lifted.doc().child_count(), 2, "a paragraph before the list");
    }

    /// Return at the start of a list's first item, ↑ into the empty item it
    /// leaves, Backspace to lift it out, then type: the file saved from the
    /// original source has one blank line either side of the new paragraph,
    /// for an ordered list and a bullet list alike. The last item, which has an
    /// item before it, joins that one instead, as Typora does: what is typed is
    /// a paragraph of the item before, and the list is written loose.
    #[test]
    fn typing_into_a_lifted_first_item_saves_one_blank_line_either_side() {
        use markraft_commonmark::SourceDocument;
        for (source, line, at_start, expected) in [
            (
                "# Lists\n\n1. one\n2. two\n",
                "one",
                true,
                "# Lists\n\n5\n\n1. one\n2. two\n",
            ),
            (
                "# Lists\n\n- one\n- two\n",
                "one",
                true,
                "# Lists\n\n5\n\n- one\n- two\n",
            ),
            (
                "# Lists\n\n1. eight\n9. nine\n",
                "nine",
                false,
                "# Lists\n\n1. eight\n\n9. nine\n\n   5\n",
            ),
        ] {
            let state = state_of(source);
            let types = types_of(&state);
            let caret = caret_in(&state, line) + if at_start { 0 } else { line.len() };
            let state = applied(&at(&state, caret), &enter(&types)).expect("Return applies");
            let empty = projection_of(&state)
                .lines()
                .iter()
                .find(|line| line.is_empty())
                .expect("an empty item")
                .from();
            let lifted =
                applied(&at(&state, empty), &backspace(&types)).expect("Backspace lifts the item");
            let typed =
                applied(&lifted, &markraft_core::commands::insert_text("5")).expect("typing");
            let saved = SourceDocument::parse(typed.schema(), source).expect("the source parses");
            assert_eq!(
                saved.render(typed.schema(), lifted.doc()).as_deref(),
                Ok(source),
                "the empty paragraph alone changes nothing on disk: {source:?}"
            );
            assert_eq!(
                saved.render(typed.schema(), typed.doc()).as_deref(),
                Ok(expected),
                "{source:?}"
            );
        }
    }

    /// Word motion steps over what a reader sees: never between the two `*`
    /// of a hidden `**`, and over a hidden span's delimiters to the word they
    /// open, so what is typed there lands outside the span.
    #[test]
    fn word_motion_steps_over_hidden_delimiters() {
        let state = state_of("hello **world** end");
        let types = types_of(&state);
        let end = caret_in(&state, "hello **world** end") + "hello **world** end".len();
        let back = move_word(&types, Direction::Backward, false);
        let once = applied(&at(&state, end), &back).expect("⌥← applies");
        let twice = applied(&once, &back).expect("⌥← applies again");
        let typed = markraft_core::commands::insert_text("X");
        assert_eq!(
            after(&once, &typed).as_deref(),
            Some("hello **world** Xend")
        );
        assert_eq!(
            after(&twice, &typed).as_deref(),
            Some("hello X**world** end")
        );

        let start = caret_in(&state, "hello **world** end") + "hello".len();
        let forward = move_word(&types, Direction::Forward, false);
        let moved = applied(&at(&state, start), &forward).expect("⌥→ applies");
        assert_eq!(
            after(&moved, &typed).as_deref(),
            Some("hello **world**X end")
        );
    }

    /// ⌥⌫ deletes a hidden span whole rather than one of its delimiter
    /// characters at a time.
    #[test]
    fn word_deletion_takes_a_hidden_span_whole() {
        let state = state_of("hello **world** end");
        let types = types_of(&state);
        let end = caret_in(&state, "hello **world** end") + "hello **world** end".len();
        let delete = delete_word(&types, Direction::Backward);
        let once = applied(&at(&state, end), &delete).expect("⌥⌫ applies");
        let twice = applied(&once, &delete).expect("⌥⌫ applies again");
        let typed = markraft_core::commands::insert_text("X");
        assert_eq!(after(&once, &typed).as_deref(), Some("hello **world** X"));
        assert_eq!(after(&twice, &typed).as_deref(), Some("hello X"));
    }

    /// Inside a revealed span its delimiters are text a reader sees, but a run
    /// of them is still one stop, not one per character.
    #[test]
    fn word_motion_never_stops_inside_a_revealed_delimiter_run() {
        let state = state_of("a **bc** d");
        let types = types_of(&state);
        let inside = caret_in(&state, "a **bc** d") + "a **bc".len();
        let forward = move_word(&types, Direction::Forward, false);
        let moved = applied(&at(&state, inside), &forward).expect("⌥→ applies");
        let typed = markraft_core::commands::insert_text("X");
        assert_eq!(after(&moved, &typed).as_deref(), Some("a **bc**X d"));
    }

    /// Shift-Return breaks the line inside the block: a hard break, spelled
    /// with the trailing `\` the file keeps, and a newline in a code block.
    #[test]
    fn shift_return_breaks_the_line_inside_the_block() {
        let typed = markraft_core::commands::insert_text("b");
        let state = state_of("a");
        let types = types_of(&state);
        let broken = applied(
            &at(&state, caret_in(&state, "a") + 1),
            &line_break(&types, None),
        )
        .expect("Shift-Return applies");
        assert_eq!(after(&broken, &typed).as_deref(), Some("a\\\nb"));
        // Left with nothing after it, the break goes, and its `\` with it.
        let state = state_of("a\n\nz");
        let broken = applied(
            &at(&state, caret_in(&state, "a") + 1),
            &line_break(&types, None),
        )
        .expect("Shift-Return applies");
        let left = at(&broken, caret_in(&broken, "z"));
        assert_eq!(to_markdown(left.schema(), left.doc()), "a\n\nz");

        let state = state_of("**ab**");
        let broken = applied(
            &at(&state, caret_in(&state, "**ab**") + 3),
            &line_break(&types, None),
        )
        .expect("Shift-Return applies inside a span");
        assert_eq!(to_markdown(broken.schema(), broken.doc()), "**a\\\nb**");

        let state = state_of("```\nx\n```");
        let broken = applied(
            &at(&state, caret_in(&state, "x") + 1),
            &line_break(&types, None),
        )
        .expect("Shift-Return applies in code");
        assert_eq!(after(&broken, &typed).as_deref(), Some("```\nx\nb\n```"));

        // A GFM row is one line: in a table cell the break is `<br />`, as
        // Typora writes it.
        let (state, _) = table_state();
        let caret = caret_in(&state, "c") + 1;
        let broken = applied(&at(&state, caret), &line_break(&types, None))
            .expect("Shift-Return applies in a cell");
        let markdown = to_markdown(broken.schema(), broken.doc());
        assert!(markdown.contains("c<br />"), "{markdown:?}");
    }

    /// A kind that spells hard breaks with two spaces gets them from
    /// Shift-Return, and a break already spelled with `\` keeps it.
    #[test]
    fn shift_return_spells_the_break_as_the_kind_says() {
        let spaces: crate::BreakSpelling = std::sync::Arc::new(|| "  ");
        let typed = markraft_core::commands::insert_text("c");
        let state = state_of("x\\\ny\n\nb");
        let types = types_of(&state);
        let command = line_break(&types, Some(&spaces));
        let broken = applied(&at(&state, caret_in(&state, "b") + 1), &command)
            .expect("Shift-Return applies");
        assert_eq!(after(&broken, &typed).as_deref(), Some("x\\\ny\n\nb  \nc"));
        // Left with nothing after it, the break goes, and its spaces with it.
        let state = state_of("a\n\nz");
        let broken = applied(&at(&state, caret_in(&state, "a") + 1), &command)
            .expect("Shift-Return applies");
        let left = at(&broken, caret_in(&broken, "z"));
        assert_eq!(to_markdown(left.schema(), left.doc()), "a\n\nz");
    }

    /// Letting the caret into a picture's source is not an edit of its own:
    /// one undo takes back what was typed before it, and gives the picture
    /// back as the atom it was.
    #[test]
    fn letting_the_caret_into_a_picture_is_not_an_undo_step() {
        let state = state_of("after\n\n![a](x.png)");
        let typed = applied(
            &at(&state, caret_in(&state, "after") + "after".len()),
            &markraft_core::commands::insert_text("z"),
        )
        .expect("typing");
        let picture = projection_of(&typed).lines()[1].from();
        let reached = applied(
            &typed,
            &markraft_core::commands::command(move |_| {
                Some(TransactionSpec::new().selection(Selection::cursor(picture)))
            }),
        )
        .expect("the caret moves");
        assert_eq!(
            projection_of(&reached).line_text(1),
            Some("![a](x.png)"),
            "the caret found the source"
        );
        let undone = applied(&reached, &history(true)).expect("undo applies");
        let schema = undone.schema();
        assert_eq!(to_markdown(schema, undone.doc()), "after\n\n![a](x.png)");
        assert_eq!(undone.doc(), state.doc(), "the picture is the atom again");
    }
}
