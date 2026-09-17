//! The edits vim commands make, as functions over a [`Transaction`]. Each one is the
//! whole of a command's effect on the document, so a command is one undo step and every
//! one of them can be driven straight from `markraft_core::Editor` in a test.

use crate::motion::{self, Span};
use markraft_core::{Block, BlockKind, Document, Position, Selection, Transaction};
use std::ops::Range;

/// What a yank or delete put in the unnamed register.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Register {
    /// The rich text, so marks, links and block kinds survive a round trip.
    pub fragment: Document,
    /// The same content as plain text, for other applications.
    pub text: String,
    /// Whole blocks, which `p` and `P` put below and above the cursor's block.
    pub linewise: bool,
}

/// The register a block range would yank: whole blocks, with their kind, depth, marks
/// and links, so a linewise paste restores the lines exactly as they were.
pub(crate) fn linewise_register(document: &Document, blocks: Range<usize>) -> Register {
    let blocks = clamp_blocks(document, blocks);
    let mut fragment = Document {
        blocks: document.blocks[blocks].to_vec(),
    };
    fragment.normalize();
    let text = fragment.plain_text();
    Register {
        fragment,
        text,
        linewise: true,
    }
}

fn clamp_blocks(document: &Document, blocks: Range<usize>) -> Range<usize> {
    let end = blocks.end.min(document.blocks.len());
    blocks.start.min(end)..end
}

/// Delete a charwise range and leave the cursor where it started.
pub(crate) fn delete_charwise(tx: &mut Transaction<'_>, range: Range<Position>) {
    let start = range.start;
    tx.delete_range(range);
    tx.set_selection(Selection::caret(start));
}

/// Delete whole blocks. The document keeps at least one block, so deleting everything
/// leaves the empty paragraph the core normalizes to.
pub(crate) fn delete_linewise(tx: &mut Transaction<'_>, blocks: Range<usize>) {
    let blocks = clamp_blocks(tx.document(), blocks);
    tx.remove_blocks(blocks.clone());
    let block = blocks.start.min(tx.document().blocks.len() - 1);
    tx.set_selection(Selection::caret(motion::first_non_blank(
        tx.document(),
        block,
    )));
}

/// `cc`: empty the blocks but keep the first one, so the new text inherits the list,
/// quote or code line that was changed rather than becoming a paragraph.
pub(crate) fn change_linewise(tx: &mut Transaction<'_>, blocks: Range<usize>) {
    let blocks = clamp_blocks(tx.document(), blocks);
    if blocks.is_empty() {
        return;
    }
    if blocks.end > blocks.start + 1 {
        tx.remove_blocks(blocks.start + 1..blocks.end);
    }
    let block = blocks.start;
    let end = Position {
        block,
        byte: tx.document().blocks[block].len(),
    };
    tx.delete_range(Position { block, byte: 0 }..end);
    tx.set_selection(Selection::caret(Position { block, byte: 0 }));
}

/// `p` and `P`. A linewise register becomes whole blocks below or above the cursor's
/// block; a charwise one is pasted inline, after the cursor's grapheme for `p` and at it
/// for `P`. The cursor lands where vim leaves it: on the first non-blank of the first
/// pasted line, or on the last pasted grapheme.
pub(crate) fn paste(tx: &mut Transaction<'_>, cursor: Position, register: &Register, after: bool) {
    if register.linewise {
        let index = if after {
            cursor.block + 1
        } else {
            cursor.block
        };
        tx.insert_blocks(index, register.fragment.blocks.clone());
        let block = index.min(tx.document().blocks.len() - 1);
        tx.set_selection(Selection::caret(motion::first_non_blank(
            tx.document(),
            block,
        )));
        return;
    }
    let text = tx.document().blocks[cursor.block].text();
    let at = if after {
        Position {
            block: cursor.block,
            byte: motion::next_grapheme(&text, cursor.byte),
        }
    } else {
        cursor
    };
    tx.set_selection(Selection::caret(at));
    tx.insert_fragment(register.fragment.clone());
    // The caret sits after the pasted text; vim leaves it on its last grapheme.
    let head = tx.selection().head;
    let text = tx.document().blocks[head.block].text();
    tx.set_selection(Selection::caret(Position {
        block: head.block,
        byte: motion::previous_grapheme(&text, head.byte),
    }));
}

/// `o` and `O`: a new line of the kind Enter would produce, and the cursor in it.
/// A heading or divider gives a paragraph; a list, task, quote or code line repeats
/// itself at the same depth, with a task left unchecked and a code line keeping its
/// language, so no fence is ever written or stripped.
pub(crate) fn open_line(tx: &mut Transaction<'_>, cursor: Position, below: bool) {
    let source = &tx.document().blocks[cursor.block];
    let kind = match &source.kind {
        BlockKind::Task { .. } => BlockKind::Task { checked: false },
        BlockKind::Bullet | BlockKind::Ordered | BlockKind::Quote | BlockKind::Code { .. } => {
            source.kind.clone()
        }
        BlockKind::Heading(_) | BlockKind::Paragraph | BlockKind::Divider => BlockKind::Paragraph,
    };
    let depth = source.depth;
    let index = if below {
        cursor.block + 1
    } else {
        cursor.block
    };
    tx.insert_blocks(
        index,
        vec![Block {
            kind,
            depth,
            spans: Vec::new(),
        }],
    );
    tx.set_selection(Selection::caret(Position {
        block: index,
        byte: 0,
    }));
}

/// `x`: the `count` graphemes at the cursor, never reaching past the end of the line so
/// that it cannot join two of them. A block with no text — a divider, or an empty line —
/// yields an empty range, which makes the command a no-op.
pub(crate) fn delete_chars_range(
    document: &Document,
    cursor: Position,
    count: usize,
) -> Range<Position> {
    let text = document.blocks[cursor.block].text();
    let mut end = cursor.byte;
    for _ in 0..count.clamp(1, motion::MAX_COUNT) {
        let next = motion::next_grapheme(&text, end);
        if next == end {
            break;
        }
        end = next;
    }
    cursor..Position {
        block: cursor.block,
        byte: end,
    }
}

/// `D` and `C`: from the cursor to the end of its line.
pub(crate) fn to_line_end(document: &Document, cursor: Position) -> Range<Position> {
    cursor..Position {
        block: cursor.block,
        byte: document.blocks[cursor.block].len(),
    }
}

/// The inclusive charwise range a Visual selection covers: from the anchor grapheme to
/// the cursor's, whichever way round they are.
pub(crate) fn visual_range(
    document: &Document,
    anchor: Position,
    cursor: Position,
) -> Range<Position> {
    motion::charwise_range(document, anchor, cursor, Span::Inclusive)
}

#[cfg(test)]
mod tests {
    use super::*;
    use markraft_core::{Editor, Origin, TransactionOptions};

    /// One vim command: exactly what the extension runs through `EditorCx::transact`.
    pub(crate) fn command(editor: &mut Editor, action: impl FnOnce(&mut Transaction<'_>)) {
        editor.transact(
            TransactionOptions {
                group: None,
                origin: Origin::Extension("vim"),
            },
            action,
        );
    }

    fn at(block: usize, byte: usize) -> Position {
        Position { block, byte }
    }

    #[test]
    fn a_linewise_yank_keeps_block_kinds_marks_and_depth() {
        let editor = Editor::new(Document::from_markdown("# head\n- **a**\n  - b"));
        let register = linewise_register(editor.document(), 1..3);
        assert!(register.linewise);
        assert_eq!(register.text, "a\nb");
        assert_eq!(register.fragment.blocks[0].kind, BlockKind::Bullet);
        assert!(register.fragment.blocks[0].spans[0].marks.bold);
        assert_eq!(register.fragment.blocks[1].depth, 1);
    }

    #[test]
    fn a_linewise_paste_puts_whole_blocks_below_and_above() {
        let mut editor = Editor::new(Document::from_markdown("one\ntwo"));
        let register = linewise_register(editor.document(), 0..1);
        command(&mut editor, |tx| paste(tx, at(1, 0), &register, true));
        assert_eq!(editor.document().plain_text(), "one\ntwo\none");
        assert_eq!(editor.selection().head, at(2, 0));
        command(&mut editor, |tx| paste(tx, at(0, 0), &register, false));
        assert_eq!(editor.document().plain_text(), "one\none\ntwo\none");
        assert_eq!(editor.selection().head, at(0, 0));
    }

    #[test]
    fn a_charwise_paste_lands_inline_and_leaves_the_cursor_on_its_last_grapheme() {
        let mut editor = Editor::new(Document::from_markdown("ab"));
        let register = Register {
            fragment: Document::from_markdown("**XY**"),
            text: "XY".into(),
            linewise: false,
        };
        command(&mut editor, |tx| paste(tx, at(0, 0), &register, true));
        assert_eq!(editor.document().plain_text(), "aXYb");
        assert_eq!(editor.selection().head, at(0, 2));
        assert!(editor.document().blocks[0].spans[1].marks.bold);

        let mut editor = Editor::new(Document::from_markdown("ab"));
        command(&mut editor, |tx| paste(tx, at(0, 1), &register, false));
        assert_eq!(editor.document().plain_text(), "aXYb");
    }

    #[test]
    fn open_line_repeats_a_list_and_turns_a_heading_into_a_paragraph() {
        let mut editor = Editor::new(Document::from_markdown("- [x] task\n# head"));
        command(&mut editor, |tx| open_line(tx, at(0, 0), true));
        assert_eq!(
            editor.document().blocks[1].kind,
            BlockKind::Task { checked: false }
        );
        assert_eq!(editor.selection().head, at(1, 0));
        command(&mut editor, |tx| open_line(tx, at(2, 0), false));
        assert_eq!(editor.document().blocks[2].kind, BlockKind::Paragraph);
    }

    #[test]
    fn open_line_inside_a_code_run_keeps_the_language_and_the_run() {
        let mut editor = Editor::new(Document::from_markdown("```rust\nlet a = 1;\n```"));
        command(&mut editor, |tx| open_line(tx, at(0, 0), true));
        assert_eq!(
            editor.document().blocks[1].kind,
            BlockKind::Code {
                language: "rust".into()
            }
        );
        assert_eq!(
            editor.document().to_markdown(),
            "```rust\nlet a = 1;\n\n```"
        );
    }

    #[test]
    fn open_line_next_to_a_divider_makes_a_paragraph() {
        let mut editor = Editor::new(Document::from_markdown("***"));
        command(&mut editor, |tx| open_line(tx, at(0, 0), true));
        assert_eq!(editor.document().blocks[0].kind, BlockKind::Divider);
        assert_eq!(editor.document().blocks[1].kind, BlockKind::Paragraph);
    }

    #[test]
    fn deleting_characters_stops_at_the_end_of_the_line() {
        let document = Document::from_markdown("ab\ncd");
        assert_eq!(
            delete_chars_range(&document, at(0, 1), 9),
            at(0, 1)..at(0, 2)
        );
        // A divider holds no text, so `x` finds nothing to remove.
        let document = Document::from_markdown("***");
        assert_eq!(
            delete_chars_range(&document, at(0, 0), 1),
            at(0, 0)..at(0, 0)
        );
    }

    #[test]
    fn a_linewise_change_empties_the_line_but_keeps_its_kind() {
        let mut editor = Editor::new(Document::from_markdown("- one\n- two\n- three"));
        command(&mut editor, |tx| change_linewise(tx, 0..2));
        assert_eq!(editor.document().plain_text(), "\nthree");
        assert_eq!(editor.document().blocks[0].kind, BlockKind::Bullet);
        assert_eq!(editor.selection().head, at(0, 0));
    }
}
