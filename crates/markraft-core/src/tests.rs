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
            depth: 0,
            kind: BlockKind::Heading(99),
            spans: vec![
                Span {
                    text: "a".into(),
                    marks: Marks::default(),
                    link: None,
                },
                Span {
                    text: "b\nc".into(),
                    marks: Marks::default(),
                    link: None,
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
    let document = Document::from_markdown("| a | b |\n[text] (url)\n![alt](image.png)");
    assert_eq!(
        document.plain_text(),
        "| a | b |\n[text] (url)\n![alt](image.png)"
    );
    assert_eq!(Document::from_markdown(&document.to_markdown()), document);
    for text in ["`literal`", " a ", "  ", "one``two", "中文 😀"] {
        let document = Document {
            blocks: vec![Block {
                depth: 0,
                kind: BlockKind::Paragraph,
                spans: vec![Span {
                    text: text.into(),
                    marks: Marks {
                        code: true,
                        bold: true,
                        italic: true,
                        ..Marks::default()
                    },
                    link: None,
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
                depth: 0,
                kind: BlockKind::Paragraph,
                spans: vec![Span {
                    text: text.into(),
                    marks: Marks {
                        code: true,
                        ..Marks::default()
                    },
                    link: None,
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
    for a in 0..32 {
        for b in 0..32 {
            let marks = |bits: u8| Marks {
                bold: bits & 1 != 0,
                italic: bits & 2 != 0,
                code: bits & 4 != 0,
                strikethrough: bits & 8 != 0,
                underline: bits & 16 != 0,
            };
            let mut document = Document {
                blocks: vec![Block {
                    depth: 0,
                    kind: BlockKind::Paragraph,
                    spans: vec![
                        Span {
                            text: "left".into(),
                            marks: marks(a),
                            link: None,
                        },
                        Span {
                            text: "right".into(),
                            marks: marks(b),
                            link: None,
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
        strikethrough: bits & 8 != 0,
        underline: bits & 16 != 0,
    };
    for left in 0..32 {
        for right in 0..32 {
            let mut document = Document {
                blocks: vec![Block {
                    depth: 0,
                    kind: BlockKind::Paragraph,
                    spans: vec![
                        Span {
                            text: "left".into(),
                            marks: marks(left),
                            link: None,
                        },
                        Span {
                            text: "right".into(),
                            marks: marks(right),
                            link: None,
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

#[test]
fn strikethrough_and_underline_round_trip_and_load_from_older_json() {
    let source = "~~gone~~ and <u>**kept**</u>";
    let document = Document::from_markdown(source);
    let spans = &document.blocks[0].spans;
    assert!(spans[0].marks.strikethrough);
    assert!(spans[2].marks.underline && spans[2].marks.bold);
    assert_eq!(document.plain_text(), "gone and kept");
    assert_eq!(document.to_markdown(), source);

    let mut editor = Editor::new(Document::default());
    type_chars(&mut editor, "~~done~~");
    assert_eq!(editor.document().plain_text(), "done");
    assert!(editor.document().blocks[0].spans[0].marks.strikethrough);

    // Libraries written before these marks existed omit the fields.
    let older = r#"{"version":1,"document":{"blocks":[{"kind":"Paragraph","spans":[
        {"text":"a","marks":{"bold":true,"italic":false,"code":false}}]}]}}"#;
    let document = Document::from_json(older).unwrap();
    assert!(document.blocks[0].spans[0].marks.bold);
    assert!(!document.blocks[0].spans[0].marks.underline);
}

#[test]
fn quotes_continue_on_enter_and_round_trip() {
    let mut editor = editor("");
    type_chars(&mut editor, "> first");
    editor.insert_text("\n");
    type_chars(&mut editor, "second");
    editor.insert_text("\n");
    editor.insert_text("\n");
    let kinds: Vec<_> = editor.document().blocks.iter().map(|b| &b.kind).collect();
    assert_eq!(
        kinds,
        [&BlockKind::Quote, &BlockKind::Quote, &BlockKind::Paragraph]
    );
    assert_eq!(editor.document().to_markdown(), "> first\n> second\n");
    assert_eq!(
        Document::from_markdown("> first\n> second\n"),
        *editor.document()
    );
    // A literal marker in a paragraph must not come back as a quote.
    let literal = Document::from_markdown("\\> not a quote");
    assert_eq!(literal.blocks[0].kind, BlockKind::Paragraph);
    assert_eq!(literal.to_markdown(), "\\> not a quote");
}

#[test]
fn divider_takes_its_line_and_never_holds_text() {
    let mut editor = editor("");
    type_chars(&mut editor, "---");
    assert_eq!(editor.document().blocks[0].kind, BlockKind::Divider);
    assert_eq!(editor.selection().head, Position { block: 1, byte: 0 });
    type_chars(&mut editor, "after");
    assert_eq!(editor.document().to_markdown(), "---\nafter");
    assert_eq!(Document::from_markdown("---\nafter"), *editor.document());

    // Enter on the rule opens a line below; typing on it replaces it.
    editor.set_selection(Selection::caret(Position::default()));
    editor.insert_text("\n");
    assert_eq!(editor.document().blocks[0].kind, BlockKind::Divider);
    assert_eq!(editor.document().blocks.len(), 3);
    editor.set_selection(Selection::caret(Position::default()));
    editor.insert_text("x");
    assert_eq!(editor.document().blocks[0].kind, BlockKind::Paragraph);

    // Backspace from the following line removes the rule.
    let mut editor = Editor::new(Document::from_markdown("***\nafter"));
    editor.set_selection(Selection::caret(Position { block: 1, byte: 0 }));
    editor.backspace();
    assert_eq!(editor.document().to_markdown(), "after");

    editor.undo();
    assert_eq!(editor.document().blocks[0].kind, BlockKind::Divider);
    assert_eq!(
        Document::from_markdown("\\-\\-\\-").blocks[0].kind,
        BlockKind::Paragraph
    );
}

#[test]
fn ordered_lists_number_each_run_and_round_trip() {
    let mut editor = editor("");
    type_chars(&mut editor, "1. one");
    editor.insert_text("\n");
    type_chars(&mut editor, "two");
    editor.insert_text("\n");
    editor.insert_text("\n");
    type_chars(&mut editor, "break");
    editor.insert_text("\n");
    type_chars(&mut editor, "7. again");
    let document = editor.document();
    let ordinals: Vec<_> = (0..4).map(|block| document.ordinal(block)).collect();
    assert_eq!(ordinals, [Some(1), Some(2), None, Some(1)]);
    assert_eq!(document.to_markdown(), "1. one\n2. two\nbreak\n1. again");
    assert_eq!(
        Document::from_markdown("3) one\n9. two\nbreak\n1. again"),
        *document
    );

    let literal = Document::from_markdown("1\\. not a list");
    assert_eq!(literal.blocks[0].kind, BlockKind::Paragraph);
    assert_eq!(literal.plain_text(), "1. not a list");
    assert_eq!(literal.to_markdown(), "1\\. not a list");
}

#[test]
fn fenced_code_imports_as_plain_lines_and_round_trips() {
    let source = "```rust\n# not a heading\n\n**not bold**\n```\nafter\n````\n```\n````";
    let document = Document::from_markdown(source);
    let rust = BlockKind::Code {
        language: "rust".into(),
    };
    assert_eq!(
        document.plain_text(),
        "# not a heading\n\n**not bold**\nafter\n```"
    );
    assert!(document.blocks[..3].iter().all(|block| block.kind == rust));
    assert_eq!(document.blocks[2].spans[0].marks, Marks::default());
    assert_eq!(document.to_markdown(), source);
    assert_eq!(
        Document::from_markdown("~~~\n~~~").to_markdown(),
        "```\n\n```"
    );
}

#[test]
fn code_blocks_keep_blank_lines_and_exit_with_an_explicit_command() {
    let mut editor = editor("");
    type_chars(&mut editor, "```js");
    editor.insert_text("\n");
    let js = BlockKind::Code {
        language: "js".into(),
    };
    assert_eq!(editor.document().blocks.len(), 1);
    assert_eq!(editor.document().blocks[0].kind, js);
    editor.toggle_mark(Mark::Bold);
    type_chars(&mut editor, "# **x** - ");
    editor.insert_text("\n");
    type_chars(&mut editor, "y");
    // Blank lines above the last one are inside the block, so Enter keeps them.
    editor.set_selection(Selection::caret(Position { block: 0, byte: 10 }));
    editor.insert_text("\n");
    editor.insert_text("\n");
    assert!(
        editor
            .document()
            .blocks
            .iter()
            .all(|block| block.kind == js)
    );
    assert_eq!(editor.document().plain_text(), "# **x** - \n\n\ny");
    assert_eq!(editor.document().blocks[0].spans[0].marks, Marks::default());

    editor.move_document_end(false);
    editor.insert_text("\n");
    editor.insert_text("\n");
    assert_eq!(editor.document().blocks.last().unwrap().kind, js);
    editor.exit_code_block();
    assert_eq!(
        editor.document().blocks.last().unwrap().kind,
        BlockKind::Paragraph
    );
    assert_eq!(
        editor.document().to_markdown(),
        "```js\n# **x** - \n\n\ny\n\n\n```\n"
    );

    let mut editor = Editor::new(Document::default());
    type_chars(&mut editor, "``` ");
    assert_eq!(
        editor.document().blocks[0].kind,
        BlockKind::Code {
            language: String::new()
        }
    );
}

#[test]
fn links_round_trip_and_stay_whole_while_editing() {
    let source = "see [the **docs**](https://example.com/a_(b)) and [x](<a b>)";
    let document = Document::from_markdown(source);
    assert_eq!(document.plain_text(), "see the docs and x");
    let spans = &document.blocks[0].spans;
    assert_eq!(spans[1].link.as_deref(), Some("https://example.com/a_(b)"));
    assert!(spans[2].marks.bold && spans[2].link == spans[1].link);
    assert_eq!(spans[4].link.as_deref(), Some("a b"));
    assert_eq!(document.to_markdown(), source);
    assert_eq!(
        Document::from_json(&document.to_json().unwrap()).unwrap(),
        document
    );

    let mut editor = Editor::new(document);
    // Inside the link the text joins it; at its end it does not.
    editor.set_selection(Selection::caret(Position { block: 0, byte: 6 }));
    editor.insert_text("!");
    editor.set_selection(Selection::caret(Position { block: 0, byte: 13 }));
    assert_eq!(editor.active_link(), Some("https://example.com/a_(b)"));
    editor.insert_text("?");
    assert_eq!(
        editor.document().to_markdown(),
        "see [th\\!e **docs**](https://example.com/a_(b))**?** and [x](<a b>)"
    );

    // A caret edits or removes the whole link it touches.
    editor.set_selection(Selection::caret(Position { block: 0, byte: 5 }));
    editor.set_link(Some("https://new.example"));
    assert!(
        editor
            .document()
            .to_markdown()
            .starts_with("see [th\\!e **docs**](https://new.example)")
    );
    editor.set_link(None);
    assert!(
        editor
            .document()
            .to_markdown()
            .starts_with("see th\\!e **docs?** and")
    );
    assert_eq!(editor.active_link(), None);
    editor.undo();
    assert_eq!(editor.active_link(), Some("https://new.example"));
}

#[test]
fn links_are_created_from_a_selection_a_caret_or_typed_markdown() {
    let mut editor = editor("read this");
    editor.set_selection(Selection {
        anchor: Position { block: 0, byte: 5 },
        head: Position { block: 0, byte: 9 },
    });
    editor.set_link(Some("https://a.example"));
    assert_eq!(
        editor.document().to_markdown(),
        "read [this](https://a.example)"
    );
    assert_eq!(editor.active_link(), Some("https://a.example"));

    let mut editor = Editor::new(Document::default());
    editor.set_link(Some("https://b.example"));
    assert_eq!(
        editor.document().to_markdown(),
        "[https://b\\.example](https://b.example)"
    );

    let mut editor = Editor::new(Document::default());
    type_chars(&mut editor, "go [here](https://c.example)");
    assert_eq!(editor.document().plain_text(), "go here");
    assert_eq!(editor.selection().head, Position { block: 0, byte: 7 });
    assert_eq!(
        editor.document().to_markdown(),
        "go [here](https://c.example)"
    );
    editor.undo();
    assert_eq!(
        editor.document().plain_text(),
        "go [here](https://c.example"
    );

    let mut editor = Editor::new(Document::default());
    type_chars(&mut editor, "![alt](image.png)");
    assert_eq!(editor.document().plain_text(), "![alt](image.png)");
}

#[test]
fn nested_lists_and_quotes_round_trip_and_number_at_each_level() {
    let source = "1. first\n    1. child\n        - grandchild\n    2. child two\n2. second\n- [ ] task\n  - [x] nested\n> quote\n> > nested quote";
    let document = Document::from_markdown(source);
    assert_eq!(
        document
            .blocks
            .iter()
            .map(|block| block.depth)
            .collect::<Vec<_>>(),
        vec![0, 1, 2, 1, 0, 0, 1, 0, 1]
    );
    assert!(
        document
            .to_markdown()
            .contains("\n    1. child\n        - grandchild")
    );
    assert_eq!(document.ordinal(1), Some(1));
    assert_eq!(document.ordinal(3), Some(2));
    assert_eq!(document.ordinal(4), Some(2));
    assert_eq!(Document::from_markdown(&document.to_markdown()), document);
}

#[test]
fn list_indentation_moves_selected_siblings_and_their_subtrees_atomically() {
    let mut editor = editor("- first\n- second\n  - child\n- third\n- last");
    editor.set_selection(Selection {
        anchor: Position { block: 1, byte: 0 },
        head: Position { block: 4, byte: 0 },
    });
    let before = editor.document().clone();
    assert!(editor.indent().is_some());
    assert_eq!(
        editor
            .document()
            .blocks
            .iter()
            .map(|block| block.depth)
            .collect::<Vec<_>>(),
        vec![0, 1, 2, 1, 0]
    );
    editor.undo();
    assert_eq!(*editor.document(), before);
    editor.redo();
    editor.outdent();
    assert_eq!(*editor.document(), before);
    editor.set_selection(Selection::caret(Position::default()));
    assert!(editor.indent().is_none());
}

#[test]
fn nested_enter_and_backspace_outdent_before_exiting_the_list() {
    let mut editor = editor("- first\n  - ");
    editor.set_selection(Selection::caret(Position { block: 1, byte: 0 }));
    editor.insert_text("\n");
    assert_eq!(editor.document().blocks.len(), 2);
    assert_eq!(editor.document().blocks[1].depth, 0);
    assert_eq!(editor.document().blocks[1].kind, BlockKind::Bullet);
    editor.insert_text("\n");
    assert_eq!(editor.document().blocks[1].kind, BlockKind::Paragraph);
    editor.undo();
    editor.undo();
    editor.insert_text("child");
    editor.set_selection(Selection::caret(Position { block: 1, byte: 0 }));
    editor.backspace();
    assert_eq!(editor.document().blocks[1].depth, 0);
    assert_eq!(editor.document().blocks[1].text(), "child");
    editor.backspace();
    assert_eq!(editor.document().blocks[1].kind, BlockKind::Paragraph);
}

#[test]
fn nested_split_preserves_depth_and_format_conversion_clears_it() {
    let mut editor = editor("- first\n  - child");
    editor.move_document_end(false);
    editor.insert_text("\n");
    assert_eq!(editor.document().blocks[2].depth, 1);
    editor.set_block_kind(BlockKind::Task { checked: false });
    assert_eq!(editor.document().blocks[2].depth, 1);
    editor.set_block_kind(BlockKind::Heading(2));
    assert_eq!(editor.document().blocks[2].depth, 0);
}

#[test]
fn quote_depth_supports_input_rules_and_empty_enter() {
    let mut editor = editor("");
    editor.insert_text("> ");
    editor.insert_text("> ");
    assert_eq!(editor.document().blocks[0].depth, 1);
    editor.insert_text("\n");
    assert_eq!(editor.document().blocks[0].depth, 0);
    assert_eq!(editor.document().blocks[0].kind, BlockKind::Quote);
    editor.insert_text("\n");
    assert_eq!(editor.document().blocks[0].kind, BlockKind::Paragraph);
}

#[test]
fn nesting_is_backward_compatible_with_legacy_json() {
    let legacy = r#"{"version":1,"document":{"blocks":[{"kind":"Bullet","spans":[]}]}}"#;
    let document = Document::from_json(legacy).unwrap();
    assert_eq!(document.blocks[0].depth, 0);
    assert!(!document.to_json().unwrap().contains("depth"));
    let nested = Document::from_markdown("- root\n  - child");
    assert_eq!(
        Document::from_json(&nested.to_json().unwrap()).unwrap(),
        nested
    );
}

#[test]
fn code_language_changes_whole_run_and_copies_plain_text() {
    let mut editor = editor("```rust\nlet x = 1;\nprintln!(\"{x}\");\n```\nparagraph");
    editor.set_selection(Selection::caret(Position { block: 1, byte: 3 }));
    assert_eq!(editor.document().code_block_range(1), Some(0..2));
    assert_eq!(
        editor.code_block_text().as_deref(),
        Some("let x = 1;\nprintln!(\"{x}\");")
    );
    assert!(editor.set_code_language("typescript").is_some());
    assert!(editor.document().blocks[..2].iter().all(|block| block.kind
        == BlockKind::Code {
            language: "typescript".into()
        }));
    editor.undo();
    assert_eq!(
        editor.document().blocks[0].kind,
        BlockKind::Code {
            language: "rust".into()
        }
    );
    editor.move_document_end(false);
    assert_eq!(editor.code_block_text(), None);
    assert!(editor.set_code_language("rust").is_none());
}

#[test]
fn nesting_commands_preserve_first_item_and_unrelated_block_formats() {
    let mut editor = editor("- root\n  - child\n- sibling");
    editor.set_selection(Selection {
        anchor: Position::default(),
        head: Position { block: 2, byte: 7 },
    });
    assert!(editor.indent().is_none());
    editor.set_selection(Selection::caret(Position::default()));
    editor.backspace();
    assert_eq!(editor.document().blocks[0].kind, BlockKind::Paragraph);
    assert_eq!(editor.document().blocks[1].kind, BlockKind::Bullet);
    assert_eq!(editor.document().blocks[1].depth, 0);
    editor.set_block_kind(BlockKind::Heading(1));
    assert!(editor.outdent().is_none());
    assert_eq!(editor.document().blocks[0].kind, BlockKind::Heading(1));
}

#[test]
fn format_changes_normalize_orphaned_list_subtrees_without_changing_siblings() {
    let mut editor = editor("- parent\n  - child\n    - grandchild\n  - second child\n- root");
    editor.set_block_kind(BlockKind::Heading(1));
    assert_eq!(
        editor
            .document()
            .blocks
            .iter()
            .map(|block| block.depth)
            .collect::<Vec<_>>(),
        vec![0, 0, 1, 0, 0]
    );
    assert_eq!(
        Document::from_markdown(&editor.document().to_markdown()),
        *editor.document()
    );
    editor.undo();
    assert_eq!(editor.document().blocks[2].depth, 2);
}

#[test]
fn block_format_toggles_ignore_task_state_and_code_language() {
    let mut editor = editor("- [x] done\n- [ ] open\nuntouched");
    editor.set_selection(Selection {
        anchor: Position::default(),
        head: Position { block: 2, byte: 0 },
    });
    editor.toggle_block_kind(BlockKind::Task { checked: false });
    assert!(
        editor.document().blocks[..2]
            .iter()
            .all(|block| block.kind == BlockKind::Paragraph)
    );
    editor.undo();
    assert_eq!(
        editor.document().blocks[0].kind,
        BlockKind::Task { checked: true }
    );
    let mut editor = Editor::new(Document::from_markdown("```rust\none\ntwo\n```"));
    editor.set_selection(Selection::caret(Position { block: 1, byte: 1 }));
    editor.toggle_block_kind(BlockKind::Code {
        language: String::new(),
    });
    assert!(
        editor
            .document()
            .blocks
            .iter()
            .all(|block| block.kind == BlockKind::Paragraph)
    );
}

#[test]
fn explicit_code_exit_maps_positions_and_undoes_once() {
    let mut editor = editor("```rust\none\ntwo\n```\n# after");
    let before = editor.document().clone();
    let selection = Selection::caret(Position { block: 0, byte: 2 });
    editor.set_selection(selection);
    let change = editor.exit_code_block().unwrap();
    assert_eq!(editor.selection().head, Position { block: 2, byte: 0 });
    assert_eq!(editor.document().blocks[2].kind, BlockKind::Paragraph);
    assert_eq!(
        change
            .mapping
            .map(Position { block: 2, byte: 1 }, Affinity::After),
        Position { block: 3, byte: 1 }
    );
    editor.undo();
    assert_eq!(*editor.document(), before);
    assert_eq!(editor.selection(), selection);
    let mut editor = Editor::new(Document::from_markdown("```\nx\n```\nafter"));
    assert!(editor.exit_code_block().is_none());
    assert_eq!(editor.selection().head, Position { block: 1, byte: 0 });
    assert_eq!(editor.document().blocks.len(), 2);
}

#[test]
fn code_indentation_preserves_selected_text_and_maps_positions() {
    let mut editor = editor("```rust\none\n  two\nlast\n```");
    let selection = Selection {
        anchor: Position::default(),
        head: Position { block: 2, byte: 0 },
    };
    editor.set_selection(selection);
    let change = editor.indent().unwrap();
    assert_eq!(editor.document().plain_text(), "\tone\n\t  two\nlast");
    assert_eq!(
        change
            .mapping
            .map(Position { block: 1, byte: 4 }, Affinity::After),
        Position { block: 1, byte: 5 }
    );
    assert_eq!(editor.selection().head, selection.head);
    editor.outdent();
    assert_eq!(editor.document().plain_text(), "one\n  two\nlast");
    editor.set_selection(Selection::caret(Position { block: 1, byte: 3 }));
    editor.outdent();
    assert_eq!(editor.document().blocks[1].text(), "two");
    assert_eq!(editor.selection().head.byte, 1);
    editor.undo();
    assert_eq!(editor.selection().head.byte, 3);
}

#[test]
fn code_language_updates_at_a_block_preserve_the_original_undo_selection() {
    let mut editor = editor("```rust\nx\n```\nafter");
    editor.move_document_end(false);
    let selection = editor.selection();
    editor.set_code_language_at(0, "typescript");
    assert_eq!(editor.selection(), selection);
    editor.undo();
    assert_eq!(editor.selection(), selection);
    assert_eq!(
        editor.document().blocks[0].kind,
        BlockKind::Code {
            language: "rust".into()
        }
    );
}

#[test]
fn backspace_joins_code_lines_without_splitting_the_code_block() {
    let mut editor = editor("```rust\na\nb\n```");
    let before = editor.document().clone();
    editor.set_selection(Selection::caret(Position { block: 1, byte: 0 }));
    editor.backspace();
    assert_eq!(editor.document().blocks.len(), 1);
    assert_eq!(editor.document().blocks[0].text(), "ab");
    assert_eq!(
        editor.document().blocks[0].kind,
        BlockKind::Code {
            language: "rust".into()
        }
    );
    assert_eq!(editor.selection().head, Position { block: 0, byte: 1 });
    editor.undo();
    assert_eq!(*editor.document(), before);
    assert_eq!(editor.selection().head, Position { block: 1, byte: 0 });
}

#[test]
fn backspace_only_clears_a_wholly_empty_code_block() {
    for source in [
        "```rust\n\nafter\n```",
        "```rust\ntext\n```",
        "before\n```rust\n\nafter\n```",
    ] {
        let mut editor = editor(source);
        let index = editor
            .document()
            .blocks
            .iter()
            .position(|block| matches!(block.kind, BlockKind::Code { .. }))
            .unwrap();
        editor.set_selection(Selection::caret(Position {
            block: index,
            byte: 0,
        }));
        let before = editor.document().clone();
        assert!(editor.backspace().is_none());
        assert_eq!(*editor.document(), before);
    }
    let mut editor = editor("```\n```");
    assert!(editor.backspace().is_some());
    assert_eq!(editor.document().blocks[0].kind, BlockKind::Paragraph);
    editor.undo();
    assert!(matches!(
        editor.document().blocks[0].kind,
        BlockKind::Code { .. }
    ));
}

fn at(block: usize, byte: usize) -> Position {
    Position { block, byte }
}

#[test]
fn a_transaction_composes_edits_into_one_undo_step() {
    let mut editor = editor("# Title\nbody text");
    caret(&mut editor, 1, 4);
    let selection = editor.selection();
    let change = editor
        .transact(TransactionOptions::default(), |tx| {
            tx.delete_range(at(1, 0)..at(1, 5));
            tx.set_block_kind_at(1, BlockKind::Quote);
        })
        .unwrap();
    assert_eq!(change.origin, Origin::Command);
    assert_eq!(editor.document().plain_text(), "Title\ntext");
    assert_eq!(editor.document().blocks[1].kind, BlockKind::Quote);
    editor.undo();
    assert_eq!(editor.document().plain_text(), "Title\nbody text");
    assert_eq!(editor.document().blocks[1].kind, BlockKind::Paragraph);
    assert_eq!(editor.selection(), selection);
    assert!(!editor.can_undo());
    editor.redo();
    assert_eq!(editor.document().plain_text(), "Title\ntext");
    assert_eq!(editor.document().blocks[1].kind, BlockKind::Quote);
}

#[test]
fn an_empty_transaction_publishes_nothing_and_keeps_redo() {
    let mut editor = editor("text");
    editor.move_document_end(false);
    editor.insert_text("!");
    editor.undo();
    assert!(editor.can_redo() && !editor.can_undo());
    let revision = editor.revision();
    assert!(
        editor
            .transact(TransactionOptions::default(), |_| {})
            .is_none()
    );
    assert!(
        editor
            .transact(TransactionOptions::default(), |tx| {
                tx.delete_range(at(0, 2)..at(0, 2));
                tx.set_block_kind_at(9, BlockKind::Quote);
            })
            .is_none()
    );
    assert_eq!(editor.revision(), revision);
    assert!(editor.can_redo() && !editor.can_undo());
    editor.redo();
    assert_eq!(editor.document().plain_text(), "text!");
}

#[test]
fn replace_range_edits_within_and_across_blocks_in_either_order() {
    let mut editor = editor("abcdef");
    assert_eq!(
        editor
            .replace_range(at(0, 1)..at(0, 3), "XYZ")
            .unwrap()
            .origin,
        Origin::Command
    );
    assert_eq!(editor.document().plain_text(), "aXYZdef");
    assert_eq!(editor.selection(), Selection::caret(at(0, 4)));
    editor.undo();
    assert_eq!(editor.document().plain_text(), "abcdef");

    for range in [at(0, 3)..at(2, 2), at(2, 2)..at(0, 3)] {
        let mut editor = Editor::new(Document::from_markdown("first\nsecond\nthird"));
        editor.replace_range(range, "X");
        assert_eq!(editor.document().plain_text(), "firXird");
        assert_eq!(editor.selection(), Selection::caret(at(0, 4)));
    }

    let mut editor = Editor::new(Document::from_markdown("one\ntwo"));
    editor.delete_range(at(0, 3)..at(1, 0));
    assert_eq!(editor.document().plain_text(), "onetwo");
}

#[test]
fn replace_range_snaps_partial_grapheme_clusters() {
    let family = "👨‍👩‍👧‍👦";
    let mut editor = editor(&format!("a{family}b"));
    let inside = 1 + "👨".len();
    editor.replace_range(at(0, inside)..at(0, inside + 1), "z");
    assert_eq!(editor.document().plain_text(), "azb");
    assert_eq!(editor.selection(), Selection::caret(at(0, 2)));

    // An empty range stays an insertion point, floored onto the cluster it sits in.
    let mut editor = Editor::new(Document::from_markdown("e\u{301}x"));
    editor.replace_range(at(0, 1)..at(0, 1), "!");
    assert_eq!(editor.document().plain_text(), "!e\u{301}x");
    // A non-empty range covering part of a cluster takes all of it.
    editor.replace_range(at(0, 2)..at(0, 3), "");
    assert_eq!(editor.document().plain_text(), "!x");
}

#[test]
fn replace_range_maps_positions_around_the_replacement() {
    let mut editor = editor("abcdef");
    let change = editor.replace_range(at(0, 1)..at(0, 3), "XYZ").unwrap();
    for affinity in [Affinity::Before, Affinity::After] {
        assert_eq!(change.mapping.map(at(0, 0), affinity), at(0, 0));
        assert_eq!(change.mapping.map(at(0, 4), affinity), at(0, 5));
    }
    assert_eq!(change.mapping.map(at(0, 2), Affinity::Before), at(0, 1));
    assert_eq!(change.mapping.map(at(0, 2), Affinity::After), at(0, 4));

    let mut editor = Editor::new(Document::from_markdown("first\nsecond\nthird"));
    let change = editor.replace_range(at(0, 3)..at(2, 2), "X").unwrap();
    for affinity in [Affinity::Before, Affinity::After] {
        assert_eq!(change.mapping.map(at(0, 1), affinity), at(0, 1));
        assert_eq!(change.mapping.map(at(2, 4), affinity), at(0, 6));
    }
    assert_eq!(change.mapping.map(at(1, 3), Affinity::Before), at(0, 3));
    assert_eq!(change.mapping.map(at(1, 3), Affinity::After), at(0, 4));
}

#[test]
fn replace_range_follows_link_edges_and_strips_code_formatting() {
    let url = "https://example.com";
    let mut editor = editor(&format!("see [the docs]({url}) end"));
    // Strictly inside the link the replacement joins it.
    editor.replace_range(at(0, 5)..at(0, 7), "HE");
    assert_eq!(editor.document().plain_text(), "see tHE docs end");
    assert_eq!(editor.link_at(at(0, 6)), Some((4..12, url)));
    // Ending at the link's edge, it does not.
    editor.replace_range(at(0, 8)..at(0, 12), "guide");
    assert_eq!(editor.document().plain_text(), "see tHE guide end");
    assert_eq!(editor.link_at(at(0, 6)), Some((4..8, url)));
    assert_eq!(editor.link_at(at(0, 10)), None);

    let mut editor = Editor::new(Document::from_markdown("```rust\nlet x = 1;\n```"));
    editor.replace_range(at(0, 4)..at(0, 5), "**y**");
    assert_eq!(editor.document().blocks[0].text(), "let **y** = 1;");
    assert!(
        editor.document().blocks[0]
            .spans
            .iter()
            .all(|span| span.marks == Marks::default() && span.link.is_none())
    );
}

#[test]
fn set_block_kind_at_keeps_the_selection_and_enforces_block_invariants() {
    let mut editor = editor("- root\n  - child\nparagraph");
    let selection = Selection {
        anchor: at(2, 1),
        head: at(2, 5),
    };
    editor.set_selection(selection);
    editor.set_block_kind_at(1, BlockKind::Heading(2));
    assert_eq!(editor.selection(), selection);
    assert_eq!(editor.document().blocks[1].kind, BlockKind::Heading(2));
    assert_eq!(editor.document().blocks[1].depth, 0);
    assert!(editor.set_block_kind_at(9, BlockKind::Quote).is_none());
    editor.undo();
    assert_eq!(editor.document().blocks[1].depth, 1);
    assert_eq!(editor.selection(), selection);

    let mut editor = Editor::new(Document::from_markdown(
        "**bold** [link](https://example.com)",
    ));
    editor.set_block_kind_at(
        0,
        BlockKind::Code {
            language: "rust".into(),
        },
    );
    assert!(
        editor.document().blocks[0]
            .spans
            .iter()
            .all(|span| span.marks == Marks::default() && span.link.is_none())
    );

    // A divider never holds text, so it only takes on an empty block.
    let mut editor = Editor::new(Document::from_markdown("text"));
    assert!(editor.set_block_kind_at(0, BlockKind::Divider).is_none());
    assert_eq!(editor.document().blocks[0].kind, BlockKind::Paragraph);
    let mut editor = Editor::new(Document::default());
    assert!(editor.set_block_kind_at(0, BlockKind::Divider).is_some());
    assert_eq!(editor.document().blocks[0].kind, BlockKind::Divider);
}

#[test]
fn range_reads_agree_with_the_selection_based_readers() {
    let mut editor = editor("# a **bold** tail\n- item\n中文");
    for (anchor, head) in [
        (at(0, 2), at(0, 6)),
        (at(0, 4), at(2, 3)),
        (at(2, 6), at(1, 2)),
    ] {
        editor.set_selection(Selection { anchor, head });
        let (start, end) = editor.selection().ordered();
        for range in [start..end, end..start] {
            assert_eq!(editor.text_in(range.clone()), editor.selection_text());
            assert_eq!(editor.fragment_in(range), editor.selection_fragment());
        }
    }
}

#[test]
fn every_change_reports_its_origin() {
    let mut editor = editor("text");
    assert_eq!(editor.insert_text("a").unwrap().origin, Origin::Typed);
    assert_eq!(
        editor.insert_text_grouped("b", 1).unwrap().origin,
        Origin::Typed
    );
    assert_eq!(editor.insert_text_plain("c").unwrap().origin, Origin::Typed);
    assert_eq!(editor.delete_forward().unwrap().origin, Origin::Typed);
    assert_eq!(editor.backspace().unwrap().origin, Origin::Typed);
    assert_eq!(editor.delete_word_backward().unwrap().origin, Origin::Typed);
    assert_eq!(editor.delete_word_forward().unwrap().origin, Origin::Typed);
    assert_eq!(
        editor.replace_utf16(0..0, "hello").unwrap().origin,
        Origin::Typed
    );

    editor.set_selection(Selection {
        anchor: at(0, 0),
        head: at(0, 5),
    });
    assert_eq!(
        editor.toggle_mark(Mark::Bold).unwrap().origin,
        Origin::Command
    );
    assert_eq!(
        editor.set_link(Some("https://example.com")).unwrap().origin,
        Origin::Command
    );
    assert_eq!(
        editor.set_block_kind(BlockKind::Quote).unwrap().origin,
        Origin::Command
    );
    assert_eq!(editor.indent().unwrap().origin, Origin::Command);
    assert_eq!(editor.outdent().unwrap().origin, Origin::Command);
    assert_eq!(
        editor.toggle_block_kind(BlockKind::Quote).unwrap().origin,
        Origin::Command
    );
    assert_eq!(
        editor
            .replace_range(at(0, 0)..at(0, 1), "H")
            .unwrap()
            .origin,
        Origin::Command
    );
    assert_eq!(
        editor
            .set_block_kind_at(0, BlockKind::Heading(1))
            .unwrap()
            .origin,
        Origin::Command
    );

    assert_eq!(
        editor
            .insert_fragment(Document::from_markdown("**paste**"))
            .unwrap()
            .origin,
        Origin::Paste
    );
    assert_eq!(
        editor.set_composition(None, "候", None).unwrap().origin,
        Origin::Composition
    );
    assert_eq!(
        editor.commit_composition(None, "候选").unwrap().origin,
        Origin::Composition
    );
    assert_eq!(
        editor.set_composition(None, "临时", None).unwrap().origin,
        Origin::Composition
    );
    assert_eq!(
        editor.cancel_composition().unwrap().origin,
        Origin::Composition
    );
    assert_eq!(editor.undo().unwrap().origin, Origin::History);
    assert_eq!(editor.redo().unwrap().origin, Origin::History);
    assert_eq!(
        editor
            .transact(
                TransactionOptions {
                    origin: Origin::Extension("slash-menu"),
                    ..TransactionOptions::default()
                },
                |tx| tx.insert_text("!"),
            )
            .unwrap()
            .origin,
        Origin::Extension("slash-menu")
    );
}

#[test]
fn a_grouped_transaction_merges_typing_but_not_a_block_kind_change() {
    let group = TransactionOptions {
        group: Some(7),
        origin: Origin::Extension("test"),
    };
    let mut editor = editor("");
    editor.transact(group, |tx| tx.insert_text("a"));
    editor.transact(group, |tx| tx.insert_text("b"));
    editor.undo();
    assert_eq!(editor.document().plain_text(), "");
    assert!(!editor.can_undo());
    editor.redo();
    assert_eq!(editor.document().plain_text(), "ab");

    editor.transact(group, |tx| {
        tx.insert_text("c");
        tx.set_block_kind(BlockKind::Quote);
    });
    editor.transact(group, |tx| tx.insert_text("d"));
    assert_eq!(editor.document().plain_text(), "abcd");
    editor.undo();
    assert_eq!(editor.document().plain_text(), "abc");
    assert_eq!(editor.document().blocks[0].kind, BlockKind::Quote);
    editor.undo();
    assert_eq!(editor.document().plain_text(), "ab");
    assert_eq!(editor.document().blocks[0].kind, BlockKind::Paragraph);
}

#[test]
fn mapping_keeps_the_edges_of_a_replaced_range_and_collapses_only_its_interior() {
    let mut editor = editor("abcdef");
    let change = editor.replace_range(at(0, 1)..at(0, 3), "XYZ").unwrap();
    assert_eq!(editor.document().plain_text(), "aXYZdef");
    for affinity in [Affinity::Before, Affinity::After] {
        assert_eq!(change.mapping.map(at(0, 0), affinity), at(0, 0));
        assert_eq!(change.mapping.map(at(0, 1), affinity), at(0, 1));
        assert_eq!(change.mapping.map(at(0, 3), affinity), at(0, 4));
        assert_eq!(change.mapping.map(at(0, 4), affinity), at(0, 5));
        assert_eq!(change.mapping.map(at(0, 6), affinity), at(0, 7));
    }
    assert_eq!(change.mapping.map(at(0, 2), Affinity::Before), at(0, 1));
    assert_eq!(change.mapping.map(at(0, 2), Affinity::After), at(0, 4));
}

#[test]
fn mapping_moves_a_position_at_a_pure_insertion_only_with_after_affinity() {
    let mut editor = editor("abcdef");
    let change = editor.replace_range(at(0, 2)..at(0, 2), "XY").unwrap();
    assert_eq!(editor.document().plain_text(), "abXYcdef");
    assert_eq!(change.mapping.map(at(0, 2), Affinity::Before), at(0, 2));
    assert_eq!(change.mapping.map(at(0, 2), Affinity::After), at(0, 4));
    for affinity in [Affinity::Before, Affinity::After] {
        assert_eq!(change.mapping.map(at(0, 1), affinity), at(0, 1));
        assert_eq!(change.mapping.map(at(0, 3), affinity), at(0, 5));
        assert_eq!(
            change.mapping.map_tracked(at(0, 2), affinity),
            Some(change.mapping.map(at(0, 2), affinity))
        );
    }
}

#[test]
fn mapping_follows_block_splits_and_joins() {
    let mut editor = editor("ab");
    caret(&mut editor, 0, 1);
    let split = editor.insert_text("\n").unwrap();
    assert_eq!(editor.document().plain_text(), "a\nb");
    assert_eq!(split.mapping.map(at(0, 1), Affinity::Before), at(0, 1));
    assert_eq!(split.mapping.map(at(0, 1), Affinity::After), at(1, 0));
    for affinity in [Affinity::Before, Affinity::After] {
        assert_eq!(split.mapping.map(at(0, 0), affinity), at(0, 0));
        assert_eq!(split.mapping.map(at(0, 2), affinity), at(1, 1));
    }

    let mut editor = Editor::new(Document::from_markdown("a\nb"));
    let join = editor.delete_range(at(0, 1)..at(1, 0)).unwrap();
    assert_eq!(editor.document().plain_text(), "ab");
    for affinity in [Affinity::Before, Affinity::After] {
        // The removed newline is a non-empty range, so both of its edges survive.
        assert_eq!(join.mapping.map(at(0, 1), affinity), at(0, 1));
        assert_eq!(join.mapping.map(at(1, 0), affinity), at(0, 1));
        assert_eq!(join.mapping.map(at(1, 1), affinity), at(0, 2));
        assert_eq!(join.mapping.map_tracked(at(0, 1), affinity), Some(at(0, 1)));
        assert_eq!(join.mapping.map_tracked(at(1, 0), affinity), Some(at(0, 1)));
    }
}

#[test]
fn tracked_mapping_drops_positions_inside_deleted_text() {
    let mut editor = editor("abcdef");
    let change = editor.replace_range(at(0, 1)..at(0, 3), "XYZ").unwrap();
    for affinity in [Affinity::Before, Affinity::After] {
        assert_eq!(
            change.mapping.map_tracked(at(0, 0), affinity),
            Some(at(0, 0))
        );
        assert_eq!(
            change.mapping.map_tracked(at(0, 1), affinity),
            Some(at(0, 1))
        );
        assert_eq!(change.mapping.map_tracked(at(0, 2), affinity), None);
        assert_eq!(
            change.mapping.map_tracked(at(0, 3), affinity),
            Some(at(0, 4))
        );
        assert_eq!(
            change.mapping.map_tracked(at(0, 4), affinity),
            Some(at(0, 5))
        );
    }

    let mut editor = Editor::new(Document::from_markdown("abcdef"));
    let change = editor
        .transact(TransactionOptions::default(), |tx| {
            tx.delete_range(at(0, 4)..at(0, 6));
            tx.delete_range(at(0, 0)..at(0, 2));
        })
        .unwrap();
    assert_eq!(editor.document().plain_text(), "cd");
    for affinity in [Affinity::Before, Affinity::After] {
        assert_eq!(change.mapping.map_tracked(at(0, 1), affinity), None);
        assert_eq!(change.mapping.map_tracked(at(0, 5), affinity), None);
        assert_eq!(
            change.mapping.map_tracked(at(0, 3), affinity),
            Some(at(0, 1))
        );
        assert_eq!(
            change.mapping.map_tracked(at(0, 4), affinity),
            Some(at(0, 2))
        );
        assert_eq!(
            change.mapping.map_tracked(at(0, 6), affinity),
            Some(at(0, 2))
        );
    }
}

#[test]
fn undo_and_redo_mappings_round_trip_positions_outside_the_replacement() {
    let mut editor = editor("abcdef");
    let forward = editor.replace_range(at(0, 1)..at(0, 3), "XYZ").unwrap();
    let backward = editor.undo().unwrap();
    assert_eq!(editor.document().plain_text(), "abcdef");
    for byte in [0, 1, 3, 4, 6] {
        for affinity in [Affinity::Before, Affinity::After] {
            let mapped = forward.mapping.map(at(0, byte), affinity);
            assert_eq!(backward.mapping.map(mapped, affinity), at(0, byte));
        }
    }
}

#[test]
fn a_merged_typing_group_maps_positions_across_every_keystroke() {
    let mut editor = editor("xz");
    caret(&mut editor, 0, 1);
    for text in ["a", "b", "c"] {
        editor.insert_text_grouped(text, 1);
    }
    assert_eq!(editor.document().plain_text(), "xabcz");
    let undo = editor.undo().unwrap();
    assert_eq!(editor.document().plain_text(), "xz");
    assert_eq!(undo.mapping.map(at(0, 4), Affinity::After), at(0, 1));
    assert_eq!(undo.mapping.map(at(0, 5), Affinity::After), at(0, 2));
    let redo = editor.redo().unwrap();
    // The caret still ends up after the whole merged insertion.
    assert_eq!(redo.mapping.map(at(0, 1), Affinity::After), at(0, 4));
    assert_eq!(redo.mapping.map(at(0, 1), Affinity::Before), at(0, 1));
    assert_eq!(
        redo.mapping.map_tracked(at(0, 1), Affinity::After),
        Some(at(0, 4))
    );
}

#[test]
fn undo_and_redo_restore_the_caret_around_typed_text() {
    let mut editor = editor("xz");
    caret(&mut editor, 0, 1);
    editor.insert_text("abc");
    assert_eq!(editor.selection(), Selection::caret(at(0, 4)));
    editor.undo();
    assert_eq!(editor.selection(), Selection::caret(at(0, 1)));
    editor.redo();
    assert_eq!(editor.selection(), Selection::caret(at(0, 4)));
}

#[test]
fn mapping_clamps_out_of_range_and_mid_grapheme_positions() {
    let family = "👨‍👩‍👧‍👦";
    let mut editor = editor(&format!("a{family}\nb"));
    let change = editor.replace_range(at(0, 0)..at(0, 1), "Z").unwrap();
    for affinity in [Affinity::Before, Affinity::After] {
        assert_eq!(change.mapping.map(at(9, usize::MAX), affinity), at(1, 1));
        assert_eq!(
            change.mapping.map_tracked(at(9, usize::MAX), affinity),
            Some(at(1, 1))
        );
        // Raw bytes: a position inside a cluster maps by byte and never panics.
        assert_eq!(change.mapping.map(at(0, 3), affinity), at(0, 3));
        assert_eq!(editor.document().clamp_position(at(0, 3)), at(0, "Z".len()));
    }
}

#[test]
fn text_in_slices_across_blocks_like_the_whole_document() {
    let editor = editor("# a **bold** tail\n- item\n中文\n```\n\ncode\n```\nlast");
    let document = editor.document();
    let plain = document.plain_text();
    let positions: Vec<Position> = document
        .blocks
        .iter()
        .enumerate()
        .flat_map(|(block, content)| {
            let text = content.text();
            let mut bytes: Vec<usize> = text.grapheme_indices(true).map(|(i, _)| i).collect();
            bytes.push(text.len());
            bytes.into_iter().map(move |byte| Position { block, byte })
        })
        .collect();
    assert!(document.blocks.len() > 4);
    for &start in &positions {
        for &end in &positions {
            let (low, high) = (start.min(end), start.max(end));
            let expected = &plain[document.global_byte(low)..document.global_byte(high)];
            assert_eq!(editor.text_in(start..end), expected, "{start:?}..{end:?}");
        }
    }
}

#[test]
fn finish_composition_reports_whether_it_committed() {
    let mut editor = editor("ab");
    assert!(!editor.finish_composition());
    caret(&mut editor, 0, 1);
    editor.set_composition(None, "候", None);
    let revision = editor.revision();
    assert!(editor.finish_composition());
    assert_eq!(editor.revision(), revision);
    assert_eq!(editor.committed_document().plain_text(), "a候b");
    assert!(!editor.finish_composition());

    // A composition that never changed the document has nothing to commit.
    editor.set_composition(None, "", None);
    assert!(editor.is_composing());
    assert!(!editor.finish_composition());
    assert!(!editor.is_composing());
    editor.undo();
    assert_eq!(editor.document().plain_text(), "ab");
    assert!(!editor.can_undo());
}

#[test]
fn a_history_entry_retains_one_document_and_a_compact_mapping() {
    let text = "x".repeat(64 * 1024);
    let mut editor = editor(&text);
    editor.move_document_end(false);
    editor.insert_text("!");
    assert_eq!(editor.undo.len(), 1);
    let entry = editor.undo.last().unwrap();
    let snapshot = document_bytes(&entry.state.document);
    assert!(snapshot >= text.len());
    let overhead = entry.retained_bytes() - snapshot;
    assert!(overhead < 1024, "mapping retained {overhead} bytes");
}

#[test]
fn a_transaction_commits_an_active_composition_exactly_once() {
    let mut editor = editor("ab");
    caret(&mut editor, 0, 1);
    editor.set_composition(None, "候选", None);
    assert!(editor.is_composing());
    assert_eq!(editor.committed_document().plain_text(), "ab");
    editor
        .transact(TransactionOptions::default(), |tx| tx.insert_text("!"))
        .unwrap();
    assert!(!editor.is_composing());
    assert_eq!(editor.document().plain_text(), "a候选!b");
    assert_eq!(editor.committed_document().plain_text(), "a候选!b");
    editor.undo();
    assert_eq!(editor.document().plain_text(), "a候选b");
    editor.undo();
    assert_eq!(editor.document().plain_text(), "ab");
    assert!(!editor.can_undo());
}

#[test]
fn an_undo_group_folds_structural_edits_and_typing_into_one_entry() {
    let mut editor = editor("- item");
    caret(&mut editor, 0, 4);
    editor.begin_undo_group();
    assert!(editor.is_undo_grouping());
    editor.insert_text_grouped(" one", 1);
    // Enter opens a new list item: a block-count change no typing group may cross.
    editor.insert_text("\n");
    editor.insert_text_grouped("two", 2);
    // A heading input rule owns its own boundary outside a group.
    editor.insert_text("\n");
    editor.insert_text_grouped("#", 3);
    editor.insert_text_grouped(" ", 3);
    editor.insert_text_grouped("head", 4);
    let end = editor.selection().head;
    editor.end_undo_group();
    assert!(!editor.is_undo_grouping());
    assert_eq!(editor.document().to_markdown(), "- item one\n- two\n# head");

    let change = editor.undo().unwrap();
    assert_eq!(editor.document().to_markdown(), "- item");
    assert_eq!(editor.selection().head, at(0, 4));
    assert_eq!(change.mapping.map(end, Affinity::After), at(0, 4));
    assert!(!editor.can_undo());
    let change = editor.redo().unwrap();
    assert_eq!(editor.document().to_markdown(), "- item one\n- two\n# head");
    assert_eq!(change.mapping.map(at(0, 4), Affinity::After), end);
    assert!(!editor.can_redo());
}

#[test]
fn an_undo_group_starts_at_its_first_edit_and_ends_on_history() {
    let mut editor = editor("ab");
    caret(&mut editor, 0, 2);
    editor.insert_text_plain("c");
    // Grouping does not reach back to entries made before it opened.
    editor.begin_undo_group();
    editor.insert_text_plain("d");
    editor.insert_text_plain("\n");
    editor.insert_text_plain("e");
    // Undo closes the group and takes the whole of it in one step.
    editor.undo();
    assert_eq!(editor.document().plain_text(), "abc");
    assert!(!editor.is_undo_grouping());
    editor.insert_text_plain("f");
    editor.insert_text_plain("g");
    editor.undo();
    assert_eq!(editor.document().plain_text(), "abcf");

    // An empty group leaves nothing behind.
    let mut empty = super::tests::editor("x");
    empty.begin_undo_group();
    empty.end_undo_group();
    assert!(!empty.can_undo());
}

#[test]
fn an_undo_group_folds_a_committed_composition() {
    let mut editor = editor("");
    editor.begin_undo_group();
    editor.insert_text_plain("a");
    editor.set_composition(None, "ni", None);
    editor.commit_composition(None, "你");
    editor.insert_text_plain("b");
    editor.end_undo_group();
    assert_eq!(editor.document().plain_text(), "a你b");
    editor.undo();
    assert_eq!(editor.document().plain_text(), "");
    assert!(!editor.can_undo());
}

#[test]
fn blank_lines_come_back_as_empty_blocks_at_every_nesting() {
    let source = "# h\n\n\npara\n\n- a\n\n- b\n\n> q\n> \n> r";
    let document = Document::from_markdown(source);
    // A blank line inside a quote stays quoted; one between list items does not, so the
    // list keeps the blank line that separates its items.
    let kinds: Vec<_> = document.blocks.iter().map(|block| &block.kind).collect();
    assert_eq!(
        kinds,
        [
            &BlockKind::Heading(1),
            &BlockKind::Paragraph,
            &BlockKind::Paragraph,
            &BlockKind::Paragraph,
            &BlockKind::Paragraph,
            &BlockKind::Bullet,
            &BlockKind::Paragraph,
            &BlockKind::Bullet,
            &BlockKind::Paragraph,
            &BlockKind::Quote,
            &BlockKind::Quote,
            &BlockKind::Quote,
        ]
    );
    assert_eq!(document.plain_text(), "h\n\n\npara\n\na\n\nb\n\nq\n\nr");
    assert_eq!(document.to_markdown(), source);
}

#[test]
fn every_source_line_is_its_own_block_however_the_lines_join() {
    // Soft breaks, hard breaks and a trailing backslash all end a block.
    let document = Document::from_markdown("one\ntwo  \nthree\\\nfour");
    assert_eq!(document.plain_text(), "one\ntwo\nthree\nfour");
    assert!(
        document
            .blocks
            .iter()
            .all(|block| block.kind == BlockKind::Paragraph)
    );
    // A line that repeats none of its container's markers leaves the container, where
    // CommonMark would read it as part of the block above.
    let source = "> quote\nlazy\n- item\ncontinued";
    let document = Document::from_markdown(source);
    let kinds: Vec<_> = document.blocks.iter().map(|block| &block.kind).collect();
    assert_eq!(
        kinds,
        [
            &BlockKind::Quote,
            &BlockKind::Paragraph,
            &BlockKind::Bullet,
            &BlockKind::Paragraph,
        ]
    );
    assert_eq!(document.to_markdown(), source);
}

#[test]
fn a_list_and_a_quote_inside_each_other_keep_the_outer_one_and_read_the_rest() {
    let document = Document::from_markdown("> - item\n> - two\n- > quoted\n  - > deep");
    let shape: Vec<_> = document
        .blocks
        .iter()
        .map(|block| (&block.kind, block.depth, block.text()))
        .collect();
    assert_eq!(
        shape,
        [
            (&BlockKind::Quote, 0, "- item".to_owned()),
            (&BlockKind::Quote, 0, "- two".to_owned()),
            (&BlockKind::Bullet, 0, "> quoted".to_owned()),
            (&BlockKind::Bullet, 1, "> deep".to_owned()),
        ]
    );
    assert_eq!(Document::from_markdown(&document.to_markdown()), document);
}

#[test]
fn an_indented_code_block_imports_as_code_lines_without_a_language() {
    let document = Document::from_markdown("    indented\n    more\n\ntext");
    let code = BlockKind::Code {
        language: String::new(),
    };
    assert!(document.blocks[..2].iter().all(|block| block.kind == code));
    assert_eq!(document.plain_text(), "indented\nmore\ntext");
    // A code block inside a list item keeps its content and loses the nesting.
    let document = Document::from_markdown("- item\n\n  ```rust\n  code\n  ```");
    assert_eq!(document.to_markdown(), "- item\n\n```rust\ncode\n```");
}

#[test]
fn escapes_entities_and_edge_whitespace_survive_a_round_trip() {
    let document = Document::from_markdown("a \\* b &amp; c &copy;");
    assert_eq!(document.plain_text(), "a * b & c ©");
    assert_eq!(Document::from_markdown(&document.to_markdown()), document);
    let kinds = [
        BlockKind::Paragraph,
        BlockKind::Bullet,
        BlockKind::Quote,
        BlockKind::Heading(2),
    ];
    let block = |kind: &BlockKind, text: &str| {
        let mut document = Document {
            blocks: vec![Block {
                kind: kind.clone(),
                depth: 0,
                spans: vec![Span {
                    text: text.to_owned(),
                    marks: Marks::default(),
                    link: None,
                }],
            }],
        };
        document.normalize();
        document
    };
    // Text a reader would take for a character reference or an indented code block is
    // written so it comes back unchanged.
    for text in ["    four", "\tfour", "   \tfour", "&amp; &#32; \\* a"] {
        for kind in &kinds {
            let document = block(kind, text);
            let markdown = document.to_markdown();
            assert!(!markdown.contains("\n"), "{text:?} as {markdown:?}");
            assert_eq!(
                Document::from_markdown(&markdown),
                document,
                "{text:?} as {markdown:?}"
            );
        }
    }
    // Trailing whitespace is not worth an entity in the file: a reader strips it, and
    // nothing else about the block changes.
    for (text, kept) in [
        ("trail  ", "trail"),
        ("both\t", "both"),
        (" ", ""),
        ("  ", ""),
    ] {
        for kind in &kinds {
            let document = block(kind, text);
            let reloaded = Document::from_markdown(&document.to_markdown());
            assert_eq!(reloaded.plain_text(), kept, "{text:?} as {kind:?}");
            assert_eq!(reloaded.blocks[0].kind, *kind, "{text:?}");
        }
    }
    // Lesser indentation survives on a paragraph, which owns its line, and is stripped
    // inside a container, which owns the space after its marker.
    assert_eq!(
        block(&BlockKind::Paragraph, "  lead").to_markdown(),
        "  lead"
    );
    assert_eq!(Document::from_markdown("  lead").plain_text(), "  lead");
    assert_eq!(Document::from_markdown("-   lead").plain_text(), "lead");
}

#[test]
fn constructs_with_no_model_keep_their_source_text() {
    for source in [
        "| a | b |\n|---|---|\n| c | d |",
        "<div>\nraw\n</div>",
        "[ref]: https://example.com",
        "![alt](image.png)",
        "term\n: definition",
        "[^1]: a footnote",
        "<br>",
        "$$\nx = 1\n$$",
    ] {
        let document = Document::from_markdown(source);
        assert_eq!(document.plain_text(), source, "{source:?}");
        assert_eq!(
            Document::from_markdown(&document.to_markdown()),
            document,
            "{source:?}"
        );
    }
}

#[test]
fn canonical_markdown_survives_a_round_trip_unchanged() {
    for source in [
        "# h\n\n\npara\n\n- a\n\n- b\n\n> q\n> \n> r",
        "- first\n- \n- [ ] \n1. \n",
        "- first\n    - child\n        - grandchild",
        "1. one\n2. two\nbreak\n1. again",
        "> first\n> second\n",
        "see [the **docs**](https://example.com/a_(b)) and [x](<a b>)",
        "~~gone~~ and <u>**kept**</u>",
        "# Title\n- **bold** and *italic* and `code`\n- [ ] todo\n- [x] done\n\n***both***",
        "```rust\ncode\n\nmore\n```",
        "text\n---\nmore",
        "Title\n=====",
    ] {
        let document = Document::from_markdown(source);
        assert_eq!(document.to_markdown(), source, "{source:?}");
        assert_eq!(
            Document::from_markdown(&document.to_markdown()),
            document,
            "{source:?}"
        );
    }
}

#[test]
fn link_destinations_survive_repeated_markdown_round_trips() {
    for url in [
        "https://example.com/?q=&copy;",
        "https://example.com/?q=&amp;&#32;",
        r"https://example.com/a\(b)",
        "https://example.com/<tag>",
        "https://example.com/a b&copy;",
        "https://example.com/a(b",
        "a\nb",
        "a\rb",
        "a\u{1}b",
        "a\u{7f}b",
        "  a b  ",
        "",
    ] {
        let document = Document {
            blocks: vec![Block {
                kind: BlockKind::Paragraph,
                depth: 0,
                spans: vec![Span {
                    text: "link".into(),
                    marks: Marks::default(),
                    link: Some(url.into()),
                }],
            }],
        };
        let mut reloaded = document.clone();
        for _ in 0..3 {
            let markdown = reloaded.to_markdown();
            reloaded = Document::from_markdown(&markdown);
            assert_eq!(reloaded, document, "{url:?} as {markdown:?}");
        }
    }
    let source = "[x](https://example.com/?q=&amp;copy;)";
    let document = Document::from_markdown(source);
    assert_eq!(
        document.blocks[0].spans[0].link.as_deref(),
        Some("https://example.com/?q=&copy;")
    );
    assert_eq!(Document::from_markdown(&document.to_markdown()), document);
}

#[test]
fn formatting_next_to_unicode_marks_and_symbols_round_trips() {
    for neighbour in ["a\u{301}", "\u{200d}", "中", "。", "©", "👩"] {
        for text in ["!", "!word", "word!", "&"] {
            for bits in 1..32 {
                let formatted = Span {
                    text: text.into(),
                    marks: Marks {
                        bold: bits & 1 != 0,
                        italic: bits & 2 != 0,
                        code: bits & 4 != 0,
                        strikethrough: bits & 8 != 0,
                        underline: bits & 16 != 0,
                    },
                    link: None,
                };
                let plain = Span {
                    text: neighbour.into(),
                    marks: Marks::default(),
                    link: None,
                };
                for spans in [
                    vec![plain.clone(), formatted.clone()],
                    vec![formatted.clone(), plain.clone()],
                ] {
                    let document = Document {
                        blocks: vec![Block {
                            kind: BlockKind::Paragraph,
                            depth: 0,
                            spans,
                        }],
                    };
                    let markdown = document.to_markdown();
                    assert_eq!(
                        Document::from_markdown(&markdown),
                        document,
                        "{bits}: {markdown:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn markdown_fragment_paste_keeps_word_separators_and_undoes_atomically() {
    for (source, expected) in [
        ("hello ", "hello tail"),
        ("**bold** ", "bold tail"),
        ("hello\t", "hello\ttail"),
        (" \t", " \ttail"),
        ("hello\n  ", "hello\n  tail"),
    ] {
        let mut editor = Editor::new(Document::from_markdown("tail"));
        editor.insert_fragment(Document::from_markdown_fragment(source));
        assert_eq!(editor.document().plain_text(), expected, "{source:?}");
        editor.undo();
        assert_eq!(editor.document().plain_text(), "tail");
        editor.redo();
        assert_eq!(editor.document().plain_text(), expected);
    }
}
