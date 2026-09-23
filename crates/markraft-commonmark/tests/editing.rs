//! The editing behaviour the preset adds: input rules and corrections.

mod common;

use markraft_commonmark::schema as md;
use markraft_commonmark::{
    commonmark_extensions, commonmark_extensions_with_shortcuts, commonmark_schema,
};
use markraft_core::commands::{insert_text, run_command};
use markraft_core::{EditorState, EditorStateConfig, Node, Schema, Selection, attrs};

fn start_from(doc: Node, schema: &Schema, cursor: usize) -> EditorState {
    EditorState::create(
        EditorStateConfig::new(schema.clone())
            .doc(doc)
            .selection(Selection::cursor(cursor))
            .extensions(commonmark_extensions(schema)),
    )
    .expect("a valid starting state")
}

fn empty() -> (Schema, EditorState) {
    let schema = commonmark_schema();
    let doc = schema
        .doc([schema.node(md::PARAGRAPH, []).expect("a paragraph")])
        .expect("a document");
    let state = start_from(doc, &schema, 1);
    (schema, state)
}

fn type_all(state: &EditorState, text: &str) -> EditorState {
    let mut current = state.clone();
    for character in text.chars() {
        let command = insert_text(&character.to_string());
        current = run_command(&current, &command)
            .expect("typing applies")
            .expect("the transaction resolves")
            .state()
            .clone();
    }
    current
}

fn typed(text: &str) -> String {
    let (schema, state) = empty();
    schema.describe(type_all(&state, text).doc())
}

/// `text` typed into the first of two paragraphs, and the caret then moved
/// into the second: what the first reads as once the caret has left it.
fn typed_and_left(text: &str) -> String {
    let schema = commonmark_schema();
    let doc = schema
        .doc([
            schema.node(md::PARAGRAPH, []).expect("a paragraph"),
            schema
                .node(md::PARAGRAPH, [schema.text("x")])
                .expect("a paragraph"),
        ])
        .expect("a document");
    let typed = type_all(&start_from(doc, &schema, 1), text);
    let end = typed.doc().content_size() - 1;
    let left = typed
        .update([markraft_core::TransactionSpec::new().selection(Selection::cursor(end))])
        .expect("the caret moves")
        .state()
        .clone();
    schema.describe(left.doc())
}

#[test]
fn hash_markers_make_headings_of_every_level() {
    for level in 1..=6 {
        let marker = format!("{} ", "#".repeat(level));
        assert_eq!(
            typed(&marker),
            format!("doc(heading[level=Int({level})]())"),
            "{marker:?}"
        );
    }
    // Seven is not a heading, so the text stays.
    assert_eq!(typed("####### "), "doc(paragraph(\"####### \"))");
}

#[test]
fn markdown_shortcuts_follow_their_switch() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let schema = commonmark_schema();
    let shortcuts = Arc::new(AtomicBool::new(false));
    let doc = schema
        .doc([schema.node(md::PARAGRAPH, []).expect("a paragraph")])
        .expect("a document");
    let state = EditorState::create(
        EditorStateConfig::new(schema.clone())
            .doc(doc)
            .selection(Selection::cursor(1))
            .extensions(commonmark_extensions_with_shortcuts(
                &schema,
                shortcuts.clone(),
            )),
    )
    .expect("a valid starting state");

    // Off, the marker is only text.
    assert_eq!(
        schema.describe(type_all(&state, "# ").doc()),
        "doc(paragraph(\"# \"))"
    );
    // On, the same state makes a heading: the switch is read per keystroke.
    shortcuts.store(true, Ordering::Relaxed);
    assert_eq!(
        schema.describe(type_all(&state, "# ").doc()),
        "doc(heading[level=Int(1)]())"
    );
}

#[test]
fn bullet_markers_make_a_list_that_remembers_its_character() {
    for bullet in ["-", "*", "+"] {
        assert_eq!(
            typed(&format!("{bullet} ")),
            format!(
                "doc(bullet_list[bullet_char=Str(\"{bullet}\"),tight=Bool(true)](list_item(paragraph())))"
            )
        );
    }
}

#[test]
fn ordered_markers_keep_their_start_and_delimiter() {
    assert_eq!(
        typed("1. "),
        r#"doc(ordered_list[delimiter=Str("."),start=Int(1),tight=Bool(true)](list_item(paragraph())))"#
    );
    assert_eq!(
        typed("7) "),
        r#"doc(ordered_list[delimiter=Str(")"),start=Int(7),tight=Bool(true)](list_item(paragraph())))"#
    );
}

#[test]
fn a_quote_marker_wraps_the_block() {
    assert_eq!(
        typed("> "),
        r#"doc(blockquote[callout=Str(""),fold=Str(""),title=Str("")](paragraph()))"#
    );
}

#[test]
fn a_fence_makes_a_code_block_and_rules_stop_firing_inside_it() {
    assert_eq!(
        typed("``` "),
        r#"doc(code_block[fence_char=Str("`"),fence_length=Int(3),language=Str("")]())"#
    );
    assert_eq!(
        typed("```rust "),
        r#"doc(code_block[fence_char=Str("`"),fence_length=Int(3),language=Str("rust")]())"#
    );
    // Input rules are suppressed in code, so a marker typed there stays text.
    let (schema, state) = empty();
    let inside = type_all(&type_all(&state, "``` "), "- ");
    assert_eq!(
        schema.describe(inside.doc()),
        r#"doc(code_block[fence_char=Str("`"),fence_length=Int(3),language=Str("")]("- "))"#
    );
}

#[test]
fn three_dashes_make_a_thematic_break_and_leave_a_block_to_type_in() {
    assert_eq!(typed("---"), "doc(horizontal_rule, paragraph())");
}

#[test]
fn a_check_box_turns_a_bullet_item_into_a_task() {
    let (schema, state) = empty();
    let list = type_all(&state, "- ");
    for (marker, checked) in [("[ ] ", false), ("[x] ", true), ("[X] ", true)] {
        let task = type_all(&list, marker);
        assert_eq!(
            schema.describe(task.doc()),
            format!(
                "doc(bullet_list[bullet_char=Str(\"-\"),tight=Bool(true)](task_item[checked=Bool({checked})](paragraph())))"
            ),
            "{marker:?}"
        );
    }
    // Outside a list item there is nothing to convert.
    assert_eq!(typed("[ ] "), r#"doc(paragraph("[ ] "))"#);
}

#[test]
fn two_lists_a_reader_would_join_are_merged() {
    let schema = commonmark_schema();
    let paragraph = |text: &str| {
        let content = (!text.is_empty()).then(|| schema.text(text));
        schema.node(md::PARAGRAPH, content).expect("a paragraph")
    };
    let list = schema
        .node_with(
            md::BULLET_LIST,
            attrs! {"tight" => true, "bullet_char" => "-"},
            [schema
                .node(md::LIST_ITEM, [paragraph("a")])
                .expect("an item")],
        )
        .expect("a list");
    let doc = schema.doc([list, paragraph("")]).expect("a document");
    // The cursor sits in the empty paragraph after the list.
    let state = start_from(doc, &schema, 8);
    // Typing a bullet marker wraps it in a second list, which the correction
    // then joins to the one before it, because a reader would read one list.
    let after = type_all(&state, "- ");
    assert_eq!(
        schema.describe(after.doc()),
        r#"doc(bullet_list[bullet_char=Str("-"),tight=Bool(true)](list_item(paragraph("a")), list_item(paragraph())))"#
    );
}

#[test]
fn a_list_whose_marker_differs_is_left_alone() {
    let schema = commonmark_schema();
    let paragraph = |text: &str| {
        let content = (!text.is_empty()).then(|| schema.text(text));
        schema.node(md::PARAGRAPH, content).expect("a paragraph")
    };
    let list = schema
        .node_with(
            md::BULLET_LIST,
            attrs! {"tight" => true, "bullet_char" => "*"},
            [schema
                .node(md::LIST_ITEM, [paragraph("a")])
                .expect("an item")],
        )
        .expect("a list");
    let doc = schema.doc([list, paragraph("")]).expect("a document");
    let state = start_from(doc, &schema, 8);
    let after = type_all(&state, "- ");
    // Different bullet characters are two lists in CommonMark too.
    assert_eq!(
        schema.describe(after.doc()),
        r#"doc(bullet_list[bullet_char=Str("*"),tight=Bool(true)](list_item(paragraph("a"))), bullet_list[bullet_char=Str("-"),tight=Bool(true)](list_item(paragraph())))"#
    );
}

#[test]
fn everything_the_rules_build_survives_a_round_trip() {
    let codec = common::Codec::new();
    let schema = commonmark_schema();
    for typing in ["# ", "- ", "1. ", "> ", "``` ", "---"] {
        let doc = schema
            .doc([schema.node(md::PARAGRAPH, []).expect("a paragraph")])
            .expect("a document");
        let state = start_from(doc, &schema, 1);
        let built = type_all(&type_all(&state, typing), "x");
        let written = codec.write(built.doc());
        assert_eq!(
            codec.describe(&codec.parse(&written)),
            codec.describe(built.doc()),
            "{typing:?} produced {written:?}"
        );
    }
}

#[test]
fn a_closing_bracket_pair_makes_the_wiki_link_atom_once_the_caret_leaves() {
    let second = r#"paragraph("x")"#;
    for (typing, described) in [
        (
            "[[Note]]",
            r#"paragraph(wiki_link[alias=Str(""),embed=Bool(false),target=Str("Note")])"#,
        ),
        (
            "[[Note|Alias]]",
            r#"paragraph(wiki_link[alias=Str("Alias"),embed=Bool(false),target=Str("Note")])"#,
        ),
        (
            "![[x.png]]",
            r#"paragraph(wiki_link[alias=Str(""),embed=Bool(true),target=Str("x.png")])"#,
        ),
        (
            "see [[a#H]] now",
            concat!(
                r#"paragraph("see ", "#,
                r#"wiki_link[alias=Str(""),embed=Bool(false),target=Str("a#H")], " now")"#
            ),
        ),
        // Half of one is still the text it is.
        ("[[Note]", r#"paragraph("[[Note]")"#),
        // And so is a spelling the codec does not read.
        ("[[a|]]", r#"paragraph("[[a|]]")"#),
    ] {
        assert_eq!(
            typed_and_left(typing),
            format!("doc({described}, {second})"),
            "{typing:?}"
        );
    }
    // While the caret still touches it, it is the text being typed: the caret
    // can go back into it.
    assert_eq!(typed("[[Note]]"), r#"doc(paragraph("[[Note]]"))"#);
    // One the caret has typed past folds as soon as it is complete.
    assert_eq!(
        typed("see [[a#H]] now"),
        concat!(
            r#"doc(paragraph("see ", "#,
            r#"wiki_link[alias=Str(""),embed=Bool(false),target=Str("a#H")], " now"))"#
        )
    );
    // A code span keeps what is typed in it literal, and so does a code block.
    let (schema, state) = empty();
    assert_eq!(
        schema.describe(type_all(&type_all(&state, "``` "), "[[Note]]").doc()),
        r#"doc(code_block[fence_char=Str("`"),fence_length=Int(3),language=Str("")]("[[Note]]"))"#
    );
}

#[test]
fn a_wiki_link_is_text_until_the_source_spells_one() {
    // The canonicalising correction folds text into the atom only once a
    // reader would read one there, and never by the caret's own rule.
    let (schema, state) = empty();
    assert_eq!(
        schema.describe(type_all(&state, "[[Note]").doc()),
        r#"doc(paragraph("[[Note]"))"#
    );
    assert_eq!(
        schema.describe(type_all(&state, "[[a|]]").doc()),
        r#"doc(paragraph("[[a|]]"))"#
    );
}

#[test]
fn a_pasted_wiki_link_arrives_as_the_atom_without_an_input_rule() {
    let schema = commonmark_schema();
    let slice = markraft_commonmark::from_markdown_fragment(&schema, "see [[Note|Alias]] now")
        .expect("a fragment");
    let pasted = schema
        .doc(slice.content().iter().cloned())
        .expect("a document");
    assert_eq!(
        schema.describe(&pasted),
        concat!(
            r#"doc(paragraph("see ", "#,
            r#"wiki_link[alias=Str("Alias"),embed=Bool(false),target=Str("Note")], " now"))"#
        )
    );
}

#[test]
fn a_marker_typed_in_a_quote_turns_it_into_a_callout() {
    let (schema, state) = empty();
    let quote = type_all(&state, "> ");
    for (typing, kind, fold) in [
        ("[!note] ", "note", ""),
        ("[!tip]- ", "tip", "-"),
        ("[!warning]+ ", "warning", "+"),
        ("[!custom-type] ", "custom-type", ""),
    ] {
        assert_eq!(
            schema.describe(type_all(&quote, typing).doc()),
            format!(
                "doc(blockquote[callout=Str(\"{kind}\"),fold=Str(\"{fold}\"),title=Str(\"\")](paragraph()))"
            ),
            "{typing:?}"
        );
    }
    // Outside a quote, past its first block, or with a title already typed,
    // the marker stays the text it is.
    assert_eq!(typed("[!note] "), r#"doc(paragraph("[!note] "))"#);
    assert_eq!(
        schema.describe(type_all(&quote, "[!note] Title ").doc()),
        r#"doc(blockquote[callout=Str("note"),fold=Str(""),title=Str("")](paragraph("Title ")))"#,
        "the marker fires on its own space and the title is typed as body"
    );
    // A second marker inside a callout is ordinary text.
    let callout = type_all(&quote, "[!note] ");
    assert_eq!(
        schema.describe(type_all(&callout, "[!tip] ").doc()),
        r#"doc(blockquote[callout=Str("note"),fold=Str(""),title=Str("")](paragraph("[!tip] ")))"#
    );
    // Rules are suppressed in code.
    assert_eq!(
        schema.describe(type_all(&type_all(&state, "``` "), "> [!note] ").doc()),
        r#"doc(code_block[fence_char=Str("`"),fence_length=Int(3),language=Str("")]("> [!note] "))"#
    );
}

#[test]
fn undoing_the_callout_rule_gives_the_typed_marker_back() {
    let (schema, state) = empty();
    let built = type_all(&type_all(&state, "> "), "[!note] ");
    let undo = markraft_core::commands::undo_input_rule();
    let back = markraft_core::commands::run_command(&built, &undo)
        .expect("the rule can be taken back")
        .expect("the transaction resolves")
        .state()
        .clone();
    assert_eq!(
        schema.describe(back.doc()),
        r#"doc(blockquote[callout=Str(""),fold=Str(""),title=Str("")](paragraph("[!note] ")))"#
    );
}

// -- reference links ------------------------------------------------------

/// The state of `source`, the caret at the start.
fn opened(source: &str) -> (Schema, EditorState) {
    let schema = commonmark_schema();
    let doc = markraft_commonmark::from_markdown(&schema, source).expect("parses");
    let state = start_from(doc, &schema, 1);
    (schema, state)
}

fn apply(state: &EditorState, changes: Vec<markraft_core::Change>) -> EditorState {
    let tr = state
        .update([markraft_core::TransactionSpec::new().changes(changes)])
        .expect("the edit applies");
    assert_eq!(
        tr.annotation(markraft_core::protocol::corrections_diverged()),
        None,
        "the corrections settle"
    );
    tr.state().clone()
}

/// The text each run of link marks in `doc` covers, with its destination.
fn links(schema: &Schema, doc: &Node) -> Vec<(String, String)> {
    let link = schema.mark_id(md::LINK).expect("the link mark");
    let mut out: Vec<(String, String)> = Vec::new();
    let mut open = false;
    doc.descendants(&mut |node, _, _, _| {
        if !node.is_leaf() {
            open = false;
            return true;
        }
        let Some(mark) = node.marks().get(link) else {
            open = false;
            return true;
        };
        let href = mark
            .attrs
            .get("href")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string();
        let text = node.text().unwrap_or_default();
        match out.last_mut() {
            Some((linked, last)) if open && *last == href => linked.push_str(text),
            _ => out.push((text.to_string(), href)),
        }
        open = true;
        true
    });
    out
}

fn text(schema: &Schema, content: &str) -> markraft_core::Slice {
    markraft_core::Slice::from_fragment(markraft_core::Fragment::from_node(schema.text(content)))
}

#[test]
fn editing_a_definition_moves_every_link_that_refers_to_it() {
    // paragraph `[a][ref]` is 0..10, the definition block's text starts at 11.
    let (schema, state) = opened("[a][ref] and [ref]\n\n> [ref][]\n\n[ref]: /u");
    assert_eq!(
        links(&schema, state.doc()),
        [
            ("[a][ref]".to_string(), "/u".to_string()),
            ("[ref]".to_string(), "/u".to_string()),
            ("[ref][]".to_string(), "/u".to_string()),
        ]
    );
    let definition = state.doc().content_size() - "/u".len() - 1;
    let state = apply(
        &state,
        vec![markraft_core::Change::replace(
            definition + 1,
            definition + 2,
            text(&schema, "v"),
        )],
    );
    assert_eq!(
        markraft_commonmark::to_markdown(&schema, state.doc()),
        "[a][ref] and [ref]\n\n> [ref][]\n\n[ref]: /v"
    );
    let hrefs: Vec<String> = links(&schema, state.doc())
        .into_iter()
        .map(|(_, href)| href)
        .collect();
    assert_eq!(hrefs, ["/v", "/v", "/v"]);
}

#[test]
fn deleting_a_definition_leaves_its_links_plain_text_and_a_new_one_links_them() {
    let (schema, state) = opened("[a][ref]\n\n[ref]: /u");
    let paragraph = state.doc().child(0).node_size();
    let end = state.doc().content_size();
    let gone = apply(&state, vec![markraft_core::Change::delete(paragraph, end)]);
    assert_eq!(schema.describe(gone.doc()), r#"doc(paragraph("[a][ref]"))"#);
    // A definition block put back links it again, and typing into it moves
    // the link with every keystroke.
    let definition = schema
        .node(md::RAW_BLOCK, [schema.text("[ref]: /w")])
        .expect("a raw block");
    let end = gone.doc().content_size();
    let back = apply(
        &gone,
        vec![markraft_core::Change::insert(
            end,
            markraft_core::Slice::from_fragment(markraft_core::Fragment::from_node(definition)),
        )],
    );
    assert_eq!(
        links(&schema, back.doc()),
        [("[a][ref]".to_string(), "/w".to_string())]
    );
    let end = back.doc().content_size() - 1;
    let back = back
        .update([markraft_core::TransactionSpec::new().selection(Selection::cursor(end))])
        .expect("the caret moves")
        .state()
        .clone();
    let typed = type_all(&back, "x");
    assert_eq!(
        markraft_commonmark::to_markdown(&schema, typed.doc()),
        "[a][ref]\n\n[ref]: /wx"
    );
    assert_eq!(
        links(&schema, typed.doc()),
        [("[a][ref]".to_string(), "/wx".to_string())]
    );
    // A label that no longer matches unlinks it.
    let label = typed.doc().child(0).node_size() + 2;
    let renamed = apply(
        &typed,
        vec![markraft_core::Change::replace(
            label,
            label + 3,
            text(&schema, "rex"),
        )],
    );
    assert!(links(&schema, renamed.doc()).is_empty());
}

#[test]
fn one_edit_to_a_reference_and_its_definition_settles_both() {
    let (schema, state) = opened("[a][ref] x\n\n[ref]: /u");
    let definition = state.doc().content_size() - "/u".len() - 1;
    // Type after the reference and change the destination in one transaction.
    let state = apply(
        &state,
        vec![
            markraft_core::Change::insert(11, text(&schema, "y")),
            markraft_core::Change::replace(definition + 1, definition + 2, text(&schema, "v")),
        ],
    );
    assert_eq!(
        markraft_commonmark::to_markdown(&schema, state.doc()),
        "[a][ref] xy\n\n[ref]: /v"
    );
    assert_eq!(
        links(&schema, state.doc()),
        [("[a][ref]".to_string(), "/v".to_string())]
    );
}

/// A list item's first line holds what its marker would read as more of
/// itself — a check box, the rest of a thematic break — as text while the
/// caret is on it, and gets its backslash when the caret leaves.
#[test]
fn text_a_list_marker_would_complete_is_escaped_once_the_caret_leaves() {
    let schema = commonmark_schema();
    for (typed, kept) in [("- [ ]", r"\[ ]"), ("- --", r"\--")] {
        let doc = markraft_commonmark::from_markdown(&schema, "x\n").expect("a document");
        let state = start_from(doc, &schema, 2);
        let state = run_command(&state, &markraft_core::commands::split_block())
            .expect("Enter applies")
            .expect("the transaction resolves")
            .state()
            .clone();
        let state = type_all(&state, typed);
        let left = state
            .update([markraft_core::TransactionSpec::new().selection(Selection::cursor(1))])
            .expect("the caret moves")
            .state()
            .clone();
        assert_eq!(
            markraft_commonmark::to_markdown(&schema, left.doc()),
            format!("x\n\n- {kept}"),
            "{typed:?}"
        );
        let reread = markraft_commonmark::from_markdown(&schema, &format!("x\n\n- {kept}\n"))
            .expect("the file parses");
        assert_eq!(reread, *left.doc(), "{typed:?}");
    }
}

/// Type `line` into an empty document, as a writer would, and press the
/// Enter rule at its end.
fn entered(line: &str) -> Option<EditorState> {
    let (_, state) = empty();
    let state = type_all(&state, line);
    run_command(&state, &markraft_commonmark::block_from_line())
        .map(|result| result.expect("the transaction resolves").state().clone())
}

#[test]
fn enter_after_a_fence_opens_a_code_block_in_its_language() {
    let schema = commonmark_schema();
    for (line, markdown) in [
        ("```", "```\n```"),
        ("```rust", "```rust\n```"),
        ("~~~~", "~~~~\n~~~~"),
        ("- ```py", "- ```py\n  ```"),
    ] {
        let state = entered(line).unwrap_or_else(|| panic!("{line:?} makes a block"));
        assert_eq!(
            markraft_commonmark::to_markdown(&schema, state.doc()),
            markdown,
            "{line:?}"
        );
        let typed = markraft_commonmark::to_markdown(&schema, type_all(&state, "x").doc());
        assert_eq!(
            typed,
            markdown
                .replacen('\n', "\nx\n", 1)
                .replace("\nx\n  ", "\n  x\n  "),
            "{line:?}: the caret is in the code"
        );
    }
    for line in ["``", "```a`b", "text```", "- [ ]```"] {
        assert!(entered(line).is_none(), "{line:?}");
    }
}

#[test]
fn enter_after_a_header_row_makes_a_table_with_a_row_to_type_in() {
    let schema = commonmark_schema();
    let state = entered("|a|**b**|").expect("a table");
    assert_eq!(
        schema.describe(state.doc()),
        r#"doc(table[alignments=Str("none,none")](table_row(table_cell("a"), table_cell("**"{strong,syntax}, "b"{strong}, "**"{strong,syntax})), table_row(table_cell(), table_cell())))"#
    );
    let typed = type_all(&state, "c");
    assert_eq!(
        markraft_commonmark::to_markdown(&schema, typed.doc()),
        "| a   | **b** |\n| --- | ----- |\n| c   |       |"
    );
    for line in ["|a", "a|b|", "|", r"|a\|"] {
        assert!(entered(line).is_none(), "{line:?}");
    }
}

#[test]
fn a_footnote_marker_makes_a_definition_its_references_then_resolve_to() {
    let schema = commonmark_schema();
    let doc = markraft_commonmark::from_markdown(&schema, "see[^n]\n").expect("a document");
    let state = start_from(doc, &schema, 8);
    assert_eq!(
        schema.describe(state.doc()),
        r#"doc(paragraph("see[^n]"))"#,
        "no definition, no reference"
    );
    let state = run_command(&state, &markraft_core::commands::split_block())
        .expect("Enter applies")
        .expect("the transaction resolves")
        .state()
        .clone();
    let state = type_all(&state, "[^n]: the note");
    assert_eq!(
        schema.describe(state.doc()),
        r#"doc(paragraph("see", "[^"{footnote_reference,syntax}, "n"{footnote_reference}, "]"{footnote_reference,syntax}), footnote_definition[label=Str("n")](paragraph("the note")))"#
    );
    assert_eq!(
        markraft_commonmark::to_markdown(&schema, state.doc()),
        "see[^n]\n\n[^n]: the note"
    );
}

#[test]
fn enter_at_the_end_of_a_footnote_goes_on_after_the_definition() {
    let schema = commonmark_schema();
    let doc = markraft_commonmark::from_markdown(&schema, "see[^n]\n\n[^n]: the note\n")
        .expect("a document");
    let end = doc.content_size() - 2;
    let state = start_from(doc, &schema, end);
    let state = run_command(&state, &markraft_commonmark::block_from_line())
        .expect("Enter leaves the footnote")
        .expect("the transaction resolves")
        .state()
        .clone();
    let state = type_all(&state, "## after");
    assert_eq!(
        markraft_commonmark::to_markdown(&schema, state.doc()),
        "see[^n]\n\n[^n]: the note\n\n## after"
    );
    // Anywhere but the end of the definition's last paragraph, Enter is the
    // ordinary split.
    let doc = markraft_commonmark::from_markdown(&schema, "see[^n]\n\n[^n]: the note\n")
        .expect("a document");
    let middle = doc.content_size() - 4;
    let state = start_from(doc, &schema, middle);
    assert!(run_command(&state, &markraft_commonmark::block_from_line()).is_none());
}

#[test]
fn enter_after_a_thematic_break_of_stars_or_underscores_makes_a_divider() {
    let schema = commonmark_schema();
    for line in ["***", "___", "*****", "_ _ _"] {
        let state = entered(line).unwrap_or_else(|| panic!("{line:?} makes a divider"));
        assert_eq!(
            schema.describe(state.doc()),
            "doc(horizontal_rule, paragraph())",
            "{line:?}"
        );
        let typed = type_all(&state, "x");
        assert_eq!(
            markraft_commonmark::to_markdown(&schema, typed.doc()),
            "---\n\nx",
            "{line:?}: the caret is in the paragraph after it"
        );
    }
    for line in ["**", "***a", "*_*"] {
        assert!(entered(line).is_none(), "{line:?}");
    }
}

/// `state` with the caret moved to `pos`, as an arrow key or a click moves it.
fn moved(state: &EditorState, pos: usize) -> EditorState {
    state
        .update([markraft_core::TransactionSpec::new().selection(Selection::cursor(pos))])
        .expect("the caret moves")
        .state()
        .clone()
}

/// A caret that reaches a picture finds its source, as in Typora: it can walk
/// into `![alt](logo.png)` and edit it, and the picture comes back once the
/// caret has gone. The file never sees the difference.
#[test]
fn a_caret_reaching_a_picture_finds_its_source() {
    let source = "![alt](logo.png)\n\nafter";
    let (schema, state) = opened(source);
    let picture = r#"image[alt=Str("alt"),source=Str(""),src=Str("logo.png"),title=Str("")]"#;
    let away = moved(&state, state.doc().content_size() - 1);
    assert_eq!(
        schema.describe(away.doc()),
        format!(r#"doc(paragraph({picture}), paragraph("after"))"#)
    );

    // Before the picture: its spelling, the caret where it starts.
    let before = moved(&away, 1);
    assert_eq!(
        schema.describe(before.doc()),
        r#"doc(paragraph("![alt](logo.png)"), paragraph("after"))"#
    );
    assert_eq!(before.selection(), &Selection::cursor(1));
    assert_eq!(
        markraft_commonmark::to_markdown(&schema, before.doc()),
        source
    );

    // Inside it, and editing it.
    let inside = moved(&before, 1 + "![al".len());
    let edited = type_all(&inside, "t");
    assert_eq!(
        markraft_commonmark::to_markdown(&schema, edited.doc()),
        "![altt](logo.png)\n\nafter"
    );

    // Gone again: the picture, with what was typed.
    let left = moved(&edited, edited.doc().content_size() - 1);
    assert_eq!(
        schema.describe(left.doc()),
        r#"doc(paragraph(image[alt=Str("altt"),source=Str(""),src=Str("logo.png"),title=Str("")]), paragraph("after"))"#
    );

    // After the picture: the caret at the end of its spelling.
    let after = moved(&away, 2);
    assert_eq!(
        schema.describe(after.doc()),
        r#"doc(paragraph("![alt](logo.png)"), paragraph("after"))"#
    );
    assert_eq!(
        after.selection(),
        &Selection::cursor(1 + "![alt](logo.png)".chars().count())
    );
}

/// A caret moving along its own line out of a spelling folds it too, not only
/// one leaving for another block.
#[test]
fn a_spelling_folds_when_the_caret_leaves_along_its_line() {
    let (schema, state) = opened("see [[Note]] now");
    let reached = moved(&state, 1 + "see ".len());
    assert_eq!(
        schema.describe(reached.doc()),
        r#"doc(paragraph("see [[Note]] now"))"#
    );
    let end = reached.doc().content_size() - 1;
    let left = moved(&reached, end);
    assert_eq!(
        schema.describe(left.doc()),
        concat!(
            r#"doc(paragraph("see ", "#,
            r#"wiki_link[alias=Str(""),embed=Bool(false),target=Str("Note")], " now"))"#
        )
    );
}

/// A shortcode is an emoji once the caret has left it, and its spelling while
/// the caret is in it — typed or reached — as in Typora. The file keeps the
/// shortcode either way.
#[test]
fn a_shortcode_reads_as_its_emoji_once_the_caret_leaves() {
    // Typed: the spelling while the caret is at its closing colon.
    assert_eq!(typed("hi :smile:"), r#"doc(paragraph("hi :smile:"))"#);
    assert_eq!(
        typed_and_left("hi :smile:"),
        r#"doc(paragraph("hi ", emoji[code=Str("smile")]), paragraph("x"))"#
    );

    // Read, reached and left.
    let source = "a :tada: b\n\nafter";
    let (schema, state) = opened(source);
    let away = moved(&state, state.doc().content_size() - 1);
    assert_eq!(
        schema.describe(away.doc()),
        r#"doc(paragraph("a ", emoji[code=Str("tada")], " b"), paragraph("after"))"#
    );
    let reached = moved(&away, 3);
    assert_eq!(
        schema.describe(reached.doc()),
        r#"doc(paragraph("a :tada: b"), paragraph("after"))"#
    );
    assert_eq!(
        markraft_commonmark::to_markdown(&schema, reached.doc()),
        source
    );
    let left = moved(&reached, reached.doc().content_size() - 1);
    assert_eq!(left.doc(), away.doc());
    assert_eq!(
        markraft_commonmark::to_markdown(&schema, left.doc()),
        source
    );
}
