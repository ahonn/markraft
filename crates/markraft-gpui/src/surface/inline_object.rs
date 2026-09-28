//! Geometry shared by inline renderers. Content and source spelling belong to
//! the caller; layout only needs a display position, advance and alignment.

use super::*;

pub(super) const OBJECT: char = '\u{fffc}';

#[derive(Clone, Copy, Debug)]
pub(super) enum InlineAlignment {
    Baseline { ascent: Pixels, descent: Pixels },
    Center { height: Pixels },
}

#[derive(Clone, Debug)]
pub(super) struct InlineObject {
    pub display: Range<usize>,
    pub width: Pixels,
    pub alignment: InlineAlignment,
}

/// All coordinates are relative to the paragraph's text origin. `text_top`
/// places the default text band; objects can extend above and below it.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct VisualRow {
    pub top: Pixels,
    pub height: Pixels,
    pub baseline: Pixels,
    pub text_top: Pixels,
}

/// Derive visual-row geometry once, for painting and every position query.
pub(super) fn measure_rows(
    rows: &[LayoutRow],
    objects: &[InlineObject],
    text_height: Pixels,
) -> Vec<VisualRow> {
    let mut result = Vec::new();
    let mut top = px(0.);
    for row in rows {
        let baseline =
            (text_height - row.line.ascent() - row.line.descent()) / 2. + row.line.ascent();
        let starts = row.wrap_starts();
        for (visual, &start) in starts.iter().enumerate() {
            let end = starts.get(visual + 1).copied().unwrap_or(row.text().len());
            let from = row.char_start + byte_to_char(row.text(), start);
            let to = row.char_start + byte_to_char(row.text(), end);
            let mut above = baseline;
            let mut below = text_height - baseline;
            let mut centered = text_height;
            for object in objects
                .iter()
                .filter(|object| from <= object.display.start && object.display.start < to)
            {
                match object.alignment {
                    InlineAlignment::Baseline { ascent, descent } => {
                        above = above.max(ascent);
                        below = below.max(descent);
                    }
                    InlineAlignment::Center { height } => centered = centered.max(height),
                }
            }
            let extra = (centered - above - below).max(px(0.)) / 2.;
            above += extra;
            below += extra;
            result.push(VisualRow {
                top,
                height: above + below,
                baseline: top + above,
                text_top: top + above - baseline,
            });
            top += above + below;
        }
    }
    result
}
