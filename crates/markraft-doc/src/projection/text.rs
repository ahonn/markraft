//! Position conversions, grapheme and word boundaries.
//!
//! Every conversion goes through a line: within a line one token is one `char`,
//! and the line's text is a real `str` that
//! [`unicode_segmentation`](unicode_segmentation) can be asked about directly.
//!
//! A position between the scalar values of one grapheme cluster is never
//! returned by the boundary helpers, so a caret driven by them cannot land
//! inside a cluster.

use unicode_segmentation::{GraphemeCursor, UnicodeSegmentation};

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
            .is_some_and(|line| line.kind == LineKind::Textblock)
    }

    /// `pos` as a line index and a `char` offset into that line's text.
    pub fn pos_to_line_offset(&self, pos: usize) -> Option<(usize, usize)> {
        let index = self.line_at(pos)?;
        Some((index, pos - self.lines()[index].from))
    }

    /// The inverse of [`Projection::pos_to_line_offset`].
    pub fn line_offset_to_pos(&self, line: usize, offset: usize) -> Option<usize> {
        let line = self.line(line)?;
        (offset <= line.len()).then(|| line.from + offset)
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
        Some(self.line(line)?.from + byte_to_char(text, byte))
    }

    /// The UTF-16 offset of `pos` inside line `line`'s text.
    pub fn utf16_offset(&self, line: usize, pos: usize) -> Option<usize> {
        let entry = self.line(line)?;
        if pos < entry.from || pos > entry.to {
            return None;
        }
        let text = self.line_text(line)?;
        let byte = char_to_byte(text, pos - entry.from);
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
            if units >= offset {
                return Some(entry.from + chars);
            }
            units += ch.len_utf16();
        }
        Some(entry.to)
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
        GraphemeCursor::new(byte, text.len(), true)
            .is_boundary(text, 0)
            .unwrap_or(true)
    }

    /// The next grapheme cluster boundary after `pos`, crossing into the next
    /// line when `pos` is at the end of one.
    pub fn next_grapheme_boundary(&self, pos: usize) -> Option<usize> {
        let (line, offset) = self.pos_to_line_offset(pos)?;
        let text = self.line_text(line)?;
        let byte = char_to_byte(text, offset);
        if byte < text.len()
            && let Ok(Some(next)) =
                GraphemeCursor::new(byte, text.len(), true).next_boundary(text, 0)
        {
            return Some(self.lines()[line].from + byte_to_char(text, next));
        }
        self.line(line + 1).map(|next| next.from)
    }

    /// The previous grapheme cluster boundary before `pos`.
    pub fn prev_grapheme_boundary(&self, pos: usize) -> Option<usize> {
        let (line, offset) = self.pos_to_line_offset(pos)?;
        let text = self.line_text(line)?;
        let byte = char_to_byte(text, offset);
        if byte > 0
            && let Ok(Some(prev)) =
                GraphemeCursor::new(byte, text.len(), true).prev_boundary(text, 0)
        {
            return Some(self.lines()[line].from + byte_to_char(text, prev));
        }
        line.checked_sub(1)
            .and_then(|index| self.line(index))
            .map(|previous| previous.to)
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
        for (start, segment) in text.split_word_bound_indices() {
            let end = start + segment.len();
            if end <= byte || segment.trim().is_empty() {
                continue;
            }
            return Some(self.lines()[line].from + byte_to_char(text, end));
        }
        let entry = &self.lines()[line];
        if pos < entry.to {
            Some(entry.to)
        } else {
            self.line(line + 1).map(|next| next.from)
        }
    }

    /// The start of the previous word before `pos`. The mirror of
    /// [`Projection::next_word_boundary`].
    pub fn prev_word_boundary(&self, pos: usize) -> Option<usize> {
        let (line, offset) = self.pos_to_line_offset(pos)?;
        let text = self.line_text(line)?;
        let byte = char_to_byte(text, offset);
        let mut found = None;
        for (start, segment) in text.split_word_bound_indices() {
            if start >= byte || segment.trim().is_empty() {
                continue;
            }
            found = Some(start);
        }
        if let Some(start) = found {
            return Some(self.lines()[line].from + byte_to_char(text, start));
        }
        let entry = &self.lines()[line];
        if pos > entry.from {
            Some(entry.from)
        } else {
            line.checked_sub(1)
                .and_then(|index| self.line(index))
                .map(|previous| previous.to)
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
    let filler = if is_line_break(schema, node.type_id()) {
        '\n'
    } else {
        OBJECT_REPLACEMENT
    };
    for _ in 0..node.node_size() {
        out.push(filler);
    }
}
