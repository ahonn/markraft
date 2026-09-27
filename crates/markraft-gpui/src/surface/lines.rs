//! The document's lines as the view lays them out: how tall every line is, and
//! the shaped rows of the lines near what the view shows.
//!
//! Shaping a line asks the platform to lay its text out, and a long note holds
//! tens of thousands of lines, most of which are never on screen. So only the
//! lines a frame shows — the visible ones, a screen's worth either side, and
//! the few the caret, the selection or an input method stand on — are shaped.
//! Every other line only has a height: exact where it was shaped at this width
//! once and nothing it reads has changed since, and estimated from its text
//! where not. The heights stack into where each line starts, which is all the
//! note's length and the scroll position need.
//!
//! A table's cells are shaped together, since a column is as wide as its widest
//! cell: a frame that shows one cell of a table shapes the whole grid.
//!
//! What a line's shaping reads besides its own body is its [`LineKey`], so a
//! line that kept its body across an edit keeps its rows and its height for as
//! long as its key stays the same.

use super::*;
use markraft_core::ends::KeptEnds;

/// Where a height came from.
#[derive(Clone, PartialEq)]
enum Measured {
    /// Shaping the line at this width, under this key. A line with no key —
    /// one the selection touches, a table cell, a line with a picture — is
    /// only exact until the document or the selection moves.
    Shaped(Option<LineKey>),
    /// Estimated from the line's text; see [`estimate_height`].
    Estimated,
}

struct Slot {
    /// The line's top gap and height together: how far below where it starts
    /// the next line starts.
    height: Pixels,
    measured: Measured,
    /// The line shaped, while it is near what the view shows. Its origin is
    /// relative to its own place, as shaping leaves it; a frame places it.
    line: Option<LayoutLine>,
    /// Whether the line draws a picture, whose size arrives when its file does.
    picture: bool,
}

/// Every line of one projection, at one width, under one revision of what
/// shaping reads.
#[derive(Default)]
pub(crate) struct Lines {
    projection: Option<Arc<Projection>>,
    width: Pixels,
    revision: u64,
    /// What the selection and marked text reveal and which lines they touch;
    /// a line they touch is shaped with its source shown.
    focus: (u64, Range<usize>, Option<Range<usize>>),
    slots: Vec<Slot>,
    /// Where each line starts, and past the last one where the document ends.
    /// Rebuilt when a height changed.
    tops: Vec<Pixels>,
    tops_stale: bool,
    /// The lines the last frame was asked to show, in order and disjoint.
    shown: Vec<Range<usize>>,
}

impl Lines {
    /// Line the slots up with `input`'s projection at `width`.
    ///
    /// Lines the projection kept from the one before — the same body at the
    /// same place from either end, which is how an edit hands them over — keep
    /// their rows and their height while their key is unchanged. Everything
    /// else starts from an estimate. A different width or a different revision
    /// of the inputs starts every line from one.
    pub(crate) fn sync(
        &mut self,
        input: &ShapeInput<'_>,
        projection: &Arc<Projection>,
        width: Pixels,
        revision: u64,
    ) {
        let focus = focus_of(input);
        let same_inputs = self.width == width && self.revision == revision;
        match &self.projection {
            Some(held) if same_inputs && Arc::ptr_eq(held, projection) => {
                if self.focus != focus {
                    let touched = [&self.focus, &focus]
                        .into_iter()
                        .flat_map(|(_, selection, marked)| [Some(selection), marked.as_ref()])
                        .flatten()
                        .map(|range| range.start..range.end + 1)
                        .collect::<Vec<_>>();
                    self.revealed(input, &touched);
                    // A picture the caret spells out is text now, and kept only
                    // through the spelled atoms; see `retain_pictures`.
                    retain_pictures(input);
                }
            }
            Some(held) if same_inputs => {
                let held = held.clone();
                self.carry_over(input, &held);
                retain_pictures(input);
            }
            _ => {
                self.slots = (0..projection.line_count())
                    .map(|index| estimated(input, index, width))
                    .collect();
                self.tops_stale = true;
                retain_pictures(input);
            }
        }
        self.projection = Some(projection.clone());
        self.width = width;
        self.revision = revision;
        self.focus = focus;
    }

    /// Whether the slots are lined up with `projection`, whatever width and
    /// revision they were measured under.
    pub(crate) fn holds(&self, projection: &Arc<Projection>) -> bool {
        self.projection
            .as_ref()
            .is_some_and(|held| Arc::ptr_eq(held, projection))
    }

    /// The caret or the marked text moved over an unchanged document: a line
    /// with no key read the old selection, a keyed line it now touches has
    /// lost its key, and a line the old or the new selection touches — `touched`
    /// — shows its source or stops showing it, whatever its height said.
    fn revealed(&mut self, input: &ShapeInput<'_>, touched: &[Range<usize>]) {
        for (index, slot) in self.slots.iter_mut().enumerate() {
            let stale = match (&slot.measured, &slot.line) {
                (Measured::Shaped(None), _) => true,
                (_, Some(line)) => line.reuse.is_none() || line.reuse != line_key(input, index),
                _ => touched.iter().any(|range| range.contains(&index)),
            };
            if stale {
                forget(slot);
                self.tops_stale = true;
            }
        }
    }

    /// Carry the slots of `held` over to the lines `input`'s projection kept of
    /// it, and estimate the rest.
    fn carry_over(&mut self, input: &ShapeInput<'_>, held: &Projection) {
        let before = held.lines();
        let now = input.projection.lines();
        let kept = kept_ends(before, now);
        let intact = intact_cells(input, |index| kept.is_kept(index));
        self.slots = kept
            .carry(std::mem::take(&mut self.slots))
            .into_iter()
            .enumerate()
            .map(|(index, slot)| match slot {
                Some(slot) => kept_for(input, index, slot, intact[index]),
                None => estimated(input, index, self.width),
            })
            .collect();
        self.tops_stale = true;
    }

    /// Shape every line of `range` that is not shaped yet.
    pub(crate) fn lay_out_range(
        &mut self,
        input: &ShapeInput<'_>,
        range: Range<usize>,
        text_system: &WindowTextSystem,
    ) {
        self.lay_out(input, std::slice::from_ref(&range), text_system);
    }

    /// Shape every line of `wanted` that is not shaped yet.
    pub(crate) fn lay_out(
        &mut self,
        input: &ShapeInput<'_>,
        wanted: &[Range<usize>],
        text_system: &WindowTextSystem,
    ) {
        let width = self.width;
        for range in wanted {
            let mut index = range.start;
            while index < range.end.min(self.slots.len()) {
                if self.slots[index].line.is_some() {
                    index += 1;
                    continue;
                }
                match table_cell(input, index) {
                    Some((table, _, _)) => {
                        let cells = self.table_at(input, index, table);
                        self.lay_out_table(input, table, cells.clone(), text_system);
                        index = cells.end.max(index + 1);
                    }
                    None => {
                        let mut line = shape_line(input, index, width, None, text_system);
                        line.reuse = line_key(input, index);
                        self.store(index, line);
                        index += 1;
                    }
                }
            }
        }
    }

    /// The lines of the table the cell at `index` belongs to.
    fn table_at(&self, input: &ShapeInput<'_>, index: usize, table: usize) -> Range<usize> {
        let of_table = |at: usize| table_cell(input, at).is_some_and(|(it, _, _)| it == table);
        let mut start = index;
        while start > 0 && of_table(start - 1) {
            start -= 1;
        }
        let mut end = index + 1;
        while end < self.slots.len() && of_table(end) {
            end += 1;
        }
        start..end
    }

    fn lay_out_table(
        &mut self,
        input: &ShapeInput<'_>,
        table: usize,
        cells: Range<usize>,
        text_system: &WindowTextSystem,
    ) {
        let width = self.width;
        let mut shaped: Vec<LayoutLine> = cells
            .clone()
            .map(|index| shape_line(input, index, width, Some(CellWidth::Natural), text_system))
            .collect();
        shape_table(input, table, &mut shaped, width, text_system);
        for (index, line) in cells.zip(shaped) {
            self.store(index, line);
        }
    }

    fn store(&mut self, index: usize, line: LayoutLine) {
        let slot = &mut self.slots[index];
        let height = line.top_gap + line.height;
        if slot.height != height {
            slot.height = height;
            self.tops_stale = true;
        }
        slot.measured = Measured::Shaped(line.reuse.clone());
        // A rendered page may hold pictures, whose sizes arrive with them.
        slot.picture |= line.preview.is_some() || line.rendered.is_some();
        slot.line = Some(line);
    }

    /// Measure lines whose height is still an estimate, nearest `around`
    /// first, for as long as `budget` allows, keeping only their heights.
    /// Returns whether any line is left unmeasured.
    pub(crate) fn measure_some(
        &mut self,
        input: &ShapeInput<'_>,
        around: usize,
        budget: std::time::Duration,
        text_system: &WindowTextSystem,
    ) -> bool {
        let started = std::time::Instant::now();
        let count = self.slots.len();
        let mut measured = Vec::new();
        let mut left = false;
        // Outwards from `around`: below, then above, one line further each time.
        for step in 0..count * 2 {
            let index = if step % 2 == 0 {
                around.checked_add(step / 2)
            } else {
                around.checked_sub(step / 2 + 1)
            };
            let Some(index) = index.filter(|&index| index < count) else {
                continue;
            };
            if self.slots[index].measured != Measured::Estimated {
                continue;
            }
            if started.elapsed() >= budget {
                left = true;
                break;
            }
            let fresh = self.slots[index].line.is_none();
            self.lay_out_range(input, index..index + 1, text_system);
            if fresh {
                measured.push(index);
            }
        }
        // Only the heights were wanted; the rows go unless a frame shows them.
        for index in measured {
            let range = match table_cell(input, index) {
                Some((table, _, _)) => self.table_at(input, index, table),
                None => index..index + 1,
            };
            for index in range {
                if !self.shown.iter().any(|shown| shown.contains(&index)) {
                    self.slots[index].line = None;
                }
            }
        }
        left
    }

    /// Drop the rows of every line the frame does not show. Heights stay.
    pub(crate) fn keep_shown_rows(&mut self) {
        let shown = &self.shown;
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if !shown.iter().any(|range| range.contains(&index)) {
                slot.line = None;
            }
        }
    }

    /// The lines the last frame was asked to show.
    pub(crate) fn shown_ranges(&self) -> &[Range<usize>] {
        &self.shown
    }

    /// The lines a frame shows from now on.
    ///
    /// A range that reaches a table cell takes in the whole grid, as shaping
    /// does. The cells before the last of a row start where it does and have
    /// no height, so a range found by y starts at the row's last cell, and
    /// the cells before it would otherwise go undrawn.
    pub(crate) fn set_shown(&mut self, shown: Vec<Range<usize>>) {
        let table = |index: usize| {
            let line = self.slots.get(index)?.line.as_ref()?;
            Some(line.table?.table)
        };
        let grown = shown
            .into_iter()
            .filter(|range| !range.is_empty())
            .map(|range| {
                let (mut start, mut end) = (range.start, range.end);
                if let Some(first) = table(start) {
                    while start > 0 && table(start - 1) == Some(first) {
                        start -= 1;
                    }
                }
                if let Some(last) = table(end - 1) {
                    while table(end) == Some(last) {
                        end += 1;
                    }
                }
                start..end
            })
            .collect();
        // Grown ranges may now overlap, and a line in two would be drawn twice.
        self.shown = crate::layout::merged(grown);
    }

    /// The shaped lines the last frame was asked to show, each with where it
    /// starts, in document order.
    pub(crate) fn shown(&mut self) -> Vec<(Pixels, LayoutLine)> {
        self.refresh_tops();
        let mut out = Vec::new();
        for range in &self.shown {
            for index in range.clone() {
                if let Some(line) = self.slots.get(index).and_then(|slot| slot.line.clone()) {
                    out.push((self.tops[index], line));
                }
            }
        }
        out
    }

    /// Whether line `index` is shaped.
    pub(crate) fn is_laid(&self, index: usize) -> bool {
        self.slots
            .get(index)
            .is_some_and(|slot| slot.line.is_some())
    }

    /// Give back every line's rows, for an editor that is not drawn. The
    /// heights stay, so the note comes back where it was left.
    pub(crate) fn release(&mut self) {
        for slot in &mut self.slots {
            slot.line = None;
        }
        self.shown.clear();
    }

    /// A picture arrived or changed on disk: every line drawing one is shaped
    /// and measured again.
    pub(crate) fn forget_pictures(&mut self) {
        for slot in &mut self.slots {
            if slot.picture {
                forget(slot);
                self.tops_stale = true;
            }
        }
    }

    pub(crate) fn forget_all_math(&mut self, types: &DocTypes) {
        let Some(projection) = &self.projection else {
            return;
        };
        let affected = projection
            .lines()
            .iter()
            .map(|line| has_math(types, line))
            .collect();
        self.forget_math_lines(types, affected);
    }

    /// Completed formulas invalidate only the blocks that requested them and
    /// their table grids, whose column widths and row heights are shared.
    /// Resetting all measured lines would continually requeue evicted raster
    /// results in notes larger than the math cache.
    pub(crate) fn forget_math(
        &mut self,
        types: &DocTypes,
        equations: Option<&markraft_core::kind::equations::EquationIndex>,
        requests: &[crate::math::MathRequest],
    ) {
        let Some(projection) = &self.projection else {
            return;
        };
        let affected = projection
            .lines()
            .iter()
            .enumerate()
            .map(|(index, line)| {
                let Some(source) = projection.line_text(index) else {
                    return false;
                };
                crate::math_spans::formula_spans(line, source, types)
                    .iter()
                    .any(|span| {
                        let equation =
                            equations.and_then(|indexing| indexing.get(index, span.source.start));
                        let source = equation.map_or(span.tex.as_str(), |equation| {
                            equation.render_source.as_str()
                        });
                        requests.iter().any(|request| {
                            request.source.as_ref() == source
                                || equation.and_then(|equation| equation.tag.as_deref())
                                    == Some(request.source.as_ref())
                        })
                    })
            })
            .collect();
        self.forget_math_lines(types, affected);
    }

    fn forget_math_lines(&mut self, types: &DocTypes, mut affected: Vec<bool>) {
        let Some(projection) = &self.projection else {
            return;
        };
        let mut index = 0;
        while index < projection.line_count() {
            let Some((table, _, _)) = types.table_cell_of(&projection.lines()[index]) else {
                index += 1;
                continue;
            };
            let start = index;
            while index < projection.line_count()
                && types
                    .table_cell_of(&projection.lines()[index])
                    .is_some_and(|(at, _, _)| at == table)
            {
                index += 1;
            }
            if affected[start..index].iter().any(|&value| value) {
                affected[start..index].fill(true);
            }
        }
        for (slot, affected) in self.slots.iter_mut().zip(affected) {
            if affected {
                forget(slot);
                self.tops_stale = true;
            }
        }
    }

    /// Every line's rows, for a test that shaped them all.
    #[cfg(test)]
    #[cfg_attr(coverage_nightly, coverage(off))]
    pub(crate) fn all(&self) -> Vec<LayoutLine> {
        self.slots
            .iter()
            .map(|slot| slot.line.clone().expect("every line laid out"))
            .collect()
    }

    /// How many lines still have only an estimated height.
    #[cfg(test)]
    #[cfg_attr(coverage_nightly, coverage(off))]
    pub(crate) fn estimated(&self) -> usize {
        self.slots
            .iter()
            .filter(|slot| slot.measured == Measured::Estimated)
            .count()
    }

    /// How many lines there are.
    pub(crate) fn len(&self) -> usize {
        self.slots.len()
    }

    /// Where line `index` starts; past the last line, where the document ends.
    pub(crate) fn top(&mut self, index: usize) -> Pixels {
        self.refresh_tops();
        self.tops[index.min(self.slots.len())]
    }

    /// How tall the document is.
    pub(crate) fn total(&mut self) -> Pixels {
        self.top(self.slots.len())
    }

    /// The line `y` falls in, clamped to the document.
    pub(crate) fn index_at(&mut self, y: Pixels) -> usize {
        self.refresh_tops();
        let after = self.tops[1..].partition_point(|&top| top <= y);
        after.min(self.slots.len().saturating_sub(1))
    }

    fn refresh_tops(&mut self) {
        if !self.tops_stale && self.tops.len() == self.slots.len() + 1 {
            return;
        }
        self.tops.clear();
        self.tops.reserve(self.slots.len() + 1);
        let mut y = px(0.);
        self.tops.push(y);
        for slot in &self.slots {
            y += slot.height;
            self.tops.push(y);
        }
        self.tops_stale = false;
    }
}

/// What of the selection and the marked text a line's shaping reads: which
/// delimiters they reveal, and which lines they touch.
fn focus_of(input: &ShapeInput<'_>) -> (u64, Range<usize>, Option<Range<usize>>) {
    let lines = |range: &Range<usize>| {
        let line = |pos: usize| input.projection.line_at(pos).unwrap_or(0);
        line(range.start)..line(range.end)
    };
    (
        reveal_key(input),
        lines(&input.selection),
        input.composition.as_ref().map(lines),
    )
}

/// How the lines of `now` line up with those of `before`, a line kept where
/// it keeps its body.
///
/// An edit rebuilds one run of lines and hands those before and after it over
/// with their bodies, so the kept ends are exactly what can be kept. A pairing
/// that is wrong costs a reshape, never a stale row, since a kept line is
/// checked against the key of the line it is kept for.
pub(crate) fn kept_ends(before: &[Line], now: &[Line]) -> KeptEnds {
    KeptEnds::of(before, now, Line::same_body)
}

/// A slot whose line survived an edit at `index`, checked against the line's
/// key there: what it read of its neighbours may have changed.
///
/// A table cell has no key, since its size is settled by the whole grid, but
/// `intact` says every cell of its grid survived: the grid reads nothing
/// outside itself unless it contains math, so the cell's height still holds. Its rows do not — a cell
/// records where the grid placed it — and the grid is laid out again whole
/// when it is next shown.
fn kept_for(input: &ShapeInput<'_>, index: usize, mut slot: Slot, intact: bool) -> Slot {
    let key = line_key(input, index);
    let line = &input.projection.lines()[index];
    slot.line = slot
        .line
        .take()
        .filter(|kept| key.is_some() && kept.reuse == key)
        .map(|kept| kept.moved_to(line, index));
    let grid_holds = intact
        && slot.measured == Measured::Shaped(None)
        && !slot.picture
        && !line_focused(input, line);
    if !grid_holds && (slot.measured != Measured::Shaped(key.clone()) || key.is_none()) {
        slot.measured = Measured::Estimated;
    }
    slot
}

/// For each line of `input`'s projection, whether it is a cell of a grid
/// every cell of which `is_kept` says survived the edit. Math references can
/// depend on labels outside a grid, so those grids must be measured again.
fn intact_cells(input: &ShapeInput<'_>, is_kept: impl Fn(usize) -> bool) -> Vec<bool> {
    let count = input.projection.line_count();
    let mut intact = vec![false; count];
    let mut index = 0;
    while index < count {
        let Some((table, _, _)) = table_cell(input, index) else {
            index += 1;
            continue;
        };
        let start = index;
        while index < count && table_cell(input, index).is_some_and(|(at, _, _)| at == table) {
            index += 1;
        }
        let kept = (start..index)
            .all(|at| is_kept(at) && !has_math(input.types, &input.projection.lines()[at]));
        intact[start..index].fill(kept);
    }
    intact
}

fn has_math(types: &DocTypes, line: &Line) -> bool {
    types
        .math
        .is_some_and(|math| line.runs().iter().any(|run| run.marks.contains_type(math)))
}

/// Forget what shaping said of a line; its height stays as the estimate.
fn forget(slot: &mut Slot) {
    slot.line = None;
    slot.measured = Measured::Estimated;
}

fn estimated(input: &ShapeInput<'_>, index: usize, width: Pixels) -> Slot {
    let (height, picture) = estimate_height(input, index, width);
    Slot {
        height,
        measured: Measured::Estimated,
        line: None,
        picture,
    }
}

/// Keep the pictures the document shows, and drop the decoded copies of any
/// it no longer does: its atoms', and those of a picture the caret has spelled
/// out, which is text now but still drawn under its source, and those of an
/// HTML block drawn as its page. Dropping the latter would throw away a remote
/// fetch as soon as it started, and decode a local file again on every frame.
fn retain_pictures(input: &ShapeInput<'_>) {
    let spelled: Vec<Node> = input
        .spelling
        .map(|spelling| {
            let lines = input.projection.lines();
            let spelled = lines
                .iter()
                .filter(|line| line_focused(input, line))
                .flat_map(|line| spelling.spelled_atoms(line))
                .map(|(_, node)| node);
            let rendered = lines
                .iter()
                .filter(|line| !line_focused(input, line))
                .filter_map(|line| spelling.rendered(line))
                .flat_map(|page| {
                    page.projection
                        .lines()
                        .iter()
                        .flat_map(|line| line.runs().to_vec())
                        .filter_map(|run| match run.content {
                            RunContent::Atom(node) => Some(node),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                });
            spelled.chain(rendered).collect()
        })
        .unwrap_or_default();
    input.images.retain_sources(
        input
            .projection
            .lines()
            .iter()
            .flat_map(|line| line.runs())
            .filter_map(|run| match &run.content {
                RunContent::Atom(node) => picture_source(input.types, node),
                _ => None,
            })
            .chain(
                spelled
                    .iter()
                    .filter_map(|node| picture_source(input.types, node)),
            ),
    );
}

/// How tall line `index` is likely to be at `width`, without shaping it, and
/// whether it draws a picture.
///
/// Everything about a line's height but the number of rows its text wraps to
/// follows from the document and the style: its font size, its gaps, a code
/// panel's padding, a callout's header. The rows are estimated from the text's
/// length at an average advance, and a picture from the frame a picture still
/// loading is drawn in. Shaping the line corrects both when it comes into view.
pub(super) fn estimate_height(
    input: &ShapeInput<'_>,
    index: usize,
    width: Pixels,
) -> (Pixels, bool) {
    let ShapeInput {
        types,
        projection,
        style,
        single_line,
        ..
    } = *input;
    let line = &projection.lines()[index];
    let picture = line.runs().iter().any(|run| match &run.content {
        RunContent::Atom(node) => picture_source(types, node).is_some(),
        _ => false,
    });
    let heading = types.heading_level(line);
    let code = types.is_code_block(line);
    let font_size = style.font_size(heading, code);
    let line_height = font_size * style.line_height_ratio;
    let text = projection.line_text(index).unwrap_or_default();

    if let Some((table, row, _)) = types.table_cell_of(line) {
        // Only the last cell of a grid row carries the row's height, and the
        // last row the gap below the table; see `place_table`.
        let next = projection.line(index + 1);
        let next_cell = next.and_then(|next| types.table_cell_of(next));
        let last_of_row = next_cell.is_none_or(|(at, below, _)| at != table || below != row);
        if !last_of_row {
            return (px(0.), picture);
        }
        let rows = text.split('\n').count().max(1) as f32;
        let last_row = next_cell.is_none_or(|(at, _, _)| at != table);
        let gap = if last_row {
            gap_below(input, index, line, None, false, &None)
        } else {
            px(0.)
        };
        return (line_height * rows + CELL_PADDING_Y * 2. + gap, picture);
    }

    let indent = indent_of(types, line, style, None).min(max_indent(style, width));
    let wrap = (width - indent - if code { CODE_PADDING } else { px(0.) }).max(px(40.));
    let advance = |c: char| -> f32 {
        if is_wide(c) {
            1.
        } else if code {
            0.6
        } else {
            0.52
        }
    };
    let rows: usize = text
        .split('\n')
        .map(|row| {
            if single_line {
                return 1;
            }
            let em: f32 = row.chars().map(advance).sum();
            ((font_size * em) / wrap).ceil().max(1.) as usize
        })
        .sum();
    let mut text_height = line_height * rows.max(1) as f32;
    if picture
        && text
            .chars()
            .all(|c| c == markraft_core::projection::OBJECT_REPLACEMENT || c.is_whitespace())
    {
        // A picture alone on its line is drawn as tall as it is; until it
        // arrives, as tall as the frame that holds its place.
        text_height = text_height.max(loading_frame(wrap).height);
    }
    let marker = chrome_marker(types, line, None);
    let gap = gap_below(input, index, line, heading, code, &marker);
    let code_inset = if code { CODE_INSET } else { px(0.) };
    let top_gap = if code {
        code_inset
    } else if index == 0 {
        px(0.)
    } else if let Some(level) = heading {
        style.heading_top_gap(level)
    } else {
        px(0.)
    };
    let above = index.checked_sub(1).map(|above| &projection.lines()[above]);
    let header = if crate::callout::header_of(types, line, above).is_some() {
        CALLOUT_HEADER_HEIGHT
    } else {
        px(0.)
    };
    (top_gap + header + text_height + code_inset + gap, picture)
}

/// Whether `c` is drawn a full em wide: CJK, Hangul, full-width forms and
/// emoji.
fn is_wide(c: char) -> bool {
    matches!(u32::from(c),
        0x1100..=0x115F
        | 0x2E80..=0xA4CF
        | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF
        | 0xFE30..=0xFE4F
        | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6
        | 0x1F300..=0x1FAFF
        | 0x20000..=0x3FFFD)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod math_tests {
    use super::Lines;
    use crate::{style::EditorStyle, surface::ShapeInput};
    use gpui::{NoopTextSystem, TextSystem, WindowTextSystem, px};
    use markraft_commonmark::{commonmark_doc_type_names, commonmark_schema, from_markdown};
    use markraft_core::{kind::DocTypes, projection::Projection};
    use std::{collections::HashSet, sync::Arc, time::Duration};

    const TABLES: &str = "$$x\\tag{A}\\label{eq}$$

| Reference | Numbered |
|---|---|
| $\\ref{eq}$ | $$y\\tag{T}$$ |

| Separate | Grid |
|---|---|
| $z$ | other |

| Plain | Grid |
|---|---|
| one | two |

$w$";

    fn sync_state(state: &markraft_core::EditorState, lines: &mut Lines, measure: bool) {
        let projection = markraft_core::projection::projection_of(state);
        let types = DocTypes::from_schema_names(state.schema(), &commonmark_doc_type_names());
        let equations =
            markraft_core::kind::equations::EquationIndex::build(&projection, &types, false);
        let images = crate::images::Images::default();
        let style = EditorStyle::notes();
        let input = ShapeInput {
            images: &images,
            maths: None,
            equations: Some(&equations),
            scale_factor: 1.,
            doc: state.doc(),
            types: &types,
            projection: &projection,
            style: &style,
            single_line: false,
            wiki: None,
            spelling: None,
            selection: 0..0,
            composition: None,
        };
        lines.sync(&input, &projection, px(400.), 0);
        if measure {
            let text_system =
                WindowTextSystem::new(Arc::new(TextSystem::new(Arc::new(NoopTextSystem::new()))));
            lines.lay_out_range(&input, 0..lines.len(), &text_system);
        }
    }

    #[test]
    fn an_external_tag_edit_invalidates_offscreen_math_table_heights() {
        use crate::typeahead::tests::{at, run, state_of};
        use markraft_core::{commands::insert_text, projection::projection_of};
        let state = state_of(TABLES);
        let projection = projection_of(&state);
        let tag = projection.line_text(0).unwrap().find("{A}").unwrap() + 2;
        let state = at(&state, projection.lines()[0].offset_to_pos(tag).unwrap());
        let mut lines = Lines::default();
        sync_state(&state, &mut lines, true);
        assert_eq!(lines.estimated(), 0);
        lines.release();
        let edited = run(&state, &insert_text(" longer"));
        let next = projection_of(&edited);
        let types = DocTypes::from_schema_names(state.schema(), &commonmark_doc_type_names());
        let dependent = next
            .lines()
            .iter()
            .enumerate()
            .find(|(index, _)| next.line_text(*index).unwrap().contains("\\ref{eq}"))
            .unwrap()
            .0;
        assert!(
            projection.lines()[dependent].same_body(&next.lines()[dependent]),
            "the reference source did not change"
        );
        sync_state(&edited, &mut lines, false);
        let table = types.table_cell_of(&next.lines()[dependent]).unwrap().0;
        for (index, line) in next.lines().iter().enumerate() {
            if types
                .table_cell_of(line)
                .is_some_and(|(at, _, _)| at == table)
            {
                assert!(
                    matches!(lines.slots[index].measured, super::Measured::Estimated),
                    "dependent grid cell {index} must be remeasured"
                );
            }
        }
        let plain = next
            .lines()
            .iter()
            .enumerate()
            .find(|(index, _)| next.line_text(*index) == Some("Plain"))
            .unwrap()
            .0;
        assert!(
            matches!(lines.slots[plain].measured, super::Measured::Shaped(_)),
            "unrelated plain grid stays measured"
        );
    }

    #[test]
    fn completed_reference_or_tag_invalidates_only_its_entire_grid() {
        let state = crate::typeahead::tests::state_of(TABLES);
        let projection = markraft_core::projection::projection_of(&state);
        let types = DocTypes::from_schema_names(state.schema(), &commonmark_doc_type_names());
        let equations =
            markraft_core::kind::equations::EquationIndex::build(&projection, &types, false);
        let dependent = projection
            .lines()
            .iter()
            .enumerate()
            .find(|(index, _)| projection.line_text(*index).unwrap().contains("\\ref{eq}"))
            .unwrap()
            .0;
        let table = types
            .table_cell_of(&projection.lines()[dependent])
            .unwrap()
            .0;
        let mut lines = Lines::default();
        for source in ["{A}", "(T)"] {
            sync_state(&state, &mut lines, true);
            lines.release();
            lines.forget_math(
                &types,
                Some(&equations),
                &[crate::math::MathRequest::new(
                    source,
                    false,
                    14.,
                    1.,
                    gpui::black(),
                )],
            );
            for (index, line) in projection.lines().iter().enumerate() {
                let affected = types
                    .table_cell_of(line)
                    .is_some_and(|(at, _, _)| at == table);
                assert_eq!(
                    matches!(lines.slots[index].measured, super::Measured::Estimated),
                    affected,
                    "request {source}, line {index}"
                );
            }
        }
    }

    #[test]
    fn numbering_preference_invalidates_math_grids_but_preserves_plain_grids() {
        let state = crate::typeahead::tests::state_of(TABLES);
        let projection = markraft_core::projection::projection_of(&state);
        let types = DocTypes::from_schema_names(state.schema(), &commonmark_doc_type_names());
        let math_tables: HashSet<_> = projection
            .lines()
            .iter()
            .filter(|line| super::has_math(&types, line))
            .filter_map(|line| types.table_cell_of(line).map(|(table, _, _)| table))
            .collect();
        let mut lines = Lines::default();
        sync_state(&state, &mut lines, true);
        lines.release();
        lines.forget_all_math(&types);
        for (index, line) in projection.lines().iter().enumerate() {
            let affected = super::has_math(&types, line)
                || types
                    .table_cell_of(line)
                    .is_some_and(|(table, _, _)| math_tables.contains(&table));
            assert_eq!(
                matches!(lines.slots[index].measured, super::Measured::Estimated),
                affected,
                "line {index}"
            );
        }
    }

    #[test]
    fn more_formulas_than_the_raster_cache_settle_without_idle_render_churn() {
        const FORMULAS: usize = 240;
        let source = (0..FORMULAS)
            .map(|index| format!("$$x_{{{index}}}$$"))
            .collect::<Vec<_>>()
            .join("\n\n");
        let schema = commonmark_schema();
        let doc = from_markdown(&schema, &source).unwrap();
        let projection = Arc::new(Projection::of(&doc, &schema));
        let types = DocTypes::from_schema_names(&schema, &commonmark_doc_type_names());
        let maths = crate::maths::Maths::default();
        let images = crate::images::Images::default();
        let style = EditorStyle::notes();
        let input = ShapeInput {
            images: &images,
            maths: Some(&maths),
            equations: None,
            scale_factor: 1.,
            doc: &doc,
            types: &types,
            projection: &projection,
            style: &style,
            single_line: false,
            wiki: None,
            spelling: None,
            selection: 0..0,
            composition: None,
        };
        let text_system =
            WindowTextSystem::new(Arc::new(TextSystem::new(Arc::new(NoopTextSystem::new()))));
        let mut lines = Lines::default();
        let mut rendered = HashSet::new();
        lines.sync(&input, &projection, px(400.), 0);
        for _ in 0..FORMULAS {
            lines.measure_some(&input, 0, Duration::from_secs(1), &text_system);
            let requests = maths.take_requests();
            if requests.is_empty() {
                break;
            }
            for request in &requests {
                assert!(
                    rendered.insert(request.clone()),
                    "idle layout must not requeue an evicted formula"
                );
            }
            let results = requests
                .iter()
                .map(|request| (request.clone(), crate::math::render_math(request)))
                .collect();
            maths.finish(results);
            lines.forget_math(&types, None, &requests);
            lines.sync(&input, &projection, px(400.), 0);
        }
        assert_eq!(rendered.len(), FORMULAS);
        assert_eq!(lines.estimated(), 0);
        assert!(maths.take_requests().is_empty());
        for _ in 0..3 {
            lines.sync(&input, &projection, px(400.), 0);
            assert!(!lines.measure_some(&input, 0, Duration::from_secs(1), &text_system));
            assert!(maths.take_requests().is_empty());
        }
    }
}
