//! Source-preserving persistence and protected-syntax regression cases.

use markraft_commonmark::{SourceDocument, SourceError, commonmark_schema};

fn edit(original: &str, edited: &str) -> Result<String, SourceError> {
    let schema = commonmark_schema();
    let source = SourceDocument::parse(&schema, original).unwrap();
    let target = SourceDocument::parse(&schema, edited).unwrap();
    source.render(&schema, target.document())
}

#[test]
fn unchanged_source_is_byte_identical() {
    for source in [
        "\u{feff}---\r\nid: user-owned\r\npinned: false\r\n---\r\nTitle\r\n=====\r\n\r\ntext  \r\n\r\n",
        "[unused]: /target \"title\"\n\n[link][unused]\n\n",
        "\tcode\n\n+ one\n+ two\n",
        "![[image.png|100]]\n\n> [!note]\n> contents\n\nparagraph ^block-id\n",
        "---\ninvalid: yaml\nwithout closing delimiter",
        "paragraph\r\n\r\nnext\n\nlast\r",
        "",
        "  \n\n",
    ] {
        let schema = commonmark_schema();
        let source = SourceDocument::parse(&schema, source).unwrap();
        assert_eq!(
            source.render(&schema, source.document()).unwrap(),
            source.source()
        );
    }
}

#[test]
fn editing_text_preserves_frontmatter_setext_references_and_trivia() {
    let original = "\u{feff}---\r\nid: custom\r\ncreated: yesterday\r\n---\r\nHeading\r\n=======\r\n\r\n\r\nRead [the docs][d] for details.\r\n\r\n[d]: <https://example.org> \"Docs\"\r\n\r\n";
    let expected = original.replace("details", "examples");
    assert_eq!(edit(original, &expected).unwrap(), expected);
}

#[test]
fn local_marks_preserve_neighboring_reference_spelling() {
    let original = "Read [docs][d] and the guide.\n\n[d]: /docs\n";
    let expected = original.replace("guide", "**guide**");
    assert_eq!(edit(original, &expected).unwrap(), expected);
}

#[test]
fn edits_with_duplicate_text_find_the_semantically_correct_position() {
    let original = "same [same][ref] same\n\n[ref]: /same\n";
    let expected = "same [same][ref] changed\n\n[ref]: /same\n";
    assert_eq!(edit(original, expected).unwrap(), expected);
}

#[test]
fn text_in_nested_lists_and_quotes_keeps_source_prefixes() {
    for original in [
        "+ one\n+ two\n  + 中文🙂\n  + three\n",
        "> text\n>\n> nested **strong** phrase\n",
        "    code\n    next\n",
    ] {
        let expected = original
            .replace("中文🙂", "中文🚀")
            .replace("phrase", "sentence")
            .replace("next", "changed");
        assert_eq!(edit(original, &expected).unwrap(), expected);
    }
}

#[test]
fn multiple_changed_blocks_keep_untouched_blocks_and_blank_lines() {
    let original = "First old\n\n\nMiddle _original_\n\nLast old\n\n";
    let expected = original.replace("old", "new");
    assert_eq!(edit(original, &expected).unwrap(), expected);
}

#[test]
fn untouched_extension_tokens_survive_adjacent_text_edits() {
    for original in [
        "Read [[page|label]] and old\n",
        "> [!note]\n> old text\n",
        "old text ^stable-id\n",
        "old text with $x + y$\n",
    ] {
        let expected = original.replace("old", "new");
        assert_eq!(edit(original, &expected).unwrap(), expected);
    }
}

#[test]
fn modifications_inside_unsupported_extension_syntax_are_refused() {
    for original in [
        "Read [[old]] here\n",
        "> [!old]\n> contents\n",
        "text ^old\n",
        "math $old$\n",
    ] {
        assert_eq!(
            edit(original, &original.replace("old", "new")),
            Err(SourceError::UnsupportedEdit)
        );
    }
}

#[test]
fn structural_edits_preserve_outside_source() {
    for (original, expected) in [
        (
            "# Title\n\nold\n\nLast\n",
            "# Title\n\nnew\n\nextra\n\nLast\n",
        ),
        ("First\n\nLast\n", "First\n\nMiddle\n\nLast\n"),
        ("First\n", "First\n\nLast\n"),
        ("Last\n", "First\n\nLast\n"),
        ("First\n\nMiddle\n\nLast\n", "First\n\nLast\n"),
    ] {
        let result = edit(original, expected).unwrap();
        let schema = commonmark_schema();
        assert_eq!(
            SourceDocument::parse(&schema, &result).unwrap().document(),
            SourceDocument::parse(&schema, expected).unwrap().document()
        );
        assert!(result.ends_with('\n'));
    }
}

#[test]
fn structural_changes_cannot_remove_unrepresented_definitions() {
    let original = "first\n\n[unused]: /keep\n\nlast\n";
    assert_eq!(
        edit(original, "replacement"),
        Err(SourceError::UnsupportedEdit)
    );
}

#[test]
fn undo_after_a_render_restores_original_spelling() {
    let schema = commonmark_schema();
    let original = "Title\n=====\n\n__strong__\n";
    let source = SourceDocument::parse(&schema, original).unwrap();
    let edited = SourceDocument::parse(&schema, "Title\n=====\n\n__stronger__\n").unwrap();
    assert!(
        source
            .render(&schema, edited.document())
            .unwrap()
            .contains("__stronger__")
    );
    assert_eq!(source.render(&schema, source.document()).unwrap(), original);
}

#[test]
fn new_file_and_frontmatter_only_draft_keep_the_preamble() {
    for original in ["", "\u{feff}", "---\r\ntitle: custom\r\n---\r\n"] {
        let expected = format!("{original}hello");
        assert_eq!(edit(original, &expected).unwrap(), expected);
    }
}

#[test]
fn separate_edits_in_one_paragraph_do_not_rewrite_the_reference_between_them() {
    let original = "first [docs][d] last\n\n[d]: /docs\n";
    let expected = original
        .replace("first", "initial")
        .replace("last", "final");
    assert_eq!(edit(original, &expected).unwrap(), expected);
}

#[test]
fn removing_an_existing_underscore_mark_keeps_neighboring_source() {
    let original = "__strong__ and [docs][d]\n\n[d]: /docs\n";
    let expected = original.replace("__strong__", "strong");
    assert_eq!(edit(original, &expected).unwrap(), expected);
}

#[test]
fn inserting_and_removing_text_inside_a_table_keeps_column_padding() {
    let original = "| First    | Second |\n| :------- | -----: |\n| old      | value  |\n";
    let expected = original.replace("old", "new");
    assert_eq!(edit(original, &expected).unwrap(), expected);
}

#[test]
fn lengthening_a_table_cell_does_not_repad_its_other_cells() {
    let original = "| First    | Second |\n| :------- | -----: |\n| old      | value  |\n";
    let expected = original.replace("old", "a considerably longer text");
    assert_eq!(edit(original, &expected).unwrap(), expected);
}

#[test]
fn new_content_keeps_an_existing_reference_only_document() {
    let schema = commonmark_schema();
    let original = "[unused]: /keep\n";
    let source = SourceDocument::parse(&schema, original).unwrap();
    let target = SourceDocument::parse(&schema, "hello").unwrap();
    let result = source.render(&schema, target.document()).unwrap();
    assert!(result.starts_with(original));
    assert!(result.ends_with("hello"));
}

#[test]
fn edits_keep_bare_cr_and_mixed_newline_separators() {
    for original in ["old\r\rnext\r", "old\r\n\r\nnext\n\nlast\r"] {
        let expected = original.replace("old", "new");
        assert_eq!(edit(original, &expected).unwrap(), expected);
    }
}

#[test]
fn typed_markdown_metacharacters_do_not_accidentally_add_marks() {
    let schema = commonmark_schema();
    let source = SourceDocument::parse(&schema, "hello").unwrap();
    let target = SourceDocument::parse(&schema, r"hello \*world\*").unwrap();
    let result = source.render(&schema, target.document()).unwrap();
    assert_eq!(
        SourceDocument::parse(&schema, &result).unwrap().document(),
        target.document()
    );
}

#[test]
fn long_repeated_text_still_locates_an_edit_without_unbounded_search() {
    let original = "a".repeat(1000);
    let expected = format!("{original}b");
    assert_eq!(edit(&original, &expected).unwrap(), expected);
}

#[test]
fn syntax_like_text_inside_code_literals_remains_editable() {
    for original in [
        "```sh\necho $old\n```\n",
        "    echo $old\n",
        "Run `echo $old` here\n",
        "```text\n[[old]]\n[old]: /url\n```\n",
    ] {
        let expected = original.replace("old", "new");
        assert_eq!(edit(original, &expected).unwrap(), expected);
    }
}

fn editor_at_end(source: &SourceDocument) -> markraft_core::EditorState {
    let schema = commonmark_schema();
    let mut end = 1;
    source.document().descendants(&mut |node, pos, _, _| {
        if node.is_textblock(&schema) {
            end = pos + 1 + node.content_size();
        }
        true
    });
    markraft_core::EditorState::create(
        markraft_core::EditorStateConfig::new(schema)
            .doc(source.document().clone())
            .selection(markraft_core::Selection::cursor(end)),
    )
    .unwrap()
}

fn enter_command(state: &markraft_core::EditorState) -> markraft_core::commands::Command {
    use markraft_core::commands::*;
    let item = state
        .schema()
        .node_id(markraft_commonmark::schema::LIST_ITEM)
        .unwrap();
    chain(vec![
        split_list_item(item),
        lift_list_item(item),
        new_line_in_code(),
        create_paragraph_near(),
        lift_empty_block(),
        split_block_keep_marks(),
    ])
}

fn applied(
    state: &markraft_core::EditorState,
    command: markraft_core::commands::Command,
) -> markraft_core::EditorState {
    markraft_core::commands::run_command(state, &command)
        .expect("command applies")
        .unwrap()
        .state()
        .clone()
}

#[test]
fn real_enter_commands_leave_an_existing_list_and_type_a_plain_paragraph() {
    let schema = commonmark_schema();
    let original = "# Shared Markdown\n\nUse Markraft alongside your other editors.\n\n## Quick checks\n\n- Open existing files\n- Keep filenames stable\n- Choose where new images go\n";
    let source = SourceDocument::parse(&schema, original).unwrap();
    let state = editor_at_end(&source);
    let state = applied(&state, enter_command(&state));
    let rendered = source.render(&schema, state.doc());
    assert!(
        rendered.is_ok(),
        "first Return: {} -> {rendered:?}",
        markraft_commonmark::to_markdown(&schema, state.doc())
    );
    let state = applied(&state, enter_command(&state));
    let rendered = source.render(&schema, state.doc());
    assert!(
        rendered.is_ok(),
        "second Return: {} -> {rendered:?}",
        markraft_commonmark::to_markdown(&schema, state.doc())
    );
    let state = applied(
        &state,
        markraft_core::commands::insert_text("A plain paragraph"),
    );
    let rendered = source.render(&schema, state.doc()).unwrap();
    assert_eq!(rendered, format!("{original}\nA plain paragraph\n"));
}

#[test]
fn real_enter_and_typing_commands_preserve_empty_paragraphs() {
    let schema = commonmark_schema();
    for original in ["", "hello\n", "# Heading\n"] {
        let source = SourceDocument::parse(&schema, original).unwrap();
        let mut state = editor_at_end(&source);
        for _ in 0..2 {
            state = applied(&state, enter_command(&state));
            assert!(
                source.render(&schema, state.doc()).is_ok(),
                "Return in {original:?}: {}",
                markraft_commonmark::to_markdown(&schema, state.doc())
            );
        }
        state = applied(&state, markraft_core::commands::insert_text("typed"));
        assert!(source.render(&schema, state.doc()).is_ok());
    }
}

#[test]
fn typing_and_backspace_work_after_an_empty_paragraph_has_been_saved() {
    let schema = commonmark_schema();
    let source = SourceDocument::parse(&schema, "hello\n").unwrap();
    let state = editor_at_end(&source);
    let empty = applied(&state, enter_command(&state));
    let saved = source.render(&schema, empty.doc()).unwrap();
    let source = SourceDocument::parse(&schema, &saved).unwrap();
    let typed = applied(&empty, markraft_core::commands::insert_text("中文🙂"));
    assert_eq!(
        source.render(&schema, typed.doc()).unwrap(),
        "hello\n\n中文🙂\n"
    );
    let joined = applied(&empty, markraft_core::commands::join_backward());
    let rendered = source.render(&schema, joined.doc()).unwrap();
    assert_eq!(
        SourceDocument::parse(&schema, &rendered)
            .unwrap()
            .document(),
        state.doc()
    );
}

#[test]
fn typing_into_a_saved_empty_paragraph_replaces_its_marker() {
    let schema = commonmark_schema();
    for original in ["<br>\n", "---\ntitle: mine\n---\n<br>\n"] {
        let source = SourceDocument::parse(&schema, original).unwrap();
        let state = editor_at_end(&source);
        let typed = applied(&state, markraft_core::commands::insert_text("hello"));
        assert_eq!(
            source.render(&schema, typed.doc()).unwrap(),
            original.replace("<br>", "hello")
        );
    }
}

#[test]
fn real_enter_commands_preserve_existing_list_markers_and_code_source() {
    let schema = commonmark_schema();
    for original in ["+ one\n", "3. one\n", "```rust\nlet a = 1;\n```\n"] {
        let source = SourceDocument::parse(&schema, original).unwrap();
        let mut state = editor_at_end(&source);
        for _ in 0..2 {
            state = applied(&state, enter_command(&state));
            assert!(
                source.render(&schema, state.doc()).is_ok(),
                "{original:?}: {}",
                markraft_commonmark::to_markdown(&schema, state.doc())
            );
        }
        state = applied(&state, markraft_core::commands::insert_text("typed"));
        assert!(source.render(&schema, state.doc()).is_ok());
    }
}

#[test]
fn typing_each_character_after_leaving_a_list_keeps_every_space() {
    let schema = commonmark_schema();
    let original = "# Enter Test\n\n- Last item\n";
    let source = SourceDocument::parse(&schema, original).unwrap();
    let mut state = editor_at_end(&source);
    for _ in 0..2 {
        state = applied(&state, enter_command(&state));
        source.render(&schema, state.doc()).unwrap();
    }
    let mut typed = String::new();
    for ch in "Outside the list.".chars() {
        typed.push(ch);
        state = applied(
            &state,
            markraft_core::commands::insert_text(&ch.to_string()),
        );
        let rendered = source
            .render(&schema, state.doc())
            .unwrap_or_else(|error| panic!("after {typed:?}: {error}"));
        assert_eq!(rendered, format!("{original}\n{typed}\n"));
    }
}

#[test]
fn typing_consecutive_and_trailing_spaces_survives_each_guard_check() {
    let schema = commonmark_schema();
    for original in ["hello\n", "- item\n", "# Heading\n", "```text\ncode\n```\n"] {
        let source = SourceDocument::parse(&schema, original).unwrap();
        let mut state = editor_at_end(&source);
        let mut typed = String::new();
        for ch in " one  two  ".chars() {
            typed.push(ch);
            state = applied(
                &state,
                markraft_core::commands::insert_text(&ch.to_string()),
            );
            let rendered = source
                .render(&schema, state.doc())
                .unwrap_or_else(|error| panic!("{original:?}, after {typed:?}: {error}"));
            assert!(rendered.contains(&typed), "{rendered:?} lost {typed:?}");
        }
        state = applied(&state, enter_command(&state));
        assert!(
            source.render(&schema, state.doc()).is_ok(),
            "Return after trailing spaces in {original:?}"
        );
    }
}
