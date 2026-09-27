//! The source contract for standalone display formulas.
//!
//! TeX remains literal inline source in a paragraph. All consumers use the
//! same delimiter and container rules; the Markdown parser only adapts those
//! rules to its AST. Offsets in [`DisplaySource`] are characters, while file
//! columns in [`DisplayBlock`] are bytes, matching Comrak's source positions.

use std::ops::Range;

const DISPLAY_FENCE: &str = "$$";
const FENCE_CHARS: usize = 2;

/// One complete display formula occupying a whole textblock. Distinct
/// formulas separated by text are deliberately not a literal textblock.
pub(crate) struct DisplaySource<'a> {
    source: &'a str,
}

impl<'a> DisplaySource<'a> {
    pub(crate) fn parse(source: &'a str) -> Option<Self> {
        let body = source
            .strip_prefix(DISPLAY_FENCE)?
            .strip_suffix(DISPLAY_FENCE)?;
        (!body.contains(DISPLAY_FENCE)).then_some(Self { source })
    }

    pub(crate) fn range(&self) -> Range<usize> {
        0..self.source.chars().count()
    }

    pub(crate) fn body(&self) -> Range<usize> {
        FENCE_CHARS..self.range().end - FENCE_CHARS
    }

    pub(crate) fn fences(&self) -> [Range<usize>; 2] {
        let body = self.body();
        [0..body.start, body.end..self.range().end]
    }

    /// Both body boundaries are editable insertion positions; after the
    /// closing fence, Enter has ordinary paragraph behavior.
    pub(crate) fn contains_insertion(&self, offset: usize) -> bool {
        let body = self.body();
        (body.start..=body.end).contains(&offset)
    }
}

/// Literal source to insert for Enter at a character offset in a paragraph.
/// The first newline is the new caret position when opening a paired fence.
pub(crate) fn enter_insertion(source: &str, offset: usize) -> Option<&'static str> {
    if source == DISPLAY_FENCE && offset == FENCE_CHARS {
        Some("\n\n$$")
    } else {
        DisplaySource::parse(source)?
            .contains_insertion(offset)
            .then_some("\n")
    }
}

/// A multiline display formula's file source, with zero-based line numbers
/// and a byte column for its opening fence. Container syntax is not TeX.
pub(crate) struct DisplayBlock {
    pub(crate) lines: Range<usize>,
    pub(crate) column: usize,
    continuation: String,
}

impl DisplayBlock {
    /// Recognize a formula at a paragraph start supplied by the block parser.
    /// Requiring that context prevents fences inside code from becoming math.
    pub(crate) fn starting_at(lines: &[&str], start: usize, column: usize) -> Option<Self> {
        let first = *lines.get(start)?;
        let opening = first.get(column..)?.strip_prefix(DISPLAY_FENCE)?;
        if opening.contains(DISPLAY_FENCE) {
            return None;
        }
        let continuation = continuation_prefix(first.get(..column)?);
        for (end, line) in lines.iter().enumerate().skip(start + 1) {
            if let Some(body) = line
                .strip_prefix(&continuation)
                .and_then(|line| line.strip_suffix(DISPLAY_FENCE))
                && !body.contains(DISPLAY_FENCE)
            {
                return Some(Self {
                    lines: start..end + 1,
                    column,
                    continuation,
                });
            }
            // An inner fence or departure from the container ends this
            // candidate. A later container may not supply its closing fence.
            if line.contains(DISPLAY_FENCE)
                || (!line.starts_with(&continuation)
                    && line.trim() != continuation.trim()
                    && (continuation.contains('>') || !line.trim().is_empty()))
            {
                return None;
            }
        }
        None
    }

    pub(crate) fn continuation(&self) -> &str {
        &self.continuation
    }

    /// Remove only the container prefix, preserving indentation and blank
    /// lines inside the TeX body. A bare quote prefix represents a blank line.
    pub(crate) fn source(&self, lines: &[&str]) -> String {
        lines[self.lines.clone()]
            .iter()
            .enumerate()
            .map(|(index, line)| {
                if index == 0 {
                    &line[self.column..]
                } else if line.trim() == self.continuation.trim() {
                    ""
                } else {
                    line.strip_prefix(&self.continuation).unwrap_or(line)
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// List markers become indentation on continuation lines; quote markers keep
/// their positions. Markdown container prefixes use ASCII characters only.
fn continuation_prefix(first: &str) -> String {
    let first = ["[ ] ", "[x] ", "[X] "]
        .iter()
        .find_map(|suffix| first.strip_suffix(suffix))
        .unwrap_or(first);
    first
        .chars()
        .map(|c| if c == '>' { '>' } else { ' ' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_source_uses_character_offsets_and_one_pair_of_fences() {
        let formula = DisplaySource::parse("$$中文 + α$$").unwrap();
        assert_eq!(formula.range(), 0..10);
        assert_eq!(formula.body(), 2..8);
        assert_eq!(formula.fences(), [0..2, 8..10]);
        for source in ["$$", "before $$x$$", "$$x$$ after", "$$x$$ and $$y$$"] {
            assert!(DisplaySource::parse(source).is_none(), "{source:?}");
        }
        assert!(DisplaySource::parse("$$$$").is_some());
    }

    #[test]
    fn enter_obeys_source_boundaries_including_an_empty_body() {
        assert_eq!(enter_insertion("$$", 2), Some("\n\n$$"));
        assert_eq!(enter_insertion("$$$$", 2), Some("\n"));
        for offset in 2..=3 {
            assert_eq!(enter_insertion("$$α$$", offset), Some("\n"));
        }
        for offset in [0, 1, 4, 5] {
            assert_eq!(enter_insertion("$$α$$", offset), None);
        }
    }

    #[test]
    fn container_source_keeps_tex_and_stops_at_container_boundaries() {
        let lines = ["> - [x] $$", ">   ", ">     中文", ">   $$"];
        let block = DisplayBlock::starting_at(&lines, 0, 8).unwrap();
        assert_eq!(block.lines, 0..4);
        assert_eq!(block.continuation(), ">   ");
        let source = block.source(&lines);
        assert_eq!(source, "$$\n\n  中文\n$$");
        assert!(DisplaySource::parse(&source).is_some());
        for lines in [
            vec!["> $$", "> x", "outside", "> $$"],
            vec!["- $$", "  x", "outside", "  $$"],
            vec!["$$", "x $$ y", "$$"],
        ] {
            let column = usize::from(lines[0] != "$$") * 2;
            assert!(DisplayBlock::starting_at(&lines, 0, column).is_none());
        }
    }
}
