//! Position conversions, grapheme and word boundaries.
//!
//! Every conversion goes through a line and its visible-character mapping;
//! and the line's text is a real `str` that
//! [`unicode_segmentation`](unicode_segmentation) can be asked about directly.
//!
//! This is where the whole editor's answers about graphemes and words live, in
//! document positions, so nothing above has to hold a line's text and convert
//! offsets itself. Boundary motion comes in two shapes: the pair that crosses
//! into the neighbouring line, and the `_in_line` pair that stops at the line's
//! own ends. [`Projection::graphemes`] and [`Projection::word_ranges`] are the
//! underlying enumerations, for a caller that scans rather than steps.
//!
//! A position between the scalar values of one grapheme cluster is never
//! returned by the boundary helpers, so a caret driven by them cannot land
//! inside a cluster.

use std::ops::Range;
use unicode_segmentation::{GraphemeCursor, GraphemeIndices, UnicodeSegmentation};

use crate::fragment::Fragment;
use crate::node::Node;
use crate::schema::Schema;
use crate::slice::Slice;

use super::{LineKind, OBJECT_REPLACEMENT, Projection, is_line_break};

/// The byte index of the `n`th `char` of `text`, clamped to its length.
fn char_to_byte(text: &str, n: usize) -> usize {
    text.char_indices()
        .nth(n)
        .map(|(i, _)| i)
        .unwrap_or(text.len())
}

/// The number of `char`s before byte index `byte`.
fn byte_to_char(text: &str, byte: usize) -> usize {
    text[..byte.min(text.len())].chars().count()
}

/// The UAX#29 word-boundary segments of `text` that are not wholly whitespace,
/// as `(byte offset, segment)`.
///
/// This is the one definition of "a word" the whole editor uses: ⌥← and ⌥→,
/// double-click, and vim's `w`, `b` and `e`.
fn words(text: &str) -> impl DoubleEndedIterator<Item = (usize, &str)> {
    text.split_word_bound_indices()
        .filter(|(_, segment)| !segment.chars().all(char::is_whitespace))
}

struct Graphemes<'a> {
    inner: GraphemeIndices<'a>,
    line: Option<&'a super::Line>,
    front: usize,
    back: usize,
}

impl<'a> Iterator for Graphemes<'a> {
    type Item = (usize, &'a str);
    fn next(&mut self) -> Option<Self::Item> {
        let (_, text) = self.inner.next()?;
        let pos = self.line?.offset_to_pos(self.front)?;
        self.front += text.chars().count();
        Some((pos, text))
    }
}

impl DoubleEndedIterator for Graphemes<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        let (_, text) = self.inner.next_back()?;
        self.back -= text.chars().count();
        Some((self.line?.offset_to_pos(self.back)?, text))
    }
}

impl Projection {
    /// The index of the line `pos` falls in.
    ///
    /// Positions between blocks — the tokens that close one and open the next —
    /// belong to no line and answer `None`.
    pub fn line_at(&self, pos: usize) -> Option<usize> {
        let lines = self.lines();
        let index = match lines.binary_search_by(|line| line.from.cmp(&pos)) {
            Ok(index) => index,
            Err(0) => return None,
            Err(index) => index - 1,
        };
        let line = lines.get(index)?;
        (pos >= line.from && pos <= line.to).then_some(index)
    }

    /// Whether a caret may sit at `pos`: inside some line's text.
    pub fn is_caret_position(&self, pos: usize) -> bool {
        self.line_at(pos)
            .and_then(|index| self.line(index))
            .is_some_and(|line| {
                line.kind == LineKind::Textblock && line.positions.binary_search(&pos).is_ok()
            })
    }

    /// `pos` as a line index and a `char` offset into that line's text.
    pub fn pos_to_line_offset(&self, pos: usize) -> Option<(usize, usize)> {
        let index = self.line_at(pos)?;
        Some((index, self.lines()[index].pos_to_offset(pos)?))
    }

    /// The inverse of [`Projection::pos_to_line_offset`].
    pub fn line_offset_to_pos(&self, line: usize, offset: usize) -> Option<usize> {
        let line = self.line(line)?;
        line.offset_to_pos(offset)
    }

    /// `pos` as a line index and a byte offset into that line's text, for a
    /// host that lays out one line of text at a time.
    pub fn pos_to_line_byte(&self, pos: usize) -> Option<(usize, usize)> {
        let (index, offset) = self.pos_to_line_offset(pos)?;
        let text = self.line_text(index)?;
        Some((index, char_to_byte(text, offset)))
    }

    /// The inverse of [`Projection::pos_to_line_byte`]. A byte offset inside a
    /// `char` rounds down.
    pub fn line_byte_to_pos(&self, line: usize, byte: usize) -> Option<usize> {
        let text = self.line_text(line)?;
        self.line_offset_to_pos(line, byte_to_char(text, byte))
    }

    /// The UTF-16 offset of `pos` inside line `line`'s text.
    pub fn utf16_offset(&self, line: usize, pos: usize) -> Option<usize> {
        let entry = self.line(line)?;
        if pos < entry.from || pos > entry.to {
            return None;
        }
        let text = self.line_text(line)?;
        let byte = char_to_byte(text, entry.pos_to_offset(pos)?);
        Some(text[..byte].encode_utf16().count())
    }

    /// The position at UTF-16 offset `offset` inside line `line`.
    ///
    /// An offset that falls between the two halves of a surrogate pair rounds
    /// down to the start of that `char`.
    pub fn pos_from_utf16(&self, line: usize, offset: usize) -> Option<usize> {
        let entry = self.line(line)?;
        let text = self.line_text(line)?;
        let mut units = 0usize;
        for (chars, ch) in text.chars().enumerate() {
            if units >= offset || units + ch.len_utf16() > offset {
                return entry.offset_to_pos(chars);
            }
            units += ch.len_utf16();
        }
        entry.offset_to_pos(entry.len())
    }

    /// A document position as a UTF-16 offset into [`Projection::plain_text`].
    pub fn pos_to_utf16(&self, pos: usize) -> Option<usize> {
        let line = self.line_at(pos)?;
        Some(self.line(line)?.utf16_start + self.utf16_offset(line, pos)?)
    }

    /// A UTF-16 offset into [`Projection::plain_text`] as a document position.
    pub fn utf16_to_pos(&self, offset: usize) -> Option<usize> {
        let lines = self.lines();
        if lines.is_empty() {
            return None;
        }
        let index = match lines.binary_search_by(|line| line.utf16_start.cmp(&offset)) {
            Ok(index) => index,
            Err(0) => 0,
            Err(index) => index - 1,
        };
        let line = &lines[index];
        self.pos_from_utf16(index, offset.saturating_sub(line.utf16_start))
    }

    /// A UTF-16 range over [`Projection::plain_text`] as a position range.
    pub fn utf16_range_to_pos_range(&self, from: usize, to: usize) -> Option<(usize, usize)> {
        let start = self.utf16_to_pos(from.min(to))?;
        let end = self.utf16_to_pos(from.max(to))?;
        Some((start, end))
    }

    /// A position range as a UTF-16 range over [`Projection::plain_text`].
    pub fn pos_range_to_utf16_range(&self, from: usize, to: usize) -> Option<(usize, usize)> {
        let start = self.pos_to_utf16(from.min(to))?;
        let end = self.pos_to_utf16(from.max(to))?;
        Some((start, end))
    }

    /// Whether `pos` sits on a grapheme cluster boundary.
    pub fn is_grapheme_boundary(&self, pos: usize) -> bool {
        let Some((line, offset)) = self.pos_to_line_offset(pos) else {
            return false;
        };
        let Some(text) = self.line_text(line) else {
            return false;
        };
        let byte = char_to_byte(text, offset);
        self.is_caret_position(pos)
            && GraphemeCursor::new(byte, text.len(), true)
                .is_boundary(text, 0)
                .unwrap_or(true)
    }

    /// The next grapheme cluster boundary after `pos`, crossing into the next
    /// line when `pos` is at the end of one.
    pub fn next_grapheme_boundary(&self, pos: usize) -> Option<usize> {
        let line = self.line_at(pos)?;
        match self.next_grapheme_in_line(pos)? {
            next if next != pos => Some(next),
            _ => self.line(line + 1).and_then(|next| next.offset_to_pos(0)),
        }
    }

    /// The previous grapheme cluster boundary before `pos`, crossing into the
    /// previous line when `pos` is at the start of one.
    pub fn prev_grapheme_boundary(&self, pos: usize) -> Option<usize> {
        let line = self.line_at(pos)?;
        match self.prev_grapheme_in_line(pos)? {
            previous if previous != pos => Some(previous),
            _ => line
                .checked_sub(1)
                .and_then(|index| self.line(index))
                .and_then(|previous| previous.offset_to_pos(previous.len())),
        }
    }

    /// `pos`, or the next grapheme boundary when it splits a cluster.
    pub fn ceil_grapheme(&self, pos: usize) -> usize {
        if self.is_grapheme_boundary(pos) {
            return pos;
        }
        self.next_grapheme_boundary(pos).unwrap_or(pos)
    }

    /// `pos`, or the previous grapheme boundary when it splits a cluster.
    pub fn floor_grapheme(&self, pos: usize) -> usize {
        if self.is_grapheme_boundary(pos) {
            return pos;
        }
        self.prev_grapheme_boundary(pos).unwrap_or(pos)
    }

    /// `pos`, or the next grapheme boundary when it splits a cluster, never
    /// leaving the line `pos` sits in.
    ///
    /// The line's own end counts as a boundary, so this is total within a line;
    /// `None` only means `pos` belongs to no line.
    pub fn next_grapheme_in_line(&self, pos: usize) -> Option<usize> {
        let (line, offset) = self.pos_to_line_offset(pos)?;
        let text = self.line_text(line)?;
        let byte = char_to_byte(text, offset);
        let next = (byte < text.len())
            .then(|| GraphemeCursor::new(byte, text.len(), true).next_boundary(text, 0))
            .and_then(|found| found.ok().flatten());
        self.line_offset_to_pos(
            line,
            next.map_or(self.lines()[line].len(), |next| byte_to_char(text, next)),
        )
    }

    /// The previous grapheme boundary before `pos`, never leaving the line
    /// `pos` sits in. The mirror of [`Projection::next_grapheme_in_line`]: the
    /// line's own start counts as a boundary.
    pub fn prev_grapheme_in_line(&self, pos: usize) -> Option<usize> {
        let (line, offset) = self.pos_to_line_offset(pos)?;
        let text = self.line_text(line)?;
        let byte = char_to_byte(text, offset);
        let previous = (byte > 0)
            .then(|| GraphemeCursor::new(byte, text.len(), true).prev_boundary(text, 0))
            .and_then(|found| found.ok().flatten());
        self.line_offset_to_pos(
            line,
            previous.map_or(0, |previous| byte_to_char(text, previous)),
        )
    }

    /// The grapheme clusters of one line, as `(document position, cluster)`
    /// pairs, walkable from either end.
    ///
    /// A line that holds no text — a block-level leaf — yields nothing.
    pub fn graphemes(&self, line: usize) -> impl DoubleEndedIterator<Item = (usize, &str)> {
        let entry = self.line(line);
        let text = entry.and_then(|_| self.line_text(line)).unwrap_or_default();
        Graphemes {
            inner: text.grapheme_indices(true),
            line: entry,
            front: 0,
            back: entry.map_or(0, |line| line.len()),
        }
    }

    /// The grapheme cluster starting at `pos`.
    ///
    /// `None` at the end of a line, at a position that belongs to no line, and
    /// at one that falls inside a cluster.
    pub fn grapheme_at(&self, pos: usize) -> Option<&str> {
        let line = self.line_at(pos)?;
        self.graphemes(line)
            .find(|(start, _)| *start >= pos)
            .and_then(|(start, grapheme)| (start == pos).then_some(grapheme))
    }

    /// The text between `from` and `to`, which must lie in one line.
    ///
    /// `None` for a range that is inverted, that reaches past its line, or
    /// whose start belongs to no line — the tokens between two blocks carry no
    /// text, so no string can stand for a range that crosses them.
    pub fn text_between(&self, from: usize, to: usize) -> Option<&str> {
        let (line, offset) = self.pos_to_line_offset(from)?;
        let entry = self.line(line)?;
        if to < from || to > entry.to {
            return None;
        }
        let text = self.line_text(line)?;
        Some(&text[char_to_byte(text, offset)..char_to_byte(text, entry.pos_to_offset(to)?)])
    }

    /// The words between `from` and `to`, as document position ranges.
    ///
    /// A word is a UAX#29 word-boundary segment that is not wholly whitespace.
    /// Segmentation runs over exactly the text the range covers, so a bound
    /// that falls inside a word segments the part it covers on its own — which
    /// is what a caller scanning backwards from the cursor wants. A range
    /// [`Projection::text_between`] rejects has no words.
    pub fn word_ranges(&self, from: usize, to: usize) -> Vec<Range<usize>> {
        let Some(text) = self.text_between(from, to) else {
            return Vec::new();
        };
        let mut ranges = Vec::new();
        let Some((line, offset)) = self.pos_to_line_offset(from) else {
            return ranges;
        };
        for (start, segment) in words(text) {
            let char_start = offset + byte_to_char(text, start);
            let char_end = char_start + segment.chars().count();
            if let (Some(start), Some(end)) = (
                self.line_offset_to_pos(line, char_start),
                self.line_offset_to_pos(line, char_end),
            ) {
                ranges.push(start..end);
            }
        }
        ranges
    }

    /// The end of the next word at or after `pos`.
    ///
    /// Segments that are entirely whitespace are skipped. When the rest of the
    /// line holds no word, the line's end is returned, and from there the next
    /// line's start — which is the only way word motion crosses a block
    /// boundary.
    pub fn next_word_boundary(&self, pos: usize) -> Option<usize> {
        let (line, offset) = self.pos_to_line_offset(pos)?;
        let text = self.line_text(line)?;
        let byte = char_to_byte(text, offset);
        if let Some((start, segment)) =
            words(text).find(|(start, segment)| start + segment.len() > byte)
        {
            let end = start + segment.len();
            return self.line_offset_to_pos(line, byte_to_char(text, end));
        }
        let entry = &self.lines()[line];
        if offset < entry.len() {
            entry.offset_to_pos(entry.len())
        } else {
            self.line(line + 1).and_then(|next| next.offset_to_pos(0))
        }
    }

    /// The start of the previous word before `pos`. The mirror of
    /// [`Projection::next_word_boundary`].
    pub fn prev_word_boundary(&self, pos: usize) -> Option<usize> {
        let (line, offset) = self.pos_to_line_offset(pos)?;
        let text = self.line_text(line)?;
        let byte = char_to_byte(text, offset);
        if let Some((start, _)) = words(text).rev().find(|(start, _)| *start < byte) {
            return self.line_offset_to_pos(line, byte_to_char(text, start));
        }
        let entry = &self.lines()[line];
        if offset > 0 {
            entry.offset_to_pos(0)
        } else {
            line.checked_sub(1)
                .and_then(|index| self.line(index))
                .and_then(|previous| previous.offset_to_pos(previous.len()))
        }
    }
}

/// The text of a slice, using the same conventions as
/// [`Projection::plain_text`]: blocks are joined by `'\n'`, hard breaks become
/// `'\n'`, and every other inline atom becomes [`OBJECT_REPLACEMENT`], one per
/// token it occupies.
pub fn slice_to_plain_text(schema: &Schema, slice: &Slice) -> String {
    let mut out = String::new();
    let mut first = true;
    append_fragment(schema, slice.content(), &mut out, &mut first);
    out
}

fn append_fragment(schema: &Schema, content: &Fragment, out: &mut String, first: &mut bool) {
    for child in content.iter() {
        let ty = schema.node_type(child.type_id());
        if ty.is_textblock() {
            start_block(out, first);
            append_inline(schema, child.content(), out);
        } else if ty.is_block() && child.is_container() {
            append_fragment(schema, child.content(), out, first);
        } else if ty.is_block() {
            start_block(out, first);
        } else {
            // An open slice can carry inline content at its top level.
            *first = false;
            append_inline_node(schema, child, out);
        }
    }
}

fn start_block(out: &mut String, first: &mut bool) {
    if *first {
        *first = false;
    } else {
        out.push('\n');
    }
}

fn append_inline(schema: &Schema, content: &Fragment, out: &mut String) {
    for child in content.iter() {
        append_inline_node(schema, child, out);
    }
}

fn append_inline_node(schema: &Schema, node: &Node, out: &mut String) {
    if let Some(text) = node.text() {
        out.push_str(text);
        return;
    }
    if node.is_container() && !schema.node_type(node.type_id()).is_atom() {
        append_inline(schema, node.content(), out);
        return;
    }
    let filler = if is_line_break(schema, node.type_id()) {
        '\n'
    } else if schema.node_type(node.type_id()).in_group("soft_break") {
        ' '
    } else {
        OBJECT_REPLACEMENT
    };
    for _ in 0..node.node_size() {
        out.push(filler);
    }
}
