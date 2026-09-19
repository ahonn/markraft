//! AccessKit text coordinates are selectable units, not UTF-16 offsets.
use crate::surface::LayoutLine;
use gpui::{A11ySubtreeBuilder, Role, accesskit};
use markraft_core::projection::Projection;
use markraft_core::{EditorState, Selection};
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
    /// Document positions for each selectable unit boundary in `offsets`.
    positions: Vec<usize>,
    bounds: accesskit::Rect,
    /// The run's row and column, when it sits in a table cell.
    ///
    /// Announced on the run itself rather than through a `Role::Cell`
    /// container: the subtree builder appends leaves under the editor's own
    /// node and has no way to nest one, and a container between the editor and
    /// its runs would break the flat run sequence its text selection is
    /// expressed in.
    cell: Option<(usize, usize)>,
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
                let offsets = character_offsets(&value);
                let positions = offsets
                    .iter()
                    .map(|&byte| {
                        let offset = (inner.start + value[..byte].chars().count()).min(inner.end);
                        line.offset_to_pos(offset)
                            .expect("a visible offset inside the line")
                    })
                    .collect();
                self.runs.push(TextRun {
                    node_id: None,
                    from: if inner.start == 0 {
                        line.from
                    } else {
                        line.offset_to_pos(inner.start)
                            .expect("a row starts in its line")
                    },
                    content_end: if inner.end == line.len() {
                        line.to
                    } else {
                        line.offset_to_pos(inner.end)
                            .expect("a row ends in its line")
                    },
                    offsets,
                    positions,
                    text: value,
                    bounds: accesskit::Rect {
                        x0: f64::from(x),
                        y0: f64::from(y),
                        x1: f64::from(x + f32::from(row.width) * scale),
                        y1: f64::from(y + f32::from(row.line_height) * scale),
                    },
                    cell: row.table.map(|cell| (cell.row, cell.column)),
                });
            }
        }
    }

    pub(crate) fn write(&mut self, builder: &mut A11ySubtreeBuilder) {
        for (index, run) in self.runs.iter_mut().enumerate() {
            // A row that stands in for content with no position of its own —
            // an empty line, a divider — shares its document position with the
            // row beside it. Each displayed row still needs its own AccessKit
            // ID, so the index is part of the key.
            let id = builder.synthetic_node_id(("text", run.from, index));
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
            if let Some((row, column)) = run.cell {
                node.set_row_index(row);
                node.set_column_index(column);
            }
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
        let index = run.positions.partition_point(|&boundary| boundary < pos);
        let character_index = if run.positions.get(index) == Some(&pos) {
            index
        } else {
            index.saturating_sub(1)
        };
        Some(accesskit::TextPosition {
            node: run.node_id?,
            character_index,
        })
    }

    fn position(&self, position: accesskit::TextPosition) -> Option<usize> {
        let run = self
            .runs
            .iter()
            .find(|run| run.node_id == Some(position.node))?;
        run.positions.get(position.character_index).copied()
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
    fn accessible_positions_skip_hidden_inline_boundaries() {
        let state = crate::typeahead::tests::state_of("*你 **好** é*");
        let projection = markraft_core::projection::projection_of(&state);
        let line = &projection.lines()[0];
        let value = projection.line_text(0).unwrap();
        let offsets = character_offsets(value);
        let positions: Vec<_> = offsets
            .iter()
            .map(|&byte| line.offset_to_pos(value[..byte].chars().count()).unwrap())
            .collect();
        let text = AccessibleText {
            runs: vec![TextRun {
                node_id: Some(accesskit::NodeId(1)),
                from: line.from,
                content_end: line.to,
                text: value.into(),
                offsets,
                positions: positions.clone(),
                bounds: accesskit::Rect::ZERO,
                cell: None,
            }],
            selection: (line.from, line.to),
        };
        for (index, pos) in positions.into_iter().enumerate() {
            let accessible = text.text_position(pos).unwrap();
            assert_eq!(accessible.character_index, index);
            assert_eq!(text.position(accessible), Some(pos));
        }
        assert_eq!(text.text_position(line.from).unwrap().character_index, 0);
    }

    #[test]
    fn accessible_positions_roundtrip_wrapped_lines_and_block_boundaries() {
        let run = |id, from, end, text: &str| TextRun {
            node_id: Some(accesskit::NodeId(id)),
            from,
            content_end: end,
            text: text.into(),
            offsets: character_offsets(text),
            positions: character_offsets(text)
                .into_iter()
                .map(|byte| (from + text[..byte].chars().count()).min(end))
                .collect(),
            bounds: accesskit::Rect::ZERO,
            cell: None,
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
