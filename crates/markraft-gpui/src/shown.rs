//! The text a reader sees, and the document range each of its characters
//! stands for.
//!
//! Concealment, atom labels and the accessibility tree each used to decide
//! that text on their own. This is the one reading: text runs come from
//! [`conceal::pieces`](markraft_core::kind::conceal::pieces), atoms from
//! [`shown_atom_label`](crate::surface::shown_atom_label), which is what the row
//! shapes. Layout — pill fillers, wrapping, line height — stays in the
//! surface and is not part of the index, so a search position does not move
//! when the column gets wider.
//!
//! A run that displays something other than its own characters, an entity or
//! a wiki-link label, maps every displayed character onto the whole source
//! run. A selection can only take or leave that run whole, which is the same
//! rule the accessibility tree already follows.

use markraft_core::kind::DocTypes;
use markraft_core::kind::conceal::{self, Reveal};
use markraft_core::projection::{Line, Projection, RunContent};
use std::ops::Range;

use crate::surface::shown_atom_label;

/// One stretch of what a line shows.
pub(crate) struct ShownPiece {
    /// `char` offsets into the line's projected text.
    pub(crate) source: Range<usize>,
    /// What is shown for it.
    pub(crate) text: String,
    /// Whether `text` is the source itself, character for character.
    pub(crate) own: bool,
}

/// What line `index` shows under `reveal`.
pub(crate) fn line_pieces(
    projection: &Projection,
    types: &DocTypes,
    index: usize,
    reveal: &Reveal,
) -> Vec<ShownPiece> {
    let Some(line) = projection.line(index) else {
        return Vec::new();
    };
    let source = projection.line_text(index).unwrap_or("");
    let shown = conceal::shown(types.syntax, line, reveal);
    let pieces = conceal::pieces(line, source, &shown);
    line.runs()
        .iter()
        .zip(pieces)
        .filter_map(|(run, piece)| {
            let (text, own) = if let RunContent::Atom(node) = &run.content {
                (
                    shown_atom_label(types, node).unwrap_or("").to_owned(),
                    false,
                )
            } else {
                (piece.text.to_owned(), piece.own)
            };
            (!text.is_empty()).then_some(ShownPiece {
                source: piece.source,
                text,
                own,
            })
        })
        .collect()
}

/// The document as a reader sees it.
pub(crate) struct ShownText {
    text: String,
    /// The document range each character of [`ShownText::text`] stands for,
    /// in order.
    spans: Vec<Range<usize>>,
}

impl ShownText {
    /// `reveal` says which concealed spans show their source. Search passes
    /// [`Reveal::nothing`], so a caret resting in a span does not change what
    /// can be found.
    pub(crate) fn build(projection: &Projection, types: &DocTypes, reveal: &Reveal) -> Self {
        let mut text = String::new();
        let mut spans = Vec::new();
        let count = projection.line_count();
        for index in 0..count {
            let Some(line) = projection.line(index) else {
                continue;
            };
            for piece in line_pieces(projection, types, index, reveal) {
                push_piece(&mut text, &mut spans, line, &piece);
            }
            if index + 1 < count {
                let end = line.to();
                let next = projection
                    .line(index + 1)
                    .map(|line| line.from())
                    .unwrap_or(end);
                text.push('\n');
                spans.push(end..next.max(end));
            }
        }
        debug_assert_eq!(text.chars().count(), spans.len());
        ShownText { text, spans }
    }

    #[cfg(test)]
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// The shown characters whose ranges sit wholly inside `range`, when they
    /// do not cross a line. What ⌘F offers as the query for a selection.
    pub(crate) fn text_inside(&self, range: Range<usize>) -> Option<String> {
        if range.start == range.end {
            return None;
        }
        let mut out = String::new();
        for (span, ch) in self.spans.iter().zip(self.text.chars()) {
            if span.start >= range.start && span.end <= range.end {
                out.push(ch);
            }
        }
        (!out.is_empty() && !out.contains('\n')).then_some(out)
    }

    /// Non-overlapping matches of `query`, case-folded per character. Each
    /// range is the document span to select. An empty query matches nothing.
    pub(crate) fn matches(&self, query: &str) -> Vec<Range<usize>> {
        let folded_query = fold(query);
        if folded_query.text.is_empty() || self.text.is_empty() {
            return Vec::new();
        }
        let folded = fold(&self.text);
        let mut from = 0usize;
        let mut hits = Vec::new();
        while let Some(at) = folded.text[from..].find(&folded_query.text) {
            let start_byte = from + at;
            let end_byte = start_byte + folded_query.text.len();
            let start = folded.origin[char_index(&folded.text, start_byte)];
            // Convert the exclusive UTF-8 boundary before stepping back a character.
            let end = folded.origin[char_index(&folded.text, end_byte) - 1] + 1;
            let range = self.spans[start].start..self.spans[end - 1].end;
            if range.start < range.end {
                hits.push(range);
            }
            from = end_byte;
        }
        hits
    }
}

fn push_piece(text: &mut String, spans: &mut Vec<Range<usize>>, line: &Line, piece: &ShownPiece) {
    if piece.own {
        for (index, ch) in piece.text.chars().enumerate() {
            let at = piece.source.start + index;
            let Some(start) = line.offset_to_pos(at) else {
                continue;
            };
            let Some(end) = line.offset_to_pos(at + 1) else {
                continue;
            };
            if start < end {
                text.push(ch);
                spans.push(start..end);
            }
        }
        return;
    }
    let (Some(start), Some(end)) = (
        line.offset_to_pos(piece.source.start),
        line.offset_to_pos(piece.source.end),
    ) else {
        return;
    };
    if start >= end {
        return;
    }
    for ch in piece.text.chars() {
        text.push(ch);
        spans.push(start..end);
    }
}

struct Folded {
    text: String,
    /// The original character index each character of `text` came from.
    origin: Vec<usize>,
}

/// Case-fold `text` one character at a time. A fold that grows — `İ`
/// becoming `i` plus a combining dot — keeps the base letter and drops the
/// mark, so `İstanbul` is found by `ist`, and that `i` still points at `İ`.
/// Folding the whole string at once can disagree with that.
fn fold(text: &str) -> Folded {
    let mut folded = Folded {
        text: String::new(),
        origin: Vec::new(),
    };
    for (index, ch) in text.chars().enumerate() {
        for lower in ch.to_lowercase() {
            if is_combining_mark(lower) {
                continue;
            }
            folded.text.push(lower);
            folded.origin.push(index);
        }
    }
    folded
}

fn is_combining_mark(ch: char) -> bool {
    matches!(
        ch,
        '\u{0300}'..='\u{036F}'
            | '\u{1AB0}'..='\u{1AFF}'
            | '\u{1DC0}'..='\u{1DFF}'
            | '\u{20D0}'..='\u{20FF}'
            | '\u{FE20}'..='\u{FE2F}'
    )
}

fn char_index(text: &str, byte: usize) -> usize {
    text[..byte].chars().count()
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use markraft_commonmark::{commonmark_doc_type_names, commonmark_schema, from_markdown};
    use markraft_core::projection::projection_of;
    use markraft_core::{EditorState, EditorStateConfig};

    fn shown(source: &str) -> (ShownText, markraft_core::projection::Projection) {
        let schema = commonmark_schema();
        let doc = from_markdown(&schema, source).expect("valid Markdown");
        let state = EditorState::create(
            EditorStateConfig::new(schema.clone())
                .doc(doc)
                .extensions(markraft_core::projection::projection()),
        )
        .expect("a valid state");
        let projection = projection_of(&state);
        let types = DocTypes::from_schema_names(&schema, &commonmark_doc_type_names());
        let owned = projection.as_ref().clone();
        (
            ShownText::build(&projection, &types, &Reveal::nothing()),
            owned,
        )
    }

    fn line_offset(projection: &markraft_core::projection::Projection, offset: usize) -> usize {
        projection.lines()[0].offset_to_pos(offset).unwrap()
    }

    #[test]
    fn concealed_markup_is_absent_and_an_entity_is_its_character() {
        let (text, projection) = shown("x **ab** &amp; y");
        assert_eq!(text.text(), "x ab & y");
        let line = &projection.lines()[0];
        // The same source offsets the accessibility tree reports for this line.
        assert_eq!(
            text.spans[2],
            line.offset_to_pos(4).unwrap()..line.offset_to_pos(5).unwrap()
        );
        assert_eq!(
            text.spans[5],
            line.offset_to_pos(9).unwrap()..line.offset_to_pos(14).unwrap(),
            "the ampersand selects the whole entity"
        );
        assert_eq!(
            text.matches("ab"),
            vec![line_offset(&projection, 4)..line_offset(&projection, 6)]
        );
        assert_eq!(text.matches("**"), Vec::<Range<usize>>::new());
    }

    #[test]
    fn a_wiki_link_is_found_by_its_label_and_selects_the_atom() {
        let (text, projection) = shown("see [[Notes]] now");
        assert_eq!(text.text(), "see Notes now");
        let hit = text.matches("Notes").remove(0);
        let line = &projection.lines()[0];
        let atom = line
            .runs()
            .iter()
            .find(|run| matches!(run.content, RunContent::Atom(_)))
            .unwrap();
        assert_eq!(hit, line.abs(atom.start)..line.abs(atom.end));
    }

    #[test]
    fn a_code_block_and_a_table_cell_are_their_source() {
        let (text, _) = shown("```\nlet needle = 1\n```\n\n| a |\n| --- |\n| needle |\n");
        assert!(text.text().contains("let needle = 1"), "{}", text.text());
        assert_eq!(text.matches("needle").len(), 2);
    }

    #[test]
    fn matches_ignore_case_and_an_empty_query_matches_nothing() {
        let (text, _) = shown("Say Bold please");
        assert_eq!(text.matches("bold").len(), 1);
        assert!(text.matches("").is_empty());
        assert!(text.matches("   ").is_empty());
    }

    #[test]
    fn multibyte_matches_map_to_complete_source_characters() {
        for (word, query) in [
            ("你好", "你好"),
            ("CAFÉ", "café"),
            ("🙂", "🙂"),
            ("👩‍💻", "👩‍💻"),
        ] {
            let (text, projection) = shown(&format!("前 {word} / {word}"));
            let first = 2;
            let len = word.chars().count();
            let second = first + len + 3;
            let range =
                |start| line_offset(&projection, start)..line_offset(&projection, start + len);
            assert_eq!(
                text.matches(query),
                vec![range(first), range(second)],
                "{query}"
            );
        }
    }

    #[test]
    fn a_fold_that_grows_still_points_at_one_character() {
        let (text, _) = shown("İstanbul");
        let hits = text.matches("ist");
        assert_eq!(hits.len(), 1);
        assert!(hits[0].start < hits[0].end);
    }

    #[test]
    fn the_text_inside_a_selection_is_what_the_reader_sees() {
        let (text, projection) = shown("x **ab** y");
        let line = &projection.lines()[0];
        let word = line.offset_to_pos(4).unwrap()..line.offset_to_pos(6).unwrap();
        assert_eq!(text.text_inside(word).as_deref(), Some("ab"));
        assert_eq!(text.text_inside(0..0), None);
    }
}
