//! Structured clipboard operations share the editor's normal transaction and history.
use crate::{
    BlockKind, Change, Document, Editor, Position, Replacement, Selection, ceil_grapheme,
    slice_spans,
};

impl Editor {
    /// A selection expressed in document coordinates, with partial edge blocks sliced.
    pub fn selection_fragment(&self) -> Document {
        let (start, end) = self.selection().ordered();
        let mut blocks = self.document().blocks[start.block..=end.block].to_vec();
        for (offset, block) in blocks.iter_mut().enumerate() {
            let index = start.block + offset;
            let from = if index == start.block { start.byte } else { 0 };
            let to = if index == end.block {
                end.byte
            } else {
                block.len()
            };
            if (index == start.block && (from > 0 || (index == end.block && to < block.len())))
                || (index == end.block && end.block > start.block && to == 0 && !block.is_empty())
            {
                block.kind = BlockKind::Paragraph;
                block.depth = 0;
            }
            block.spans = slice_spans(&block.spans, from..to);
        }
        let mut fragment = Document { blocks };
        fragment.normalize();
        fragment
    }

    /// Insert formatted content as one undo step. Inline edge paragraphs merge with
    /// surrounding text; structural blocks keep their own lines. Code accepts literals.
    pub fn insert_fragment(&mut self, mut fragment: Document) -> Option<Change> {
        fragment.normalize();
        self.transaction(|editor| {
            let (start, end) = editor.selection().ordered();
            if matches!(
                editor.document().blocks[start.block].kind,
                BlockKind::Code { .. }
            ) {
                editor.replace_selection(&fragment.plain_text());
                return;
            }
            let global_start = editor.document().global_byte(start);
            let global_end = editor.document().global_byte(end);
            let first = editor.document().blocks[start.block].clone();
            let last = editor.document().blocks[end.block].clone();
            let prefix = slice_spans(&first.spans, 0..start.byte);
            let suffix = slice_spans(&last.spans, end.byte..last.len());
            let mut blocks = fragment.blocks;

            if blocks[0].kind == BlockKind::Paragraph {
                let mut spans = prefix;
                spans.append(&mut blocks[0].spans);
                blocks[0].spans = spans;
                // An inline paste does not turn a heading or list item into a paragraph.
                if first.kind != BlockKind::Divider {
                    blocks[0].kind = first.kind.clone();
                    blocks[0].depth = first.depth;
                }
            } else if !prefix.is_empty() {
                let mut before = first.clone();
                before.spans = prefix;
                blocks.insert(0, before);
            }

            let inserted_last = blocks.len() - 1;
            let mut caret = Position {
                block: start.block + inserted_last,
                byte: blocks[inserted_last].len(),
            };
            if blocks[inserted_last].kind == BlockKind::Paragraph
                || (blocks.len() == 1 && blocks[0].kind == first.kind)
            {
                blocks[inserted_last].spans.extend(suffix);
            } else if !suffix.is_empty() {
                let mut after = last;
                after.spans = suffix;
                blocks.push(after);
            }

            let replaced_len = blocks.iter().map(|block| block.len()).sum::<usize>()
                + blocks.len().saturating_sub(1);
            let inserted_len =
                replaced_len - start.byte - (editor.document().blocks[end.block].len() - end.byte);
            editor.pending_steps.push(Replacement {
                range: global_start..global_end,
                inserted_len,
            });
            editor
                .state
                .document
                .blocks
                .splice(start.block..=end.block, blocks);
            editor.state.document.normalize();
            let text = editor.document().blocks[caret.block].text();
            caret.byte = ceil_grapheme(&text, caret.byte);
            editor.state.selection = Selection::caret(caret);
            editor.state.typing_marks = editor.document().blocks[caret.block].marks_at(caret.byte);
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::{Affinity, BlockKind, Document, Editor, Position, Selection};

    #[test]
    fn formatted_paste_merges_inline_edges_and_undoes_atomically() {
        let mut editor = Editor::new(Document::from_markdown("# Hello world"));
        editor.set_selection(Selection::caret(Position { block: 0, byte: 6 }));
        let change = editor
            .insert_fragment(Document::from_markdown("**bold**\n*second*"))
            .unwrap();
        assert_eq!(editor.document().plain_text(), "Hello bold\nsecondworld");
        assert_eq!(editor.document().blocks[0].kind, BlockKind::Heading(1));
        assert!(editor.document().blocks[0].spans.last().unwrap().marks.bold);
        assert_eq!(editor.selection().head, Position { block: 1, byte: 6 });
        assert_eq!(
            change
                .mapping
                .map(Position { block: 0, byte: 11 }, Affinity::After),
            Position { block: 1, byte: 11 }
        );
        editor.undo();
        assert_eq!(editor.document().to_markdown(), "# Hello world");
        editor.redo();
        assert_eq!(editor.document().plain_text(), "Hello bold\nsecondworld");
    }

    #[test]
    fn block_paste_splits_surrounding_text_and_retains_unicode() {
        let mut editor = Editor::new(Document::from_markdown("前后"));
        editor.set_selection(Selection::caret(Position { block: 0, byte: 3 }));
        editor.insert_fragment(Document::from_markdown("- parent\n  - child"));
        assert_eq!(editor.document().plain_text(), "前\nparent\nchild\n后");
        assert_eq!(editor.document().blocks[2].depth, 1);
        assert_eq!(editor.selection().head, Position { block: 2, byte: 5 });
        editor.undo();
        assert_eq!(editor.document().plain_text(), "前后");
    }

    #[test]
    fn copied_partial_selection_preserves_marks_without_changing_host_kind() {
        let mut editor = Editor::new(Document::from_markdown("# a **bold** tail"));
        editor.set_selection(Selection {
            anchor: Position { block: 0, byte: 2 },
            head: Position { block: 0, byte: 6 },
        });
        let fragment = editor.selection_fragment();
        assert_eq!(fragment.to_markdown(), "**bold**");
        editor.insert_fragment(fragment);
        assert_eq!(editor.document().to_markdown(), "# a **bold** tail");
    }

    #[test]
    fn copied_trailing_newline_does_not_copy_the_unselected_block_kind() {
        let mut editor = Editor::new(Document::from_markdown("- root\n  - child\n# untouched"));
        editor.set_selection(Selection {
            anchor: Position { block: 1, byte: 0 },
            head: Position { block: 2, byte: 0 },
        });
        let fragment = editor.selection_fragment();
        assert_eq!(fragment.plain_text(), "child\n");
        assert_eq!(fragment.blocks[0].depth, 0);
        assert_eq!(fragment.blocks[1].kind, BlockKind::Paragraph);
        assert_eq!(fragment.to_markdown(), "- child\n");
    }

    #[test]
    fn copying_a_document_preserves_a_trailing_empty_code_line() {
        let document = Document::from_markdown("before\n```python\nprint(1)\n\n```");
        let start = Position { block: 0, byte: 0 };
        let end = Position {
            block: document.blocks.len() - 1,
            byte: 0,
        };
        assert!(document.blocks[end.block].is_empty());
        assert_eq!(
            document.blocks[end.block].kind,
            BlockKind::Code {
                language: "python".into()
            }
        );
        for (anchor, head) in [(start, end), (end, start)] {
            let mut source = Editor::new(document.clone());
            source.set_selection(Selection { anchor, head });
            let fragment = source.selection_fragment();
            assert_eq!(fragment, document);

            let mut target = Editor::new(Document::default());
            target.insert_fragment(fragment);
            assert_eq!(target.document(), &document);
            target.undo();
            assert_eq!(target.document(), &Document::default());
            target.redo();
            assert_eq!(target.document(), &document);
        }
    }

    #[test]
    fn fragment_paste_places_the_caret_after_a_joined_grapheme() {
        let mut editor = Editor::new(Document::from_markdown("👩👧"));
        editor.set_selection(Selection::caret(Position {
            block: 0,
            byte: "👩".len(),
        }));
        editor.insert_fragment(Document::from_markdown("\u{200d}"));
        assert_eq!(editor.document().plain_text(), "👩‍👧");
        assert_eq!(editor.selection().head.byte, "👩‍👧".len());
        editor.undo();
        assert_eq!(editor.document().plain_text(), "👩👧");
        assert_eq!(editor.selection().head.byte, "👩".len());
    }

    #[test]
    fn replacing_multiple_blocks_and_code_paste_preserve_history() {
        let mut editor = Editor::new(Document::from_markdown("first\nsecond\nlast"));
        editor.set_selection(Selection {
            anchor: Position { block: 0, byte: 2 },
            head: Position { block: 2, byte: 2 },
        });
        editor.insert_fragment(Document::from_markdown("**new**"));
        assert_eq!(editor.document().plain_text(), "finewst");
        editor.undo();
        assert_eq!(editor.document().plain_text(), "first\nsecond\nlast");
        let mut code = Editor::new(Document::from_markdown("```rust\nx\n```"));
        code.insert_fragment(Document::from_markdown("**bold**"));
        assert_eq!(code.document().blocks[0].text(), "boldx");
        assert!(!code.document().blocks[0].spans[0].marks.bold);
    }
}
