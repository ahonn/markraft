//! Text objects: `iw` and `aw`, the quote and bracket pairs, `ip` and `ap`.
//!
//! Each finds a range around the cursor, which an operator then takes or a visual
//! mode selects. Like every motion here they read the projection and a position and
//! nothing else, so they are tested without a window.
//!
//! The pairs are looked for on the cursor's own line, which is a projection line: a
//! code block's newlines are inside its one line, so `di(` reaches across the lines
//! of a function call in a code block, while outside one no pair spans two blocks.
//! A paragraph is the cursor's line, which is its block; the projection holds no
//! blank lines for `ap` to take as well, so it takes the same block `ip` does.

use crate::motion::{self, Hidden};
use markraft_core::projection::Projection;
use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TextObject {
    /// `w`: a word, or the run of blanks the cursor is on.
    Word,
    /// `"`, `'` and `` ` ``: text between two of the same quote on the line.
    Quote(char),
    /// `(`, `[`, `{` and `<`, and `b` and `B`: text between a pair of brackets.
    Pair(char, char),
    /// `p`: the cursor's block.
    Paragraph,
}

/// What a text object covers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Selected {
    /// A range of document positions, taken charwise. It may be empty, as `i(` is
    /// between `()`: nothing to delete, but a place for `c` to type.
    Chars(Range<usize>),
    /// A range of line indices, taken linewise.
    Lines(Range<usize>),
}

/// The range `object` covers around `cursor`, the inner one or, when `around`, the one
/// with its delimiters or its surrounding blanks. `None` when the cursor is in no such
/// object, which vim answers by doing nothing.
pub(crate) fn find(
    projection: &Projection,
    hidden: &Hidden,
    cursor: usize,
    object: TextObject,
    around: bool,
) -> Option<Selected> {
    let line = motion::line_of(projection, cursor);
    match object {
        TextObject::Word => word(projection, hidden, line, cursor, around).map(Selected::Chars),
        TextObject::Quote(quote) => {
            quoted(projection, line, cursor, quote, around).map(Selected::Chars)
        }
        TextObject::Pair(open, close) => {
            bracketed(projection, line, cursor, open, close, around).map(Selected::Chars)
        }
        TextObject::Paragraph => Some(Selected::Lines(line..line + 1)),
    }
}

/// `iw` and `aw`. On a word, `iw` is the word and `aw` adds the blanks after it, or
/// those before it when none follow. On blanks, `iw` is the blanks and `aw` adds the
/// word after them.
fn word(
    projection: &Projection,
    hidden: &Hidden,
    line: usize,
    cursor: usize,
    around: bool,
) -> Option<Range<usize>> {
    let words = motion::line_words(projection, hidden, line);
    let (start, end) = (
        motion::line_start(projection, line),
        motion::line_end(projection, line),
    );
    if start == end {
        return None;
    }
    // The first word not wholly before the cursor: the one it is on, or the next.
    let next = words.iter().position(|word| word.end > cursor);
    if let Some(index) = next.filter(|index| words[*index].start <= cursor) {
        let word = words[index].clone();
        if !around {
            return Some(word);
        }
        let after = words.get(index + 1).map_or(end, |next| next.start);
        if after > word.end {
            return Some(word.start..after);
        }
        let before = index
            .checked_sub(1)
            .map_or(start, |previous| words[previous].end);
        return Some(before..word.end);
    }
    // Between words, before the first or after the last.
    let blanks_start = next
        .unwrap_or(words.len())
        .checked_sub(1)
        .map_or(start, |previous| words[previous].end);
    let (blanks_end, word_end) =
        next.map_or((end, end), |index| (words[index].start, words[index].end));
    Some(blanks_start..if around { word_end } else { blanks_end })
}

/// The characters of line `line` with the position each starts at, and the line's end.
fn chars(projection: &Projection, line: usize) -> (Vec<(usize, char)>, usize) {
    let entry = &projection.lines()[line];
    let text = projection.line_text(line).unwrap_or_default();
    let chars = text
        .chars()
        .enumerate()
        .filter_map(|(offset, ch)| Some((entry.offset_to_pos(offset)?, ch)))
        .collect();
    (chars, entry.to())
}

/// Where the character at index `index` of `chars` ends.
fn after(chars: &[(usize, char)], index: usize, end: usize) -> usize {
    chars.get(index + 1).map_or(end, |(pos, _)| *pos)
}

/// `i"` and `a"`, and the other quotes. Quotes pair up from the start of the line, as
/// vim reads them; the pair round the cursor is taken, or failing that the first one
/// after it. `a"` takes the quotes and the blanks after them, or before them when none
/// follow.
fn quoted(
    projection: &Projection,
    line: usize,
    cursor: usize,
    quote: char,
    around: bool,
) -> Option<Range<usize>> {
    let (chars, end) = chars(projection, line);
    let quotes: Vec<usize> = chars
        .iter()
        .enumerate()
        .filter(|(index, (_, ch))| *ch == quote && (*index == 0 || chars[index - 1].1 != '\\'))
        .map(|(index, _)| index)
        .collect();
    let pairs: Vec<(usize, usize)> = quotes
        .chunks_exact(2)
        .map(|pair| (pair[0], pair[1]))
        .collect();
    let (open, close) = pairs
        .iter()
        .find(|(open, close)| chars[*open].0 <= cursor && cursor <= chars[*close].0)
        .or_else(|| pairs.iter().find(|(open, _)| chars[*open].0 > cursor))
        .copied()?;
    if !around {
        return Some(after(&chars, open, end)..chars[close].0);
    }
    let from = chars[open].0;
    let to = after(&chars, close, end);
    let blank = |index: usize| chars.get(index).is_some_and(|(_, ch)| ch.is_whitespace());
    let trailing = (close + 1..chars.len())
        .take_while(|index| blank(*index))
        .last();
    if let Some(last) = trailing {
        return Some(from..after(&chars, last, end));
    }
    let leading = (0..open).rev().take_while(|index| blank(*index)).last();
    Some(leading.map_or(from, |first| chars[first].0)..to)
}

/// `i(` and `a(`, and the other brackets: the innermost pair round the cursor, a
/// bracket under the cursor counting as inside its own pair.
fn bracketed(
    projection: &Projection,
    line: usize,
    cursor: usize,
    open: char,
    close: char,
    around: bool,
) -> Option<Range<usize>> {
    let (chars, end) = chars(projection, line);
    let at = chars.iter().rposition(|(pos, _)| *pos <= cursor)?;
    let opening = if chars[at].1 == open {
        at
    } else {
        matching(&chars, at, close, open, false)?
    };
    let closing = matching(&chars, opening, open, close, true)?;
    if around {
        Some(chars[opening].0..after(&chars, closing, end))
    } else {
        Some(after(&chars, opening, end)..chars[closing].0)
    }
}

/// The bracket `target` that balances the one at or around `from`, looking forward
/// when `forward` and backward otherwise, skipping nested pairs of `nested` and
/// `target`.
fn matching(
    chars: &[(usize, char)],
    from: usize,
    nested: char,
    target: char,
    forward: bool,
) -> Option<usize> {
    let mut depth = 0usize;
    let indices: Box<dyn Iterator<Item = usize>> = if forward {
        Box::new(from + 1..chars.len())
    } else if chars[from].1 == nested {
        // Starting on the closing bracket: its own pair is the one to find.
        Box::new((0..from).rev())
    } else {
        Box::new((0..=from).rev())
    };
    for index in indices {
        let ch = chars[index].1;
        if ch == nested {
            depth += 1;
        } else if ch == target {
            if depth == 0 {
                return Some(index);
            }
            depth -= 1;
        }
    }
    None
}
