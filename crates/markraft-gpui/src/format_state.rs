use markraft_core::{BlockKind, Editor, Marks};

pub(crate) fn active_marks(editor: &Editor) -> Marks {
    if editor.selection().is_empty() {
        return editor.typing_marks();
    }
    let (start, end) = editor.selection().ordered();
    let mut common: Option<Marks> = None;
    for index in start.block..=end.block {
        let block = &editor.document().blocks[index];
        let start_byte = if index == start.block { start.byte } else { 0 };
        let end_byte = if index == end.block {
            end.byte
        } else {
            block.len()
        };
        let mut offset = 0;
        for span in &block.spans {
            let span_end = offset + span.text.len();
            if offset.max(start_byte) < span_end.min(end_byte) {
                common = Some(match common {
                    None => span.marks,
                    Some(previous) => Marks {
                        bold: previous.bold && span.marks.bold,
                        italic: previous.italic && span.marks.italic,
                        code: previous.code && span.marks.code,
                        strikethrough: previous.strikethrough && span.marks.strikethrough,
                        underline: previous.underline && span.marks.underline,
                    },
                });
            }
            offset = span_end;
        }
    }
    common.unwrap_or_default()
}

pub(crate) fn active_block_kind(editor: &Editor) -> Option<BlockKind> {
    let (start, end) = editor.selection().ordered();
    // Match core::set_block_kind: a selection ending at a following block's
    // beginning does not format that block.
    let last = if end.byte == 0 && end.block > start.block {
        end.block - 1
    } else {
        end.block
    };
    let blocks = &editor.document().blocks[start.block..=last];
    let kind = &blocks[0].kind;
    blocks
        .iter()
        .all(|block| &block.kind == kind)
        .then(|| kind.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use markraft_core::{Block, Document, Mark, Position, Selection, Span};

    fn select(editor: &mut Editor, anchor: (usize, usize), head: (usize, usize)) {
        editor.set_selection(Selection {
            anchor: Position {
                block: anchor.0,
                byte: anchor.1,
            },
            head: Position {
                block: head.0,
                byte: head.1,
            },
        });
    }

    #[test]
    fn collapsed_selection_reports_typing_marks_even_without_document_changes() {
        let mut editor = Editor::new(Document::default());
        editor.toggle_mark(Mark::Bold);
        editor.toggle_mark(Mark::Code);
        assert_eq!(
            active_marks(&editor),
            Marks {
                bold: true,
                code: true,
                ..Marks::default()
            }
        );
        assert_eq!(active_block_kind(&editor), Some(BlockKind::Paragraph));
        assert_eq!(editor.document().plain_text(), "");
    }

    #[test]
    fn selected_marks_intersect_only_overlapping_spans_in_either_direction() {
        let mut editor = Editor::new(Document {
            blocks: vec![Block {
                kind: BlockKind::Paragraph,
                spans: vec![
                    Span {
                        text: "ab".into(),
                        marks: Marks {
                            bold: true,
                            italic: true,
                            ..Marks::default()
                        },
                        link: None,
                    },
                    Span {
                        text: "cd".into(),
                        marks: Marks {
                            bold: true,
                            ..Marks::default()
                        },
                        link: None,
                    },
                    Span {
                        text: "ef".into(),
                        marks: Marks::default(),
                        link: None,
                    },
                ],
            }],
        });
        let bold = Marks {
            bold: true,
            ..Marks::default()
        };
        select(&mut editor, (0, 1), (0, 3));
        assert_eq!(active_marks(&editor), bold);
        select(&mut editor, (0, 3), (0, 1));
        assert_eq!(active_marks(&editor), bold);
        select(&mut editor, (0, 2), (0, 4));
        assert_eq!(active_marks(&editor), bold);
        select(&mut editor, (0, 4), (0, 6));
        assert_eq!(active_marks(&editor), Marks::default());
        select(&mut editor, (0, 0), (0, 2));
        assert!(active_marks(&editor).italic);
        select(&mut editor, (0, 0), (0, 6));
        assert_eq!(active_marks(&editor), Marks::default());
    }

    #[test]
    fn block_boundary_excludes_the_unselected_following_block() {
        let mut editor = Editor::new(Document::from_markdown("# **你好**\nplain"));
        select(&mut editor, (0, 0), (1, 0));
        assert!(active_marks(&editor).bold);
        assert_eq!(active_block_kind(&editor), Some(BlockKind::Heading(1)));
        select(&mut editor, (1, 0), (0, 0));
        assert_eq!(active_block_kind(&editor), Some(BlockKind::Heading(1)));
        select(&mut editor, (0, 0), (1, 1));
        assert_eq!(active_marks(&editor), Marks::default());
        assert_eq!(active_block_kind(&editor), None);
        select(&mut editor, (0, "你好".len()), (1, 0));
        assert_eq!(active_marks(&editor), Marks::default());
        assert_eq!(active_block_kind(&editor), Some(BlockKind::Heading(1)));
    }

    #[test]
    fn empty_blocks_do_not_invent_inline_marks_but_count_for_block_format() {
        let mut editor = Editor::new(Document::from_markdown("**first**\n\n**last**"));
        editor.move_document_end(true);
        assert!(active_marks(&editor).bold);
        assert_eq!(active_block_kind(&editor), Some(BlockKind::Paragraph));
        select(&mut editor, (1, 0), (1, 0));
        assert_eq!(active_marks(&editor), Marks::default());
        editor.set_block_kind(BlockKind::Heading(2));
        select(&mut editor, (0, 0), (2, 4));
        assert_eq!(active_block_kind(&editor), None);
    }
}
