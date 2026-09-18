//! What each key does, given the state and the editor: the cursor is read from the
//! [`Host`], the pure motion and edit layers decide, and the result goes back through
//! the host's selection, transaction and clipboard funnels.

use crate::host::{self, Host};
use crate::{
    edit::{self, Register},
    motion::{self, Motion, Span},
    state::{Mode, Operator, State},
};
use markraft_core::Selection;
use markraft_core::projection::{LineKind, Projection};
use std::ops::Range;

/// Normal mode keeps the cursor on a grapheme, never past the last one of a non-empty
/// line, so that the block caret always has a character to cover.
pub(crate) fn clamp(projection: &Projection, pos: usize) -> usize {
    let pos = motion::clamp(projection, pos);
    let index = motion::line_of(projection, pos);
    let line = &projection.lines()[index];
    if line.is_empty() || pos < motion::line_end(projection, index) {
        return pos;
    }
    motion::last_grapheme_of(projection, index)
}

/// The grapheme the cursor rests on. In Normal mode that is the caret itself; a forward
/// Visual selection ends one grapheme past it, because vim's selection includes the
/// character under the cursor.
pub(crate) fn cursor(state: &State, cx: &impl Host) -> usize {
    let projection = cx.projection();
    let head = host::head(cx);
    let anchor = host::anchor(cx);
    match state.mode {
        Mode::VisualLine => motion::line_start(&projection, motion::line_of(&projection, head)),
        Mode::Visual if head > anchor => {
            let line = &projection.lines()[motion::line_of(&projection, head)];
            if head > line.from {
                motion::previous_in_line(&projection, head)
            } else {
                clamp(&projection, head)
            }
        }
        _ => clamp(&projection, head),
    }
}

/// The selection a visual mode shows for an anchor and a cursor, keeping its direction
/// so that the caret stays at the moving end.
fn visual_selection(
    projection: &Projection,
    anchor: usize,
    cursor: usize,
    linewise: bool,
) -> Selection {
    if linewise {
        let a = motion::line_of(projection, anchor);
        let b = motion::line_of(projection, cursor);
        let (first, last) = (a.min(b), a.max(b));
        let start = motion::line_start(projection, first);
        let end = motion::line_end(projection, last);
        return if b >= a {
            Selection::text(start, end)
        } else {
            Selection::text(end, start)
        };
    }
    // The grapheme at the far end belongs to the selection, so whichever end leads
    // reaches one grapheme past it.
    let past = |pos: usize| motion::next_in_line(projection, pos);
    if cursor >= anchor {
        Selection::text(anchor, past(cursor))
    } else {
        Selection::text(past(anchor), cursor)
    }
}

/// Show `target` as the new cursor: a caret in Normal mode, the moving end of the
/// selection in a visual one.
fn move_cursor(state: &State, cx: &mut impl Host, target: usize) {
    let projection = cx.projection();
    let selection = if state.mode.is_visual() {
        visual_selection(
            &projection,
            state.visual_anchor,
            target,
            state.mode == Mode::VisualLine,
        )
    } else {
        Selection::cursor(clamp(&projection, target))
    };
    cx.select(selection, false);
}

/// `h l w b e 0 ^ $ gg G`, alone or completing a pending operator.
pub(crate) fn motion(state: &mut State, cx: &mut impl Host, motion: Motion) {
    let operator = state.pending.operator();
    let count = state.pending.take();
    let projection = cx.projection();
    let from = cursor(state, cx);
    // vim's special case: `cw` on a word changes to its end, like `ce`, leaving the
    // whitespace after it alone.
    let motion = if operator == Some(Operator::Change)
        && motion == Motion::WordForward
        && !motion::on_whitespace(&projection, from)
    {
        Motion::WordEnd
    } else {
        motion
    };
    let mut target = motion::target(&projection, from, motion, count);
    // vim's `dw` on the last word of a line stops at the line's end instead of pulling
    // the next line up; the same rule keeps `cw` and `yw` inside one line.
    if operator.is_some()
        && motion == Motion::WordForward
        && motion::line_of(&projection, target) != motion::line_of(&projection, from)
    {
        target = motion::line_end(&projection, motion::line_of(&projection, from));
    }
    let Some(operator) = operator else {
        move_cursor(state, cx, target);
        return;
    };
    match motion.span() {
        Span::Linewise => {
            let lines = motion::line_range(&projection, from, target);
            linewise(state, cx, operator, lines);
        }
        span => {
            let range = motion::charwise_range(&projection, from, target, span);
            charwise(state, cx, operator, range);
        }
    }
}

/// `j` and `k`. They follow visual rows in Normal and charwise Visual mode, where the
/// caret is somewhere in a wrapped line; with an operator pending and in Visual Line
/// mode they are linewise, so `dj` takes two whole lines however they wrap.
pub(crate) fn vertical(state: &mut State, cx: &mut impl Host, delta: isize) {
    let operator = state.pending.operator();
    let count = state.pending.take();
    if operator.is_some() || state.mode == Mode::VisualLine {
        let projection = cx.projection();
        let from = cursor(state, cx);
        let target = motion::target(&projection, from, Motion::LineDelta(delta), count);
        match operator {
            Some(operator) => {
                let lines = motion::line_range(&projection, from, target);
                linewise(state, cx, operator, lines);
            }
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
    let projection = cx.projection();
    let target = cursor(state, cx);
    let selection = visual_selection(&projection, state.visual_anchor, target, false);
    if selection != *cx.state().selection() {
        cx.select(selection, false);
    }
}

fn yank(state: &mut State, cx: &mut impl Host, register: Register) {
    cx.write_clipboard(&register.slice);
    state.register = Some(register);
}

fn charwise(state: &mut State, cx: &mut impl Host, operator: Operator, range: Range<usize>) {
    let register = edit::charwise_register(cx.state(), range.clone());
    yank(state, cx, register);
    match operator {
        Operator::Yank => {
            // vim leaves the cursor at the start of what it yanked.
            let projection = cx.projection();
            let start = clamp(&projection, range.start);
            cx.select(Selection::cursor(start), false);
            state.mode = Mode::Normal;
        }
        Operator::Delete => {
            if let Some(spec) = edit::delete_charwise(cx.state(), range) {
                cx.dispatch(vec![spec]);
            }
            state.mode = Mode::Normal;
        }
        Operator::Change => {
            enter_insert(state, cx);
            if let Some(spec) = edit::delete_charwise(cx.state(), range) {
                cx.dispatch(vec![spec]);
            }
        }
    }
}

fn linewise(state: &mut State, cx: &mut impl Host, operator: Operator, lines: Range<usize>) {
    let projection = cx.projection();
    if let Some(register) = edit::linewise_register(cx.state(), &projection, lines.clone()) {
        yank(state, cx, register);
    }
    match operator {
        Operator::Yank => {
            let start = motion::first_non_blank(&projection, lines.start);
            cx.select(Selection::cursor(start), false);
            state.mode = Mode::Normal;
        }
        Operator::Delete => {
            if let Some(spec) = edit::delete_linewise(cx.state(), &projection, lines) {
                cx.dispatch(vec![spec]);
            }
            state.mode = Mode::Normal;
        }
        Operator::Change => {
            enter_insert(state, cx);
            if let Some(spec) = edit::change_linewise(cx.state(), &projection, lines) {
                cx.dispatch(vec![spec]);
            }
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
    let projection = cx.projection();
    let from = motion::line_of(&projection, cursor(state, cx));
    let end = from.saturating_add(count).min(projection.line_count());
    linewise(state, cx, operator, from..end);
}

/// `d`, `x`, `y` and `c` in a visual mode.
pub(crate) fn selection_operator(state: &mut State, cx: &mut impl Host, operator: Operator) {
    state.pending.clear();
    let projection = cx.projection();
    let from = cursor(state, cx);
    let anchor = motion::clamp(&projection, state.visual_anchor);
    if state.mode == Mode::VisualLine {
        let lines = motion::line_range(&projection, anchor, from);
        linewise(state, cx, operator, lines);
    } else {
        let range = edit::visual_range(&projection, anchor, from);
        charwise(state, cx, operator, range);
    }
}

/// `x`: the graphemes under the cursor, or the whole selection in a visual mode.
pub(crate) fn delete_chars(state: &mut State, cx: &mut impl Host) {
    if state.mode.is_visual() {
        selection_operator(state, cx, Operator::Delete);
        return;
    }
    let count = state.pending.take();
    let projection = cx.projection();
    let from = cursor(state, cx);
    let range = edit::delete_chars_range(&projection, from, count);
    if range.start == range.end {
        return;
    }
    charwise(state, cx, Operator::Delete, range);
}

/// `D` and `C`. The count is consumed but not applied: they always take the rest of the
/// cursor's own line.
pub(crate) fn to_line_end(state: &mut State, cx: &mut impl Host, operator: Operator) {
    state.pending.take();
    let projection = cx.projection();
    let from = cursor(state, cx);
    let range = edit::to_line_end(&projection, from);
    charwise(state, cx, operator, range);
}

/// The content `p` and `P` insert. The clipboard wins, so ⌘C in another application
/// pastes here; it counts as linewise only while it still holds the last yank, which is
/// what remembers that whole lines were taken.
fn register(state: &State, cx: &mut impl Host) -> Option<Register> {
    let schema = cx.state().schema().clone();
    let Some(slice) = cx.read_clipboard() else {
        return state.register.clone();
    };
    match &state.register {
        Some(register) if register.slice == slice => Some(register.clone()),
        _ => Some(Register {
            text: markraft_core::projection::slice_to_plain_text(&schema, &slice),
            slice,
            linewise: false,
            depth: 0,
        }),
    }
}

/// `p` and `P`. The count is consumed but not repeated.
pub(crate) fn paste(state: &mut State, cx: &mut impl Host, after: bool) {
    state.pending.take();
    let Some(register) = register(state, cx) else {
        return;
    };
    let projection = cx.projection();
    let from = cursor(state, cx);
    if let Some(spec) = edit::paste(cx.state(), &projection, from, &register, after) {
        cx.dispatch(vec![spec]);
    }
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
    let projection = cx.projection();
    let from = cursor(state, cx);
    let index = motion::line_of(&projection, from);
    let target = match at {
        InsertAt::Cursor => from,
        InsertAt::AfterCursor => motion::next_in_line(&projection, from),
        InsertAt::FirstNonBlank => motion::first_non_blank(&projection, index),
        InsertAt::LineEnd => motion::line_end(&projection, index),
    };
    // The mode changes first, so the Normal-mode clamp does not pull `A` back a grapheme.
    enter_insert(state, cx);
    cx.select(Selection::cursor(target), false);
}

/// `o` and `O`: a new line of the kind Enter would produce, and the cursor in it.
///
/// It is literally Enter: the caret goes to the end of the line — or its start for `O` —
/// and the editor's own Enter chain runs, so a list item repeats itself, a heading gives
/// a paragraph and a code line stays inside its block without a fence being written.
pub(crate) fn open_line(state: &mut State, cx: &mut impl Host, below: bool) {
    state.pending.clear();
    let projection = cx.projection();
    let from = cursor(state, cx);
    let index = motion::line_of(&projection, from);
    let line = &projection.lines()[index];
    enter_insert(state, cx);
    if line.kind == LineKind::LeafBlock {
        // A horizontal rule holds no text to split, so a paragraph is created beside it.
        let pos = line.ancestors.last().map_or(line.from, |own| own.before);
        cx.select(Selection::node(pos), false);
        cx.run(&markraft_core::commands::create_paragraph_near());
        return;
    }
    cx.select(
        Selection::cursor(if below {
            motion::line_end(&projection, index)
        } else {
            motion::line_start(&projection, index)
        }),
        false,
    );
    let command = markraft_gpui::commands::enter(cx.types());
    if !cx.run(&command) {
        return;
    }
    if below {
        return;
    }
    if line
        .ancestors
        .last()
        .is_some_and(|own| Some(own.node_type) == cx.types().code_block)
    {
        // Code rows share one projection line; Enter inserted a newline without
        // creating a new block. Return to the original insertion position.
        cx.select(Selection::cursor(line.from), false);
        return;
    }
    // `O` split the line in two: the empty half is above, so the caret moves back to it.
    let projection = cx.projection();
    let head = host::head(cx);
    let index = motion::line_of(&projection, head);
    if index > 0 {
        cx.select(
            Selection::cursor(motion::line_start(&projection, index - 1)),
            false,
        );
    }
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
    let projection = cx.projection();
    // Insert mode may rest past the last grapheme, where `cursor` would already pull it
    // back onto one; stepping left from there would then move two.
    let from = if leaving_insert {
        motion::clamp(&projection, host::head(cx))
    } else {
        cursor(state, cx)
    };
    state.mode = Mode::Normal;
    let target = if leaving_insert {
        motion::previous_in_line(&projection, from)
    } else {
        from
    };
    cx.select(Selection::cursor(clamp(&projection, target)), false);
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
        state.visual_anchor = 0;
    }
    let projection = cx.projection();
    if state.mode.is_visual() {
        // A click collapses the selection and re-seeds the anchor; a drag keeps it, and
        // an anchor outside the selection can only have come from outside vim.
        let doc = cx.state().doc();
        let selection = cx.state().selection();
        let (start, end) = (selection.from(doc), selection.to(doc));
        let anchor = motion::clamp(&projection, state.visual_anchor);
        state.visual_anchor = if anchor < start || anchor > end {
            host::anchor(cx)
        } else {
            anchor
        };
    } else if state.mode == Mode::Normal {
        // A ranged *text* selection in Normal mode was made with the mouse; leave it
        // alone so that ⌘C still copies it. The next motion collapses it.
        let ranged = matches!(
            cx.state().selection(),
            Selection::Text { anchor, head, .. } if anchor != head
        );
        if !ranged {
            let head = host::head(cx);
            let clamped = clamp(&projection, head);
            // Normal mode always shows a caret, so a selection it cannot show one
            // for has to become one: a node or whole-document selection — which
            // `Selection::near` answers wherever a mapped position is not inline
            // content — puts the head on a boundary between blocks, where nothing
            // is painted and no motion starts. A cursor only needs clamping, which
            // a transaction vim did not make may well have left it needing.
            if clamped != head || !cx.state().selection().is_cursor() {
                // The remembered column survives, so `j` down a short line and on keeps
                // the column it started from.
                cx.select(Selection::cursor(clamped), true);
            }
        }
    }
    state.report()
}

/// Insert text the way the platform delivers it, for a test that has no window.
#[cfg(test)]
pub(crate) fn typed(cx: &mut impl Host, text: &str) {
    let command = markraft_gpui::commands::insert_plain(cx.types(), text);
    cx.run(&command);
}
