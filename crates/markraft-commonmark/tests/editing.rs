//! The editing behaviour the preset adds: input rules and corrections.

mod common;

use markraft_commonmark::schema as md;
use markraft_commonmark::{commonmark_extensions, commonmark_schema};
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
fn a_closing_bracket_pair_makes_the_wiki_link_atom() {
    for (typing, described) in [
        (
            "[[Note]]",
            r#"doc(paragraph(wiki_link[alias=Str(""),embed=Bool(false),target=Str("Note")]))"#,
        ),
        (
            "[[Note|Alias]]",
            r#"doc(paragraph(wiki_link[alias=Str("Alias"),embed=Bool(false),target=Str("Note")]))"#,
        ),
        (
            "![[x.png]]",
            r#"doc(paragraph(wiki_link[alias=Str(""),embed=Bool(true),target=Str("x.png")]))"#,
        ),
        (
            "see [[a#H]] now",
            concat!(
                r#"doc(paragraph("see ", "#,
                r#"wiki_link[alias=Str(""),embed=Bool(false),target=Str("a#H")], " now"))"#
            ),
        ),
        // Half of one is still the text it is.
        ("[[Note]", r#"doc(paragraph("[[Note]"))"#),
        // And so is a spelling the codec does not read.
        ("[[a|]]", r#"doc(paragraph("[[a|]]"))"#),
    ] {
        assert_eq!(typed(typing), described, "{typing:?}");
    }
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
