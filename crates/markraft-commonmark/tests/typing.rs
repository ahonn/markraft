//! Every keystroke can be written back.
//!
//! An editor over a file refuses a transaction whose document the file cannot
//! hold, so a character the codec has no spelling for is a character the
//! writer cannot type. These cases type short runs of the characters that open
//! blocks into each kind of place a line starts — with the input rules and
//! corrections an editor runs, one character at a time — and require the file
//! to take every step.

use markraft_commonmark::{SourceDocument, commonmark_extensions, commonmark_schema};
use markraft_core::commands::{Command, insert_text, run_command, split_block};
use markraft_core::{EditorState, EditorStateConfig, Schema, Selection};

/// A file, and where in it the typing happens: the caret at the end of the
/// last textblock, after `enter` if there is one.
struct Place {
    source: &'static str,
    enter: Option<fn(&Schema) -> Command>,
}

fn new_paragraph(_: &Schema) -> Command {
    split_block()
}

fn new_item(schema: &Schema) -> Command {
    let item = schema
        .node_id(markraft_commonmark::schema::LIST_ITEM)
        .expect("a list item type");
    markraft_core::commands::split_list_item(item)
}

const PLACES: &[Place] = &[
    Place {
        source: "",
        enter: None,
    },
    Place {
        source: "start\n",
        enter: Some(new_paragraph),
    },
    Place {
        source: "- item\n",
        enter: Some(new_item),
    },
    Place {
        source: "1. item\n",
        enter: Some(new_item),
    },
    Place {
        source: "> quote\n",
        enter: Some(new_paragraph),
    },
    Place {
        source: "# Heading ",
        enter: None,
    },
    Place {
        source: "| a |\n| - |\n| b |\n",
        enter: None,
    },
];

fn at_end(schema: &Schema, source: &SourceDocument) -> EditorState {
    let mut end = 1;
    source.document().descendants(&mut |node, pos, _, _| {
        if node.is_textblock(schema) {
            end = pos + 1 + node.content_size();
        }
        true
    });
    EditorState::create(
        EditorStateConfig::new(schema.clone())
            .doc(source.document().clone())
            .selection(Selection::cursor(end))
            .extensions(commonmark_extensions(schema)),
    )
    .expect("a valid starting state")
}

fn applied(state: &EditorState, command: &Command) -> EditorState {
    run_command(state, command)
        .expect("the command applies")
        .expect("the transaction resolves")
        .state()
        .clone()
}

/// Type `text` at `place`, requiring the file to take each character.
fn type_at(schema: &Schema, place: &Place, text: &str) {
    let source = SourceDocument::parse(schema, place.source).expect("the source parses");
    let mut state = at_end(schema, &source);
    if let Some(enter) = place.enter {
        state = applied(&state, &enter(schema));
    }
    let mut typed = String::new();
    for character in text.chars() {
        typed.push(character);
        state = applied(&state, &insert_text(&character.to_string()));
        if let Err(error) = source.render(schema, state.doc()) {
            panic!(
                "{:?}: typing {typed:?} leaves {} the file cannot hold: {error}",
                place.source,
                schema.describe(state.doc())
            );
        }
    }
}

fn strings(alphabet: &str, length: usize) -> Vec<String> {
    let mut out = vec![String::new()];
    for _ in 0..length {
        out = out
            .iter()
            .flat_map(|prefix| alphabet.chars().map(move |c| format!("{prefix}{c}")))
            .collect();
    }
    out
}

#[test]
fn the_characters_that_open_blocks_can_all_be_typed_where_a_line_starts() {
    let schema = commonmark_schema();
    // Two of anything that can start a block; three of the ones whose third
    // character is still undecided (`1. `, `[ ]`, a fence, a rule).
    let mut texts = strings("1.)-*+#>[]x `~|=<!", 2);
    texts.extend(strings("1.-#>[] x`", 3));
    for place in PLACES {
        for text in &texts {
            type_at(&schema, place, text);
        }
    }
}

#[test]
fn a_list_marker_and_a_check_box_type_through_to_a_task() {
    let schema = commonmark_schema();
    for place in PLACES {
        for text in [
            "1. 5",
            "12. x",
            "1) x",
            "- [ ] 7",
            "- [x] done",
            "```js",
            "~~~",
            "[^1]: note",
            "a[^1] b",
            "H~2~O",
        ] {
            type_at(&schema, place, text);
        }
    }
}
