//! Structured clipboard operations share the editor's normal transaction and history.
use crate::{
    Block, BlockKind, Change, Document, Editor, Origin, Position, Replacement, Selection,
    ceil_grapheme, slice_spans,
};
use std::ops::Range;

impl Document {
    /// The rich text of a range, with partial edge blocks sliced. The range is ordered
    /// and clamped.
    pub fn fragment_in(&self, range: Range<Position>) -> Document {
        let (start, end) = self.ordered_range(range);
        let mut blocks = self.blocks[start.block..=end.block].to_vec();
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
}

impl Editor {
    /// A selection expressed in document coordinates, with partial edge blocks sliced.
    pub fn selection_fragment(&self) -> Document {
        let (start, end) = self.selection().ordered();
        self.fragment_in(start..end)
    }

    /// See [`Document::fragment_in`].
    pub fn fragment_in(&self, range: Range<Position>) -> Document {
        self.document().fragment_in(range)
    }

    /// Insert formatted content as one undo step. Inline edge paragraphs merge with
    /// surrounding text; structural blocks keep their own lines. Code accepts literals.
    pub fn insert_fragment(&mut self, fragment: Document) -> Option<Change> {
        self.transaction(Origin::Paste, |editor| {
            editor.apply_insert_fragment(fragment);
        })
    }

    pub(crate) fn apply_insert_fragment(&mut self, mut fragment: Document) {
        fragment.normalize();
        {
            let editor = self;
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
        }
    }

    /// Remove whole blocks. `range` is clamped to the document, and one block always
    /// remains: removing every block leaves a single empty paragraph. The caret lands at
    /// the start of the block that followed the run, or at the end of the one before it
    /// when the run reached the end of the document.
    pub fn remove_blocks(&mut self, range: Range<usize>) -> Option<Change> {
        self.transaction(Origin::Command, |editor| editor.apply_remove_blocks(range))
    }

    pub(crate) fn apply_remove_blocks(&mut self, range: Range<usize>) {
        let count = self.document().blocks.len();
        let start = range.start.min(count);
        let end = range.end.clamp(start, count);
        if start == end {
            return;
        }
        // One block separator goes with the run: the newline after it, or the one before
        // it when the run reaches the end of the document.
        let trailing = end < count;
        let from = if trailing {
            Position {
                block: start,
                byte: 0,
            }
        } else {
            let before = start.saturating_sub(1);
            Position {
                block: before,
                byte: if start == 0 {
                    0
                } else {
                    self.document().blocks[before].len()
                },
            }
        };
        let to = if trailing {
            Position {
                block: end,
                byte: 0,
            }
        } else {
            Position {
                block: end - 1,
                byte: self.document().blocks[end - 1].len(),
            }
        };
        let from = self.document().global_byte(from);
        let to = self.document().global_byte(to);
        self.pending_steps.push(Replacement {
            range: from..to,
            inserted_len: 0,
        });
        self.state.document.blocks.drain(start..end);
        self.state.document.normalize();
        let blocks = &self.state.document.blocks;
        let caret = if start < blocks.len() {
            Position {
                block: start,
                byte: 0,
            }
        } else {
            let last = blocks.len() - 1;
            Position {
                block: last,
                byte: blocks[last].len(),
            }
        };
        self.select(Selection::caret(caret));
    }

    /// Insert whole blocks at `index`, which is clamped to the document. The caret lands
    /// at the start of the first inserted block.
    pub fn insert_blocks(&mut self, index: usize, blocks: Vec<Block>) -> Option<Change> {
        self.transaction(Origin::Command, |editor| {
            editor.apply_insert_blocks(index, blocks)
        })
    }

    pub(crate) fn apply_insert_blocks(&mut self, index: usize, blocks: Vec<Block>) {
        if blocks.is_empty() {
            return;
        }
        let count = self.document().blocks.len();
        let index = index.min(count);
        let inserted_len = blocks.iter().map(|block| block.len() + 1).sum();
        // Appending puts the separating newline before the run instead of after it.
        let at = if index == count {
            Position {
                block: index - 1,
                byte: self.document().blocks[index - 1].len(),
            }
        } else {
            Position {
                block: index,
                byte: 0,
            }
        };
        let offset = self.document().global_byte(at);
        self.pending_steps.push(Replacement {
            range: offset..offset,
            inserted_len,
        });
        self.state.document.blocks.splice(index..index, blocks);
        self.state.document.normalize();
        self.select(Selection::caret(Position {
            block: index,
            byte: 0,
        }));
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        Affinity, Block, BlockKind, Document, Editor, Origin, Position, Selection,
        TransactionOptions,
    };

    fn kinds(editor: &Editor) -> Vec<BlockKind> {
        editor
            .document()
            .blocks
            .iter()
            .map(|block| block.kind.clone())
            .collect()
    }

    #[test]
    fn removing_blocks_keeps_the_kinds_of_the_lines_around_the_run() {
        let mut editor = Editor::new(Document::from_markdown("# head\npara\n- item\n> quote"));
        editor.remove_blocks(1..3);
        assert_eq!(editor.document().plain_text(), "head\nquote");
        assert_eq!(kinds(&editor), [BlockKind::Heading(1), BlockKind::Quote]);
        assert_eq!(editor.selection().head, Position { block: 1, byte: 0 });
        editor.undo();
        assert_eq!(
            editor.document().to_markdown(),
            "# head\npara\n- item\n> quote"
        );
    }

    #[test]
    fn removing_the_last_blocks_leaves_the_caret_at_the_end_of_the_one_before() {
        let mut editor = Editor::new(Document::from_markdown("# head\npara\ntail"));
        editor.remove_blocks(1..3);
        assert_eq!(editor.document().plain_text(), "head");
        assert_eq!(kinds(&editor), [BlockKind::Heading(1)]);
        assert_eq!(editor.selection().head, Position { block: 0, byte: 4 });
    }

    #[test]
    fn removing_every_block_leaves_one_empty_paragraph() {
        let mut editor = Editor::new(Document::from_markdown("# head\n- item\n***"));
        let change = editor.remove_blocks(0..99).expect("an edit");
        assert_eq!(editor.document(), &Document::default());
        assert_eq!(editor.selection().head, Position::default());
        // Every position collapses into the one block that is left.
        assert_eq!(
            change
                .mapping
                .map(Position { block: 2, byte: 0 }, Affinity::After),
            Position::default()
        );
        editor.undo();
        assert_eq!(kinds(&editor).len(), 3);
    }

    #[test]
    fn removing_an_empty_range_changes_nothing() {
        let mut editor = Editor::new(Document::from_markdown("a\nb"));
        assert!(editor.remove_blocks(1..1).is_none());
        assert!(editor.remove_blocks(5..9).is_none());
        assert_eq!(editor.document().plain_text(), "a\nb");
    }

    #[test]
    fn inserted_blocks_keep_their_kind_and_depth_and_map_positions_after_them() {
        let mut editor = Editor::new(Document::from_markdown("first\nlast"));
        let inserted = Document::from_markdown("- a\n  - b").blocks;
        let change = editor.insert_blocks(1, inserted).expect("an edit");
        assert_eq!(editor.document().plain_text(), "first\na\nb\nlast");
        assert_eq!(editor.document().blocks[2].depth, 1);
        assert_eq!(editor.selection().head, Position { block: 1, byte: 0 });
        assert_eq!(
            change
                .mapping
                .map(Position { block: 1, byte: 4 }, Affinity::After),
            Position { block: 3, byte: 4 }
        );
        editor.undo();
        assert_eq!(editor.document().plain_text(), "first\nlast");
    }

    #[test]
    fn blocks_appended_past_the_end_land_after_the_last_one() {
        let mut editor = Editor::new(Document::from_markdown("only"));
        editor.insert_blocks(9, vec![Block::default()]);
        assert_eq!(editor.document().plain_text(), "only\n");
        assert_eq!(editor.selection().head, Position { block: 1, byte: 0 });
        assert!(editor.insert_blocks(0, Vec::new()).is_none());
    }

    #[test]
    fn a_transaction_combines_structural_edits_into_one_undo_step() {
        let mut editor = Editor::new(Document::from_markdown("# head\npara"));
        editor
            .transact(
                TransactionOptions {
                    group: None,
                    origin: Origin::Extension("test"),
                },
                |tx| {
                    let moved = tx.document().blocks[1].clone();
                    tx.remove_blocks(1..2);
                    tx.insert_blocks(0, vec![moved]);
                },
            )
            .expect("an edit");
        assert_eq!(editor.document().plain_text(), "para\nhead");
        editor.undo();
        assert_eq!(editor.document().plain_text(), "head\npara");
    }

    #[test]
    fn a_transaction_pastes_a_rich_fragment_alongside_its_own_edits() {
        let mut editor = Editor::new(Document::from_markdown("tail"));
        editor
            .transact(TransactionOptions::default(), |tx| {
                tx.insert_fragment(Document::from_markdown_fragment("**bold** "));
                tx.set_block_kind(BlockKind::Heading(2));
            })
            .expect("an edit");
        assert_eq!(editor.document().to_markdown(), "## **bold** tail");
        editor.undo();
        assert_eq!(editor.document().to_markdown(), "tail");
    }

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
