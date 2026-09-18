//! AccessKit text coordinates are selectable units, not UTF-16 offsets.
use crate::surface::LayoutLine;
use gpui::{A11ySubtreeBuilder, Role, accesskit};
use markraft_doc::projection::Projection;
use markraft_doc::{EditorState, Selection};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Default)]
pub(crate) struct AccessibleText {
    runs: Vec<TextRun>,
    selection: (usize, usize),
}

struct TextRun {
    node_id: Option<accesskit::NodeId>,
    /// Document position of the run's first character.
    from: usize,
    /// Document position just past the run's own content.
    content_end: usize,
    text: String,
    offsets: Vec<usize>,
    bounds: accesskit::Rect,
}

// AccessKit uses u8 for each selectable unit's UTF-8 length. An unusually long
// combining sequence cannot fit; expose its scalars and clamp incoming requests
// through the projection so editing still cannot split the grapheme.
fn character_offsets(text: &str) -> Vec<usize> {
    let mut offsets = vec![0];
    for (start, grapheme) in text.grapheme_indices(true) {
        if grapheme.len() <= usize::from(u8::MAX) {
            offsets.push(start + grapheme.len());
        } else {
            offsets.extend(
                grapheme
                    .char_indices()
                    .map(|(index, c)| start + index + c.len_utf8()),
            );
        }
    }
    offsets
}

impl AccessibleText {
    pub(crate) fn update(
        &mut self,
        projection: &Projection,
        state: &EditorState,
        rows: &[LayoutLine],
        scale: f32,
    ) {
        let doc = state.doc();
        self.selection = (state.selection().anchor(doc), state.selection().head(doc));
        self.runs.clear();
        let last_line = projection.line_count().saturating_sub(1);
        for row in rows {
            let Some(line) = projection.line(row.index) else {
                continue;
            };
            let text = projection.line_text(row.index).unwrap_or_default();
            for (visual, inner) in row.accessible_rows().into_iter().enumerate() {
                let mut value: String = text
                    .chars()
                    .skip(inner.start)
                    .take(inner.end - inner.start)
                    .collect();
                if inner.end == line.len() && row.index < last_line {
                    value.push('\n');
                }
                let x = f32::from(row.origin.x) * scale;
                let y = f32::from(row.origin.y + row.line_height * visual as f32) * scale;
                self.runs.push(TextRun {
                    node_id: None,
                    from: line.from + inner.start,
                    content_end: line.from + inner.end,
                    offsets: character_offsets(&value),
                    text: value,
                    bounds: accesskit::Rect {
                        x0: f64::from(x),
                        y0: f64::from(y),
                        x1: f64::from(x + f32::from(row.width) * scale),
                        y1: f64::from(y + f32::from(row.line_height) * scale),
                    },
                });
            }
        }
    }

    pub(crate) fn write(&mut self, builder: &mut A11ySubtreeBuilder) {
        for run in &mut self.runs {
            let id = builder.synthetic_node_id(("text", run.from));
            run.node_id = Some(id);
            let mut node = accesskit::Node::new(Role::TextRun);
            node.set_value(run.text.clone());
            node.set_character_lengths(
                run.offsets
                    .windows(2)
                    .map(|pair| (pair[1] - pair[0]) as u8)
                    .collect::<Vec<_>>(),
            );
            node.set_bounds(run.bounds);
            builder.push_child(id, node);
        }
        if let (Some(anchor), Some(focus)) = (
            self.text_position(self.selection.0),
            self.text_position(self.selection.1),
        ) {
            builder
                .parent_node()
                .set_text_selection(accesskit::TextSelection { anchor, focus });
        }
    }

    fn text_position(&self, pos: usize) -> Option<accesskit::TextPosition> {
        let run = self
            .runs
            .iter()
            .rev()
            .find(|run| run.from <= pos && pos <= run.content_end)?;
        let relative = pos - run.from;
        // One token is one `char` inside a line, and the run's text is that stretch.
        let byte = run
            .text
            .char_indices()
            .nth(relative)
            .map_or(run.text.len(), |(index, _)| index);
        Some(accesskit::TextPosition {
            node: run.node_id?,
            character_index: run
                .offsets
                .partition_point(|&offset| offset <= byte)
                .saturating_sub(1),
        })
    }

    fn position(&self, position: accesskit::TextPosition) -> Option<usize> {
        let run = self
            .runs
            .iter()
            .find(|run| run.node_id == Some(position.node))?;
        let byte = *run.offsets.get(position.character_index)?;
        let chars = run.text[..byte].chars().count();
        Some((run.from + chars).min(run.content_end))
    }

    pub(crate) fn selection(&self, selection: &accesskit::TextSelection) -> Option<Selection> {
        Some(Selection::text(
            self.position(selection.anchor)?,
            self.position(selection.focus)?,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accessible_units_preserve_graphemes_and_represent_every_byte() {
        let text = "你好 é 👨‍👩‍👧‍👦\n";
        let offsets = character_offsets(text);
        assert_eq!(offsets.len() - 1, text.graphemes(true).count());
        assert_eq!(offsets.last(), Some(&text.len()));
        let long = format!("e{}", "\u{301}".repeat(200));
        let offsets = character_offsets(&long);
        assert!(offsets.windows(2).all(|pair| pair[1] - pair[0] <= 255));
        assert_eq!(offsets.last(), Some(&long.len()));
    }

    #[test]
    fn accessible_positions_roundtrip_wrapped_lines_and_block_boundaries() {
        let run = |id, from, end, text: &str| TextRun {
            node_id: Some(accesskit::NodeId(id)),
            from,
            content_end: end,
            text: text.into(),
            offsets: character_offsets(text),
            bounds: accesskit::Rect::ZERO,
        };
        // Two visual rows of one line holding "你好", then the next block.
        let text = AccessibleText {
            runs: vec![run(1, 1, 2, "你"), run(2, 2, 3, "好\n")],
            selection: (1, 1),
        };
        let boundary = 2;
        let accessible = text.text_position(boundary).unwrap();
        assert_eq!(accessible.node, accesskit::NodeId(2));
        assert_eq!(accessible.character_index, 0);
        assert_eq!(text.position(accessible), Some(boundary));
        // The synthetic newline maps back to the end of the run's own content.
        assert_eq!(
            text.position(accesskit::TextPosition {
                node: accesskit::NodeId(2),
                character_index: 2
            }),
            Some(3)
        );
        assert!(
            text.position(accesskit::TextPosition {
                node: accesskit::NodeId(99),
                character_index: 0
            })
            .is_none()
        );
    }
}
