//! Cursor motions over a [`Projection`]. Every function here is pure: it reads the
//! projection and a position and returns a position, so the whole of vim's navigation is
//! testable without a window. Visual-row movement is the one exception and lives in the
//! GPUI layer, because only the laid-out editor knows where a wrapped row breaks.
//!
//! A vim "line" is a projection [`Line`](markraft_core::projection::Line): one textblock,
//! or one block-level leaf such as a horizontal rule. Positions are document token
//! offsets throughout: the projection answers every question about graphemes and words,
//! so nothing here holds a line's text or converts an offset itself.

use markraft_core::MarkTypeId;
use markraft_core::projection::{OBJECT_REPLACEMENT, Projection};
use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;

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

/// The concealed runs a cursor leaves concealed where it stands: delimiters,
/// escapes and the like that a reader does not see there.
///
/// A motion moves over what the reader sees. Each such run is one step for
/// `h`, `l` and `x`, and no motion lands inside one: `l` from before `**b**`
/// lands on the run's start — which reveals the span — rather than on its
/// second `*`. Once revealed, a span's characters are what the reader sees,
/// and the cursor walks them one by one. What is concealed is judged afresh
/// at every step of a count, as it is for a caret in the editor: `3l` from
/// before `**b**` reveals the span on its first step and walks into it on the
/// next. Which runs these are is the view's to say; see
/// [`markraft_core::kind::conceal::concealed_steps`].
pub(crate) struct Hidden {
    syntax: Option<MarkTypeId>,
    /// Where the cursor stood when the motion began.
    caret: usize,
    runs: RefCell<HashMap<usize, Vec<Range<usize>>>>,
}

impl Hidden {
    /// The runs a cursor at `caret` leaves concealed, for a kind whose conceal
    /// role is `syntax`.
    pub(crate) fn at(syntax: Option<MarkTypeId>, caret: usize) -> Hidden {
        Hidden {
            syntax,
            caret,
            runs: RefCell::default(),
        }
    }

    /// Nothing concealed: every grapheme is a step.
    pub(crate) fn none() -> Hidden {
        Hidden::at(None, 0)
    }

    /// The runs a cursor that has stepped to `caret` leaves concealed: one
    /// step of a count is taken from where the last one stopped.
    pub(crate) fn from(&self, caret: usize) -> Hidden {
        Hidden::at(self.syntax, caret)
    }

    /// The concealed run holding `pos`, as long as `pos` is past its start.
    fn inside(&self, projection: &Projection, pos: usize) -> Option<Range<usize>> {
        self.run(projection, pos, |run| run.start < pos && pos < run.end)
    }

    /// The concealed run starting at `pos`.
    fn starting(&self, projection: &Projection, pos: usize) -> Option<Range<usize>> {
        self.run(projection, pos, |run| run.start == pos)
    }

    fn run(
        &self,
        projection: &Projection,
        pos: usize,
        matches: impl Fn(&Range<usize>) -> bool,
    ) -> Option<Range<usize>> {
        self.syntax?;
        let index = projection.line_at(pos)?;
        let mut runs = self.runs.borrow_mut();
        let line = runs.entry(index).or_insert_with(|| {
            markraft_core::kind::conceal::concealed_steps(
                self.syntax,
                &projection.lines()[index],
                self.caret,
            )
        });
        line.iter().find(|run| matches(run)).cloned()
    }

    /// The characters of line `index` that spell markup, concealed or not:
    /// never part of a word, so no word motion stops on them.
    fn spelled(&self, projection: &Projection, index: usize) -> Vec<Range<usize>> {
        let Some(line) = projection.lines().get(index) else {
            return Vec::new();
        };
        markraft_core::kind::conceal::markup_spans(self.syntax, line)
            .into_iter()
            .flatten()
            .collect()
    }

    /// `pos`, or the start of the concealed run it stands inside: where a
    /// motion going back stops.
    fn snap_back(&self, projection: &Projection, pos: usize) -> usize {
        self.inside(projection, pos).map_or(pos, |run| run.start)
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
        .position(|line| line.to() >= pos)
        .unwrap_or_else(|| last_line(projection))
}

pub(crate) fn last_line(projection: &Projection) -> usize {
    projection.line_count().saturating_sub(1)
}

/// The canonical caret at the visible start or end of a line. Inline container
/// tokens are structural positions, not additional stops for Vim motions.
pub(crate) fn line_start(projection: &Projection, index: usize) -> usize {
    let line = &projection.lines()[index.min(last_line(projection))];
    line.offset_to_pos(0).unwrap_or(line.from())
}

pub(crate) fn line_end(projection: &Projection, index: usize) -> usize {
    let line = &projection.lines()[index.min(last_line(projection))];
    line.offset_to_pos(line.len()).unwrap_or(line.to())
}

/// `pos` brought inside a line of the document.
pub(crate) fn clamp(projection: &Projection, pos: usize) -> usize {
    let line = &projection.lines()[line_of(projection, pos)];
    let pos = pos.clamp(line.from(), line.to());
    line.pos_to_offset(pos)
        .and_then(|offset| line.offset_to_pos(offset))
        .unwrap_or(line.from())
}

/// The words of one whole line, as document position ranges.
pub(crate) fn line_words(
    projection: &Projection,
    hidden: &Hidden,
    line: usize,
) -> Vec<Range<usize>> {
    let line = line.min(last_line(projection));
    let entry = &projection.lines()[line];
    words(projection, hidden, entry.from(), entry.to())
}

/// The words between `from` and `to` as vim sees them: a run of keyword
/// characters, or a run of other non-blank characters.
///
/// The editor's word segmentation makes each punctuation character a word of
/// its own, which suits option-arrow but not `w`: `**` is one word to vim, so
/// adjacent punctuation segments are joined here.
///
/// Markup is no word at all, concealed or revealed: `w` from `x` in
/// `x **bold** y` lands on `b`, and `e` on `d`, where a reader's eye goes. A
/// cursor resting on a delimiter would invite `x` or `dw` to take half of it
/// and leave the rest reading as literal asterisks; `h` and `l` still step
/// onto one for whoever means to edit the spelling itself.
fn words(projection: &Projection, hidden: &Hidden, from: usize, to: usize) -> Vec<Range<usize>> {
    let spelled = projection
        .line_at(from)
        .map(|index| hidden.spelled(projection, index))
        .unwrap_or_default();
    let segments = projection
        .word_ranges(from, to)
        .into_iter()
        .flat_map(|word| {
            // The parts of the segment outside every run of markup.
            let mut parts = vec![word];
            for run in &spelled {
                parts = parts
                    .into_iter()
                    .flat_map(|part| {
                        [
                            part.start..run.start.clamp(part.start, part.end),
                            run.end.clamp(part.start, part.end)..part.end,
                        ]
                    })
                    .filter(|part| !part.is_empty())
                    .collect();
            }
            parts
        });
    let mut words: Vec<Range<usize>> = Vec::new();
    let mut last_punctuation = false;
    for word in segments {
        let punctuation = projection
            .text_between(word.start, word.end)
            .is_some_and(is_punctuation);
        match words.last_mut() {
            Some(last) if punctuation && last_punctuation && last.end == word.start => {
                last.end = word.end;
            }
            _ => words.push(word),
        }
        last_punctuation = punctuation;
    }
    words
}

/// Whether a segment is non-keyword, non-blank text. An atom's placeholder
/// stands for an object, not punctuation, and stays a word of its own.
fn is_punctuation(segment: &str) -> bool {
    segment
        .chars()
        .all(|c| !c.is_alphanumeric() && c != '_' && !c.is_whitespace() && c != OBJECT_REPLACEMENT)
}

/// The position of a word's last grapheme, where `e` rests.
fn word_end(projection: &Projection, word: &Range<usize>) -> usize {
    projection
        .prev_grapheme_in_line(word.end)
        .unwrap_or(word.start)
        .max(word.start)
}

/// Where `motion` takes a cursor at `from`, repeated `count` times. A repetition that
/// cannot move stops the walk, so an absurd count costs no more than a real one.
pub(crate) fn target(
    projection: &Projection,
    hidden: &Hidden,
    from: usize,
    motion: Motion,
    count: usize,
) -> usize {
    let from = clamp(projection, from);
    let count = count.clamp(1, MAX_COUNT);
    match motion {
        Motion::LineStart => line_start(projection, line_of(projection, from)),
        Motion::FirstNonBlank => first_non_blank(projection, line_of(projection, from)),
        Motion::LineEnd => {
            let line = (line_of(projection, from) + count - 1).min(last_line(projection));
            line_end(projection, line)
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
                let next = step(projection, &hidden.from(position), position, motion);
                if next == position {
                    break;
                }
                position = next;
            }
            position
        }
    }
}

fn step(projection: &Projection, hidden: &Hidden, from: usize, motion: Motion) -> usize {
    match motion {
        Motion::Left => previous_step(projection, hidden, from),
        Motion::Right => next_step(projection, hidden, from),
        // Markup is never part of a word, so no word motion lands in it.
        Motion::WordForward => next_word_start(projection, hidden, from),
        Motion::WordBackward => previous_word_start(projection, hidden, from),
        Motion::WordEnd => next_word_end(projection, hidden, from),
        _ => from,
    }
}

/// One step after `pos`, never leaving its line: the next grapheme, or past
/// the whole of a concealed run `pos` starts.
pub(crate) fn next_step(projection: &Projection, hidden: &Hidden, pos: usize) -> usize {
    let pos = clamp(projection, pos);
    match hidden.starting(projection, pos) {
        Some(run) => run.end,
        None => next_in_line(projection, pos),
    }
}

/// One step before `pos`, never leaving its line: the previous grapheme, or
/// the start of the concealed run that grapheme ends.
pub(crate) fn previous_step(projection: &Projection, hidden: &Hidden, pos: usize) -> usize {
    hidden.snap_back(projection, previous_in_line(projection, pos))
}

/// Whether the grapheme at `pos` is whitespace, or `pos` is past the end of its line.
pub(crate) fn on_whitespace(projection: &Projection, pos: usize) -> bool {
    projection
        .grapheme_at(clamp(projection, pos))
        .is_none_or(|grapheme| grapheme.chars().all(char::is_whitespace))
}

/// The first non-whitespace grapheme of `line`, or its start when it has none. A
/// block-level leaf holds no text, so it always answers its own start.
pub(crate) fn first_non_blank(projection: &Projection, line: usize) -> usize {
    let line = line.min(last_line(projection));
    projection
        .graphemes(line)
        .find(|(_, grapheme)| !grapheme.chars().all(char::is_whitespace))
        .map_or(line_start(projection, line), |(pos, _)| pos)
}

/// The last grapheme of a non-empty line, where a Normal-mode cursor may rest.
pub(crate) fn last_grapheme_of(projection: &Projection, line: usize) -> usize {
    let entry = &projection.lines()[line.min(last_line(projection))];
    projection
        .prev_grapheme_in_line(entry.to())
        .unwrap_or(entry.from())
}

/// The position one grapheme before `pos`, never leaving its line.
pub(crate) fn previous_in_line(projection: &Projection, pos: usize) -> usize {
    let pos = clamp(projection, pos);
    projection.prev_grapheme_in_line(pos).unwrap_or(pos)
}

/// The position one grapheme after `pos`, never leaving its line.
pub(crate) fn next_in_line(projection: &Projection, pos: usize) -> usize {
    let pos = clamp(projection, pos);
    projection.next_grapheme_in_line(pos).unwrap_or(pos)
}

fn next_word_start(projection: &Projection, hidden: &Hidden, from: usize) -> usize {
    let index = line_of(projection, from);
    let from = clamp(projection, from);
    if let Some(word) = line_words(projection, hidden, index)
        .into_iter()
        .find(|word| word.start > from)
    {
        return word.start;
    }
    if index == last_line(projection) {
        return line_end(projection, index);
    }
    // A blank line counts as a word, so `w` stops on it rather than skipping past.
    let next = line_start(projection, index + 1);
    line_words(projection, hidden, index + 1)
        .first()
        .map_or(next, |word| word.start)
}

fn previous_word_start(projection: &Projection, hidden: &Hidden, from: usize) -> usize {
    let index = line_of(projection, from);
    let from = clamp(projection, from);
    // Only the text before the cursor is segmented, so a cursor inside a word finds
    // that word's own start rather than skipping to the one before it.
    if let Some(word) = words(projection, hidden, projection.lines()[index].from(), from).last() {
        return word.start;
    }
    if index == 0 {
        return line_start(projection, 0);
    }
    let previous = line_start(projection, index - 1);
    line_words(projection, hidden, index - 1)
        .last()
        .map_or(previous, |word| word.start)
}

fn next_word_end(projection: &Projection, hidden: &Hidden, from: usize) -> usize {
    let index = line_of(projection, from);
    let from = clamp(projection, from);
    if let Some(end) = line_words(projection, hidden, index)
        .iter()
        .map(|word| word_end(projection, word))
        .find(|end| *end > from)
    {
        return end;
    }
    if index == last_line(projection) {
        return last_grapheme_of(projection, index);
    }
    let next = line_start(projection, index + 1);
    line_words(projection, hidden, index + 1)
        .first()
        .map_or(next, |word| word_end(projection, word))
}

/// The charwise range an operator covers between the cursor and a motion's target.
pub(crate) fn charwise_range(
    projection: &Projection,
    hidden: &Hidden,
    cursor: usize,
    target: usize,
    span: Span,
) -> Range<usize> {
    let start = cursor.min(target);
    let mut end = cursor.max(target);
    if span == Span::Inclusive {
        end = next_step(projection, hidden, end);
    }
    start..end
}

/// The line range a linewise operator covers.
pub(crate) fn line_range(projection: &Projection, cursor: usize, target: usize) -> Range<usize> {
    let a = line_of(projection, cursor);
    let b = line_of(projection, target);
    a.min(b)..a.max(b) + 1
}
