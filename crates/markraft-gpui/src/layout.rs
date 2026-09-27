//! Which lines a frame lays out, and where it puts them.
//!
//! The view shapes only what it shows: the lines the viewport covers, a
//! viewport's worth either side of them so a scroll lands on shaped lines, and
//! the lines the caret, the selection's two ends and an input method's marked
//! text stand on, wherever those are. Everything else is a height; see
//! [`crate::surface::Lines`].
//!
//! Heights below the viewport can change without anything on screen moving.
//! Heights above it cannot: a line shaped taller than its estimate pushes
//! everything under it down. So the frame keeps the line at the top of the
//! viewport where it was, moving the scroll offset by whatever the lines above
//! it changed by, before the scroll container lays its content out.
//!
//! A key that moves the caret further than what a frame laid out — a vertical
//! move several rows long, or one from a caret the frame never showed — lays
//! out the lines it needs first, through the text system the last frame used.

use crate::EditorView;
use crate::surface::LayoutLine;
use gpui::{Bounds, Pixels, Point, Window, point, px};
use std::ops::Range;

/// How much further than the viewport a frame lays out, above and below it,
/// at the least.
const OVERSCAN: Pixels = px(400.);

/// How long a frame spends measuring lines whose heights are estimates.
const MEASURING_BUDGET: std::time::Duration = std::time::Duration::from_millis(2);

impl EditorView {
    /// Lay out what a frame at `width` shows, and say how tall the document is.
    ///
    /// `visible` is the part of the document the viewport shows, in the
    /// document's own y. Measuring passes predict it from where the last frame
    /// stood and how far the scroll moved since, and then keep the line at the
    /// viewport's top in place; a drawing pass knows it exactly and only makes
    /// sure it is laid out.
    ///
    /// A measuring pass also measures a few lines whose heights are still
    /// estimates, before it keeps the viewport's top line in place, and says
    /// with the height whether any is left, so the caller asks for another
    /// frame.
    pub(crate) fn lay_out(
        &self,
        width: Pixels,
        visible: Option<Range<Pixels>>,
        keep_anchor: bool,
        window: &mut Window,
    ) -> (Pixels, bool) {
        let text_system = window.text_system().clone();
        *self.text_system.borrow_mut() = Some(text_system.clone());
        let input = self.shape_input();
        let shaping = self.shaping();
        let mut lines = shaping.lines();
        let visible = visible
            .or_else(|| self.predicted_visible())
            .unwrap_or_else(|| px(0.)..window.viewport_size().height);
        // The line at the viewport's top, and how far into it the viewport
        // starts as a share of its height, read before a new width or style
        // sets every line back to an estimate: that line is the one the frame
        // keeps in place, whatever the lines above it come to measure.
        let held = lines
            .holds(self.projection_arc())
            .then(|| anchor_at(&mut lines, &visible));
        lines.sync(&input, self.projection_arc(), width, shaping.revision());
        let count = lines.len();
        if self.single_line || count == 0 {
            lines.lay_out_range(&input, 0..count, &text_system);
            lines.set_shown(std::iter::once(0..count).collect());
            return (lines.total(), false);
        }
        let (anchor, share) = held.unwrap_or_else(|| anchor_at(&mut lines, &visible));
        let overscan = (visible.end - visible.start).max(OVERSCAN);
        let around = lines.index_at((visible.start - overscan).max(px(0.)))
            ..lines.index_at(visible.end + overscan) + 1;
        let mut wanted = Vec::from([around, anchor..anchor + 1]);
        for pos in self.anchored_positions() {
            if let Some(index) = self.analysis.projection().line_at(pos) {
                wanted.push(index.saturating_sub(1)..(index + 2).min(count));
            }
        }
        let wanted = merged(wanted);
        lines.lay_out(&input, &wanted, &text_system);
        if let Some(limit) = self.exact_height {
            // The host sizes its window to the note up to `limit`: the lines up
            // to there are measured, so the size it reads is not an estimate
            // that moves as the note is scrolled.
            let mut index = 0;
            while index < count && lines.top(index) < limit {
                lines.lay_out_range(&input, index..index + 1, &text_system);
                index += 1;
            }
        }
        lines.set_shown(wanted);
        // Lines not yet shaped are only estimates; a few more are measured
        // every frame until none is left, so the note's length settles without
        // any one frame paying for all of it.
        let unmeasured =
            keep_anchor && lines.measure_some(&input, anchor, MEASURING_BUDGET, &text_system);
        if keep_anchor && visible.start > px(0.) {
            let height = lines.top(anchor + 1) - lines.top(anchor);
            let moved = lines.top(anchor) + height * share - visible.start;
            if moved != px(0.) {
                let offset = self.scroll.offset();
                self.scroll.set_offset(point(offset.x, offset.y - moved));
            }
        }
        lines.keep_shown_rows();
        (lines.total(), unmeasured)
    }

    /// The part of the document the viewport shows now, from where the last
    /// frame placed it and how far the scroll moved since. `None` before the
    /// first frame.
    fn predicted_visible(&self) -> Option<Range<Pixels>> {
        let (top, scroll_y) = self.frame.placed()?;
        let viewport = self.scroll.bounds();
        if viewport.size.height <= px(0.) {
            return None;
        }
        let top_now = top + (self.scroll.offset().y - scroll_y);
        let start = (viewport.top() - top_now).max(px(0.));
        Some(start..start + viewport.size.height)
    }

    /// The positions a frame lays out whether or not they are in view: the
    /// selection's ends and the input method's marked text, which every key,
    /// every popup and the input method measure from.
    fn anchored_positions(&self) -> Vec<usize> {
        let doc = self.state.doc();
        let selection = self.state.selection();
        let mut out = vec![selection.head(doc), selection.anchor(doc)];
        if let Some(range) = markraft_core::composition::composition_range(&self.state) {
            out.extend([range.from, range.to]);
        }
        out
    }

    /// The shaped lines the frame shows, placed in `bounds`, the text column
    /// in the window.
    pub(crate) fn placed_lines(&self, bounds: Bounds<Pixels>) -> Vec<LayoutLine> {
        self.shaping()
            .lines()
            .shown()
            .into_iter()
            .map(|(top, mut line)| {
                line.origin += point(bounds.left(), bounds.top() + top + line.top_gap);
                line
            })
            .collect()
    }

    /// Lay out the `each_side` lines either side of the one holding `pos`, and
    /// add them to the frame, so a key can move over lines the frame did not
    /// show. Nothing happens before the first frame.
    pub(crate) fn lay_out_near(&mut self, pos: usize, each_side: usize) {
        if let Some(index) = self.analysis.projection().line_at(pos) {
            self.lay_out_lines(index.saturating_sub(each_side)..index + each_side + 1);
        }
    }

    /// Lay out the line under `point`, in the window, and add it to the frame.
    pub(crate) fn lay_out_at(&mut self, target: Point<Pixels>) {
        let Some((top, scroll_y)) = self.frame.placed() else {
            return;
        };
        let top_now = top + (self.scroll.offset().y - scroll_y);
        let y = (target.y - top_now).max(px(0.));
        let index = self.shaping().lines().index_at(y);
        self.lay_out_lines(index.saturating_sub(1)..index + 2);
    }

    fn lay_out_lines(&mut self, range: Range<usize>) {
        let Some(text_system) = self.text_system.borrow().clone() else {
            return;
        };
        let Some((top, scroll_y)) = self.frame.placed() else {
            return;
        };
        {
            let input = self.shape_input();
            let shaping = self.shaping();
            let mut lines = shaping.lines();
            let width = self.frame.content_bounds().size.width;
            lines.sync(&input, self.projection_arc(), width, shaping.revision());
            let range = range.start..range.end.min(lines.len());
            if range.clone().all(|index| lines.is_laid(index)) {
                return;
            }
            lines.lay_out_range(&input, range.clone(), &text_system);
            let mut shown = lines.shown_ranges().to_vec();
            shown.push(range);
            lines.set_shown(merged(shown));
        }
        let mut bounds = self.frame.content_bounds();
        bounds.origin.y = top + (self.scroll.offset().y - scroll_y);
        let rows = self.placed_lines(bounds);
        self.frame.replace_rows(rows);
    }
}

/// The line `visible` starts in, and how far into it it starts as a share of
/// the line's height.
fn anchor_at(lines: &mut crate::surface::Lines, visible: &Range<Pixels>) -> (usize, f32) {
    let anchor = lines.index_at(visible.start);
    let top = lines.top(anchor);
    let height = lines.top(anchor + 1) - top;
    let share = if height > px(0.) {
        ((visible.start - top) / height).clamp(0., 1.)
    } else {
        0.
    };
    (anchor, share)
}

/// `ranges` sorted, with overlapping and touching ones joined.
pub(crate) fn merged(mut ranges: Vec<Range<usize>>) -> Vec<Range<usize>> {
    ranges.retain(|range| !range.is_empty());
    ranges.sort_by_key(|range| range.start);
    let mut out: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
    for range in ranges {
        match out.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => out.push(range),
        }
    }
    out
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::merged;
    use crate::{DocTypes, EditorView, Setup};
    use gpui::{Entity, TestAppContext, VisualTestContext};
    use markraft_commonmark::{
        commonmark_doc_type_names, commonmark_extensions, commonmark_schema, from_markdown,
    };
    use markraft_core::{Selection, TransactionSpec};

    /// A note far taller than any window: `count` short paragraphs.
    fn long_note(count: usize) -> String {
        (0..count)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    fn laid_out<'a>(
        cx: &'a mut TestAppContext,
        markdown: &str,
    ) -> (Entity<EditorView>, &'a mut VisualTestContext) {
        let schema = commonmark_schema();
        let doc = from_markdown(&schema, markdown).expect("valid Markdown");
        let setup = Setup::new(schema.clone())
            .types(DocTypes::from_schema_names(
                &schema,
                &commonmark_doc_type_names(),
            ))
            .extensions(commonmark_extensions(&schema))
            .doc(doc);
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new(setup, cx));
        cx.run_until_parked();
        (view, cx)
    }

    /// The projection line the caret is on.
    fn caret_line(view: &Entity<EditorView>, cx: &mut VisualTestContext) -> usize {
        view.read_with(cx, |view, _| {
            view.projection().line_at(view.head()).expect("a line")
        })
    }

    /// Put the caret at the start of line `index` without letting a frame
    /// lay the note out around it.
    fn caret_to_line(view: &mut EditorView, index: usize, cx: &mut gpui::Context<EditorView>) {
        let pos = view.projection().line(index).expect("a line").from();
        view.dispatch(
            [TransactionSpec::new().selection(Selection::cursor(pos))],
            cx,
        );
    }

    #[gpui::test]
    fn a_long_note_lays_out_only_what_the_window_shows(cx: &mut TestAppContext) {
        let (view, cx) = laid_out(cx, &long_note(3000));
        view.read_with(cx, |view, _| {
            let laid = view.frame.rows().len();
            assert!(laid > 0, "the window shows something");
            assert!(
                laid < view.projection().line_count() / 4,
                "{laid} of {} lines laid out",
                view.projection().line_count()
            );
        });
    }

    /// A key that moves the caret from a line no frame showed lays the lines
    /// it moves over out first, so ↑ goes one row up rather than reading the
    /// missing rows as the document's edge.
    #[gpui::test]
    fn a_caret_no_frame_showed_moves_one_row(cx: &mut TestAppContext) {
        let (view, cx) = laid_out(cx, &long_note(3000));
        view.update(cx, |view, cx| {
            caret_to_line(view, 2000, cx);
            view.vertical(-1, false, cx);
        });
        assert_eq!(caret_line(&view, cx), 1999);
    }

    /// A caret moved far out of view is brought into view by the next frame
    /// on its own. The frame that finds it out of view scrolls, and the move
    /// only shows in the frame after; nothing else may be coming to draw it —
    /// a caret that does not blink, as vim's Normal-mode block does not, asks
    /// for no frame of its own.
    #[gpui::test]
    fn a_caret_moved_out_of_view_is_scrolled_to_without_another_event(cx: &mut TestAppContext) {
        let (view, cx) = laid_out(cx, &long_note(300));
        // Lines still estimated ask for frames of their own, which would draw
        // the scroll whatever the reveal asks for.
        while view.read_with(cx, |view, _| view.shaping().lines().estimated()) > 0 {
            cx.update(|window, _| window.refresh());
            cx.run_until_parked();
        }
        cx.update(|window, cx| window.simulate_next_frame(cx));
        cx.run_until_parked();
        view.update(cx, |view, cx| {
            let pos = view.projection().line(250).expect("a line").from();
            view.dispatch(
                [TransactionSpec::new()
                    .selection(Selection::cursor(pos))
                    .scroll_into_view()],
                cx,
            );
        });
        cx.run_until_parked();
        // The platform's frame loop, which a test has none of.
        cx.update(|window, cx| window.simulate_next_frame(cx));
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            let (_, drawn_at) = view.frame.placed().expect("a frame was drawn");
            assert_ne!(view.scroll.offset().y, gpui::px(0.), "the view scrolled");
            assert_eq!(
                drawn_at,
                view.scroll.offset().y,
                "the last frame drawn is the scrolled one"
            );
        });
    }

    /// The lines a frame did not show are measured a few at a time until none
    /// is left to estimate.
    #[gpui::test]
    fn every_line_is_measured_in_the_end(cx: &mut TestAppContext) {
        let (view, cx) = laid_out(cx, &long_note(3000));
        for _ in 0..200 {
            let left = view.read_with(cx, |view, _| view.shaping().lines().estimated());
            if left == 0 {
                return;
            }
            cx.run_until_parked();
            cx.update(|window, _| window.refresh());
        }
        panic!("lines still estimated after 200 frames");
    }

    /// The cells before the last of a row start where it does and have no
    /// height of their own, so the line the window's top falls in is the
    /// row's last cell. A frame still draws the whole grid: with the caret
    /// in a table that opens the note, the first cell is drawn too.
    #[gpui::test]
    fn a_frame_draws_every_cell_of_a_table_it_shows(cx: &mut TestAppContext) {
        let (view, cx) = laid_out(
            cx,
            "| one | two | three |\n| --- | --- | --- |\n| a | b | c |",
        );
        view.update(cx, |view, cx| caret_to_line(view, 2, cx));
        cx.run_until_parked();
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            let drawn: Vec<(usize, usize)> = view
                .frame
                .rows()
                .iter()
                .filter_map(|line| line.table.map(|cell| (cell.row, cell.column)))
                .collect();
            assert_eq!(drawn, [(0, 0), (0, 1), (0, 2), (1, 0), (1, 1), (1, 2)]);
        });
    }

    /// ↓ inside an HTML block the caret is in moves to the block's next source
    /// row. The block above it is drawn as its page, shorter than its source,
    /// and the source rows it does not draw are no rows for the arrow to stop
    /// on: read as rows they would lie inside the block below, the arrow would
    /// find no row past the caret's, and the caret would jump to the end.
    #[gpui::test]
    fn down_in_an_html_block_goes_to_its_next_source_row(cx: &mut TestAppContext) {
        let source = concat!(
            "<p align=\"center\">\n",
            "  <img src=\"missing-1.png\" alt=\"one\">\n",
            "  <img src=\"missing-2.png\" alt=\"two\">\n",
            "  <img src=\"missing-3.png\" alt=\"three\">\n",
            "</p>\n\n",
            "<p align=\"center\">\n  x<br>\n  y\n</p>\n\n",
            "end"
        );
        /// The CommonMark spelling, which is what renders an HTML block.
        struct Spelled(std::sync::Arc<dyn markraft_core::kind::SourceSpelling>);
        impl markraft_core::kind::DocumentKind for Spelled {
            fn spelling(&self) -> Option<std::sync::Arc<dyn markraft_core::kind::SourceSpelling>> {
                Some(self.0.clone())
            }
        }
        let schema = commonmark_schema();
        let doc = from_markdown(&schema, source).expect("valid Markdown");
        let setup = Setup::new(schema.clone())
            .types(DocTypes::from_schema_names(
                &schema,
                &commonmark_doc_type_names(),
            ))
            .extensions(commonmark_extensions(&schema))
            .kind(std::sync::Arc::new(Spelled(std::sync::Arc::new(
                markraft_commonmark::CommonMarkSpelling::new(schema.clone()),
            ))))
            .doc(doc);
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new(setup, cx));
        cx.run_until_parked();
        view.update(cx, |view, cx| caret_to_line(view, 1, cx));
        cx.run_until_parked();
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        let before = view.read_with(cx, |view, _| view.head());
        view.update(cx, |view, cx| view.vertical(1, false, cx));
        let after = view.read_with(cx, |view, _| view.head());
        assert_eq!(caret_line(&view, cx), 1, "still in the HTML block");
        assert!(after > before, "one source row further on");
    }

    /// An animated picture plays while the pointer rests on it and nowhere
    /// else, and never once the host turns playing off.
    #[gpui::test]
    fn an_animated_picture_plays_only_under_the_pointer(cx: &mut TestAppContext) {
        let directory = std::env::temp_dir().join(format!(
            "markraft-animation-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("clip.gif"),
            crate::animation::tests::gif(4, 40, 50),
        )
        .unwrap();
        let (view, cx) = laid_out(cx, "Above\n\n![clip](clip.gif)\n\nBelow");
        view.update(cx, |view, cx| {
            view.set_image_base(Some(directory.clone()), cx)
        });
        cx.run_until_parked();
        let (on, off) = view.read_with(cx, |view, _| {
            let picture = view
                .frame
                .rows()
                .iter()
                .flat_map(|row| row.pictures().map(|(bounds, _)| bounds))
                .next()
                .expect("the picture is drawn");
            (
                picture.center(),
                picture.bottom_right() + gpui::point(gpui::px(200.), gpui::px(0.)),
            )
        });
        let playing = |cx: &mut VisualTestContext| {
            view.read_with(cx, |view, _| view.player.playing().is_some())
        };
        assert!(!playing(cx), "a picture shows its first frame");

        cx.simulate_mouse_move(on, None, gpui::Modifiers::default());
        assert!(playing(cx));
        // Frames drawn while it plays keep it playing.
        for _ in 0..4 {
            cx.executor()
                .advance_clock(std::time::Duration::from_millis(60));
            cx.run_until_parked();
            cx.update(|window, _| window.refresh());
            cx.run_until_parked();
        }
        assert!(playing(cx));
        cx.simulate_mouse_move(off, None, gpui::Modifiers::default());
        assert!(!playing(cx), "the pointer left it");

        cx.simulate_mouse_move(on, None, gpui::Modifiers::default());
        assert!(playing(cx));
        view.update(cx, |view, cx| view.set_animate_images(false, cx));
        assert!(!playing(cx), "turning playing off stops it");
        cx.simulate_mouse_move(off, None, gpui::Modifiers::default());
        cx.simulate_mouse_move(on, None, gpui::Modifiers::default());
        assert!(!playing(cx), "and keeps it still");
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn ranges_are_sorted_and_joined() {
        assert_eq!(
            merged(vec![5..8, 0..2, 1..3, 8..9, 12..12]),
            vec![0..3, 5..9]
        );
        assert_eq!(merged(Vec::new()), Vec::<std::ops::Range<usize>>::new());
    }
}
