//! The editing behaviour the preset adds: input rules and corrections.

mod common;

use markraft_doc::commands::{insert_text, run_command};
use markraft_doc::{EditorState, EditorStateConfig, Node, Schema, Selection, attrs};
use markraft_markdown::schema as md;
use markraft_markdown::{commonmark_extensions, commonmark_schema};

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
    assert_eq!(typed("> "), "doc(blockquote(paragraph()))");
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
