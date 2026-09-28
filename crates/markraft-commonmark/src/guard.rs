//! Keeping a textblock's inline source from being read as block syntax.
//!
//! A textblock's text is its inline Markdown source, and the writer puts it in
//! the file verbatim after the block's prefix. Some text would not come back
//! as the same block: a paragraph line that starts `# ` is a heading, a
//! heading that ends ` #` loses the `#`, a `|` in a table cell ends the cell.
//! [`guard`] names the backslashes that keep such text what it is. The same
//! insertions are what [`derive`](crate::derive::derive) parses and what the
//! file holds, so the tree, the styles read from it and the file agree.
//!
//! Only backslashes are ever inserted, each before a character a backslash
//! escape makes literal, so guarding does not change what a reader sees: `\#`
//! renders as `#`. The exception is a line inside a code span that crosses a
//! line ending, where a backslash is content: `` `a\n> b` `` keeps its line
//! only as `` `a\n\> b` ``, whose code shows the backslash. The alternative is
//! a block quote in the middle of the paragraph. Guarding its own output
//! inserts nothing.
//!
//! # What cannot be guarded
//!
//! A backslash only escapes ASCII punctuation, and a line is only protected by
//! one that stops it opening a block, so three shapes stay as they are and are
//! left to whoever builds the text:
//!
//! * Whitespace at the start of a paragraph. CommonMark strips up to three
//!   columns of it and reads four as an indented code block; no character can
//!   be escaped to prevent either. `derive` reads such a paragraph as if the
//!   whitespace were not there.
//! * A blank line inside a paragraph or a heading, which ends the block, and
//!   whatever follows it: an indented line after it is a code block.
//! * A line ending inside a table cell, which ends the row.

use std::collections::BTreeSet;

use comrak::nodes::NodeValue;
use comrak::{Arena, parse_document};

use crate::derive::{BlockKind, parse_options};

/// A textblock's text with the backslashes [`guard`] adds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Guarded {
    /// The guarded text: the original with a `\` before each insertion point.
    pub text: String,
    /// The character offsets in the *original* text before which one `\` was
    /// inserted, ascending and without repeats. Empty when the text needs no
    /// protection, which is the common case.
    pub insertions: Vec<usize>,
}

impl Guarded {
    /// Where the original character at `offset` sits in [`Guarded::text`],
    /// counted in characters. `offset` may be the original length.
    pub fn to_guarded(&self, offset: usize) -> usize {
        offset + self.insertions.partition_point(|&at| at <= offset)
    }

    /// How many original characters precede the guarded character offset
    /// `offset`: the original offset of the character there, or of the next
    /// original one when an inserted backslash sits there.
    pub fn to_original(&self, offset: usize) -> usize {
        let inserted_before = self
            .insertions
            .iter()
            .enumerate()
            .take_while(|(index, at)| *at + index < offset)
            .count();
        offset - inserted_before
    }

    fn build(text: &str, insertions: Vec<usize>) -> Guarded {
        let mut out = String::with_capacity(text.len() + insertions.len());
        let mut next = insertions.iter().peekable();
        for (index, character) in text.chars().enumerate() {
            while next.next_if(|at| **at == index).is_some() {
                out.push('\\');
            }
            out.push(character);
        }
        Guarded {
            text: out,
            insertions,
        }
    }
}

/// The backslashes `text` needs to be read back as a block of `kind` holding
/// exactly this inline content.
///
/// * [`BlockKind::Paragraph`] — a line that would start another block, or turn
///   the paragraph into a setext heading or a table, has its opening character
///   escaped: `#`, `>`, a bullet, the `.`/`)` of an ordered marker, a fence, a
///   thematic break or setext underline, an HTML block's `<`, a table's
///   delimiter row, a link reference definition's `[`. Whether a line does is
///   decided by parsing, so a line that could not interrupt a paragraph there —
///   `2. ` after prose — is left alone.
/// * [`BlockKind::Heading`] — a trailing run of `#` that an ATX heading would
///   read as its closing sequence. A heading holding a line ending (a setext
///   heading) is guarded line by line as a paragraph is.
/// * [`BlockKind::TableCell`] — every `|` a row would split on, which is every
///   `|` not already escaped.
pub fn guard(kind: BlockKind, text: &str) -> Guarded {
    let insertions = match kind {
        BlockKind::Paragraph => paragraph_insertions(text),
        BlockKind::Heading if text.contains('\n') => paragraph_insertions(text),
        BlockKind::Heading => heading_insertions(text),
        BlockKind::TableCell => cell_insertions(text),
    };
    Guarded::build(text, insertions)
}

/// The character offset in `text` a backslash has to go before so that, as the
/// first paragraph of a list item written after `marker`, it is still that
/// paragraph, or `None` when it already is or no escape would help.
///
/// [`guard`] reads a paragraph on its own, and after a list marker some text
/// reads differently: `[ ] a` after `- ` is a check box, `--` after `- `
/// completes a thematic break, and `# a` after `- [ ] ` is a heading. `text` is the paragraph's guarded text and
/// `marker` everything the writer puts before its first line, a task item's
/// check box included, so the check box a task item already has is expected
/// and only a second one is not.
pub fn item_lead_insertion(marker: &str, text: &str) -> Option<usize> {
    let first = text.split('\n').next().unwrap_or_default();
    let reads = |line: &str| {
        let arena = Arena::new();
        let root = crate::parse::parse_ast(&arena, &format!("{marker}{line}\n"), &parse_options());
        let list = root.first_child()?;
        if !matches!(list.data.borrow().value, NodeValue::List(_)) {
            return None;
        }
        let item = list.first_child()?;
        let task = matches!(item.data.borrow().value, NodeValue::TaskItem(_));
        let paragraph = item
            .first_child()
            .is_some_and(|child| matches!(child.data.borrow().value, NodeValue::Paragraph));
        Some((task, paragraph || line.trim().is_empty()))
    };
    // What the marker alone makes of an ordinary line: a plain item, or a task.
    let expected = reads("a");
    if reads(first) == expected {
        return None;
    }
    let lead = leading_whitespace(first);
    let at = first[lead..].chars().next()?;
    at.is_ascii_punctuation()
        .then(|| first[..lead].chars().count())
}

/// The byte length of the whitespace a paragraph's first line starts with,
/// which a reader strips and which no escape can protect.
pub(crate) fn leading_whitespace(text: &str) -> usize {
    text.len() - text.trim_start_matches([' ', '\t']).len()
}

fn paragraph_insertions(text: &str) -> Vec<usize> {
    if crate::math::DisplaySource::parse(text).is_some() {
        return Vec::new();
    }
    let lead = leading_whitespace(text);
    let lines = text.split('\n').count();
    let mut insertions: Vec<usize> = Vec::new();
    let mut unfixable = BTreeSet::new();
    let mut broken = broken_lines(&text[lead..]);
    // Each round either inserts one backslash or gives up on one line, and a
    // line never needs more than one, so this bound is never reached.
    for _ in 0..=lines * 2 {
        let guarded = Guarded::build(text, insertions.clone());
        let body = &guarded.text[lead..];
        let Some(&line) = broken.iter().find(|line| !unfixable.contains(*line)) else {
            break;
        };
        let candidate = escape_point(body, line)
            .map(|byte| guarded.to_original(body[..byte].chars().count() + lead))
            .filter(|offset| !insertions.contains(offset))
            .map(|offset| {
                let mut next = insertions.clone();
                next.insert(next.partition_point(|&at| at < offset), offset);
                let reread = broken_lines(&Guarded::build(text, next.clone()).text[lead..]);
                (next, reread)
            });
        match candidate {
            // An escape that leaves the line opening a block — an indented
            // code block after a blank line — protects nothing, and keeping it
            // would have the next guard escape the backslash.
            Some((next, reread)) if !reread.contains(&line) => {
                insertions = next;
                broken = reread;
            }
            _ => {
                unfixable.insert(line);
            }
        }
    }
    insertions
}

/// The 1-based lines of `body` at which something other than the one
/// paragraph it should be begins, in document order.
///
/// Each is the line whose first character decides the reading: the line a
/// block starts on, except for a setext heading, which is decided by its
/// underline, and a table, which is decided by its delimiter row.
fn broken_lines(body: &str) -> Vec<usize> {
    let arena = Arena::new();
    // The file always ends the block with a line ending, and comrak reads the
    // last line differently without one: `1.` at the very end of its input is
    // a paragraph, but `1.\n` is an empty list item.
    let root = parse_document(&arena, &format!("{body}\n"), &parse_options());
    let mut out = Vec::new();
    let mut blocks = root.children().peekable();
    if blocks.peek().is_none() && !body.trim().is_empty() {
        // Nothing but link reference definitions.
        out.push(1);
    }
    for (index, block) in blocks.enumerate() {
        let data = block.data.borrow();
        let pos = data.sourcepos;
        let line = match &data.value {
            NodeValue::Paragraph if index == 0 => {
                if !starts_with_definition(body) {
                    continue;
                }
                1
            }
            // A later paragraph was split off by a blank line, which no escape
            // can join back.
            NodeValue::Paragraph => continue,
            NodeValue::Heading(heading) if heading.setext => pos.end.line,
            NodeValue::Table(_) => pos.start.line + 1,
            _ => pos.start.line,
        };
        out.push(line);
    }
    out
}

/// Whether a reader takes the start of `body` for link reference definitions,
/// which it drops from the paragraph.
///
/// comrak's positions cannot tell: after it drops leading definitions it
/// reports the paragraph's inline positions as though they had never been
/// there. A definition ends on a line of its own, so some run of whole leading
/// lines reads as nothing but definitions exactly when the text starts with one.
fn starts_with_definition(body: &str) -> bool {
    let first = body.split('\n').next().unwrap_or_default();
    if !first[leading_whitespace(first)..].starts_with('[') {
        return false;
    }
    let mut end = 0;
    body.split('\n').any(|line| {
        end += line.len() + 1;
        let arena = Arena::new();
        let prefix = &body[..(end - 1).min(body.len())];
        parse_document(&arena, prefix, &parse_options())
            .first_child()
            .is_none()
    })
}

/// The byte offset in `body` of the character on `line` a backslash has to go
/// before, or `None` when no escape can change how the line reads.
fn escape_point(body: &str, line: usize) -> Option<usize> {
    let start: usize = body.split('\n').take(line - 1).map(|l| l.len() + 1).sum();
    let text = body.split('\n').nth(line - 1)?;
    let indent = leading_whitespace(text);
    let rest = &text[indent..];
    let first = rest.chars().next()?;
    if first.is_ascii_punctuation() {
        return Some(start + indent);
    }
    // An ordered list marker: the delimiter after the digits is what escapes.
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    ((1..=9).contains(&digits) && matches!(rest[digits..].chars().next(), Some('.' | ')')))
        .then_some(start + indent + digits)
}

fn heading_insertions(text: &str) -> Vec<usize> {
    let chars: Vec<char> = text.chars().collect();
    let end = chars.len()
        - chars
            .iter()
            .rev()
            .take_while(|c| matches!(c, ' ' | '\t'))
            .count();
    let run = chars[..end].iter().rev().take_while(|c| **c == '#').count();
    let start = end - run;
    // A closing sequence is a run of `#` that follows whitespace, or that is
    // all the heading holds.
    let closes = run > 0 && (start == 0 || matches!(chars[start - 1], ' ' | '\t'));
    if closes { vec![start] } else { Vec::new() }
}

fn cell_insertions(text: &str) -> Vec<usize> {
    // GFM splits a row on every `|` an odd run of backslashes does not escape,
    // before it reads any inline content, so a code span or a link protects
    // nothing here.
    let mut out = Vec::new();
    let mut backslashes = 0;
    for (index, character) in text.chars().enumerate() {
        if character == '|' && backslashes % 2 == 0 {
            out.push(index);
        }
        backslashes = if character == '\\' {
            backslashes + 1
        } else {
            0
        };
    }
    out
}
