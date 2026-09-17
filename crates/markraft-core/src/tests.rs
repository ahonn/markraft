use super::*;

fn editor(text: &str) -> Editor {
    Editor::new(Document::from_markdown(text))
}

#[test]
fn explicit_typing_groups_preserve_history_and_position_mapping() {
    let mut editor = editor("");
    editor.insert_text_grouped("h", 1);
    editor.insert_text_grouped("é", 1);
    editor.insert_text_grouped("👨‍👩‍👧‍👦", 1);
    let end = editor.selection().head;
    let change = editor.undo().unwrap();
    assert_eq!(editor.document().plain_text(), "");
    assert_eq!(
        change.mapping.map(end, Affinity::After),
        Position::default()
    );
    let change = editor.redo().unwrap();
    assert_eq!(editor.document().plain_text(), "hé👨‍👩‍👧‍👦");
    assert_eq!(
        change.mapping.map(Position::default(), Affinity::After),
        end
    );
    editor.insert_text_grouped("!", 2);
    editor.undo();
    assert_eq!(editor.document().plain_text(), "hé👨‍👩‍👧‍👦");
}

#[test]
fn movement_commands_newlines_and_composition_break_typing_groups() {
    let mut editor = editor("");
    editor.insert_text_grouped("a", 1);
    editor.move_left(false);
    editor.move_right(false);
    editor.insert_text_grouped("b", 1);
    editor.undo();
    assert_eq!(editor.document().plain_text(), "a");
    editor.insert_text_grouped("\n", 1);
    editor.insert_text_grouped("c", 1);
    editor.undo();
    assert_eq!(editor.document().plain_text(), "a\n");
    editor.toggle_mark(Mark::Bold);
    editor.insert_text_grouped("d", 1);
    editor.set_composition(None, "中", None);
    editor.commit_composition(None, "中文");
    editor.insert_text_grouped("e", 1);
    editor.undo();
    assert_eq!(editor.document().plain_text(), "a\nd中文");
    editor.undo();
    assert_eq!(editor.document().plain_text(), "a\nd");
}

#[test]
fn grouped_input_rule_keeps_conversion_as_an_undo_boundary() {
    let mut editor = editor("");
    editor.insert_text_grouped("#", 1);
    editor.insert_text_grouped(" ", 1);
    assert_eq!(editor.document().blocks[0].kind, BlockKind::Heading(1));
    editor.insert_text_grouped("Title", 1);
    editor.undo();
    assert_eq!(editor.document().blocks[0].kind, BlockKind::Heading(1));
    assert_eq!(editor.document().plain_text(), "");
    editor.undo();
    assert_eq!(editor.document().blocks[0].kind, BlockKind::Paragraph);
    assert_eq!(editor.document().plain_text(), "#");
}

#[test]
fn plain_text_input_never_interprets_markers_in_grouped_or_native_paths() {
    for marker in ["# ", "- ", "[] ", "- [ ] ", "`code`", "**bold**"] {
        let mut editor = editor("");
        editor.insert_text_plain_grouped(marker, 1);
        assert_eq!(editor.document().plain_text(), marker);
        assert_eq!(editor.document().blocks[0].kind, BlockKind::Paragraph);
        editor.insert_text_plain_grouped("query", 1);
        editor.undo();
        assert_eq!(editor.document().plain_text(), "");
        editor.commit_composition_plain(None, marker);
        assert_eq!(editor.document().plain_text(), marker);
        assert_eq!(editor.document().blocks[0].kind, BlockKind::Paragraph);
        editor.set_composition(None, "候选", None);
        editor.commit_composition_plain(None, "确定");
        assert_eq!(editor.document().plain_text(), format!("{marker}确定"));
    }
}

#[test]
fn history_limits_bound_undo_and_redo_without_losing_live_edits() {
    let mut editor = editor("");
    editor.set_history_limits(HistoryLimits {
        entries: 2,
        bytes: usize::MAX,
    });
    for text in ["a", "b", "c"] {
        editor.insert_text(text);
    }
    editor.undo();
    editor.undo();
    assert!(editor.undo().is_none());
    assert_eq!(editor.document().plain_text(), "a");
    editor.redo();
    editor.set_history_limits(HistoryLimits {
        entries: 1,
        bytes: usize::MAX,
    });
    assert_eq!(editor.undo.len() + editor.redo.len(), 1);
    editor.redo();
    assert_eq!(editor.document().plain_text(), "abc");
    editor.set_history_limits(HistoryLimits {
        entries: 256,
        bytes: 1,
    });
    editor.insert_text(" retained even when history cannot fit");
    assert!(!editor.can_undo());
    assert!(!editor.can_redo());
    assert_eq!(
        editor.document().plain_text(),
        "abc retained even when history cannot fit"
    );
}

#[test]
fn committed_snapshot_excludes_candidates_until_commit_or_cancel() {
    let mut editor = editor("original");
    editor.move_document_end(false);
    editor.set_composition(None, "候选", None);
    assert!(editor.is_composing());
    assert_eq!(editor.document().plain_text(), "original候选");
    assert_eq!(editor.committed_document().plain_text(), "original");
    editor.commit_composition(None, "确定");
    assert!(!editor.is_composing());
    assert_eq!(editor.committed_document().plain_text(), "original确定");
    editor.set_composition(None, "临时", None);
    editor.cancel_composition();
    assert_eq!(editor.committed_document().plain_text(), "original确定");
}

#[test]
fn word_navigation_and_deletion_preserve_graphemes_and_selection() {
    let mut editor = editor("hello  é 👨‍👩‍👧‍👦\n中文");
    editor.move_word_right(false);
    assert_eq!(editor.selection().head.byte, 5);
    editor.move_word_right(true);
    assert_eq!(editor.selection_text(), "  é");
    editor.delete_word_backward();
    assert_eq!(editor.document().plain_text(), "hello 👨‍👩‍👧‍👦\n中文");
    editor.delete_word_forward();
    assert_eq!(editor.document().plain_text(), "hello\n中文");
    editor.undo();
    editor.move_word_right(false);
    editor.move_word_left(true);
    assert_eq!(editor.selection_text(), "👨‍👩‍👧‍👦");
    editor.move_document_end(false);
    editor.move_document_start(true);
    assert_eq!(editor.selection_text(), editor.document().plain_text());
    editor.delete_word_forward();
    assert_eq!(editor.document().plain_text(), "");
}

fn caret(editor: &mut Editor, block: usize, byte: usize) {
    editor.set_selection(Selection::caret(Position { block, byte }));
}

#[test]
fn split_merge_and_cross_block_delete_preserve_marks() {
    let mut editor = editor("ab**cd**\nef\ngh");
    caret(&mut editor, 0, 3);
    editor.insert_text("\n");
    assert_eq!(editor.document().plain_text(), "abc\nd\nef\ngh");
    assert!(editor.document().blocks[1].spans[0].marks.bold);
    editor.backspace();
    assert_eq!(editor.document().plain_text(), "abcd\nef\ngh");
    editor.set_selection(Selection {
        anchor: Position { block: 0, byte: 2 },
        head: Position { block: 2, byte: 1 },
    });
    assert_eq!(editor.selection_text(), "cd\nef\ng");
    editor.insert_text("中");
    assert_eq!(editor.document().plain_text(), "ab中h");
    assert_eq!(editor.selection().head.byte, 5);
    editor.undo();
    assert_eq!(editor.document().plain_text(), "abcd\nef\ngh");
}

#[test]
fn typing_marks_apply_without_mutating_document_then_selected_marks_toggle() {
    let mut editor = editor("");
    assert!(editor.toggle_mark(Mark::Bold).is_none());
    assert_eq!(editor.revision(), 0);
    editor.insert_text("hello");
    assert!(editor.document().blocks[0].spans[0].marks.bold);
    editor.toggle_mark(Mark::Bold);
    editor.insert_text("!");
    assert_eq!(editor.document().blocks[0].spans.len(), 2);
    editor.set_selection(Selection {
        anchor: Position::default(),
        head: Position { block: 0, byte: 6 },
    });
    editor.toggle_mark(Mark::Bold);
    assert_eq!(editor.document().blocks[0].spans.len(), 1);
    assert!(editor.document().blocks[0].spans[0].marks.bold);
    editor.toggle_mark(Mark::Bold);
    assert!(!editor.document().blocks[0].spans[0].marks.bold);
}

#[test]
fn unicode_offsets_and_grapheme_deletion() {
    let mut editor = editor("中😀e\u{301}👨‍👩‍👧‍👦\n末");
    let text = editor.document().blocks[0].text();
    for (byte, _) in text.grapheme_indices(true) {
        let position = Position { block: 0, byte };
        assert_eq!(
            editor.utf16_to_position(editor.position_to_utf16(position)),
            position
        );
    }
    assert_eq!(editor.utf16_to_position(2), Position { block: 0, byte: 3 });
    assert_eq!(editor.utf16_to_position(4), Position { block: 0, byte: 7 });
    caret(&mut editor, 0, text.len());
    editor.backspace();
    assert_eq!(editor.document().plain_text(), "中😀e\u{301}\n末");
    editor.backspace();
    assert_eq!(editor.document().plain_text(), "中😀\n末");
    editor.backspace();
    assert_eq!(editor.document().plain_text(), "中\n末");
    assert_eq!(editor.position_to_utf16(Position { block: 1, byte: 0 }), 2);
}

#[test]
fn normalization_preserves_text_and_merges_runs() {
    let mut document = Document {
        blocks: vec![Block {
            kind: BlockKind::Heading(99),
            spans: vec![
                Span {
                    text: "a".into(),
                    marks: Marks::default(),
                },
                Span {
                    text: "b\nc".into(),
                    marks: Marks::default(),
                },
            ],
        }],
    };
    document.normalize();
    assert_eq!(document.blocks[0].kind, BlockKind::Heading(6));
    assert_eq!(document.blocks[0].spans.len(), 1);
    assert_eq!(document.plain_text(), "ab\nc");
    assert_eq!(document.blocks[1].kind, BlockKind::Paragraph);
    assert_eq!(
        Editor::new(Document { blocks: vec![] }).document(),
        &Document::default()
    );
}

#[test]
fn document_versions_and_markdown_roundtrip() {
    let source = [
        "# Title",
        "- **bold** and *italic* and `code`",
        "- [ ] todo",
        "- [x] done",
        "",
        "***both*** **`bold code`** \\*literal\\*",
    ]
    .join("\n");
    let source = source.as_str();
    let document = Document::from_markdown(source);
    assert_eq!(
        Document::from_json(&document.to_json().unwrap()).unwrap(),
        document
    );
    assert!(
        Document::from_json(
            &document
                .to_json()
                .unwrap()
                .replace("\"version\": 1", "\"version\": 2")
        )
        .is_err()
    );
    assert_eq!(Document::from_markdown(&document.to_markdown()), document);
    assert!(document.blocks[5].spans[0].marks.bold);
    assert!(document.blocks[5].spans[0].marks.italic);
}

#[test]
fn unsupported_markdown_stays_literal_and_code_ticks_roundtrip() {
    let document = Document::from_markdown(
        "```rust\n# literal\n**literal**\n```\n> quote\n1. ordered\n![alt](image.png)",
    );
    assert_eq!(
        document.plain_text(),
        "```rust\n# literal\n**literal**\n```\n> quote\n1. ordered\n![alt](image.png)"
    );
    assert_eq!(Document::from_markdown(&document.to_markdown()), document);
    for text in ["`literal`", " a ", "  ", "one``two", "中文 😀"] {
        let document = Document {
            blocks: vec![Block {
                kind: BlockKind::Paragraph,
                spans: vec![Span {
                    text: text.into(),
                    marks: Marks {
                        code: true,
                        bold: true,
                        italic: true,
                    },
                }],
            }],
        };
        assert_eq!(
            Document::from_markdown(&document.to_markdown()),
            document,
            "{}",
            document.to_markdown()
        );
    }
}

#[test]
fn input_rules_are_atomic_and_list_enter_behaves() {
    let mut editor = editor("");
    editor.insert_text("#");
    editor.insert_text(" ");
    assert_eq!(editor.document().blocks[0].kind, BlockKind::Heading(1));
    assert_eq!(editor.document().plain_text(), "");
    editor.undo();
    assert_eq!(editor.document().plain_text(), "#");
    assert_eq!(editor.document().blocks[0].kind, BlockKind::Paragraph);
    editor = Editor::new(Document::default());
    editor.insert_text("- ");
    editor.insert_text("item");
    editor.insert_text("\n");
    assert_eq!(editor.document().blocks[1].kind, BlockKind::Bullet);
    editor.insert_text("\n");
    assert_eq!(editor.document().blocks.len(), 2);
    assert_eq!(editor.document().blocks[1].kind, BlockKind::Paragraph);
    editor.insert_text("**bold**");
    assert_eq!(editor.document().blocks[1].text(), "bold");
    assert!(editor.document().blocks[1].spans[0].marks.bold);
}

#[test]
fn task_input_rules_convert_atomically_when_typing_the_final_space() {
    for marker in [
        "[] ", "- [] ", "[ ] ", "- [ ] ", "[x] ", "- [x] ", "[X] ", "- [X] ",
    ] {
        let mut editor = editor("");
        for character in marker.trim_end().chars() {
            editor.insert_text(&character.to_string());
        }
        let before = editor.document().clone();
        let selection = editor.selection();
        let revision = editor.revision();

        let change = editor.insert_text(" ").unwrap();
        assert_eq!(change.revision, revision + 1, "{marker:?}");
        assert_eq!(editor.document().plain_text(), "", "{marker:?}");
        assert_eq!(
            editor.document().blocks[0].kind,
            BlockKind::Task {
                checked: marker.contains(['x', 'X']),
            },
            "{marker:?}"
        );
        let converted = editor.document().clone();
        editor.undo();
        assert_eq!(editor.document(), &before, "{marker:?}");
        assert_eq!(editor.selection(), selection, "{marker:?}");
        editor.redo();
        assert_eq!(editor.document(), &converted, "{marker:?}");
    }
}

#[test]
fn task_input_shorthand_accepts_a_complete_marker() {
    for marker in ["[] ", "- [] ", "[ ] ", "- [ ] "] {
        let mut editor = editor("");
        editor.insert_text(marker);
        assert_eq!(editor.document().plain_text(), "", "{marker:?}");
        assert_eq!(
            editor.document().blocks[0].kind,
            BlockKind::Task { checked: false },
            "{marker:?}"
        );
        editor.undo();
        assert_eq!(editor.document(), &Document::default(), "{marker:?}");
        assert!(!editor.can_undo(), "{marker:?}");
    }
}

#[test]
fn task_input_shorthand_stays_literal_during_composition_and_commit() {
    for marker in ["[] ", "- [] ", "[ ] ", "- [ ] "] {
        let mut editor = editor("");
        editor.set_composition(None, marker.trim_end(), None);
        editor.set_composition(None, marker, None);
        assert_eq!(editor.document().plain_text(), marker);
        assert_eq!(editor.document().blocks[0].kind, BlockKind::Paragraph);
        editor.commit_composition(None, marker);
        assert_eq!(editor.document().plain_text(), marker);
        assert_eq!(editor.document().blocks[0].kind, BlockKind::Paragraph);
        editor.undo();
        assert_eq!(editor.document(), &Document::default());
        assert!(!editor.can_undo());
    }
}

#[test]
fn composition_candidates_commit_as_one_undo_and_publish_every_update() {
    let mut editor = editor("ab");
    caret(&mut editor, 0, 1);
    editor.set_composition(None, "n", Some(1..1));
    assert_eq!(editor.marked_range(), Some(1..2));
    editor.set_composition(None, "ni", Some(2..2));
    assert_eq!(editor.marked_range(), Some(1..3));
    editor.commit_composition(None, "你");
    assert_eq!(editor.document().plain_text(), "a你b");
    assert_eq!(editor.marked_range(), None);
    assert_eq!(editor.revision(), 3);
    editor.undo();
    assert_eq!(editor.document().plain_text(), "ab");
    assert!(!editor.can_undo());
    assert_eq!(editor.revision(), 4);
    editor.redo();
    assert_eq!(editor.document().plain_text(), "a你b");
    assert_eq!(editor.revision(), 5);
}

#[test]
fn composition_replaces_selection_and_cancel_restores_full_state() {
    let mut editor = editor("中😀\ntext");
    editor.set_selection(Selection {
        anchor: Position { block: 0, byte: 3 },
        head: Position { block: 1, byte: 2 },
    });
    let before = editor.selection();
    editor.set_composition(None, "候选", Some(0..2));
    assert_eq!(editor.document().plain_text(), "中候选xt");
    editor.cancel_composition();
    assert_eq!(editor.document().plain_text(), "中😀\ntext");
    assert_eq!(editor.selection(), before);
    assert!(!editor.can_undo());
    editor.set_composition(Some(1..3), "👩🏽‍💻", None);
    editor.finish_composition();
    editor.undo();
    assert_eq!(editor.document().plain_text(), "中😀\ntext");
}

#[test]
fn input_rules_do_not_run_during_composition() {
    let mut editor = editor("");
    editor.set_composition(None, "# ", None);
    editor.commit_composition(None, "# ");
    assert_eq!(editor.document().blocks[0].kind, BlockKind::Paragraph);
    assert_eq!(editor.document().plain_text(), "# ");
}

#[test]
fn mapping_moves_positions_across_replacements_and_split() {
    let mut editor = editor("abcd\nef");
    caret(&mut editor, 0, 2);
    let change = editor.insert_text("中\nx").unwrap();
    assert_eq!(
        change
            .mapping
            .map(Position { block: 0, byte: 2 }, Affinity::Before),
        Position { block: 0, byte: 2 }
    );
    assert_eq!(
        change
            .mapping
            .map(Position { block: 0, byte: 2 }, Affinity::After),
        Position { block: 1, byte: 1 }
    );
    assert_eq!(
        change
            .mapping
            .map(Position { block: 0, byte: 4 }, Affinity::After),
        Position { block: 1, byte: 3 }
    );
    assert_eq!(
        change
            .mapping
            .map(Position { block: 1, byte: 1 }, Affinity::After),
        Position { block: 2, byte: 1 }
    );
}

#[test]
fn selection_navigation_clamps_invalid_offsets_and_keeps_direction() {
    let mut editor = editor("a😀b\nc");
    caret(&mut editor, 0, 3);
    assert_eq!(editor.selection().head.byte, 1);
    editor.move_right(true);
    assert_eq!(editor.selection_text(), "😀");
    editor.move_left(false);
    assert_eq!(editor.selection().head.byte, 1);
    editor.move_right(false);
    assert_eq!(editor.selection().head.byte, 5);
    caret(&mut editor, 10, usize::MAX);
    assert_eq!(editor.selection().head, Position { block: 1, byte: 1 });
}

#[test]
fn empty_operations_do_not_create_history_or_revisions() {
    let mut editor = editor("");
    assert!(editor.backspace().is_none());
    assert!(editor.delete_forward().is_none());
    assert!(editor.insert_text("").is_none());
    assert_eq!(editor.revision(), 0);
    assert!(!editor.can_undo());
}

#[test]
fn mapping_tracks_actual_insertion_in_repeated_text_and_history() {
    let mut editor = editor("aaa");
    let change = editor.insert_text("a").unwrap();
    assert_eq!(
        change
            .mapping
            .map(Position { block: 0, byte: 1 }, Affinity::After),
        Position { block: 0, byte: 2 }
    );
    let undo = editor.undo().unwrap();
    assert_eq!(
        undo.mapping
            .map(Position { block: 0, byte: 2 }, Affinity::After),
        Position { block: 0, byte: 1 }
    );
    let redo = editor.redo().unwrap();
    assert_eq!(
        redo.mapping
            .map(Position { block: 0, byte: 1 }, Affinity::After),
        Position { block: 0, byte: 2 }
    );
}

#[test]
fn composition_can_replace_combining_suffix_without_deleting_base() {
    let mut editor = editor("a");
    caret(&mut editor, 0, 1);
    editor.set_composition(None, "\u{301}", None);
    assert_eq!(editor.document().plain_text(), "a\u{301}");
    assert_eq!(editor.marked_range(), Some(1..2));
    editor.set_composition(None, "\u{300}", None);
    assert_eq!(editor.document().plain_text(), "a\u{300}");
    assert_eq!(editor.marked_range(), Some(1..2));
    editor.commit_composition(None, "\u{308}");
    assert_eq!(editor.document().plain_text(), "a\u{308}");
    editor.undo();
    assert_eq!(editor.document().plain_text(), "a");
    editor.redo();
    assert_eq!(editor.document().plain_text(), "a\u{308}");
}

#[test]
fn code_only_delimiters_and_adjacent_marks_roundtrip() {
    for text in ["one`two", "one``two", "one```two", "`", "``", "```"] {
        let document = Document {
            blocks: vec![Block {
                kind: BlockKind::Paragraph,
                spans: vec![Span {
                    text: text.into(),
                    marks: Marks {
                        code: true,
                        ..Marks::default()
                    },
                }],
            }],
        };
        assert_eq!(
            Document::from_markdown(&document.to_markdown()),
            document,
            "{}",
            document.to_markdown()
        );
    }
    for a in 0..8 {
        for b in 0..8 {
            let marks = |bits: u8| Marks {
                bold: bits & 1 != 0,
                italic: bits & 2 != 0,
                code: bits & 4 != 0,
            };
            let mut document = Document {
                blocks: vec![Block {
                    kind: BlockKind::Paragraph,
                    spans: vec![
                        Span {
                            text: "left".into(),
                            marks: marks(a),
                        },
                        Span {
                            text: "right".into(),
                            marks: marks(b),
                        },
                    ],
                }],
            };
            document.normalize();
            assert_eq!(
                Document::from_markdown(&document.to_markdown()),
                document,
                "{}",
                document.to_markdown()
            );
        }
    }
}

#[test]
fn all_adjacent_mark_combinations_roundtrip() {
    let marks = |bits: u8| Marks {
        bold: bits & 1 != 0,
        italic: bits & 2 != 0,
        code: bits & 4 != 0,
    };
    for left in 0..8 {
        for right in 0..8 {
            let mut document = Document {
                blocks: vec![Block {
                    kind: BlockKind::Paragraph,
                    spans: vec![
                        Span {
                            text: "left".into(),
                            marks: marks(left),
                        },
                        Span {
                            text: "right".into(),
                            marks: marks(right),
                        },
                    ],
                }],
            };
            document.normalize();
            let markdown = document.to_markdown();
            assert_eq!(
                Document::from_markdown(&markdown),
                document,
                "{left}/{right}: {markdown}"
            );
        }
    }
}

fn type_chars(editor: &mut Editor, text: &str) {
    for c in text.chars() {
        editor.insert_text(&c.to_string());
    }
}

#[test]
fn underscore_input_rules_format_words_but_not_identifiers() {
    let mut editor = editor("");
    type_chars(&mut editor, "__bold__");
    assert_eq!(editor.document().plain_text(), "bold");
    assert!(editor.document().blocks[0].spans[0].marks.bold);

    editor = Editor::new(Document::default());
    type_chars(&mut editor, "an _italic_");
    assert_eq!(editor.document().plain_text(), "an italic");
    assert!(editor.document().blocks[0].spans[1].marks.italic);

    editor = Editor::new(Document::default());
    type_chars(&mut editor, "snake_case_name and a__b__");
    assert_eq!(editor.document().plain_text(), "snake_case_name and a__b__");
}
