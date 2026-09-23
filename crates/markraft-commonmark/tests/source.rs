//! Source-preserving persistence regression cases.

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
        "[[Note]] [[Note|Alias]] ![[a.png]] [[a#Heading]] [[a^id]] [[ spaced ]]\n",
        "[[a|]] and [[a|b|c]] are not wiki links\n",
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
        "> [!note]\n> old text\n",
        "old text ^stable-id\n",
        "old text with $x + y$\n",
    ] {
        let expected = original.replace("old", "new");
        assert_eq!(edit(original, &expected).unwrap(), expected);
    }
}

/// Syntax the codec gives no meaning — math, block anchors, `%%` comments, a
/// `[[…]]` it does not read as a wiki link, a callout marker it would spell
/// differently — is ordinary text of its block, so an edit inside or beside it
/// patches exactly the bytes it changed and reparses to the edited document.
#[test]
fn edits_inside_syntax_the_codec_gives_no_meaning_save_exactly() {
    let schema = commonmark_schema();
    for original in [
        "energy $old^2$ here\n",
        "inline $a + old$ and more\n",
        "$$\nx = old\n$$\n",
        "Intro\n\nmath $old$ ^anchor\n\nOutro\n",
        "text old ^anchor\n",
        "text ^old\n",
        "a %%old comment%% b\n",
        "tail %%open old\n",
        "Read [[old|]] here\n",
        "Read [[a|]] and old here\n",
        "unterminated [[old here\n",
        "> [!note]\n> $old$ and ^id\n",
        ">[!old]\n>contents\n",
    ] {
        let expected = original.replace("old", "new");
        let saved =
            edit(original, &expected).unwrap_or_else(|error| panic!("{original:?}: {error}"));
        assert_eq!(saved, expected, "{original:?}");
        assert_eq!(
            SourceDocument::parse(&schema, &saved).unwrap().document(),
            SourceDocument::parse(&schema, &expected)
                .unwrap()
                .document(),
            "{original:?}"
        );
    }
}

/// Typing a character right against the syntax, where the old guard drew its
/// boundary, is no different from typing anywhere else.
#[test]
fn typing_at_the_edge_of_math_anchors_and_comments_saves() {
    for (original, expected) in [
        ("x $a$ y\n", "x $ab$ y\n"),
        ("x $a$ y\n", "x $a$! y\n"),
        ("para ^id\n", "para ^id2\n"),
        ("para ^id\n", "para. ^id\n"),
        ("a %%c%% b\n", "a %%cc%% b\n"),
        ("a [[x|]] b\n", "a [[xy|]] b\n"),
    ] {
        assert_eq!(edit(original, expected).unwrap(), expected, "{original:?}");
    }
}

#[test]
fn prices_in_prose_are_not_read_as_math() {
    for (original, expected) in [
        ("It costs $5 and $10\n", "It costs $5 or $10\n"),
        (r"Pay \$5 and \$10 now", r"Pay \$5 or \$10 now"),
        ("$5, $10 and $20 each\n", "$5, $10 or $20 each\n"),
    ] {
        assert_eq!(edit(original, expected).unwrap(), expected, "{original:?}");
    }
}

#[test]
fn unchanged_callouts_of_every_form_are_byte_identical() {
    for source in [
        "> [!note]\n> Body\n",
        "> [!tip] Custom title\n> Body with **marks**\n",
        "> [!faq]- Folded by default\n> Body\n",
        "> [!warning]+ Expanded by default\n> Body\n",
        "> [!custom-type] Any type is legal in Obsidian\n> Body\n",
        "> [!note]\n",
        ">[!note]\n>Body\n",
        "> [!note]\r\n> Body\r\n",
        "> [!note] Title\nlazy continuation\n",
        "> [!note]\n> > [!tip] Inner\n> > body\n",
        "- > [!note] In a list\n  > body\n",
        "> [!note]\n>\n> Second block\n",
        "> [!note] Title  \n> Body\n",
        "> \\[!note]\n> Not a callout\n",
        "> text [!note] more\n",
    ] {
        let schema = commonmark_schema();
        let document = SourceDocument::parse(&schema, source).unwrap();
        assert_eq!(
            document.render(&schema, document.document()).unwrap(),
            source,
            "{source:?}"
        );
    }
}

#[test]
fn editing_a_callouts_body_leaves_its_marker_line_and_prefixes_alone() {
    for original in [
        "> [!note]\n> old text\n",
        "> [!tip] Title\n> old text\n",
        ">[!note]\n>old text\n",
        "> [!note]\n>\n> old text\n",
        "> [!note]\n> - old item\n> - two\n",
        "> [!note]\n> > [!tip] Inner\n> > old body\n",
        // Callout-looking text that is not a marker is ordinary content now.
        "> text [!note] and old\n",
        "paragraph with [!note] and old\n",
    ] {
        let expected = original.replace("old", "new");
        assert_eq!(edit(original, &expected).unwrap(), expected, "{original:?}");
    }
}

#[test]
fn a_callout_can_be_rewritten_whole_or_patched_through_its_marker() {
    // The marker's bytes are in the quote's attributes, so writing the block
    // again writes the marker again: retyping it, dropping it and deleting the
    // whole callout all go through.
    for (original, expected) in [
        ("> [!note]\n> Body\n", "> [!tip] Now titled\n> Body\n"),
        ("> [!note]\n> Body\n", "> Body\n"),
        ("> Body\n", "> [!note]\n> Body\n"),
    ] {
        assert_eq!(edit(original, expected).unwrap(), expected, "{original:?}");
    }
    // Deleting the whole callout leaves the separators around it, as deleting
    // any other block does; what matters is that it goes at all.
    let schema = commonmark_schema();
    let expected = "First\n\nLast\n";
    let result = edit("First\n\n> [!note]\n> Body\n\nLast\n", expected).unwrap();
    assert_eq!(
        SourceDocument::parse(&schema, &result).unwrap().document(),
        SourceDocument::parse(&schema, expected).unwrap().document()
    );
    assert!(!result.contains("[!note]"));
    // A callout the codec would spell differently is not rewritten whole, but
    // retyping its marker patches just the bytes that changed.
    assert_eq!(
        edit(">[!note]\n>Body\n", ">[!tip]\n>Body\n").unwrap(),
        ">[!tip]\n>Body\n"
    );
}

#[test]
fn real_enter_inside_a_callout_keeps_its_marker_and_quote_prefixes() {
    let schema = commonmark_schema();
    let original = "> [!note] Title\n> First line\n";
    let source = SourceDocument::parse(&schema, original).unwrap();
    let state = editor_at_end(&source);
    let state = applied(&state, enter_command(&state));
    let state = applied(&state, markraft_core::commands::insert_text("Second"));
    let rendered = source
        .render(&schema, state.doc())
        .unwrap_or_else(|error| panic!("Return in a callout: {error}"));
    assert_eq!(rendered, "> [!note] Title\n> First line\n>\n> Second\n");
    assert_eq!(
        SourceDocument::parse(&schema, &rendered)
            .unwrap()
            .document(),
        state.doc()
    );
}

#[test]
fn text_beside_a_wiki_link_is_edited_without_rewriting_the_link() {
    for original in [
        "Read [[page|label]] and old\n",
        "Look at ![[image.png|100]] for old detail\n",
        "See [[ folder/note.md#Heading ]] for old notes\n",
        "Jump to [[note^block-id]] for old context\n",
    ] {
        let expected = original.replace("old", "new");
        assert_eq!(edit(original, &expected).unwrap(), expected, "{original:?}");
    }
}

#[test]
fn deleting_a_wiki_link_removes_exactly_its_source_bytes() {
    for (original, expected) in [
        ("Read [[page|label]] now\n", "Read  now\n"),
        ("Read ![[image.png]] now\n", "Read  now\n"),
        ("[[only]]\n\nnext\n", "\n\nnext\n"),
    ] {
        assert_eq!(edit(original, expected).unwrap(), expected, "{original:?}");
    }
}

#[test]
fn inserting_a_wiki_link_writes_exactly_its_serialized_form() {
    for (original, expected) in [
        ("Read now\n", "Read [[page|label]] now\n"),
        ("Read now\n", "Read ![[image.png|100]] now\n"),
        ("Read now\n", "Read [[folder/note#Heading]] now\n"),
    ] {
        assert_eq!(edit(original, expected).unwrap(), expected, "{original:?}");
    }
}

#[test]
fn undo_after_a_wiki_link_edit_restores_the_original_bytes() {
    let schema = commonmark_schema();
    let original = "Read [[page|label]] and ![[image.png]] now\n";
    let source = SourceDocument::parse(&schema, original).unwrap();
    for edited in [
        "Read  and ![[image.png]] now\n",
        "Read [[page|label]] and ![[image.png]] later\n",
        "Read [[page|label]] and ![[image.png]] and [[extra]] now\n",
    ] {
        let target = SourceDocument::parse(&schema, edited).unwrap();
        assert_eq!(
            source.render(&schema, target.document()).unwrap(),
            edited,
            "{edited:?}"
        );
        assert_eq!(source.render(&schema, source.document()).unwrap(), original);
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

/// A block inserted between two untouched ones goes in with one blank line
/// either side: the gap the source already had parts it from the block after,
/// so it is not doubled. This is what lifting a list's empty first item and
/// typing into it leaves.
#[test]
fn a_block_inserted_between_two_keeps_one_blank_line_either_side() {
    for (original, edited) in [
        (
            "# Lists\n\n1. one\n2. two\n",
            "# Lists\n\n5\n\n1. one\n2. two\n",
        ),
        (
            "# Lists\n\n- one\n- two\n",
            "# Lists\n\n5\n\n- one\n- two\n",
        ),
        ("First\n\nLast\n", "First\n\nMiddle\n\nLast\n"),
        // An extra blank line the author left stays where it was.
        ("First\n\n\nLast\n", "First\n\nMiddle\n\n\nLast\n"),
        // Nothing parts a heading from the paragraph under it, so the new block
        // brings a separator for both sides.
        ("# Title\npara\n", "# Title\n\nnew\n\npara\n"),
        // At either end of the note, one separator.
        ("1. eight\n9. nine\n", "1. eight\n9. nine\n\n5\n"),
        ("Last\n", "First\n\nLast\n"),
    ] {
        assert_eq!(edit(original, edited).unwrap(), edited, "{original:?}");
    }
}

/// A block that spells nothing — the empty paragraph a lifted list item leaves
/// until something is typed in it — adds no blank lines to the file.
#[test]
fn an_empty_paragraph_inserted_between_blocks_adds_nothing() {
    use markraft_commonmark::schema as md;
    let schema = commonmark_schema();
    let original = "# Lists\n\n1. one\n";
    let source = SourceDocument::parse(&schema, original).unwrap();
    let document = source.document();
    let mut children: Vec<_> = document.children().cloned().collect();
    children.insert(1, schema.node(md::PARAGRAPH, []).unwrap());
    let lifted = schema.doc(children).unwrap();
    assert_eq!(source.render(&schema, &lifted).unwrap(), original);
}

/// A link reference definition is a block of the document, kept verbatim, so
/// an edit that removes it removes it on purpose.
#[test]
fn a_definition_is_a_block_an_edit_may_remove() {
    let original = "first\n\n[unused]: /keep\n\nlast\n";
    let schema = commonmark_schema();
    let removed = edit(original, "first\n\nlast\n").unwrap();
    assert_eq!(
        SourceDocument::parse(&schema, &removed).unwrap().document(),
        SourceDocument::parse(&schema, "first\n\nlast\n")
            .unwrap()
            .document()
    );
    assert_eq!(
        edit(original, "first\n\n[unused]: /keep\n\nchanged\n").unwrap(),
        "first\n\n[unused]: /keep\n\nchanged\n"
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
    let target = SourceDocument::parse(&schema, "[unused]: /keep\n\nhello").unwrap();
    let result = source.render(&schema, target.document()).unwrap();
    assert!(result.starts_with(original), "{result:?}");
    assert!(result.trim_end().ends_with("hello"), "{result:?}");
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
fn real_enter_and_typing_commands_still_save() {
    // Empty paragraphs have no CommonMark spelling, so intermediate Returns may
    // not patch source in place. After typing, a save (source patch or full
    // rewrite) must still produce the typed text without a `<br>` marker.
    let schema = commonmark_schema();
    for original in ["", "hello\n", "# Heading\n"] {
        let source = SourceDocument::parse(&schema, original).unwrap();
        let mut state = editor_at_end(&source);
        for _ in 0..2 {
            state = applied(&state, enter_command(&state));
        }
        state = applied(&state, markraft_core::commands::insert_text("typed"));
        let rendered = source
            .render(&schema, state.doc())
            .unwrap_or_else(|_| markraft_commonmark::to_markdown(&schema, state.doc()));
        assert!(rendered.contains("typed"), "{original:?} -> {rendered:?}");
        assert!(
            !rendered.contains("<br>"),
            "empty paragraphs must not write <br>: {rendered:?}"
        );
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
fn typing_into_a_saved_break_tag_paragraph_writes_the_text() {
    // Older files may still contain a lone `<br>` empty-paragraph marker. Typing
    // into that paragraph must replace it with the text (via a source patch or a
    // full rewrite), never leave the tag beside the new words.
    let schema = commonmark_schema();
    for original in ["<br>\n", "---\ntitle: mine\n---\n<br>\n"] {
        let source = SourceDocument::parse(&schema, original).unwrap();
        let state = editor_at_end(&source);
        let typed = applied(&state, markraft_core::commands::insert_text("hello"));
        let rendered = source
            .render(&schema, typed.doc())
            .unwrap_or_else(|_| markraft_commonmark::to_markdown(&schema, typed.doc()));
        assert!(rendered.contains("hello"), "{original:?} -> {rendered:?}");
        assert!(!rendered.contains("<br>"), "{original:?} -> {rendered:?}");
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

/// Nothing inline is guarded any more. What is still refused is a change the
/// codec can neither patch in place nor rewrite whole: here the setext
/// underline is block-level spelling the writer would respell as `#`, so the
/// block is not rewritten and no local patch turns it into a level-three
/// heading.
#[test]
fn a_block_the_writer_would_respell_is_not_rewritten_whole() {
    assert_eq!(
        edit(
            "Title\n=====\n\nmath $x$ ^id\n",
            "### Title\n\nmath $x$ ^id\n"
        ),
        Err(SourceError::UnsupportedEdit)
    );
}

/// Footnote definitions stand where they were written, so an edit inside one
/// or around it patches exactly those bytes.
#[test]
fn edits_in_and_around_footnote_definitions_save_exactly() {
    for original in [
        "a[^n] old\n\n[^n]: the note\n\nafter\n",
        "a[^n]\n\n[^n]: old note\n    continued\n\n    old second\n\nafter\n",
        "x\n\n[^unused]: old\n\ny\n",
        "> q\n>\n> [^q]: old in a quote\n\nt[^q]\n",
    ] {
        let expected = original.replace("old", "new");
        assert_eq!(
            edit(original, &expected).unwrap_or_else(|error| panic!("{original:?}: {error}")),
            expected
        );
    }
}
