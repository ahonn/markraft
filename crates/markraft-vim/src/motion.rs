//! Cursor motions over a [`Projection`]. Every function here is pure: it reads the
//! projection and a position and returns a position, so the whole of vim's navigation is
//! testable without a window. Visual-row movement is the one exception and lives in the
//! GPUI layer, because only the laid-out editor knows where a wrapped row breaks.
//!
//! A vim "line" is a projection [`Line`](markraft_doc::projection::Line): one textblock,
//! or one block-level leaf such as a horizontal rule. Positions are document token
//! offsets, and inside a line one token is one `char`, so the line's text can be scanned
//! directly and the offsets added back to the line's start.

use markraft_doc::projection::Projection;
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

/// A count large enough for any document; it only bounds a runaway `9999999999j`.
pub(crate) const MAX_COUNT: usize = 1_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Motion {
    /// `h`: the previous grapheme, never leaving the line.
    Left,
    /// `l`: the next grapheme, never leaving the line. Its target may be the line's
    /// end, which is past the last grapheme; the Normal-mode clamp pulls a bare `l`
    /// back, while `dl` needs that end to delete the grapheme it sits on.
    Right,
    /// `w`: the start of the next word.
    WordForward,
    /// `b`: the start of the previous word.
    WordBackward,
    /// `e`: the last grapheme of the next word.
    WordEnd,
    /// `0`: the start of the line.
    LineStart,
    /// `^`: the first non-whitespace grapheme of the line, or its start.
    FirstNonBlank,
    /// `$`: the end of the line, `count - 1` lines further down.
    LineEnd,
    /// `gg` and `G`: the first non-blank of a line chosen by the caller.
    Line(usize),
    /// `j` and `k` with an operator pending, and in Visual Line mode: whole lines.
    LineDelta(isize),
}

/// How much of the text between the cursor and a motion's target an operator takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Span {
    /// Up to, but not including, the grapheme the target sits on.
    Exclusive,
    /// Including it.
    Inclusive,
    /// Whole lines, from the cursor's to the target's.
    Linewise,
}

impl Motion {
    pub(crate) fn span(self) -> Span {
        match self {
            Self::WordEnd => Span::Inclusive,
            Self::Line(_) | Self::LineDelta(_) => Span::Linewise,
            _ => Span::Exclusive,
        }
    }
}

/// The line `pos` falls in, clamped into the document.
pub(crate) fn line_of(projection: &Projection, pos: usize) -> usize {
    projection
        .line_at(pos)
        .unwrap_or_else(|| match projection.line_at(pos.saturating_sub(1)) {
            Some(index) => index,
            None => last_line(projection),
        })
}

/// The first line ending at or after `pos`, for a position that sits on a boundary
/// between two blocks and so belongs to no line.
pub(crate) fn line_from(projection: &Projection, pos: usize) -> usize {
    projection
        .lines()
        .iter()
        .position(|line| line.to >= pos)
        .unwrap_or_else(|| last_line(projection))
}

pub(crate) fn last_line(projection: &Projection) -> usize {
    projection.line_count().saturating_sub(1)
}

/// `pos` brought inside a line of the document.
pub(crate) fn clamp(projection: &Projection, pos: usize) -> usize {
    let line = &projection.lines()[line_of(projection, pos)];
    pos.clamp(line.from, line.to)
}

/// The text of the line `pos` sits in, with `pos` as a byte offset into it and the
/// line's start.
fn line_text(projection: &Projection, pos: usize) -> (usize, usize, &str) {
    let index = line_of(projection, pos);
    let line = &projection.lines()[index];
    let text = projection.line_text(index).unwrap_or_default();
    let offset = pos.clamp(line.from, line.to) - line.from;
    let byte = text
        .char_indices()
        .nth(offset)
        .map_or(text.len(), |(i, _)| i);
    (line.from, byte, text)
}

/// A byte offset inside a line's text as a document position.
fn at(start: usize, text: &str, byte: usize) -> usize {
    start + text[..byte.min(text.len())].chars().count()
}

/// Where `motion` takes a cursor at `from`, repeated `count` times. A repetition that
/// cannot move stops the walk, so an absurd count costs no more than a real one.
pub(crate) fn target(projection: &Projection, from: usize, motion: Motion, count: usize) -> usize {
    let from = clamp(projection, from);
    let count = count.clamp(1, MAX_COUNT);
    match motion {
        Motion::LineStart => projection.lines()[line_of(projection, from)].from,
        Motion::FirstNonBlank => first_non_blank(projection, line_of(projection, from)),
        Motion::LineEnd => {
            let line = (line_of(projection, from) + count - 1).min(last_line(projection));
            projection.lines()[line].to
        }
        Motion::Line(line) => first_non_blank(projection, line.min(last_line(projection))),
        Motion::LineDelta(delta) => {
            let delta = delta.saturating_mul(count as isize);
            let line = (line_of(projection, from) as isize)
                .saturating_add(delta)
                .clamp(0, last_line(projection) as isize) as usize;
            first_non_blank(projection, line)
        }
        _ => {
            let mut position = from;
            for _ in 0..count {
                let next = step(projection, position, motion);
                if next == position {
                    break;
                }
                position = next;
            }
            position
        }
    }
}

fn step(projection: &Projection, from: usize, motion: Motion) -> usize {
    let (start, byte, text) = line_text(projection, from);
    match motion {
        Motion::Left => at(start, text, previous_grapheme(text, byte)),
        Motion::Right => at(start, text, next_grapheme(text, byte)),
        Motion::WordForward => next_word_start(projection, from),
        Motion::WordBackward => previous_word_start(projection, from),
        Motion::WordEnd => next_word_end(projection, from),
        _ => from,
    }
}

/// Whether the grapheme at `at` is whitespace, or `at` is past the end of its line.
pub(crate) fn on_whitespace(projection: &Projection, pos: usize) -> bool {
    let (_, byte, text) = line_text(projection, pos);
    text[byte.min(text.len())..]
        .graphemes(true)
        .next()
        .is_none_or(|grapheme| grapheme.chars().all(char::is_whitespace))
}

/// The first non-whitespace grapheme of `line`, or its start when it has none. A
/// block-level leaf holds no text, so it always answers its own start.
pub(crate) fn first_non_blank(projection: &Projection, line: usize) -> usize {
    let line = line.min(last_line(projection));
    let entry = &projection.lines()[line];
    let text = projection.line_text(line).unwrap_or_default();
    let byte = text
        .grapheme_indices(true)
        .find(|(_, grapheme)| !grapheme.chars().all(char::is_whitespace))
        .map_or(0, |(byte, _)| byte);
    at(entry.from, text, byte)
}

/// The last grapheme of a non-empty line, where a Normal-mode cursor may rest.
pub(crate) fn last_grapheme_of(projection: &Projection, line: usize) -> usize {
    let entry = &projection.lines()[line.min(last_line(projection))];
    let text = projection
        .line_text(line.min(last_line(projection)))
        .unwrap_or_default();
    at(entry.from, text, last_grapheme(text))
}

/// The start of the grapheme before `byte`, or the text's start.
pub(crate) fn previous_grapheme(text: &str, byte: usize) -> usize {
    text.grapheme_indices(true)
        .map(|(index, _)| index)
        .take_while(|index| *index < byte)
        .last()
        .unwrap_or(0)
}

/// The start of the grapheme after `byte`, or the text's end.
pub(crate) fn next_grapheme(text: &str, byte: usize) -> usize {
    text.grapheme_indices(true)
        .map(|(index, _)| index)
        .find(|index| *index > byte)
        .unwrap_or(text.len())
}

/// The last grapheme of a non-empty text.
pub(crate) fn last_grapheme(text: &str) -> usize {
    text.grapheme_indices(true)
        .map(|(index, _)| index)
        .next_back()
        .unwrap_or(0)
}

/// The position one grapheme before `pos`, never leaving its line.
pub(crate) fn previous_in_line(projection: &Projection, pos: usize) -> usize {
    let (start, byte, text) = line_text(projection, pos);
    at(start, text, previous_grapheme(text, byte))
}

/// The position one grapheme after `pos`, never leaving its line.
pub(crate) fn next_in_line(projection: &Projection, pos: usize) -> usize {
    let (start, byte, text) = line_text(projection, pos);
    at(start, text, next_grapheme(text, byte))
}

/// A word is a UAX#29 word-boundary segment that is not wholly whitespace — the same
/// definition the editor already uses for ⌥← / ⌥→ and for double-click.
fn words(text: &str) -> impl DoubleEndedIterator<Item = (usize, &str)> {
    text.split_word_bound_indices()
        .filter(|(_, segment)| !segment.chars().all(char::is_whitespace))
}

fn next_word_start(projection: &Projection, from: usize) -> usize {
    let index = line_of(projection, from);
    let (start, byte, text) = line_text(projection, from);
    if let Some((found, _)) = words(text).find(|(found, _)| *found > byte) {
        return at(start, text, found);
    }
    if index == last_line(projection) {
        return start + text.chars().count();
    }
    // A blank line counts as a word, so `w` stops on it rather than skipping past.
    let next = &projection.lines()[index + 1];
    let text = projection.line_text(index + 1).unwrap_or_default();
    at(
        next.from,
        text,
        words(text).next().map_or(0, |(byte, _)| byte),
    )
}

fn previous_word_start(projection: &Projection, from: usize) -> usize {
    let index = line_of(projection, from);
    let (start, byte, text) = line_text(projection, from);
    if let Some((found, _)) = words(&text[..byte]).next_back() {
        return at(start, text, found);
    }
    if index == 0 {
        return projection.lines()[0].from;
    }
    let previous = &projection.lines()[index - 1];
    let text = projection.line_text(index - 1).unwrap_or_default();
    at(
        previous.from,
        text,
        words(text).next_back().map_or(0, |(byte, _)| byte),
    )
}

fn next_word_end(projection: &Projection, from: usize) -> usize {
    let index = line_of(projection, from);
    let (start, byte, text) = line_text(projection, from);
    if let Some(found) = word_end_after(text, Some(byte)) {
        return at(start, text, found);
    }
    if index == last_line(projection) {
        return at(start, text, last_grapheme(text));
    }
    let next = &projection.lines()[index + 1];
    let text = projection.line_text(index + 1).unwrap_or_default();
    at(next.from, text, word_end_after(text, None).unwrap_or(0))
}

/// The last grapheme of the first word ending strictly after `after`.
fn word_end_after(text: &str, after: Option<usize>) -> Option<usize> {
    words(text)
        .map(|(byte, segment)| last_grapheme(&text[byte..byte + segment.len()]) + byte)
        .find(|byte| after.is_none_or(|after| *byte > after))
}

/// The charwise range an operator covers between the cursor and a motion's target.
pub(crate) fn charwise_range(
    projection: &Projection,
    cursor: usize,
    target: usize,
    span: Span,
) -> Range<usize> {
    let start = cursor.min(target);
    let mut end = cursor.max(target);
    if span == Span::Inclusive {
        end = next_in_line(projection, end);
    }
    start..end
}

/// The line range a linewise operator covers.
pub(crate) fn line_range(projection: &Projection, cursor: usize, target: usize) -> Range<usize> {
    let a = line_of(projection, cursor);
    let b = line_of(projection, target);
    a.min(b)..a.max(b) + 1
}
