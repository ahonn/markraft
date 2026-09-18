//! What each key does, given the state and the editor: the cursor is read from the
//! [`Host`], the pure motion and edit layers decide, and the result goes back through
//! the host's selection, transaction and clipboard funnels.

use crate::host::Host;
use crate::{
    edit::{self, Register},
    motion::{self, Motion, Span},
    state::{Mode, Operator, State},
};
use markraft_core::{Document, Position, Selection};
use std::ops::Range;

/// Normal mode keeps the cursor on a grapheme, never past the last one of a non-empty
/// line, so that the block caret always has a character to cover.
pub(crate) fn clamp(document: &Document, position: Position) -> Position {
    let position = document.clamp_position(position);
    let text = document.blocks[position.block].text();
    if text.is_empty() || position.byte < text.len() {
        return position;
    }
    Position {
        block: position.block,
        byte: motion::last_grapheme(&text),
    }
}

/// The grapheme the cursor rests on. In Normal mode that is the caret itself; a forward
/// Visual selection ends one grapheme past it, because vim's selection includes the
/// character under the cursor.
pub(crate) fn cursor(state: &State, cx: &impl Host) -> Position {
    let selection = cx.selection();
    let document = cx.document();
    match state.mode {
        Mode::VisualLine => Position {
            block: selection.head.block,
            byte: 0,
        },
        Mode::Visual if selection.head > selection.anchor && selection.head.byte > 0 => Position {
            block: selection.head.block,
            byte: motion::previous_grapheme(
                &document.blocks[selection.head.block].text(),
                selection.head.byte,
            ),
        },
        _ => clamp(document, selection.head),
    }
}

/// The selection a visual mode shows for an anchor and a cursor, keeping its direction
/// so that the caret stays at the moving end.
fn visual_selection(
    document: &Document,
    anchor: Position,
    cursor: Position,
    linewise: bool,
) -> Selection {
    if linewise {
        let first = anchor.block.min(cursor.block);
        let last = anchor
            .block
            .max(cursor.block)
            .min(document.blocks.len() - 1);
        let start = Position {
            block: first,
            byte: 0,
        };
        let end = Position {
            block: last,
            byte: document.blocks[last].len(),
        };
        return if cursor.block >= anchor.block {
            Selection {
                anchor: start,
                head: end,
            }
        } else {
            Selection {
                anchor: end,
                head: start,
            }
        };
    }
    // The grapheme at the far end belongs to the selection, so whichever end leads
    // reaches one grapheme past it.
    let past = |position: Position| Position {
        block: position.block,
        byte: motion::next_grapheme(&document.blocks[position.block].text(), position.byte),
    };
    if cursor >= anchor {
        Selection {
            anchor,
            head: past(cursor),
        }
    } else {
        Selection {
            anchor: past(anchor),
            head: cursor,
        }
    }
}

/// Show `target` as the new cursor: a caret in Normal mode, the moving end of the
/// selection in a visual one.
fn move_cursor(state: &State, cx: &mut impl Host, target: Position) {
    let selection = if state.mode.is_visual() {
        visual_selection(
            cx.document(),
            state.visual_anchor,
            target,
            state.mode == Mode::VisualLine,
        )
    } else {
        Selection::caret(clamp(cx.document(), target))
    };
    cx.select(selection, false);
}

/// `h l w b e 0 ^ $ gg G`, alone or completing a pending operator.
pub(crate) fn motion(state: &mut State, cx: &mut impl Host, motion: Motion) {
    let operator = state.pending.operator();
    let count = state.pending.take();
    let from = cursor(state, cx);
    // vim's special case: `cw` on a word changes to its end, like `ce`, leaving the
    // whitespace after it alone.
    let motion = if operator == Some(Operator::Change)
        && motion == Motion::WordForward
        && !motion::on_whitespace(cx.document(), from)
    {
        Motion::WordEnd
    } else {
        motion
    };
    let mut target = motion::target(cx.document(), from, motion, count);
    // vim's `dw` on the last word of a line stops at the line's end instead of pulling
    // the next line up; the same rule keeps `cw` and `yw` inside one block.
    if operator.is_some() && motion == Motion::WordForward && target.block != from.block {
        target = Position {
            block: from.block,
            byte: cx.document().blocks[from.block].len(),
        };
    }
    let Some(operator) = operator else {
        move_cursor(state, cx, target);
        return;
    };
    match motion.span() {
        Span::Linewise => linewise(state, cx, operator, motion::block_range(from, target)),
        span => charwise(
            state,
            cx,
            operator,
            motion::charwise_range(cx.document(), from, target, span),
        ),
    }
}

/// `j` and `k`. They follow visual rows in Normal and charwise Visual mode, where the
/// caret is somewhere in a wrapped line; with an operator pending and in Visual Line
/// mode they are linewise, so `dj` takes two whole blocks however they wrap.
pub(crate) fn vertical(state: &mut State, cx: &mut impl Host, delta: isize) {
    let operator = state.pending.operator();
    let count = state.pending.take();
    if operator.is_some() || state.mode == Mode::VisualLine {
        let from = cursor(state, cx);
        let target = motion::target(cx.document(), from, Motion::BlockDelta(delta), count);
        match operator {
            Some(operator) => linewise(state, cx, operator, motion::block_range(from, target)),
            None => move_cursor(state, cx, target),
        }
        return;
    }
    let rows = delta.saturating_mul(count as isize);
    if state.mode != Mode::Visual {
        cx.rows(rows, false);
        return;
    }
    // Extending keeps the anchor and the remembered column, and for a selection that
    // stays the way round it was the inclusive end is already where it belongs, so the
    // rewrite below is a no-op and the column survives. Only turning round the anchor,
    // or landing on a line's first grapheme, costs the remembered column.
    cx.rows(rows, true);
    let target = cursor(state, cx);
    let selection = visual_selection(cx.document(), state.visual_anchor, target, false);
    if selection != cx.selection() {
        cx.select(selection, false);
    }
}

fn yank(state: &mut State, cx: &mut impl Host, register: Register) {
    cx.write_clipboard(register.fragment.clone(), register.text.clone());
    state.register = Some(register);
}

fn charwise(state: &mut State, cx: &mut impl Host, operator: Operator, range: Range<Position>) {
    let document = cx.document();
    let register = Register {
        fragment: document.fragment_in(range.clone()),
        text: document.text_in(range.clone()),
        linewise: false,
    };
    yank(state, cx, register);
    match operator {
        Operator::Yank => {
            // vim leaves the cursor at the start of what it yanked.
            let start = clamp(cx.document(), range.start);
            cx.select(Selection::caret(start), false);
            state.mode = Mode::Normal;
        }
        Operator::Delete => {
            cx.edit(&mut |tx| edit::delete_charwise(tx, range.clone()));
            state.mode = Mode::Normal;
        }
        Operator::Change => {
            enter_insert(state, cx);
            cx.edit(&mut |tx| edit::delete_charwise(tx, range.clone()));
        }
    }
}

fn linewise(state: &mut State, cx: &mut impl Host, operator: Operator, blocks: Range<usize>) {
    let register = edit::linewise_register(cx.document(), blocks.clone());
    yank(state, cx, register);
    match operator {
        Operator::Yank => {
            let start = motion::first_non_blank(cx.document(), blocks.start);
            cx.select(Selection::caret(start), false);
            state.mode = Mode::Normal;
        }
        Operator::Delete => {
            cx.edit(&mut |tx| edit::delete_linewise(tx, blocks.clone()));
            state.mode = Mode::Normal;
        }
        Operator::Change => {
            enter_insert(state, cx);
            cx.edit(&mut |tx| edit::change_linewise(tx, blocks.clone()));
        }
    }
}

/// `d`, `c` and `y`: on the selection in a visual mode, doubled for a whole line, and
/// otherwise armed to wait for a motion.
pub(crate) fn operator(state: &mut State, cx: &mut impl Host, operator: Operator) {
    if state.mode.is_visual() {
        selection_operator(state, cx, operator);
        return;
    }
    if !state.pending.arm(operator) {
        return;
    }
    let count = state.pending.take();
    let from = cursor(state, cx);
    let end = from
        .block
        .saturating_add(count)
        .min(cx.document().blocks.len());
    linewise(state, cx, operator, from.block..end);
}

/// `d`, `x`, `y` and `c` in a visual mode.
pub(crate) fn selection_operator(state: &mut State, cx: &mut impl Host, operator: Operator) {
    state.pending.clear();
    let from = cursor(state, cx);
    let anchor = cx.document().clamp_position(state.visual_anchor);
    if state.mode == Mode::VisualLine {
        linewise(state, cx, operator, motion::block_range(anchor, from));
    } else {
        charwise(
            state,
            cx,
            operator,
            edit::visual_range(cx.document(), anchor, from),
        );
    }
}

/// `x`: the graphemes under the cursor, or the whole selection in a visual mode.
pub(crate) fn delete_chars(state: &mut State, cx: &mut impl Host) {
    if state.mode.is_visual() {
        selection_operator(state, cx, Operator::Delete);
        return;
    }
    let count = state.pending.take();
    let from = cursor(state, cx);
    let range = edit::delete_chars_range(cx.document(), from, count);
    if range.start == range.end {
        return;
    }
    charwise(state, cx, Operator::Delete, range);
}

/// `D` and `C`. The count is consumed but not applied: they always take the rest of the
/// cursor's own line.
pub(crate) fn to_line_end(state: &mut State, cx: &mut impl Host, operator: Operator) {
    state.pending.take();
    let from = cursor(state, cx);
    let range = edit::to_line_end(cx.document(), from);
    charwise(state, cx, operator, range);
}

/// The content `p` and `P` insert. The clipboard wins, so ⌘C in another application
/// pastes here; it counts as linewise only while it still holds the last yank, which is
/// what remembers that whole blocks were taken.
fn register(state: &State, cx: &mut impl Host) -> Option<Register> {
    let Some(fragment) = cx.read_clipboard() else {
        return state.register.clone();
    };
    match &state.register {
        Some(register) if register.fragment == fragment => Some(register.clone()),
        _ => {
            let text = fragment.plain_text();
            Some(Register {
                fragment,
                text,
                linewise: false,
            })
        }
    }
}

/// `p` and `P`. The count is consumed but not repeated.
pub(crate) fn paste(state: &mut State, cx: &mut impl Host, after: bool) {
    state.pending.take();
    let Some(register) = register(state, cx) else {
        return;
    };
    let from = cursor(state, cx);
    cx.edit(&mut |tx| edit::paste(tx, from, &register, after));
    state.mode = Mode::Normal;
}

/// `u` and `ctrl-r`, `count` entries at a time.
pub(crate) fn history(state: &mut State, cx: &mut impl Host, undo: bool) {
    let count = state.pending.take();
    for _ in 0..count {
        if !cx.history(undo) {
            break;
        }
    }
    state.mode = Mode::Normal;
}

/// Where `i`, `a`, `I` and `A` leave the caret before Insert mode begins.
#[derive(Clone, Copy)]
pub(crate) enum InsertAt {
    Cursor,
    AfterCursor,
    FirstNonBlank,
    LineEnd,
}

pub(crate) fn insert(state: &mut State, cx: &mut impl Host, at: InsertAt) {
    state.pending.clear();
    let from = cursor(state, cx);
    let text = cx.document().blocks[from.block].text();
    let byte = match at {
        InsertAt::Cursor => from.byte,
        InsertAt::AfterCursor => motion::next_grapheme(&text, from.byte),
        InsertAt::FirstNonBlank => motion::first_non_blank(cx.document(), from.block).byte,
        InsertAt::LineEnd => text.len(),
    };
    // The mode changes first, so the Normal-mode clamp does not pull `A` back a grapheme.
    enter_insert(state, cx);
    cx.select(
        Selection::caret(Position {
            block: from.block,
            byte,
        }),
        false,
    );
}

/// `o` and `O`.
pub(crate) fn open_line(state: &mut State, cx: &mut impl Host, below: bool) {
    state.pending.clear();
    let from = cursor(state, cx);
    enter_insert(state, cx);
    cx.edit(&mut |tx| edit::open_line(tx, from, below));
}

/// Insert mode, with an undo group open so that the edit that opened it, everything
/// typed and any input rules undo as one step, the way vim undoes an insert session.
fn enter_insert(state: &mut State, cx: &mut impl Host) {
    state.mode = Mode::Insert;
    cx.begin_undo_group();
}

/// `v` and `V`, which toggle their own mode off and switch between each other.
pub(crate) fn visual(state: &mut State, cx: &mut impl Host, linewise: bool) {
    state.pending.clear();
    let from = cursor(state, cx);
    let wanted = if linewise {
        Mode::VisualLine
    } else {
        Mode::Visual
    };
    if state.mode == wanted {
        normal(state, cx);
        return;
    }
    if !state.mode.is_visual() {
        state.visual_anchor = from;
    }
    state.mode = wanted;
    move_cursor(state, cx, from);
}

/// Escape: back to Normal mode, with the caret on a grapheme. Leaving Insert steps one
/// grapheme left, as vim does, unless the caret already sits at the start of the line.
pub(crate) fn normal(state: &mut State, cx: &mut impl Host) {
    state.pending.clear();
    let leaving_insert = state.mode == Mode::Insert;
    if leaving_insert {
        cx.end_undo_group();
    }
    // Insert mode may rest past the last grapheme, where `cursor` would already pull it
    // back onto one; stepping left from there would then move two.
    let from = if leaving_insert {
        cx.document().clamp_position(cx.selection().head)
    } else {
        cursor(state, cx)
    };
    state.mode = Mode::Normal;
    let byte = if leaving_insert {
        motion::previous_grapheme(&cx.document().blocks[from.block].text(), from.byte)
    } else {
        from.byte
    };
    cx.select(
        Selection::caret(clamp(
            cx.document(),
            Position {
                block: from.block,
                byte,
            },
        )),
        false,
    );
}

/// Escape with half a command typed: forget the count and the operator, stay put.
pub(crate) fn clear_pending(state: &mut State, _: &mut impl Host) {
    state.pending.clear();
}

/// Keep the editor and the extension in step after anything that moved the caret or
/// changed the document, including edits the extension did not make.
pub(crate) fn settle(state: &mut State, cx: &mut impl Host, replaced: bool) -> Option<Mode> {
    if replaced {
        state.pending.clear();
        state.visual_anchor = Position::default();
    }
    if state.mode.is_visual() {
        // A click collapses the selection and re-seeds the anchor; a drag keeps it, and
        // an anchor outside the selection can only have come from outside vim.
        let (start, end) = cx.selection().ordered();
        let anchor = cx.document().clamp_position(state.visual_anchor);
        state.visual_anchor = if anchor < start || anchor > end {
            cx.selection().anchor
        } else {
            anchor
        };
    } else if state.mode == Mode::Normal {
        let selection = cx.selection();
        // A ranged selection in Normal mode was made with the mouse; leave it alone so
        // that ⌘C still copies it. The next motion collapses it.
        if selection.is_empty() {
            let clamped = clamp(cx.document(), selection.head);
            if clamped != selection.head {
                // The remembered column survives, so `j` down a short line and on keeps
                // the column it started from.
                cx.select(Selection::caret(clamped), true);
            }
        }
    }
    state.report()
}
