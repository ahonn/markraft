//! Which runs of a line spell rather than say, and what the view shows for
//! each.
//!
//! A document kind that keeps its markup in the text marks the characters
//! that spell it with the conceal role
//! ([`DocTypeNames::syntax`](markraft_core::kind::DocTypeNames::syntax)): each such
//! run carries the id of the span it belongs to and what it displays while
//! concealed. This module is the view's one reading of that contract. The
//! surface, the accessibility tree, the plain-text fallbacks and the
//! formatting state all ask it, so none of them looks at a concealed run's
//! own characters to decide what a reader sees — and they cannot disagree.
//!
//! A span is revealed while the selection, a caret or an input method's
//! marked text touches it: anywhere from the start of its first run to the
//! end of its last one on the line, both edges included for a caret. Every run
//! of a revealed span shows its own characters; every run of a concealed one
//! shows its `display` in their place, or nothing when that is empty.

use std::ops::Range;

use markraft_core::commands::Direction;
use markraft_core::kind::{SYNTAX_DISPLAY_ATTR, SYNTAX_SPAN_ATTR};
use markraft_core::projection::{Line, Projection, RunContent};
use markraft_core::{Fragment, MarkSet, MarkTypeId, Node, Schema, Slice};

/// What a run marked with the conceal role says about itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Concealed<'a> {
    /// The span the run belongs to, shared by the runs that open and close it.
    pub(crate) span: i64,
    /// What a reader sees in its place while it is concealed.
    pub(crate) display: &'a str,
}

/// The conceal-role mark among `marks`, read, when `syntax` names the role.
pub(crate) fn concealed(syntax: Option<MarkTypeId>, marks: &MarkSet) -> Option<Concealed<'_>> {
    let mark = marks.get(syntax?)?;
    let attr = |name: &str| mark.attrs.get(name);
    Some(Concealed {
        span: attr(SYNTAX_SPAN_ATTR)
            .and_then(|value| value.as_int())
            .unwrap_or(0),
        display: attr(SYNTAX_DISPLAY_ATTR)
            .and_then(|value| value.as_str())
            .unwrap_or_default(),
    })
}

/// What the view shows for one run of a line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Shown<'a> {
    /// The run's own characters: it spells nothing.
    Source,
    /// The run's own characters: it spells, and its span is revealed.
    Revealed,
    /// This text in place of the run's characters, which are concealed.
    Display(&'a str),
    /// Nothing: the run is concealed and displays nothing.
    Hidden,
}

/// What reveals a concealed span: the selection and an input method's marked
/// text, as document ranges. A plain-text reading reveals nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Reveal {
    selection: Option<Range<usize>>,
    composition: Option<Range<usize>>,
}

impl Reveal {
    /// Every concealed run concealed: how a reader who is not editing sees it.
    pub(crate) fn nothing() -> Reveal {
        Reveal::default()
    }

    /// Revealed where `selection` (`from..to`, empty for a caret) or
    /// `composition` touches.
    pub(crate) fn at(selection: Range<usize>, composition: Option<Range<usize>>) -> Reveal {
        Reveal {
            selection: Some(selection),
            composition,
        }
    }

    /// Whether anything here touches the document range `from..to`. A caret
    /// touches at either edge; a range has to overlap.
    pub(crate) fn touches(&self, from: usize, to: usize) -> bool {
        let touches = |range: &Range<usize>| {
            if range.start == range.end {
                range.start >= from && range.start <= to
            } else {
                range.start < to && range.end > from
            }
        };
        self.selection.as_ref().is_some_and(touches)
            || self.composition.as_ref().is_some_and(touches)
    }
}

/// What the view shows for each run of `line`, index for index.
pub(crate) fn shown<'l>(
    syntax: Option<MarkTypeId>,
    line: &'l Line,
    reveal: &Reveal,
) -> Vec<Shown<'l>> {
    let concealed: Vec<Option<Concealed<'l>>> = line
        .runs()
        .iter()
        .map(|run| match run.content {
            RunContent::Text(_) => concealed(syntax, &run.marks),
            RunContent::Atom(_) => None,
        })
        .collect();
    // Where each span runs on this line: from its first run's start to its
    // last run's end. A line holds a handful of spans at most.
    let mut extents: Vec<(i64, usize, usize)> = Vec::new();
    for (run, concealed) in line.runs().iter().zip(&concealed) {
        let Some(concealed) = concealed else { continue };
        match extents
            .iter_mut()
            .find(|(span, ..)| *span == concealed.span)
        {
            Some((_, from, to)) => {
                *from = (*from).min(line.abs(run.start));
                *to = (*to).max(line.abs(run.end));
            }
            None => extents.push((concealed.span, line.abs(run.start), line.abs(run.end))),
        }
    }
    concealed
        .into_iter()
        .map(|concealed| {
            let Some(concealed) = concealed else {
                return Shown::Source;
            };
            let revealed = extents
                .iter()
                .find(|(span, ..)| *span == concealed.span)
                .is_some_and(|(_, from, to)| reveal.touches(*from, *to));
            if revealed {
                Shown::Revealed
            } else if concealed.display.is_empty() {
                Shown::Hidden
            } else {
                Shown::Display(concealed.display)
            }
        })
        .collect()
}

/// The concealed runs of `line` a caret at `caret` leaves concealed, as
/// document ranges in order: what a caret moving over what a reader sees
/// takes as one step each, and never stops inside.
///
/// Neighbouring runs that show nothing are one step — `***` closing two spans
/// is not two invisible stops. A run that shows its display in place of its
/// characters is a step of its own. A caret that lands on the start of one
/// touches its span, which reveals it.
pub fn concealed_steps(syntax: Option<MarkTypeId>, line: &Line, caret: usize) -> Vec<Range<usize>> {
    let shown = shown(syntax, line, &Reveal::at(caret..caret, None));
    let mut out: Vec<Range<usize>> = Vec::new();
    let mut joinable = false;
    for (run, shown) in line.runs().iter().zip(shown) {
        match shown {
            Shown::Hidden => {
                match out.last_mut() {
                    Some(last) if joinable && last.end == line.abs(run.start) => {
                        last.end = line.abs(run.end)
                    }
                    _ => out.push(line.abs(run.start)..line.abs(run.end)),
                }
                joinable = true;
            }
            Shown::Display(_) => {
                out.push(line.abs(run.start)..line.abs(run.end));
                joinable = false;
            }
            Shown::Source | Shown::Revealed => joinable = false,
        }
    }
    out
}

/// Where a word motion from `caret` in `dir` stops, over what a reader sees.
///
/// The projection's own word boundaries segment the source, where each `*` of
/// a `**` is a word of its own, so a caret stepping by them lands between two
/// delimiter characters — and whatever is typed or deleted there breaks the
/// span. Here no stop falls strictly inside a run that spells markup, a stop
/// next to a run the caret leaves concealed moves past it, and a step that
/// covers nothing a reader sees does not count: from the end of
/// `a **b** c`, word motion back stops before `c`, then before `**b`.
pub(crate) fn word_boundary(
    syntax: Option<MarkTypeId>,
    projection: &Projection,
    caret: usize,
    dir: Direction,
) -> Option<usize> {
    let backward = dir == Direction::Backward;
    let step = |pos| {
        if backward {
            projection.prev_word_boundary(pos)
        } else {
            projection.next_word_boundary(pos)
        }
    };
    let Some(index) = projection.line_at(caret) else {
        return step(caret);
    };
    let line = &projection.lines()[index];
    let hidden = concealed_steps(syntax, line, caret);
    let spelled = spelling(syntax, line);
    // Markup is no word to stop at, shown or not: as in Typora, ⌥← from the
    // end of `**abc**` reaches `abc`, not the gap before the closing `**`.
    let markup: Vec<Range<usize>> = hidden.iter().chain(&spelled).cloned().collect();
    let mut at = caret;
    loop {
        let mut target = step(at)?;
        if projection.line_at(target) != Some(index) {
            return Some(target);
        }
        if let Some(run) = spelled
            .iter()
            .find(|run| run.start < target && target < run.end)
        {
            target = if backward { run.start } else { run.end };
        }
        while let Some(run) = hidden.iter().find(|run| {
            if backward {
                run.end == target && run.start < target
            } else {
                run.start == target && run.end > target
            }
        }) {
            target = if backward { run.start } else { run.end };
        }
        let edge = target == line.from() || target == line.to();
        if target == at || edge || shows_something(projection, line, &markup, at, target) {
            return Some(target);
        }
        at = target;
    }
}

/// The markup of `line`, one entry per span it spells — a bold's two `**`,
/// a link's `[` and `](url)`, an escape's `\` — each the span's runs in
/// order, as document ranges, concealed or not.
///
/// What a reader edits is the text between a span's first run and its last;
/// the runs themselves are the span's spelling, and an edit that takes one of
/// them without the rest leaves markup that reads as something else.
pub fn markup_spans(syntax: Option<MarkTypeId>, line: &Line) -> Vec<Vec<Range<usize>>> {
    let mut spans: Vec<(i64, Vec<Range<usize>>)> = Vec::new();
    for run in line.runs() {
        let RunContent::Text(_) = run.content else {
            continue;
        };
        let Some(concealed) = concealed(syntax, &run.marks) else {
            continue;
        };
        let range = line.abs(run.start)..line.abs(run.end);
        match spans.iter_mut().find(|(span, _)| *span == concealed.span) {
            Some((_, runs)) => match runs.last_mut() {
                Some(last) if last.end == range.start => last.end = range.end,
                _ => runs.push(range),
            },
            None => spans.push((concealed.span, vec![range])),
        }
    }
    spans.into_iter().map(|(_, runs)| runs).collect()
}

/// The runs of `line` that spell markup, concealed or not, with neighbours
/// merged, as document ranges.
fn spelling(syntax: Option<MarkTypeId>, line: &Line) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::new();
    for run in line.runs() {
        let RunContent::Text(_) = run.content else {
            continue;
        };
        if concealed(syntax, &run.marks).is_none() {
            continue;
        }
        let range = line.abs(run.start)..line.abs(run.end);
        match out.last_mut() {
            Some(last) if last.end == range.start => last.end = range.end,
            _ => out.push(range),
        }
    }
    out
}

/// Whether a reader sees anything but whitespace between `a` and `b` on
/// `line`, given the runs a caret there leaves `hidden`.
fn shows_something(
    projection: &Projection,
    line: &Line,
    hidden: &[Range<usize>],
    a: usize,
    b: usize,
) -> bool {
    let (from, to) = (a.min(b), a.max(b));
    line.runs().iter().any(|run| {
        let start = line.abs(run.start).max(from);
        let end = line.abs(run.end).min(to);
        if start >= end || hidden.iter().any(|h| h.start <= start && end <= h.end) {
            return false;
        }
        match run.content {
            RunContent::Atom(_) => true,
            RunContent::Text(_) => projection
                .text_between(start, end)
                .is_none_or(|text| !text.chars().all(char::is_whitespace)),
        }
    })
}

/// One stretch of what a line shows, and the stretch of the line's text it
/// stands for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Piece<'a> {
    /// `char` offsets into the line's projected text.
    pub(crate) source: Range<usize>,
    /// What is shown for it. Empty for a hidden run.
    pub(crate) text: &'a str,
    /// Whether `text` is the source itself, character for character.
    pub(crate) own: bool,
}

/// What `line`, whose projected text is `text`, shows under `shown`, run by
/// run — an atom as the projection's own placeholder.
pub(crate) fn pieces<'a>(line: &Line, text: &'a str, shown: &[Shown<'a>]) -> Vec<Piece<'a>> {
    let mut out = Vec::with_capacity(line.runs().len());
    let mut chars = text
        .char_indices()
        .map(|(byte, _)| byte)
        .chain([text.len()]);
    let mut byte = chars.next().unwrap_or(0);
    for (run, shown) in line.runs().iter().zip(shown) {
        let start = byte;
        for _ in run.char_from..run.char_to {
            byte = chars.next().unwrap_or(text.len());
        }
        let source = run.char_from..run.char_to;
        out.push(match shown {
            Shown::Source | Shown::Revealed => Piece {
                source,
                text: &text[start..byte],
                own: true,
            },
            Shown::Display(display) => Piece {
                source,
                text: display,
                own: false,
            },
            Shown::Hidden => Piece {
                source,
                text: "",
                own: false,
            },
        });
    }
    out
}

/// `slice` as plain text with every concealed run read as what it displays —
/// the flattening a view falls back to when its host has no codecs of its own.
pub(crate) fn slice_text(schema: &Schema, syntax: Option<MarkTypeId>, slice: &Slice) -> String {
    let content = match syntax {
        Some(_) => displayed(syntax, slice.content()),
        None => slice.content().clone(),
    };
    markraft_core::projection::slice_to_plain_text(schema, &Slice::new(content, 0, 0))
}

/// `content` with each concealed run replaced by what it displays.
fn displayed(syntax: Option<MarkTypeId>, content: &Fragment) -> Fragment {
    let children: Vec<Node> = content
        .iter()
        .filter_map(|node| {
            if node.is_text()
                && let Some(concealed) = concealed(syntax, node.marks())
            {
                return (!concealed.display.is_empty()).then(|| node.with_text(concealed.display));
            }
            if node.is_container() {
                return Some(node.copy(displayed(syntax, node.content())));
            }
            Some(node.clone())
        })
        .collect();
    Fragment::from_nodes(children)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typeahead::tests::state_of;
    use crate::types::DocTypes;
    use markraft_commonmark::{commonmark_doc_type_names, commonmark_schema};
    use markraft_core::projection::projection_of;

    fn syntax() -> Option<MarkTypeId> {
        let schema = commonmark_schema();
        DocTypes::from_schema_names(&schema, &commonmark_doc_type_names()).syntax
    }

    /// What the first line of `source` shows with `reveal`, as text.
    fn showing(source: &str, reveal: impl Fn(&Line) -> Reveal) -> String {
        let state = state_of(source);
        let projection = projection_of(&state);
        let line = &projection.lines()[0];
        let text = projection.line_text(0).unwrap_or_default();
        let shown = shown(syntax(), line, &reveal(line));
        pieces(line, text, &shown)
            .iter()
            .map(|piece| piece.text)
            .collect()
    }

    /// A caret at the `offset`th character of the line's source.
    fn caret(offset: usize) -> impl Fn(&Line) -> Reveal {
        move |line| {
            let pos = line.offset_to_pos(offset).expect("a position in the line");
            Reveal::at(pos..pos, None)
        }
    }

    #[test]
    fn a_concealed_line_reads_as_prose() {
        let away = |_: &Line| Reveal::nothing();
        assert_eq!(showing("**a** *b* `c` [d](e)", away), "a b c d");
        assert_eq!(showing(r"\*f\* &amp; g", away), "*f* & g");
    }

    /// Two spans of one style side by side are two spans: the caret in one
    /// opens only that one, though the style runs on unbroken across both.
    #[test]
    fn adjacent_spans_of_one_style_reveal_apart() {
        assert_eq!(showing("**a**__b__", caret(3)), "**a**b");
        assert_eq!(showing("**a**__b__", caret(8)), "a__b__");
        // `**a****b**` is one span to CommonMark — the middle run cannot close
        // the first pair (the rule of three) — so its middle is text, and the
        // caret anywhere in it opens the one pair around it.
        assert_eq!(showing("**a****b**", caret(3)), "**a****b**");
        assert_eq!(showing("**a****b** c", caret(12)), "a****b c");
    }

    /// The span a caret stands at the edge of is revealed, from outside as
    /// well as inside.
    #[test]
    fn a_caret_at_a_span_edge_reveals_it() {
        assert_eq!(showing("x **a** y", caret(2)), "x **a** y");
        assert_eq!(showing("x **a** y", caret(7)), "x **a** y");
        assert_eq!(showing("x **a** y", caret(1)), "x a y");
        assert_eq!(showing("x **a** y", caret(8)), "x a y");
    }

    /// A caret at the start edge of a span one character wide reveals it,
    /// as it does a longer one.
    #[test]
    fn a_caret_at_the_start_of_a_short_span_reveals_it() {
        assert_eq!(showing(r"x \*66\* y", caret(2)), r"x \*66* y");
        assert_eq!(showing("x &#38; y", caret(2)), "x &#38; y");
        assert_eq!(showing("x [44](55) y", caret(2)), "x [44](55) y");
    }

    /// Nested spans are revealed by where the caret is in each: inside the
    /// inner one both open, inside only the outer one only it does.
    #[test]
    fn nested_spans_reveal_by_their_own_extent() {
        assert_eq!(showing("*a **b** c*", caret(5)), "*a **b** c*");
        assert_eq!(showing("*a **b** c*", caret(10)), "*a b c*");
    }

    /// An escape is a span of its own: the caret beside it opens it, and a
    /// style span next to it stays as it was.
    #[test]
    fn an_escape_next_to_a_span_is_its_own_span() {
        assert_eq!(showing(r"\***a**", caret(1)), r"\*a");
        assert_eq!(showing(r"\***a**", caret(5)), "***a**");
    }

    /// An entity shows what it stands for until the caret reaches it.
    #[test]
    fn an_entity_shows_its_character_until_revealed() {
        assert_eq!(showing("a &amp; b", caret(0)), "a & b");
        assert_eq!(showing("a &amp; b", caret(4)), "a &amp; b");
    }

    /// A hard break's spelling is hidden unless the caret ends its row.
    #[test]
    fn a_hard_break_spelling_shows_only_at_the_caret() {
        let state = state_of("a\\\nb");
        let projection = projection_of(&state);
        let line = &projection.lines()[0];
        let text = projection.line_text(0).unwrap_or_default();
        let read = |reveal: Reveal| -> String {
            let shown = shown(syntax(), line, &reveal);
            pieces(line, text, &shown)
                .iter()
                .map(|piece| piece.text)
                .collect()
        };
        let at = |offset| {
            let pos = line.offset_to_pos(offset).expect("a position");
            Reveal::at(pos..pos, None)
        };
        assert_eq!(read(Reveal::nothing()), "a\nb");
        assert_eq!(read(at(2)), "a\\\nb", "the caret right after the backslash");
        assert_eq!(read(at(3)), "a\nb", "the caret on the next row");
    }

    /// Marked text reveals as a caret does, wherever the selection is.
    #[test]
    fn a_composition_inside_a_span_reveals_it() {
        let reveal = |line: &Line| {
            let pos = line.offset_to_pos(3).expect("a position");
            Reveal::at(line.from()..line.from(), Some(pos..pos + 1))
        };
        assert_eq!(showing("x **ab** y", reveal), "x **ab** y");
    }

    /// What `concealed_steps` finds on the first line of `source` for a caret
    /// at `offset`, as `char` offsets into the line.
    fn steps(source: &str, offset: usize) -> Vec<Range<usize>> {
        let state = state_of(source);
        let projection = projection_of(&state);
        let line = &projection.lines()[0];
        let caret = line.offset_to_pos(offset).expect("a position");
        concealed_steps(syntax(), line, caret)
            .into_iter()
            .map(|range| {
                let offset = |pos| line.pos_to_offset(pos).expect("an offset");
                offset(range.start)..offset(range.end)
            })
            .collect()
    }

    #[test]
    fn a_caret_steps_over_what_it_leaves_concealed() {
        // Away from the span both delimiter runs are steps; beside it neither.
        assert_eq!(steps("x **a** y", 0), [2..4, 5..7]);
        assert!(steps("x **a** y", 2).is_empty());
        // Two runs that close two spans at once are one step.
        assert_eq!(steps("x ***a*** y", 0), [2..5, 6..9]);
        // An entity shows its character: a step of its own.
        assert_eq!(steps("x &amp;*a* y", 0), [2..7, 7..8, 9..10]);
    }

    #[test]
    fn a_slice_flattens_to_what_it_displays() {
        let state = state_of("**a** &amp; \\*b");
        let slice = Slice::new(state.doc().content().clone(), 0, 0);
        assert_eq!(slice_text(state.schema(), syntax(), &slice), "a & *b");
        assert_eq!(
            slice_text(state.schema(), None, &slice),
            "**a** &amp; \\*b",
            "a kind with no conceal role flattens to its characters"
        );
    }
}
