//! Which runs of a line spell rather than say, and what the view shows for
//! each.
//!
//! A document kind that keeps its markup in the text marks the characters
//! that spell it with the conceal role
//! ([`DocTypeNames::syntax`](crate::kind::DocTypeNames::syntax)): each such
//! run carries the id of the span it belongs to and what it displays while
//! concealed. This module is the one reading of that contract — for the view,
//! for the key chains it binds, and for any extension, vim among them, that
//! edits around markup. The surface, the accessibility tree, the plain-text
//! fallbacks, the formatting state and the operators all ask it, so none of
//! them looks at a concealed run's own characters to decide what a reader sees
//! — and they cannot disagree.
//!
//! A span is revealed while the selection, a caret or an input method's
//! marked text touches it: anywhere from the start of its first run to the
//! end of its last one on the line, both edges included for a caret. Every run
//! of a revealed span shows its own characters; every run of a concealed one
//! shows its `display` in their place, or nothing when that is empty.

use std::ops::Range;

use crate::commands::Direction;
use crate::kind::{SYNTAX_DISPLAY_ATTR, SYNTAX_SPAN_ATTR};
use crate::projection::{Line, Projection, RunContent};
use crate::{Fragment, MarkSet, MarkTypeId, Node, Schema, Slice};

/// What a run marked with the conceal role says about itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Concealed<'a> {
    /// The span the run belongs to, shared by the runs that open and close it.
    pub span: i64,
    /// What a reader sees in its place while it is concealed.
    pub display: &'a str,
}

/// The conceal-role mark among `marks`, read, when `syntax` names the role.
pub fn concealed(syntax: Option<MarkTypeId>, marks: &MarkSet) -> Option<Concealed<'_>> {
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
pub enum Shown<'a> {
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
pub struct Reveal {
    selection: Option<Range<usize>>,
    composition: Option<Range<usize>>,
}

impl Reveal {
    /// Every concealed run concealed: how a reader who is not editing sees it.
    pub fn nothing() -> Reveal {
        Reveal::default()
    }

    /// Revealed where `selection` (`from..to`, empty for a caret) or
    /// `composition` touches.
    pub fn at(selection: Range<usize>, composition: Option<Range<usize>>) -> Reveal {
        Reveal {
            selection: Some(selection),
            composition,
        }
    }

    /// Whether anything here touches the document range `from..to`. A caret
    /// touches at either edge; a range has to overlap.
    pub fn touches(&self, from: usize, to: usize) -> bool {
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
pub fn shown<'l>(syntax: Option<MarkTypeId>, line: &'l Line, reveal: &Reveal) -> Vec<Shown<'l>> {
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
pub fn word_boundary(
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
    // Markup is no word to stop at, shown or not: ⌥← from the
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
pub struct Piece<'a> {
    /// `char` offsets into the line's projected text.
    pub source: Range<usize>,
    /// What is shown for it. Empty for a hidden run.
    pub text: &'a str,
    /// Whether `text` is the source itself, character for character.
    pub own: bool,
}

/// What `line`, whose projected text is `text`, shows under `shown`, run by
/// run — an atom as the projection's own placeholder.
pub fn pieces<'a>(line: &Line, text: &'a str, shown: &[Shown<'a>]) -> Vec<Piece<'a>> {
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
pub fn slice_text(schema: &Schema, syntax: Option<MarkTypeId>, slice: &Slice) -> String {
    let content = match syntax {
        Some(_) => displayed(syntax, slice.content()),
        None => slice.content().clone(),
    };
    crate::projection::slice_to_plain_text(schema, &Slice::new(content, 0, 0))
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

/// `range` with the markup it must not split taken out and the markup it
/// empties put in, as ordered, disjoint ranges.
///
/// A span whose text the range takes entirely goes with its spelling, and a
/// span the range only reaches into keeps every run of its spelling: taking
/// `bold` out of `x **bold** y` leaves `x y`, and taking `old` leaves
/// `x **b**`, where a plain deletion would leave `x ** y` and `x **b` —
/// asterisks that no longer pair, read back as text. With `keep_emptied` the
/// spelling of a span the range empties stays, so text typed next goes inside
/// it; see [`emptied_pair`] for what is left standing then.
pub fn markup_safe(
    projection: &Projection,
    syntax: Option<MarkTypeId>,
    range: Range<usize>,
    keep_emptied: bool,
) -> Vec<Range<usize>> {
    markup_safe_with(projection, syntax, range, |_, _| keep_emptied)
}

/// Source ranges for replacing visible text while retaining the insertion
/// point's enclosing styles. Empty inner styles go away; only paired wrappers
/// containing the leading content position survive to enclose the new text.
/// Single-run substitutions such as entities are always replaced in full.
pub fn markup_replacement(
    projection: &Projection,
    syntax: Option<MarkTypeId>,
    range: Range<usize>,
) -> Vec<Range<usize>> {
    let insertion = range.start;
    markup_safe_with(projection, syntax, range, |runs, content| {
        runs.len() > 1 && content.contains(&insertion)
    })
}

fn markup_safe_with(
    projection: &Projection,
    syntax: Option<MarkTypeId>,
    range: Range<usize>,
    keep_emptied: impl Fn(&[Range<usize>], &Range<usize>) -> bool,
) -> Vec<Range<usize>> {
    let mut take = vec![range.clone()];
    let mut keep = Vec::new();
    let lines = projection
        .lines()
        .iter()
        .filter(|line| line.from() <= range.end && range.start <= line.to());
    for line in lines {
        for runs in markup_spans(syntax, line) {
            let (Some(first), Some(last)) = (runs.first(), runs.last()) else {
                continue;
            };
            // What the span holds: the text between its first run and its last,
            // or — spelled by one run, an entity — the run itself.
            let content = if runs.len() > 1 {
                first.end..last.start
            } else {
                first.clone()
            };
            let emptied =
                !content.is_empty() && range.start <= content.start && content.end <= range.end;
            if emptied && !keep_emptied(&runs, &content) {
                take.push(first.start..last.end);
            } else {
                keep.extend(runs);
            }
        }
    }
    subtract(union(take), &keep)
}

/// The spelling a change over `range` leaves with nothing between it: every
/// run of each span whose text the range takes, in order — `****` for a
/// change of `bold` in `**bold**` — where it will stand once the text is gone,
/// and how much of it comes before the caret. `None` when the range empties
/// no span.
pub fn emptied_pair(
    projection: &Projection,
    syntax: Option<MarkTypeId>,
    range: Range<usize>,
) -> Option<EmptiedPair> {
    let mut runs: Vec<Range<usize>> = projection
        .lines()
        .iter()
        .filter(|line| line.from() <= range.end && range.start <= line.to())
        .flat_map(|line| markup_spans(syntax, line))
        .filter(|runs| {
            runs.len() > 1
                && runs.first().zip(runs.last()).is_some_and(|(first, last)| {
                    first.end < last.start && range.start <= first.end && last.start <= range.end
                })
        })
        .flatten()
        .collect();
    runs.sort_by_key(|run| run.start);
    let at = runs.first()?.start;
    let text: String = runs
        .iter()
        .filter_map(|run| projection.text_between(run.start, run.end))
        .collect();
    let caret = runs
        .iter()
        .filter(|run| run.end <= range.start)
        .map(|run| run.end - run.start)
        .sum::<usize>();
    Some(EmptiedPair {
        at,
        caret: at + caret,
        text,
    })
}

/// A style's spelling a change emptied: where it stands, the caret between
/// its halves, and its characters.
#[derive(Clone, Debug, PartialEq)]
pub struct EmptiedPair {
    /// Where the spelling starts.
    pub at: usize,
    /// The caret between its halves.
    pub caret: usize,
    /// Its characters, both halves together.
    pub text: String,
}

impl EmptiedPair {
    /// The pair still as the change left it — the caret between its halves and
    /// nothing typed there — and the range it takes, for a caller to remove.
    pub fn untouched(&self, projection: &Projection, head: usize) -> Option<Range<usize>> {
        let end = self.at + self.text.chars().count();
        (head == self.caret && projection.text_between(self.at, end) == Some(self.text.as_str()))
            .then_some(self.at..end)
    }
}

/// `ranges` merged where they touch or overlap, in order.
fn union(mut ranges: Vec<Range<usize>>) -> Vec<Range<usize>> {
    ranges.sort_by_key(|range| range.start);
    let mut out: Vec<Range<usize>> = Vec::new();
    for range in ranges {
        match out.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => out.push(range),
        }
    }
    out
}

/// `ranges` without any position `holes` cover, dropping what empties.
fn subtract(ranges: Vec<Range<usize>>, holes: &[Range<usize>]) -> Vec<Range<usize>> {
    let mut out = ranges;
    for hole in holes {
        out = out
            .into_iter()
            .flat_map(|range| {
                [
                    range.start..hole.start.clamp(range.start, range.end),
                    hole.end.clamp(range.start, range.end)..range.end,
                ]
            })
            .filter(|range| !range.is_empty())
            .collect();
    }
    out
}
