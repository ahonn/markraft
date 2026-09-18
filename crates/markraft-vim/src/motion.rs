//! Cursor motions over a [`Document`]. Every function here is pure: it reads the
//! document and a position and returns a position, so the whole of vim's navigation is
//! testable without a window. Visual-row movement is the one exception and lives in the
//! GPUI layer, because only the laid-out editor knows where a wrapped row breaks.

use markraft_core::{Document, Position};
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

/// A count large enough for any document; it only bounds a runaway `9999999999j`.
pub(crate) const MAX_COUNT: usize = 1_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Motion {
    /// `h`: the previous grapheme, never leaving the block.
    Left,
    /// `l`: the next grapheme, never leaving the block. Its target may be the block's
    /// end, which is past the last grapheme; the Normal-mode clamp pulls a bare `l`
    /// back, while `dl` needs that end to delete the grapheme it sits on.
    Right,
    /// `w`: the start of the next word.
    WordForward,
    /// `b`: the start of the previous word.
    WordBackward,
    /// `e`: the last grapheme of the next word.
    WordEnd,
    /// `0`: the first byte of the block.
    LineStart,
    /// `^`: the first non-whitespace grapheme of the block, or its start.
    FirstNonBlank,
    /// `$`: the end of the block, `count - 1` blocks further down.
    LineEnd,
    /// `gg` and `G`: the first non-blank of a block chosen by the caller.
    Block(usize),
    /// `j` and `k` with an operator pending, and in Visual Line mode: whole blocks.
    BlockDelta(isize),
}

/// How much of the text between the cursor and a motion's target an operator takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Span {
    /// Up to, but not including, the grapheme the target sits on.
    Exclusive,
    /// Including it.
    Inclusive,
    /// Whole blocks, from the cursor's to the target's.
    Linewise,
}

impl Motion {
    pub(crate) fn span(self) -> Span {
        match self {
            Self::WordEnd => Span::Inclusive,
            Self::Block(_) | Self::BlockDelta(_) => Span::Linewise,
            _ => Span::Exclusive,
        }
    }
}

/// Where `motion` takes a cursor at `from`, repeated `count` times. A repetition that
/// cannot move stops the walk, so an absurd count costs no more than a real one.
pub(crate) fn target(
    document: &Document,
    from: Position,
    motion: Motion,
    count: usize,
) -> Position {
    let from = document.clamp_position(from);
    let count = count.clamp(1, MAX_COUNT);
    match motion {
        Motion::LineStart => Position {
            block: from.block,
            byte: 0,
        },
        Motion::FirstNonBlank => first_non_blank(document, from.block),
        Motion::LineEnd => {
            let block = (from.block + count - 1).min(last_block(document));
            Position {
                block,
                byte: document.blocks[block].len(),
            }
        }
        Motion::Block(block) => first_non_blank(document, block.min(last_block(document))),
        Motion::BlockDelta(delta) => {
            let delta = delta.saturating_mul(count as isize);
            let block = (from.block as isize)
                .saturating_add(delta)
                .clamp(0, last_block(document) as isize) as usize;
            first_non_blank(document, block)
        }
        _ => {
            let mut position = from;
            for _ in 0..count {
                let next = step(document, position, motion);
                if next == position {
                    break;
                }
                position = next;
            }
            position
        }
    }
}

fn last_block(document: &Document) -> usize {
    document.blocks.len() - 1
}

fn step(document: &Document, from: Position, motion: Motion) -> Position {
    match motion {
        Motion::Left => Position {
            block: from.block,
            byte: previous_grapheme(&document.blocks[from.block].text(), from.byte),
        },
        Motion::Right => Position {
            block: from.block,
            byte: next_grapheme(&document.blocks[from.block].text(), from.byte),
        },
        Motion::WordForward => next_word_start(document, from),
        Motion::WordBackward => previous_word_start(document, from),
        Motion::WordEnd => next_word_end(document, from),
        _ => from,
    }
}

/// Whether the grapheme at `at` is whitespace, or `at` is past the end of its block.
pub(crate) fn on_whitespace(document: &Document, at: Position) -> bool {
    let text = document.blocks[at.block.min(last_block(document))].text();
    text[at.byte.min(text.len())..]
        .graphemes(true)
        .next()
        .is_none_or(|grapheme| grapheme.chars().all(char::is_whitespace))
}

/// The first non-whitespace grapheme of `block`, or its start when it has none. A
/// `Divider` holds no text, so it always answers zero.
pub(crate) fn first_non_blank(document: &Document, block: usize) -> Position {
    let block = block.min(last_block(document));
    let text = document.blocks[block].text();
    let byte = text
        .grapheme_indices(true)
        .find(|(_, grapheme)| !grapheme.chars().all(char::is_whitespace))
        .map_or(0, |(byte, _)| byte);
    Position { block, byte }
}

/// The start of the grapheme before `byte`, or the block's start.
pub(crate) fn previous_grapheme(text: &str, byte: usize) -> usize {
    text.grapheme_indices(true)
        .map(|(index, _)| index)
        .take_while(|index| *index < byte)
        .last()
        .unwrap_or(0)
}

/// The start of the grapheme after `byte`, or the block's end.
pub(crate) fn next_grapheme(text: &str, byte: usize) -> usize {
    text.grapheme_indices(true)
        .map(|(index, _)| index)
        .find(|index| *index > byte)
        .unwrap_or(text.len())
}

/// The last grapheme of a non-empty block, where a Normal-mode cursor may rest.
pub(crate) fn last_grapheme(text: &str) -> usize {
    text.grapheme_indices(true)
        .map(|(index, _)| index)
        .next_back()
        .unwrap_or(0)
}

/// A word is a UAX#29 word-boundary segment that is not wholly whitespace — the same
/// definition the editor already uses for ⌥← / ⌥→ and for double-click.
fn words(text: &str) -> impl DoubleEndedIterator<Item = (usize, &str)> {
    text.split_word_bound_indices()
        .filter(|(_, segment)| !segment.chars().all(char::is_whitespace))
}

fn next_word_start(document: &Document, from: Position) -> Position {
    let text = document.blocks[from.block].text();
    if let Some((byte, _)) = words(&text).find(|(byte, _)| *byte > from.byte) {
        return Position {
            block: from.block,
            byte,
        };
    }
    if from.block == last_block(document) {
        return Position {
            block: from.block,
            byte: text.len(),
        };
    }
    // A blank line counts as a word, so `w` stops on it rather than skipping past.
    let block = from.block + 1;
    let text = document.blocks[block].text();
    Position {
        block,
        byte: words(&text).next().map_or(0, |(byte, _)| byte),
    }
}

fn previous_word_start(document: &Document, from: Position) -> Position {
    let text = document.blocks[from.block].text();
    if let Some((byte, _)) = words(&text[..from.byte]).next_back() {
        return Position {
            block: from.block,
            byte,
        };
    }
    if from.block == 0 {
        return Position { block: 0, byte: 0 };
    }
    let block = from.block - 1;
    let text = document.blocks[block].text();
    Position {
        block,
        byte: words(&text).next_back().map_or(0, |(byte, _)| byte),
    }
}

fn next_word_end(document: &Document, from: Position) -> Position {
    let text = document.blocks[from.block].text();
    if let Some(byte) = word_end_after(&text, Some(from.byte)) {
        return Position {
            block: from.block,
            byte,
        };
    }
    if from.block == last_block(document) {
        return Position {
            block: from.block,
            byte: last_grapheme(&text),
        };
    }
    let block = from.block + 1;
    let text = document.blocks[block].text();
    Position {
        block,
        byte: word_end_after(&text, None).unwrap_or(0),
    }
}

/// The last grapheme of the first word ending strictly after `after`.
fn word_end_after(text: &str, after: Option<usize>) -> Option<usize> {
    words(text)
        .map(|(byte, segment)| last_grapheme(&text[byte..byte + segment.len()]) + byte)
        .find(|byte| after.is_none_or(|after| *byte > after))
}

/// The charwise range an operator covers between the cursor and a motion's target.
pub(crate) fn charwise_range(
    document: &Document,
    cursor: Position,
    target: Position,
    span: Span,
) -> Range<Position> {
    let start = cursor.min(target);
    let mut end = cursor.max(target);
    if span == Span::Inclusive {
        let text = document.blocks[end.block].text();
        end.byte = next_grapheme(&text, end.byte);
    }
    start..end
}

/// The block range a linewise operator covers.
pub(crate) fn block_range(cursor: Position, target: Position) -> Range<usize> {
    cursor.block.min(target.block)..cursor.block.max(target.block) + 1
}
