//! Complete formulas read from a document kind's math and conceal marks.
//!
//! The kind marks all literal formula source as math and gives its opening
//! and closing delimiters one shared conceal id with an empty display value.
//! Consumers use these boundaries for rendering and source editing instead of
//! recognizing delimiter spelling themselves. Empty bodies remain formulas,
//! even when a renderer has nothing to draw.

use std::ops::Range;

use crate::kind::{DocTypes, conceal};
use crate::projection::{Line, RunContent};

/// One complete formula with shared rendering and editing boundaries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormulaSpan {
    /// Character offsets in this line's projected source, including fences.
    pub source: Range<usize>,
    /// Character offsets of the editable TeX body, excluding its delimiters.
    pub content: Range<usize>,
    /// The body exactly as projected, including literal line breaks.
    pub tex: String,
    /// Whether the document kind identifies display rather than inline math.
    pub display: bool,
}

impl FormulaSpan {
    /// Whether this display formula occupies the line apart from whitespace.
    pub fn is_standalone(&self, source: &str) -> bool {
        self.display
            && source.chars().count() >= self.source.end
            && source
                .chars()
                .take(self.source.start)
                .all(char::is_whitespace)
            && source
                .chars()
                .skip(self.source.end)
                .all(char::is_whitespace)
    }

    /// Whether an absolute document selection is entirely inside the editable
    /// body. Both body edges admit a caret; delimiter characters do not.
    pub fn selection_within(&self, line: &Line, selection: Range<usize>) -> bool {
        if selection.start > selection.end {
            return false;
        }
        match (
            line.offset_to_pos(self.content.start),
            line.offset_to_pos(self.content.end),
        ) {
            (Some(from), Some(to)) => from <= selection.start && selection.end <= to,
            _ => false,
        }
    }

    /// Use the same inclusive caret edges and composition overlap as markup.
    pub fn revealed(&self, line: &Line, reveal: &conceal::Reveal) -> bool {
        match (
            line.offset_to_pos(self.source.start),
            line.offset_to_pos(self.source.end),
        ) {
            (Some(from), Some(to)) => reveal.touches(from, to),
            // Invalid coordinates must never conceal editable source.
            _ => true,
        }
    }
}

/// Extract formulas the document kind has recognized, without interpreting
/// its delimiter spelling again. Paired conceal ids distinguish adjacent formulas whose math marks
/// are identical. `source` must be the line's full projected source text.
pub fn formula_spans(line: &Line, source: &str, types: &DocTypes) -> Vec<FormulaSpan> {
    let Some(math) = types.math else {
        return Vec::new();
    };
    let mut fences: Vec<(i64, bool, Vec<Range<usize>>)> = Vec::new();
    for run in line.runs() {
        let RunContent::Text(_) = &run.content else {
            continue;
        };
        let Some(mark) = run.marks.get(math) else {
            continue;
        };
        let Some(spelling) = conceal::concealed(types.syntax, &run.marks) else {
            continue;
        };
        if !spelling.display.is_empty() {
            continue;
        }
        let display = mark
            .attrs
            .get("display")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        let range = run.char_from..run.char_to;
        if let Some((_, _, ranges)) = fences
            .iter_mut()
            .find(|(id, mode, _)| *id == spelling.span && *mode == display)
        {
            match ranges.last_mut() {
                Some(last) if last.end == range.start => last.end = range.end,
                _ => ranges.push(range),
            }
        } else {
            fences.push((spelling.span, display, vec![range]));
        }
    }
    if fences.is_empty() {
        return Vec::new();
    }
    let bytes: Vec<usize> = source
        .char_indices()
        .map(|(byte, _)| byte)
        .chain([source.len()])
        .collect();
    let slice = |range: Range<usize>| -> Option<&str> {
        source.get(*bytes.get(range.start)?..*bytes.get(range.end)?)
    };
    let mut formulas = Vec::with_capacity(fences.len());
    for (_, display, ranges) in fences {
        let [open, close] = ranges.as_slice() else {
            continue;
        };
        // Delimiter spelling belongs to the document kind. The marks give
        // every consumer the same source and body boundaries.
        if open.end > close.start {
            continue;
        }
        let whole = open.start..close.end;
        if line.runs().iter().any(|run| {
            let literal = match &run.content {
                RunContent::Text(_) => true,
                RunContent::Atom(node) => Some(node.type_id()) == types.hard_break,
            };
            run.char_from < whole.end
                && whole.start < run.char_to
                && (!run.marks.contains_type(math) || !literal)
        }) {
            continue;
        }
        let Some(tex) = slice(open.end..close.start) else {
            continue;
        };
        formulas.push(FormulaSpan {
            source: whole,
            content: open.end..close.start,
            tex: tex.to_owned(),
            display,
        });
    }
    formulas.sort_by_key(|formula| formula.source.start);
    formulas
}
