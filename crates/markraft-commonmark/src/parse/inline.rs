//! The inline half of the parser: a textblock's content read as its source.
//!
//! A paragraph's, a heading's or a table cell's text is the inline Markdown
//! a reader sees once the block's own syntax is stripped: container prefixes
//! and the indentation of continuation lines removed, the block's trailing
//! whitespace dropped, line endings as `\n`, and — for a heading — its `#`
//! marker, closing sequence or setext underline gone. That text is cut out of
//! the source lines here, never rebuilt from comrak's inline tree, and handed
//! to [`crate::textblock::build`], which folds atoms and derives every mark.
//!
//! Link reference definitions a paragraph starts with are not part of its
//! text. They stay in the document as a raw block of their own, where they
//! are written back as they were.

use comrak::nodes::{AstNode, NodeValue, Sourcepos};
use comrak::{Arena, parse_document};
use markraft_core::Node;

use super::{ParseError, Walk};
use crate::derive::BlockKind;

impl<'a> Walk<'a> {
    /// The inline content of a textblock comrak read, and the link reference
    /// definitions its source starts with, which are not part of it.
    pub(crate) fn textblock(
        &self,
        node: &'a AstNode<'a>,
    ) -> Result<(Vec<Node>, Option<String>), ParseError> {
        let pos = self.target(node).sourcepos();
        if matches!(&*self.value(node), NodeValue::Paragraph) && pos.end.line > pos.start.line {
            let lines: Vec<_> = (pos.start.line..=pos.end.line)
                .map(|line| self.cx.line(line))
                .collect();
            if let Some(block) = crate::math::DisplayBlock::starting_at(
                &lines,
                0,
                pos.start.column.saturating_sub(1),
                // The protected parse already chose this paragraph's lines.
                &crate::math::SourceBlocks::default(),
            ) && block.lines.end == lines.len()
            {
                let text = block.source(&lines);
                return Ok((
                    crate::textblock::build(self.schema, BlockKind::Paragraph, &text),
                    None,
                ));
            }
        }
        let (kind, lines) = match &*self.value(node) {
            NodeValue::Paragraph => (
                BlockKind::Paragraph,
                self.paragraph_lines(pos, pos.end.line),
            ),
            NodeValue::Heading(heading) if heading.setext => (
                BlockKind::Heading,
                self.paragraph_lines(pos, pos.end.line.saturating_sub(1).max(pos.start.line)),
            ),
            NodeValue::Heading(_) => (
                BlockKind::Heading,
                vec![atx_content(slice_from(
                    self.cx.line(pos.start.line),
                    pos.start.column,
                ))],
            ),
            NodeValue::TableCell => (BlockKind::TableCell, vec![self.cell_source(pos)]),
            // A consumer's rule that sends something else to an inline type:
            // the text a reader sees there is all there is to go on.
            _ => (BlockKind::Paragraph, vec![self.cx.source(pos)]),
        };
        let skip = match kind {
            BlockKind::TableCell => 0,
            _ => self.leading_definitions(&lines),
        };
        let definitions = (skip > 0).then(|| lines[..skip].join("\n"));
        let text = lines[skip..].join("\n");
        let text = text.trim_end_matches([' ', '\t']);
        Ok((
            crate::textblock::build(self.schema, kind, text),
            definitions,
        ))
    }

    /// A paragraph's lines from its first to `last`, each without its
    /// container prefix and its indentation.
    ///
    /// A continuation line's prefix is whatever block quote markers and
    /// indentation start it, and its content cannot start with a `>` of its
    /// own — that would open a block quote rather than continue the paragraph.
    fn paragraph_lines(&self, pos: Sourcepos, last: usize) -> Vec<String> {
        (pos.start.line..=last)
            .map(|number| {
                let line = self.cx.line(number);
                if number == pos.start.line {
                    slice_from(line, pos.start.column)
                        .trim_start_matches([' ', '\t'])
                        .to_string()
                } else {
                    line.trim_start_matches([' ', '\t', '>']).to_string()
                }
            })
            .collect()
    }

    /// A table cell's source: what its position covers, trimmed. A cell a row
    /// was short of has the position of the row's closing pipe; it is empty.
    fn cell_source(&self, pos: Sourcepos) -> String {
        let line = self.cx.line(pos.start.line);
        let from = pos.start.column.saturating_sub(1).min(line.len());
        let to = pos.end.column.clamp(from, line.len());
        let text = line.get(from..to).unwrap_or_default().trim();
        if text == "|" {
            String::new()
        } else {
            text.to_string()
        }
    }

    /// How many whole lines at the start of `lines` a reader takes for link
    /// reference definitions: the longest run that reads as nothing else.
    fn leading_definitions(&self, lines: &[String]) -> usize {
        if !lines.first().is_some_and(|line| line.starts_with('[')) {
            return 0;
        }
        (1..=lines.len())
            .rev()
            .find(|count| reads_as_definitions(&lines[..*count].join("\n"), self))
            .unwrap_or(0)
    }

    /// The link reference definitions standing between blocks, which comrak
    /// resolves and leaves no node for: each run of lines from `from` up to
    /// `to` (exclusive) that reads as nothing but definitions, without the
    /// container prefix of `parent`.
    pub(crate) fn definitions_between(
        &self,
        parent: &'a AstNode<'a>,
        from: usize,
        to: usize,
    ) -> Vec<String> {
        let parent_pos = self.target(parent).sourcepos();
        let item_padding = match &*self.value(parent) {
            NodeValue::Item(list) => Some(list.padding),
            _ => None,
        };
        let mut runs: Vec<Vec<String>> = Vec::new();
        let mut current: Vec<String> = Vec::new();
        for number in from..to {
            let line = self.cx.line(number);
            let content = match item_padding {
                // An item's first line starts with its marker.
                Some(padding) if number == parent_pos.start.line => {
                    slice_from(line, parent_pos.start.column + padding).to_string()
                }
                // A footnote definition's first line starts with its marker.
                _ if number == parent_pos.start.line
                    && matches!(&*self.value(parent), NodeValue::FootnoteDefinition(_)) =>
                {
                    let rest = slice_from(line, parent_pos.start.column);
                    rest.split_once("]:")
                        .map_or("", |(_, content)| content)
                        .trim_start_matches([' ', '\t'])
                        .to_string()
                }
                _ if number == parent_pos.start.line
                    && !matches!(&*self.value(parent), NodeValue::Document) =>
                {
                    let rest = slice_from(line, parent_pos.start.column);
                    rest.trim_start_matches([' ', '\t', '>']).to_string()
                }
                _ => line.trim_start_matches([' ', '\t', '>']).to_string(),
            };
            if content.trim().is_empty() {
                if !current.is_empty() {
                    runs.push(std::mem::take(&mut current));
                }
            } else {
                current.push(content);
            }
        }
        if !current.is_empty() {
            runs.push(current);
        }
        runs.into_iter()
            .map(|run| run.join("\n"))
            .filter(|run| reads_as_definitions(run, self))
            .collect()
    }
}

/// Whether `source` reads as nothing but link reference definitions.
///
/// comrak does not read a definition whose destination is `<>` when nothing
/// follows it, so the source is given the line ending it had in the file. It
/// also drops a footnote definition nothing refers to, which is not a link
/// reference definition, so footnotes are off for the question.
fn reads_as_definitions(source: &str, walk: &Walk<'_>) -> bool {
    let arena = Arena::new();
    let mut options = walk.options.clone();
    options.extension.footnotes = false;
    !source.trim().is_empty()
        && parse_document(&arena, &format!("{source}\n"), &options)
            .first_child()
            .is_none()
}

/// `line` from a one-based byte column on.
fn slice_from(line: &str, column: usize) -> &str {
    let start = column.saturating_sub(1).min(line.len());
    line.get(start..).unwrap_or_default()
}

/// An ATX heading's content, from the line its marker starts: without the
/// marker, the whitespace around the content and the closing sequence.
fn atx_content(line: &str) -> String {
    let rest = line.trim_start_matches(' ').trim_start_matches('#');
    let rest = rest.trim_matches([' ', '\t']);
    if rest.chars().all(|c| c == '#') {
        return String::new();
    }
    let hashes = rest.len() - rest.trim_end_matches('#').len();
    let before = &rest[..rest.len() - hashes];
    if hashes > 0 && before.ends_with([' ', '\t']) {
        before.trim_end_matches([' ', '\t']).to_string()
    } else {
        rest.to_string()
    }
}

/// Whether an HTML block's literal is nothing but a line break tag.
pub(crate) fn is_break_tag(literal: &str) -> bool {
    let trimmed = literal.trim().to_ascii_lowercase();
    matches!(trimmed.as_str(), "<br>" | "<br/>" | "<br />")
}
