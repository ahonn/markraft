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
        // A link that was the whole block takes the block's lines, and the
        // gap after them, with it.
        ("[[only]]\n\nnext\n", "next\n"),
    ] {
        assert_eq!(edit(original, expected).unwrap(), expected, "{original:?}");
    }
}

#[test]
fn deleting_a_table_spelled_without_padding_saves() {
    for (original, expected) in [
        (
            "Edit area\n\n| a | b |\n| - | - |\n| 1 | 2 |\n",
            "Edit area\n",
        ),
        (
            "Edit area\n\n| a | b |\n| - | - |\n| 1 | 2 |\n\npara\n",
            "Edit area\n\npara\n",
        ),
        ("| a | b |\n| - | - |\n\npara\n", "para\n"),
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

/// Blocks deleted outright take their lines and one gap with them, at the start
/// of the note, in its middle and at its end alike; the rest keeps its bytes.
#[test]
fn deleted_blocks_take_one_gap_with_them() {
    for (original, expected) in [
        ("First\n\n\nMiddle\n\nLast\n", "Middle\n\nLast\n"),
        ("First\n\nMiddle\n\n\nLast\n", "First\n\n\nLast\n"),
        ("First\n\n\nMiddle\n\nLast\n", "First\n\n\nMiddle\n"),
        ("|a|\n|-|\n\nMiddle\n\nLast\n", "Middle\n\nLast\n"),
    ] {
        assert_eq!(edit(original, expected).unwrap(), expected, "{original:?}");
    }
}

/// Text changed on two lines of a block whose prefixes the writer would
/// spell otherwise: each change is placed on its own line, and the prefixes
/// between them stay as they were.
#[test]
fn changes_on_separate_lines_of_a_block_are_patched_apart() {
    for (original, expected) in [
        (">one two\n>three four\n", ">ONE two\n>three FOUR\n"),
        ("-  one two\n   three four\n", "-  ONE two\n   three FOUR\n"),
        (
            ">one [x][r] two\n>three\n\n[r]: /u\n",
            ">ONE [x][r] two\n>THREE\n\n[r]: /u\n",
        ),
    ] {
        assert_eq!(edit(original, expected).unwrap(), expected, "{original:?}");
    }
}

/// The allowances a save makes for typing in progress — spaces ending a
/// paragraph, an empty paragraph — excuse only that: the rest of the note
/// still has to read back as it is. Here the first place tried for the new
/// block would run it into the paragraph below.
#[test]
fn typing_in_progress_does_not_excuse_the_rest_of_the_note() {
    use markraft_core::Fragment;
    let schema = commonmark_schema();
    let source = SourceDocument::parse(&schema, "# Title\npara\n").unwrap();
    let typed = SourceDocument::parse(&schema, "# Title\n\nnew\n\npara\n").unwrap();
    let spaced = edit_children(typed.document(), &[], &|children| {
        let paragraph = children[1].clone();
        children[1] = paragraph.copy(Fragment::from_node(paragraph.child(0).with_text("new ")));
    });
    assert_eq!(
        source.render(&schema, &spaced).unwrap(),
        "# Title\n\nnew \n\npara\n"
    );
    let opened = edit_children(typed.document(), &[], &|children| {
        children.insert(2, empty_paragraph())
    });
    assert_eq!(
        source.render(&schema, &opened).unwrap(),
        "# Title\n\nnew\n\npara\n"
    );
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
    // Empty paragraphs have no CommonMark spelling: the one the second Return
    // leaves writes as a blank line, and never as a `<br>` marker. A save that
    // could not keep the source is a failed save, so nothing falls back here.
    let schema = commonmark_schema();
    for (original, expected) in [
        ("", "\ntyped"),
        ("hello\n", "hello\n\n\ntyped\n"),
        ("# Heading\n", "# Heading\n\n\ntyped\n"),
    ] {
        let source = SourceDocument::parse(&schema, original).unwrap();
        let mut state = editor_at_end(&source);
        for _ in 0..2 {
            state = applied(&state, enter_command(&state));
        }
        state = applied(&state, markraft_core::commands::insert_text("typed"));
        let rendered = source.render(&schema, state.doc()).unwrap();
        assert_eq!(rendered, expected, "{original:?}");
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
    // into that paragraph must replace it with the text, never leave the tag
    // beside the new words.
    let schema = commonmark_schema();
    for (original, expected) in [
        ("<br>\n", "hello\n"),
        (
            "---\ntitle: mine\n---\n<br>\n",
            "---\ntitle: mine\n---\nhello\n",
        ),
    ] {
        let source = SourceDocument::parse(&schema, original).unwrap();
        let state = editor_at_end(&source);
        let typed = applied(&state, markraft_core::commands::insert_text("hello"));
        let rendered = source.render(&schema, typed.doc()).unwrap();
        assert_eq!(rendered, expected, "{original:?}");
    }
}

#[test]
fn real_enter_commands_preserve_existing_list_markers_and_code_source() {
    let schema = commonmark_schema();
    // Two Returns leave a list, and stay inside a code block.
    for (original, expected) in [
        ("+ one\n", "+ one\n\ntyped\n"),
        ("3. one\n", "3. one\n\ntyped\n"),
        (
            "```rust\nlet a = 1;\n```\n",
            "```rust\nlet a = 1;\n\ntyped\n```\n",
        ),
    ] {
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
        assert_eq!(
            source.render(&schema, state.doc()).unwrap(),
            expected,
            "{original:?}"
        );
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
    for (original, after_return) in [
        ("hello\n", "hello one  two  \n"),
        ("- item\n", "- item one  two  \n- \n"),
        ("# Heading\n", "# Heading one  two  \n"),
        ("```text\ncode\n```\n", "```text\ncode one  two  \n\n```\n"),
    ] {
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
        assert_eq!(
            source.render(&schema, state.doc()).unwrap(),
            after_return,
            "Return after trailing spaces in {original:?}"
        );
    }
}

/// A block-level change the user asks for respells the block it touches the
/// way the writer writes it: a setext heading made level three becomes an ATX
/// heading, and the paragraph after it keeps its bytes.
#[test]
fn a_structural_edit_respells_the_block_it_touches() {
    assert_eq!(
        edit(
            "Title\n=====\n\nmath $x$ ^id\n",
            "### Title\n\nmath $x$ ^id\n"
        )
        .unwrap(),
        "### Title\n\nmath $x$ ^id\n"
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

/// Ticking or unticking a task box changes the one character inside the box,
/// however the rest of the item is spelled — an `[X]`, a `+` marker, two
/// spaces after the marker — and whichever item of the list it is.
#[test]
fn toggling_a_task_box_changes_only_the_box() {
    for (original, expected) in [
        ("- [X] done\n", "- [ ] done\n"),
        ("*  [x] a\n", "*  [ ] a\n"),
        ("+ [ ] a\n", "+ [x] a\n"),
        ("+ [ ] [x] a\n", "+ [x] [x] a\n"),
        (
            "- [X] a\n-  [X] b\n\ntext [X]\n",
            "- [X] a\n-  [ ] b\n\ntext [X]\n",
        ),
        ("> 1.  [X] quoted\n", "> 1.  [ ] quoted\n"),
        ("- [ ]  ## heading\n", "- [x]  ## heading\n"),
    ] {
        assert_eq!(
            edit(original, expected).unwrap_or_else(|error| panic!("{original:?}: {error}")),
            expected
        );
    }
}

/// A table edit the user asks for by shape — a column added or deleted, an
/// alignment changed — respells the table the way the writer writes tables
/// when its hand-written spelling cannot take the change in place, as Typora
/// does. Only that table is rewritten.
#[test]
fn a_structural_edit_respells_a_hand_written_table() {
    let before = "Intro  \nwith a break\n\n|a|b|\n|-|-|\n|1|2|\n\n*  after\n";
    for (edited, table) in [
        (
            "|a|b||\n|-|-|-|\n|1|2||\n",
            "| a   | b   |     |\n| --- | --- | --- |\n| 1   | 2   |     |\n",
        ),
        ("|a|\n|-|\n|1|\n", "| a   |\n| --- |\n| 1   |\n"),
        (
            "|a|b|\n|:-:|-|\n|1|2|\n",
            "| a   | b   |\n| :-: | --- |\n| 1   | 2   |\n",
        ),
    ] {
        let original = before;
        let target = before.replace("|a|b|\n|-|-|\n|1|2|\n", edited);
        let expected = before.replace("|a|b|\n|-|-|\n|1|2|\n", table);
        assert_eq!(
            edit(original, &target).unwrap_or_else(|error| panic!("{edited:?}: {error}")),
            expected,
            "{edited:?}"
        );
    }
}

/// Text typed into a hand-written table's cell still patches just that cell:
/// only a change of the table's shape respells it.
#[test]
fn a_text_edit_keeps_a_hand_written_table_spelling() {
    let original = "|a|b|\n|-|-|\n|1|2|\n";
    for expected in ["|a|b|\n|-|-|\n|1 more|2|\n", "|a|b|\n|-|-|\n|1|2 \\| 3|\n"] {
        assert_eq!(edit(original, expected).unwrap(), expected);
    }
}

/// Deleting a body row of a hand-written table drops just that row's line.
#[test]
fn deleting_a_row_of_a_hand_written_table_drops_its_line() {
    for (original, expected) in [
        ("|a|b|\n|-|-|\n|1|2|\n", "|a|b|\n|-|-|\n"),
        (
            "|a|b|\n|-|-|\n|1|2|\n|3|4|\n\nnext\n",
            "|a|b|\n|-|-|\n|3|4|\n\nnext\n",
        ),
        (
            "|a|b|\r\n|-|-|\r\n|1|2|\r\n|3|4|\r\n",
            "|a|b|\r\n|-|-|\r\n|1|2|\r\n",
        ),
    ] {
        assert_eq!(edit(original, expected).unwrap(), expected, "{original:?}");
    }
}

/// A body row added to a hand-written table goes in as a line of its own,
/// spaced the way the header is, leaving every row that was there as it was
/// spelled.
#[test]
fn adding_a_row_to_a_hand_written_table_leaves_its_rows_alone() {
    for (original, expected) in [
        ("|a|b|\n|-|-|\n|1|2|\n", "|a|b|\n|-|-|\n| | |\n|1|2|\n"),
        (
            "|a|b|\n|-|-|\n|1|2|\n|3|4|\n\nnext\n",
            "|a|b|\n|-|-|\n|1|2|\n| | |\n|3|4|\n\nnext\n",
        ),
        (
            "|a|b|\r\n|-|-|\r\n|1|2|\r\n",
            "|a|b|\r\n|-|-|\r\n|1|2|\r\n| | |\r\n",
        ),
    ] {
        assert_eq!(edit(original, expected).unwrap(), expected, "{original:?}");
    }
}

/// `node` with `edit` applied to the container at `path`'s children.
fn edit_children(
    node: &markraft_core::Node,
    path: &[usize],
    edit: &dyn Fn(&mut Vec<markraft_core::Node>),
) -> markraft_core::Node {
    let mut children: Vec<_> = node.children().cloned().collect();
    match path {
        [] => edit(&mut children),
        [index, rest @ ..] => children[*index] = edit_children(&children[*index], rest, edit),
    }
    node.copy(markraft_core::Fragment::from_nodes(children))
}

fn empty_paragraph() -> markraft_core::Node {
    let schema = commonmark_schema();
    let source = SourceDocument::parse(&schema, "a").unwrap();
    source
        .document()
        .child(0)
        .copy(markraft_core::Fragment::empty())
}

/// `original` saved with an empty paragraph put at `index` of the container
/// at `path` — what Return at the start of a block leaves behind.
fn with_empty_paragraph(
    original: &str,
    path: &[usize],
    index: usize,
) -> Result<String, SourceError> {
    let schema = commonmark_schema();
    let source = SourceDocument::parse(&schema, original).unwrap();
    let target = edit_children(source.document(), path, &|children| {
        children.insert(index, empty_paragraph())
    });
    source.render(&schema, &target)
}

/// An empty paragraph is typing in progress — Return pressed at the start of a
/// block, before anything is typed on the new line. It says nothing a file can
/// hold, so saving one leaves the file as it was, or at most adds the blank
/// line it stands for, and the note reads back as it was.
#[test]
fn an_empty_paragraph_saves_as_nothing() {
    let schema = commonmark_schema();
    for (original, path, index) in [
        ("para one\n\nsecond *em*\n", &[][..], 0),
        ("para one\n\nsecond *em*\n", &[][..], 1),
        ("# Head\n\ntext\n", &[][..], 0),
        ("Title\n===\n\ntext\n", &[][..], 0),
        ("text[^1]\n\n[^1]: note\n", &[][..], 0),
        ("$$\nx^2\n$$\n\ninline $y$ math\n", &[][..], 0),
        ("<div>html</div>\n\nafter\n", &[][..], 0),
        ("> quote\n> more\n\n> [!note]\n> callout\n", &[0][..], 0),
        ("> quote\n> more\n\n> [!note]\n> callout\n", &[1][..], 0),
        ("> quote\n> more\n\n> [!note]\n> callout\n", &[1][..], 1),
        ("text[^1]\n\n[^1]: note\n", &[1][..], 0),
    ] {
        let saved = with_empty_paragraph(original, path, index)
            .unwrap_or_else(|error| panic!("{original:?} {path:?} {index}: {error}"));
        assert_eq!(
            SourceDocument::parse(&schema, &saved).unwrap().document(),
            SourceDocument::parse(&schema, original).unwrap().document(),
            "{original:?} {path:?} {index}: {saved:?}"
        );
    }
    assert_eq!(
        with_empty_paragraph("para one\n\nsecond\n", &[], 0).unwrap(),
        "para one\n\nsecond\n"
    );
}

/// A list item that starts with an empty paragraph and goes on — its first
/// line emptied, or Return at the end of an item's first paragraph splitting
/// it as Typora does — is written with its marker alone on its line and the
/// rest of the item on the lines after it, which reads back as the same item
/// without the empty paragraph.
#[test]
fn an_empty_paragraph_that_holds_a_list_item_open_is_written_as_an_empty_line() {
    let schema = commonmark_schema();
    let original = "- first\n\n  para2\n- next\n";
    let source = SourceDocument::parse(&schema, original).unwrap();
    let emptied = edit_children(source.document(), &[0, 0], &|children| {
        children[0] = empty_paragraph()
    });
    assert_eq!(
        source.render(&schema, &emptied).as_deref(),
        Ok("- \n  para2\n\n- next\n")
    );
    let opened = edit_children(source.document(), &[0, 0], &|children| {
        children.insert(0, empty_paragraph())
    });
    assert_eq!(
        source.render(&schema, &opened).as_deref(),
        Ok("- \n  first\n\n  para2\n- next\n")
    );
}

/// A callout whose body is emptied still saves as that callout, never as a
/// quote holding the marker as text.
#[test]
fn a_callout_emptied_to_an_empty_paragraph_stays_a_callout() {
    let schema = commonmark_schema();
    let original = "> [!note]\n> callout\n\nafter\n";
    let source = SourceDocument::parse(&schema, original).unwrap();
    let emptied = edit_children(source.document(), &[0], &|children| {
        children[0] = empty_paragraph()
    });
    let saved = source.render(&schema, &emptied).unwrap();
    let callout = SourceDocument::parse(&schema, &saved)
        .unwrap()
        .document()
        .child(0)
        .clone();
    assert!(callout.same_markup(emptied.child(0)), "{saved:?}");
    assert!(saved.ends_with("\n\nafter\n"), "{saved:?}");
}

/// `edit`, with a hand-written block either side of `original` and `edited`
/// that the save must leave byte for byte.
fn edit_between(original: &str, edited: &str) -> String {
    const BEFORE: &str = "Before\n======\n\n";
    const AFTER: &str = "\n+  after  \n   more\n";
    let saved = edit(
        &format!("{BEFORE}{original}{AFTER}"),
        &format!("{BEFORE}{edited}{AFTER}"),
    )
    .unwrap_or_else(|error| panic!("{original:?} -> {edited:?}: {error}"));
    assert!(saved.starts_with(BEFORE), "{saved:?}");
    assert!(saved.ends_with(AFTER), "{saved:?}");
    saved[BEFORE.len()..saved.len() - AFTER.len()].to_owned()
}

/// Structural edits on hand-written blocks — a setext heading, a list spaced
/// or ticked otherwise than the writer would — respell just the blocks they
/// touch, as a new note would write them, and leave the blocks around them as
/// they were: formats, list conversions, joins and lifts.
#[test]
fn structural_edits_respell_only_the_hand_written_blocks_they_touch() {
    for (original, edited) in [
        // A setext heading made a paragraph, another level, a quote, a code
        // block or a list, or joined with the paragraph after it.
        ("Title\n===\n\ntext\n", "Title\n\ntext\n"),
        ("Title\n===\n\ntext\n", "## Title\n\ntext\n"),
        ("Title\n===\n\ntext\n", "> # Title\n\ntext\n"),
        ("Title\n===\n\ntext\n", "```\nTitle\n```\n\ntext\n"),
        ("Title\n===\n\ntext\n", "- # Title\n\ntext\n"),
        ("Title\n===\n\ntext\n", "1. # Title\n\ntext\n"),
        ("Title\n===\n\ntext\n", "- [ ] Title\n\ntext\n"),
        ("Title\n===\n\ntext\n", "# Titletext\n"),
        // An item's text made a code block.
        ("* a\n*  b\n", "* a\n* ```\n  b\n  ```\n"),
        // A task list converted, joined, or its last item lifted out.
        ("- [ ] t\n- [X] u\n", "- t\n- u\n"),
        ("- [ ] t\n- [X] u\n", "1. [ ] t\n2. [X] u\n"),
        ("- [ ] t\n- [X] u\n", "- [ ] tu\n"),
        ("- [ ] t\n- [X] u\n", "- [ ] t\n\nu\n"),
        // A loose item's paragraphs joined, the list converted, a paragraph
        // lifted out of it.
        ("- first\n\n  para2\n- next\n", "- firstpara2\n- next\n"),
        ("- first\n\n  para2\n- next\n", "- first\n\n  para2next\n"),
        (
            "- first\n\n  para2\n- next\n",
            "1. first\n\n   para2\n2. next\n",
        ),
        (
            "- first\n\n  para2\n- next\n",
            "- first\n\npara2\n\n- next\n",
        ),
    ] {
        // The patch may find a smaller change than the whole block — a
        // lifted paragraph only loses its indent — so what is held is the
        // note the file reads back as.
        let schema = commonmark_schema();
        let saved = edit_between(original, edited);
        assert_eq!(
            SourceDocument::parse(&schema, &saved).unwrap().document(),
            SourceDocument::parse(&schema, edited).unwrap().document(),
            "{original:?} -> {saved:?}"
        );
    }
}

/// Typing in a setext heading or a hand-spaced item keeps its spelling, and
/// so does a style added to the heading's text.
#[test]
fn text_edits_keep_hand_written_block_spelling() {
    for (original, edited) in [
        ("Title\n===\n\ntext\n", "Title and more\n===\n\ntext\n"),
        ("Title\n===\n\ntext\n", "~~Title~~\n===\n\ntext\n"),
        ("Title\n===\n\ntext\n", "Ti~~~~tle\n===\n\ntext\n"),
        ("* a\n*  b\n", "* a\n*  bc\n"),
        ("- [ ] t\n- [X] u\n", "- [ ] t\n- [X] uv\n"),
    ] {
        assert_eq!(edit_between(original, edited), edited, "{original:?}");
    }
}

/// Text the hand-written spelling cannot hold respells its block: a setext
/// heading whose text opens with `~~~~` — a style's empty pair written at its
/// start — would read as a code fence, so the heading becomes an ATX one.
#[test]
fn text_a_spelling_cannot_hold_respells_its_block() {
    assert_eq!(
        edit_between("Title\n===\n\ntext\n", "# ~~~~Title\n\ntext\n"),
        "# ~~~~Title\n\ntext\n"
    );
}

/// Rows added, edited and removed together in a hand-written table: every row
/// left alone keeps its bytes, an edited row keeps its own spacing around the
/// text that changed, and a new row is spaced as the header is.
#[test]
fn a_hand_written_tables_rows_keep_their_spelling_through_several_edits() {
    for (original, expected) in [
        (
            "|a|b|\n|-|-|\n|1|2|\n|3|4|\n",
            "|a|b|\n|-|-|\n|1|2|\n|z| |\n|3y|4|\n",
        ),
        (
            "| a | b |\n|---|---|\n|  1 |2|\n| 3 | 4 |\n",
            "| a | b |\n|---|---|\n|  1x |2|\n| z |  |\n",
        ),
        ("a | b\n--|--\n1 | 2\n", "a | b\n--|--\n1 | 2\nz | \n"),
        // A cell emptied keeps room between its pipes.
        ("|a|b|\n|-|-|\n|1|2|\n", "|a|b|\n|-|-|\n|1| |\n"),
        (
            "| a | b |\n| - | - |\n| 1 | 2 |\n",
            "| a | b |\n| - | - |\n| 1 |  |\n",
        ),
        // The paragraph after the table joined into its last cell.
        ("|a|b|\n|-|-|\n|1|2|\n\nb\n", "|a|b|\n|-|-|\n|1|2b|\n"),
        // The same, with a block after the one it took in.
        (
            "|a|b|\n|-|-|\n|1|2|\n\nb\n\nlast\n",
            "|a|b|\n|-|-|\n|1|2b|\n\nlast\n",
        ),
        // Rows edited in place stay the rows they were, spaced their own way,
        // even when one of them now shares a cell with the other's old text.
        (
            "|a|b|\n|-|-|\n| 1 | x |\n|  2  |  y  |\n",
            "|a|b|\n|-|-|\n| 3 | 4 |\n|  5  |  x  |\n",
        ),
        // A row removed and the one after it edited: the edit goes to the
        // row that shares a cell with it, and keeps that row's spacing.
        (
            "|a|b|\n|-|-|\n| 1 | x |\n|  2  |  y  |\n",
            "|a|b|\n|-|-|\n|  2  |  z  |\n",
        ),
    ] {
        assert_eq!(edit(original, expected).unwrap(), expected, "{original:?}");
    }
}

/// The blank lines after an indented code block are the gap before the next
/// block, not part of the code: a paragraph that took the code in keeps that
/// gap from the block after it.
#[test]
fn indented_code_joined_into_a_paragraph_keeps_the_gap_after_it() {
    for (original, expected) in [
        ("one\n\n    code\n\nlast\n", "one code\n\nlast\n"),
        ("one\n\n    code\n\n\nlast\n", "one code\n\n\nlast\n"),
        ("one\n\n    code\n\nlast\n", "one\n\nlast\n"),
    ] {
        assert_eq!(edit(original, expected).unwrap(), expected, "{original:?}");
    }
}

/// A paragraph split in two while the block after a list joined it: the note
/// has as many blocks as before, but not the same ones at each index, so they
/// are saved together rather than one index at a time.
#[test]
fn blocks_that_moved_without_changing_the_count_save() {
    let original = "one\ntwo\n\n1. item\n\n<div>\nx\n</div>\n";
    let expected = "one\n\ntwo\n\n1. item\n2. <div>\n   x\n   </div>\n";
    assert_eq!(edit(original, expected).unwrap(), expected);
}

/// A patch that empties a line of the source takes the spaces left at the end
/// of it too: the quote's blank line `> ` spelled with a space, once the
/// quote's first paragraph moves out, is no line of the note's any more.
/// Spaces the kept text still ends its line with — a hard break — stay.
#[test]
fn a_line_a_patch_empties_keeps_no_trailing_spaces() {
    for (original, edited) in [
        ("12\n\n> 34\n> \n> 5\n", "17\n\n84\n\n> 5\n"),
        ("a b  \nc\n", "a  \nc\n"),
        ("```\n  \nx\n```\n", "```\n  \ny\n```\n"),
        // Two changes in one save, apart around an untouched link.
        (
            "x [l](u) y\n\n12\n\n> 34\n> \n> 5\n",
            "z [l](u) y\n\n17\n\n84\n\n> 5\n",
        ),
    ] {
        assert_eq!(
            edit(original, edited).as_deref(),
            Ok(edited),
            "{original:?}"
        );
    }
}
