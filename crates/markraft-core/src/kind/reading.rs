//! A layout-independent reading of a projected document.
//!
//! Concealed syntax, atom labels and character-to-document mappings are shared
//! by search, accessibility and future replacement consumers. Geometry and
//! wrapping never affect the positions returned here.

use crate::Node;
use crate::kind::DocTypes;
use crate::kind::conceal::{self, Reveal};
use crate::projection::{Line, Projection, RunContent};
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

/// One stretch of what a line shows.
pub struct ShownPiece {
    /// `char` offsets into the line's projected text.
    pub source: Range<usize>,
    /// What is shown for it.
    pub text: String,
    /// Whether `text` is the source itself, character for character.
    pub own: bool,
}

/// What line `index` shows under `reveal`.
pub fn line_pieces(
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
                if Some(node.type_id()) == types.hard_break {
                    // The projection owns this one-character newline. It is
                    // a text boundary, not an unlabeled embedded object.
                    (piece.text.to_owned(), piece.own)
                } else {
                    (
                        shown_atom_label(types, node).unwrap_or("").to_owned(),
                        false,
                    )
                }
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
pub struct ShownText {
    text: String,
    /// The document range each character of [`ShownText::text`] stands for,
    /// in order.
    spans: Vec<Range<usize>>,
    mappings: Vec<CharacterMapping>,
    grapheme_boundaries: Vec<bool>,
}

impl ShownText {
    /// `reveal` says which concealed spans show their source. Search passes
    /// [`Reveal::nothing`], so a caret resting in a span does not change what
    /// can be found.
    pub fn build(projection: &Projection, types: &DocTypes, reveal: &Reveal) -> Self {
        let mut text = String::new();
        let mut spans = Vec::new();
        let mut mappings = Vec::new();
        let count = projection.line_count();
        for index in 0..count {
            let Some(line) = projection.line(index) else {
                continue;
            };
            for piece in line_pieces(projection, types, index, reveal) {
                let start = spans.len();
                push_piece(&mut text, &mut spans, line, &piece);
                let end = spans.len();
                let mapping = if piece.own {
                    CharacterMapping::Exact
                } else {
                    CharacterMapping::Whole(start..end)
                };
                mappings.extend(std::iter::repeat_n(mapping, end - start));
            }
            if index + 1 < count {
                let end = line.to();
                let next = projection
                    .line(index + 1)
                    .map(|line| line.from())
                    .unwrap_or(end);
                text.push('\n');
                spans.push(end..next.max(end));
                mappings.push(CharacterMapping::Boundary);
            }
        }
        debug_assert_eq!(text.chars().count(), spans.len());
        let mut grapheme_boundaries = vec![false; spans.len() + 1];
        let mut at = 0;
        for grapheme in text.graphemes(true) {
            grapheme_boundaries[at] = true;
            at += grapheme.chars().count();
        }
        grapheme_boundaries[at] = true;
        ShownText {
            text,
            spans,
            mappings,
            grapheme_boundaries,
        }
    }

    /// The complete reading text, independent of layout.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The shown characters whose ranges sit wholly inside `range`, when they
    /// do not cross a line. What ⌘F offers as the query for a selection.
    pub fn text_inside(&self, range: Range<usize>) -> Option<String> {
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
    /// Use [`Self::mapped_matches`] before replacing matched text: a selection
    /// can cover hidden syntax or more source than the reading-text match.
    pub fn matches(&self, query: &str) -> Vec<Range<usize>> {
        self.mapped_matches(query)
            .into_iter()
            .map(|hit| hit.document.0)
            .collect()
    }

    /// Matches with explicit coordinate spaces and replacement precision.
    /// A document range alone is sufficient for selection, but callers must
    /// inspect `mapping` before treating it as replaceable source text.
    pub fn mapped_matches(&self, query: &str) -> Vec<ReadingMatch> {
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
                hits.push(ReadingMatch {
                    document: DocRange(range),
                    shown: ShownRange(start..end),
                    mapping: self.mapping_for(start..end),
                });
            }
            from = end_byte;
        }
        hits
    }

    fn mapping_for(&self, range: Range<usize>) -> MatchMapping {
        if !self.grapheme_boundaries[range.start] || !self.grapheme_boundaries[range.end] {
            return MatchMapping::Composite;
        }
        let mappings = &self.mappings[range.clone()];
        if mappings
            .iter()
            .all(|mapping| matches!(mapping, CharacterMapping::Exact))
            && self.spans[range.clone()]
                .iter()
                .all(|span| span.end == span.start + 1)
            && self.spans[range.clone()]
                .windows(2)
                .all(|pair| pair[0].end == pair[1].start)
        {
            return MatchMapping::Exact;
        }
        if mappings
            .iter()
            .all(|mapping| matches!(mapping, CharacterMapping::Whole(whole) if whole == &range))
        {
            return MatchMapping::WholeSourceSpan;
        }
        MatchMapping::Composite
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

/// A range in document positions, including structural node boundaries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocRange(pub Range<usize>);

/// A range of Unicode scalar values in [`ShownText::text`], not UTF-8 bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShownRange(pub Range<usize>);

/// How faithfully a reading-text match identifies document content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchMapping {
    /// Contiguous literal document characters; no hidden syntax is crossed.
    Exact,
    /// One complete nonliteral run, such as an entity or an atom label.
    /// Replacement must understand that source construct, not just its label.
    WholeSourceSpan,
    /// Several source constructs, a structural boundary, or part of a
    /// nonliteral run or grapheme. The selected range is not a precise replacement range.
    Composite,
}

/// One match with its selection range and the precision of that mapping.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadingMatch {
    /// Document positions to select.
    pub document: DocRange,
    /// Characters matched in the reading text.
    pub shown: ShownRange,
    /// Whether the selection is suitable for literal replacement.
    pub mapping: MatchMapping,
}

#[derive(Clone)]
enum CharacterMapping {
    Exact,
    Whole(Range<usize>),
    Boundary,
}

/// How an atom's reading text participates in a sentence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AtomTextRole {
    /// A descriptive stand-in for an image or embedded document.
    Placeholder,
    /// Verbatim source or an unknown shortcode.
    Literal,
    /// The visible label of a wiki link.
    Link,
    /// A resolved emoji shortcode.
    Glyph,
}

fn attr<'a>(node: &'a crate::Node, name: &str) -> &'a str {
    node.attrs()
        .get(name)
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .trim()
}

fn wiki_link_embed(node: &crate::Node) -> bool {
    node.attrs()
        .get("embed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn wiki_link_label(node: &crate::Node) -> &str {
    match attr(node, "alias") {
        "" => attr(node, "target"),
        alias => alias,
    }
}

/// The final path component an image or embed uses without an explicit label.
pub fn file_name(src: &str) -> Option<&str> {
    let path = src
        .split(['?', '#'])
        .next()
        .unwrap_or(src)
        .trim_end_matches('/');
    let name = path.rsplit(['/', '\\']).next()?;
    (!name.is_empty()).then_some(name)
}

/// The text an atom shows, without the shape the row uses to draw it.
pub fn shown_atom_label<'a>(types: &DocTypes, node: &'a Node) -> Option<&'a str> {
    atom_label(types, node).map(|(_, label)| label)
}

/// The reading role and text of a supported inline atom: an
/// image's label, the verbatim source of an inline HTML primitive, a wiki
/// link's label, or the emoji a shortcode names. Every other atom keeps the
/// object-replacement character the projection gave it, which is blank.
pub fn atom_label<'a>(types: &DocTypes, node: &'a Node) -> Option<(AtomTextRole, &'a str)> {
    let ty = node.type_id();
    if Some(ty) == types.image {
        let label = match (attr(node, "alt"), file_name(attr(node, "src"))) {
            ("", Some(name)) => name,
            ("", None) => "Image",
            (alt, _) => alt,
        };
        Some((AtomTextRole::Placeholder, label))
    } else if Some(ty) == types.raw_inline {
        // HTML is kept verbatim and shown as source, so the tag reads exactly as
        // it was written — a closing tag included.
        Some((AtomTextRole::Literal, attr(node, "source")))
    } else if Some(ty) == types.wiki_link {
        if wiki_link_embed(node) {
            // `![[…]]` puts a file in the note rather than pointing at a page, so
            // it reads as the picture it is: its alias, or the file it names.
            let label = match (attr(node, "alias"), file_name(attr(node, "target"))) {
                ("", Some(name)) => name,
                ("", None) => "Embed",
                (alias, _) => alias,
            };
            return Some((AtomTextRole::Placeholder, label));
        }
        // The alias is what the author wrote it to read as; without one the
        // target stands in, with whatever `#heading` or `^block` it names,
        // because that is what the link says.
        Some((AtomTextRole::Link, wiki_link_label(node)))
    } else if Some(ty) == types.emoji {
        // A code the table does not know was never read as one, but an atom
        // built by hand could hold one: it reads as its name.
        let code = attr(node, "code");
        match emojis::get_by_shortcode(code) {
            Some(emoji) => Some((AtomTextRole::Glyph, emoji.as_str())),
            None => Some((AtomTextRole::Literal, code)),
        }
    } else {
        None
    }
}
