//! AccessKit text coordinates are selectable units, not UTF-16 offsets.
use crate::surface::LayoutBlock;
use gpui::{A11ySubtreeBuilder, Role, accesskit};
use markraft_core::{Document, Position, Selection};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Default)]
pub(crate) struct AccessibleText {
    runs: Vec<TextRun>,
    selection: Selection,
}

struct TextRun {
    node_id: Option<accesskit::NodeId>,
    block: usize,
    start: usize,
    content_end: usize,
    text: String,
    offsets: Vec<usize>,
    bounds: accesskit::Rect,
}

// AccessKit uses u8 for each selectable unit's UTF-8 length. An unusually long
// combining sequence cannot fit; expose its scalars and clamp incoming requests
// through the core so editing still cannot split the grapheme.
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
        document: &Document,
        selection: Selection,
        rows: &[LayoutBlock],
        scale: f32,
    ) {
        self.selection = selection;
        self.runs.clear();
        for (block_index, row) in rows.iter().enumerate() {
            let text = document.blocks[block_index].text();
            let mut starts = vec![0];
            starts.extend(
                row.line.wrap_boundaries().iter().map(|boundary| {
                    row.line.runs()[boundary.run_ix].glyphs[boundary.glyph_ix].index
                }),
            );
            for (line, &start) in starts.iter().enumerate() {
                let end = starts.get(line + 1).copied().unwrap_or(text.len());
                let mut value = text[start..end].to_owned();
                if line + 1 == starts.len() && block_index + 1 < document.blocks.len() {
                    value.push('\n');
                }
                let x = f32::from(row.origin.x) * scale;
                let y = f32::from(row.origin.y + row.line_height * line) * scale;
                self.runs.push(TextRun {
                    node_id: None,
                    block: block_index,
                    start,
                    content_end: end,
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
            let id = builder.synthetic_node_id(("text", run.block, run.start));
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
            self.text_position(self.selection.anchor),
            self.text_position(self.selection.head),
        ) {
            builder
                .parent_node()
                .set_text_selection(accesskit::TextSelection { anchor, focus });
        }
    }

    fn text_position(&self, position: Position) -> Option<accesskit::TextPosition> {
        let run = self
            .runs
            .iter()
            .rev()
            .find(|run| run.block == position.block && run.start <= position.byte)?;
        let relative = position.byte.min(run.content_end) - run.start;
        Some(accesskit::TextPosition {
            node: run.node_id?,
            character_index: run
                .offsets
                .partition_point(|&offset| offset <= relative)
                .saturating_sub(1),
        })
    }

    fn position(&self, position: accesskit::TextPosition) -> Option<Position> {
        let run = self
            .runs
            .iter()
            .find(|run| run.node_id == Some(position.node))?;
        let offset = *run.offsets.get(position.character_index)?;
        let byte = run.start + offset;
        Some(if byte > run.content_end {
            Position {
                block: run.block + 1,
                byte: 0,
            }
        } else {
            Position {
                block: run.block,
                byte,
            }
        })
    }

    pub(crate) fn selection(&self, selection: &accesskit::TextSelection) -> Option<Selection> {
        Some(Selection {
            anchor: self.position(selection.anchor)?,
            head: self.position(selection.focus)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accessible_units_preserve_graphemes_and_represent_every_byte() {
        let text = "你好 é 👨‍👩‍👧‍👦\n";
        let offsets = character_offsets(text);
        assert_eq!(offsets.len() - 1, text.graphemes(true).count());
        assert_eq!(offsets.last(), Some(&text.len()));
        let long = format!("e{}", "\u{301}".repeat(200));
        let offsets = character_offsets(&long);
        assert!(offsets.windows(2).all(|pair| pair[1] - pair[0] <= 255));
        assert_eq!(offsets.last(), Some(&long.len()));
    }

    #[test]
    fn accessible_positions_roundtrip_wrapped_lines_and_hard_breaks() {
        let run = |id, start, end, text: &str| TextRun {
            node_id: Some(accesskit::NodeId(id)),
            block: 0,
            start,
            content_end: end,
            text: text.into(),
            offsets: character_offsets(text),
            bounds: accesskit::Rect::ZERO,
        };
        let text = AccessibleText {
            runs: vec![run(1, 0, 3, "你"), run(2, 3, 6, "好\n")],
            selection: Selection::default(),
        };
        let boundary = Position { block: 0, byte: 3 };
        let accessible = text.text_position(boundary).unwrap();
        assert_eq!(accessible.node, accesskit::NodeId(2));
        assert_eq!(accessible.character_index, 0);
        assert_eq!(text.position(accessible), Some(boundary));
        assert_eq!(
            text.position(accesskit::TextPosition {
                node: accesskit::NodeId(2),
                character_index: 2
            }),
            Some(Position { block: 1, byte: 0 })
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
