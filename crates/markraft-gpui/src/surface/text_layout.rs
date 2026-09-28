//! The adapter between GPUI text shaping and independently measured inline objects.

use gpui::{
    LineLayout, Pixels, ShapedLine, TextRun, WindowTextSystem, WrappedLine, WrappedLineLayout,
};
use std::{collections::HashMap, ops::Range, sync::Arc};

/// Replace each object replacement character's glyph advance with its measured width.
///
/// Byte indices and decorations remain unchanged. The caller must recompute wraps
/// after reserving widths. A missing or ambiguous object glyph returns `false`
/// without modifying the layout, allowing source fallback.
pub(super) fn reserve_inline_widths(
    line: &mut WrappedLine,
    reservations: &[(Range<usize>, Pixels)],
) -> bool {
    if reservations.is_empty() {
        return true;
    }
    let layout = &line.unwrapped_layout;
    let mut glyphs: Vec<_> = layout
        .runs
        .iter()
        .enumerate()
        .flat_map(|(run, shaped)| {
            shaped
                .glyphs
                .iter()
                .enumerate()
                .map(move |(at, glyph)| (run, at, glyph))
        })
        .collect();
    if glyphs
        .iter()
        .any(|(_, _, glyph)| !f32::from(glyph.position.x).is_finite())
    {
        return false;
    }
    // Source indices need not increase in visual order (for example in RTL
    // text). Measure advances visually, but retain GPUI's original run order.
    glyphs.sort_by(|a, b| f32::from(a.2.position.x).total_cmp(&f32::from(b.2.position.x)));
    let mut by_index = HashMap::with_capacity(glyphs.len());
    for (visual, (_, _, glyph)) in glyphs.iter().enumerate() {
        by_index
            .entry(glyph.index)
            .and_modify(|rank| *rank = None)
            .or_insert(Some(visual));
    }

    let mut changes = Vec::with_capacity(reservations.len());
    let mut previous_end = 0;
    for (range, width) in reservations {
        if range.start < previous_end
            || line.text.get(range.clone()) != Some("\u{fffc}")
            || !f32::from(*width).is_finite()
            || *width < Pixels::ZERO
        {
            return false;
        }
        previous_end = range.end;
        let Some(&Some(at)) = by_index.get(&range.start) else {
            return false;
        };
        let glyph = glyphs[at].2;
        let next = glyphs.get(at + 1);
        let end = next.map_or(layout.width, |next| next.2.position.x);
        let advance = end - glyph.position.x;
        if advance < Pixels::ZERO {
            return false;
        }
        changes.push((at + 1, *width - advance));
    }

    changes.sort_by_key(|(visual, _)| *visual);
    let adjusted_width = layout.width + changes.iter().map(|(_, delta)| *delta).sum::<Pixels>();
    if !f32::from(adjusted_width).is_finite() {
        return false;
    }
    let mut runs = layout.runs.clone();
    let mut changes = changes.into_iter().peekable();
    let mut shift = Pixels::ZERO;
    for (visual, (run, at, _)) in glyphs.iter().enumerate() {
        while changes.peek().is_some_and(|(end, _)| *end <= visual) {
            shift += changes.next().expect("a pending width change exists").1;
        }
        runs[*run].glyphs[*at].position.x += shift;
    }
    let adjusted = Arc::new(LineLayout {
        font_size: layout.font_size,
        width: adjusted_width,
        ascent: layout.ascent,
        descent: layout.descent,
        runs,
        len: layout.len,
    });
    **line = Arc::new(WrappedLineLayout {
        unwrapped_layout: adjusted,
        wrap_boundaries: line.wrap_boundaries.clone(),
        wrap_width: line.wrap_width,
    });
    true
}

/// Copy the styles intersecting a byte range of the original text.
///
/// Returned lengths are relative to the slice, so a logical row can skip its
/// preceding newline without carrying that byte into its decoration runs.
pub(super) fn slice_runs(runs: &[TextRun], range: Range<usize>) -> Vec<TextRun> {
    let mut offset = 0;
    let mut sliced = Vec::new();
    for run in runs {
        let end = offset + run.len;
        let from = offset.max(range.start);
        let to = end.min(range.end);
        if from < to {
            sliced.push(TextRun {
                len: to - from,
                ..run.clone()
            });
        }
        offset = end;
        if offset >= range.end {
            break;
        }
    }
    sliced
}

/// Split the final wrapped layout into independently paintable visual rows.
///
/// Shape once to obtain GPUI's decorated-line wrapper, then reuse the exact
/// original glyph layout, including object advances. The forward cursor copies
/// each ordered glyph once and never reshapes text at a soft-wrap boundary.
pub(super) fn paint_rows(
    line: &WrappedLine,
    font_size: Pixels,
    runs: &[TextRun],
    text_system: &WindowTextSystem,
) -> Vec<ShapedLine> {
    let mut decorated = text_system.shape_line(line.text.clone(), font_size, runs, None);
    *decorated = line.unwrapped_layout.clone();
    let mut cursor = decorated.cursor();
    line.wrap_boundaries
        .iter()
        .map(|boundary| line.runs()[boundary.run_ix].glyphs[boundary.glyph_ix].index)
        .chain([line.text.len()])
        .map(|end| cursor.take_until(end))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{NoopTextSystem, TextSystem, WrapBoundary, black, font, px};

    fn text_system() -> WindowTextSystem {
        WindowTextSystem::new(Arc::new(TextSystem::new(Arc::new(NoopTextSystem::new()))))
    }

    fn run(len: usize, family: &str) -> TextRun {
        TextRun {
            len,
            font: font(family.to_owned()),
            color: black(),
            background_color: None,
            underline: None,
            strikethrough: None,
        }
    }

    fn shape(text: &str, runs: &[TextRun], system: &WindowTextSystem) -> WrappedLine {
        system
            .shape_text(text.to_owned().into(), px(16.), runs, None, None)
            .unwrap()
            .remove(0)
    }

    fn assert_pixels(actual: Pixels, expected: Pixels) {
        assert!(
            f32::from(actual - expected).abs() < 0.001,
            "expected {expected:?}, got {actual:?}"
        );
    }

    #[test]
    fn sliced_runs_keep_styles_and_only_the_intersecting_bytes() {
        let mut middle = run(5, "Courier");
        middle.background_color = Some(black());
        let runs = [run(3, "Arial"), middle, run(2, "Helvetica")];
        let sliced = slice_runs(&runs, 2..9);
        assert_eq!(
            sliced.iter().map(|run| run.len).collect::<Vec<_>>(),
            [1, 5, 1]
        );
        for (actual, expected) in sliced.iter().zip(&runs) {
            assert_eq!(actual.font, expected.font);
            assert_eq!(actual.background_color, expected.background_color);
        }
        assert!(slice_runs(&runs, 3..3).is_empty());
        assert!(slice_runs(&runs, 10..12).is_empty());
    }

    #[test]
    fn newline_row_slices_use_bytes_for_multibyte_text() {
        let system = text_system();
        let text = "a\néxy\nz";
        let runs = [run(4, "Arial"), run(text.len() - 4, "Courier")];
        let sliced = slice_runs(&runs, 2..6);
        assert_eq!(sliced.iter().map(|run| run.len).collect::<Vec<_>>(), [2, 2]);
        let line = shape("éxy", &sliced, &system);
        let painted = paint_rows(&line, px(16.), &sliced, &system);
        assert_eq!(painted[0].text.as_ref(), "éxy");
        assert_eq!(sliced.iter().map(|run| run.len).sum::<usize>(), "éxy".len());
    }

    #[test]
    fn exact_object_width_preserves_source_indices_and_surrounding_advances() {
        let system = text_system();
        let text = "a\u{fffc}z";
        let mut line = shape(text, &[run(text.len(), "Arial")], &system);
        let original_width = line.unwrapped_layout.width;
        let left = line.unwrapped_layout.x_for_index(1);
        let original_advance = line.unwrapped_layout.x_for_index(4) - left;
        let indices: Vec<_> = line
            .runs()
            .iter()
            .flat_map(|r| &r.glyphs)
            .map(|g| g.index)
            .collect();
        assert!(reserve_inline_widths(&mut line, &[(1..4, px(37.25))]));
        assert_eq!(line.unwrapped_layout.x_for_index(1), left);
        assert_eq!(line.unwrapped_layout.x_for_index(4) - left, px(37.25));
        assert_eq!(
            line.unwrapped_layout.width,
            original_width - original_advance + px(37.25)
        );
        assert_eq!(
            line.runs()
                .iter()
                .flat_map(|r| &r.glyphs)
                .map(|g| g.index)
                .collect::<Vec<_>>(),
            indices
        );
        assert_eq!(line.text.as_ref(), text);
    }

    #[test]
    fn adjacent_objects_across_font_runs_keep_independent_widths() {
        let system = text_system();
        let text = "a\u{fffc}\u{fffc}z";
        let runs = [run(4, "Arial"), run(4, "Courier")];
        let mut line = shape(text, &runs, &system);
        assert!(reserve_inline_widths(
            &mut line,
            &[(1..4, px(21.5)), (4..7, px(9.25))]
        ));
        let x = |index| line.unwrapped_layout.x_for_index(index);
        assert_pixels(x(4) - x(1), px(21.5));
        assert_pixels(x(7) - x(4), px(9.25));
    }

    #[test]
    fn final_object_updates_the_total_width() {
        let system = text_system();
        let text = "x\u{fffc}";
        let mut line = shape(text, &[run(text.len(), "Arial")], &system);
        let left = line.unwrapped_layout.x_for_index(1);
        assert!(reserve_inline_widths(&mut line, &[(1..4, px(100.))]));
        assert_pixels(line.unwrapped_layout.width, left + px(100.));
    }

    #[test]
    fn reordered_source_indices_reserve_width_in_visual_order() {
        let system = text_system();
        let text = "a\u{fffc}z";
        let mut line = shape(text, &[run(text.len(), "Arial")], &system);
        let original = line.unwrapped_layout.clone();
        let mut runs = original.runs.clone();
        let glyphs = &mut runs[0].glyphs;
        glyphs[0].index = 1;
        glyphs[1].index = 4;
        glyphs[2].index = 0;
        let last_advance = glyphs[2].position.x - glyphs[1].position.x;
        *line = Arc::new(WrappedLineLayout {
            unwrapped_layout: Arc::new(LineLayout {
                font_size: original.font_size,
                width: original.width,
                ascent: original.ascent,
                descent: original.descent,
                runs,
                len: original.len,
            }),
            wrap_boundaries: Default::default(),
            wrap_width: None,
        });
        assert!(reserve_inline_widths(&mut line, &[(1..4, px(41.))]));
        let glyphs = &line.runs()[0].glyphs;
        assert_pixels(glyphs[1].position.x - glyphs[0].position.x, px(41.));
        assert_pixels(glyphs[2].position.x - glyphs[1].position.x, last_advance);
        assert_eq!(
            glyphs.iter().map(|glyph| glyph.index).collect::<Vec<_>>(),
            [1, 4, 0]
        );
    }

    #[test]
    fn invalid_reservations_leave_the_original_layout_intact() {
        let system = text_system();
        let text = "a\u{fffc}b";
        let mut line = shape(text, &[run(text.len(), "Arial")], &system);
        let original = line.unwrapped_layout.clone();
        for reservations in [
            vec![(0..1, px(30.))],
            vec![(1..4, px(-1.))],
            vec![(1..4, px(f32::NAN))],
            vec![(1..4, px(30.)), (1..4, px(40.))],
        ] {
            assert!(!reserve_inline_widths(&mut line, &reservations));
            assert!(Arc::ptr_eq(&original, &line.unwrapped_layout));
        }
    }

    #[test]
    fn paint_rows_preserve_adjusted_advances_and_rebase_byte_indices() {
        let system = text_system();
        let text = "a\u{fffc}z";
        let runs = [run(text.len(), "Arial")];
        let mut line = shape(text, &runs, &system);
        assert!(reserve_inline_widths(&mut line, &[(1..4, px(33.))]));
        *line = Arc::new(WrappedLineLayout {
            unwrapped_layout: line.unwrapped_layout.clone(),
            wrap_boundaries: [WrapBoundary {
                run_ix: 0,
                glyph_ix: 2,
            }]
            .into_iter()
            .collect(),
            wrap_width: Some(px(60.)),
        });
        let rows = paint_rows(&line, px(16.), &runs, &system);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].text.as_ref(), "a\u{fffc}");
        assert_eq!(rows[1].text.as_ref(), "z");
        assert_eq!(rows[0].width - rows[0].x_for_index(1), px(33.));
        assert_eq!(
            rows.iter().map(|row| row.width).sum::<Pixels>(),
            line.unwrapped_layout.width
        );
        assert_eq!(rows[1].runs[0].glyphs[0].index, 0);
        assert_eq!(rows[1].runs[0].glyphs[0].position.x, Pixels::ZERO);
    }
}
