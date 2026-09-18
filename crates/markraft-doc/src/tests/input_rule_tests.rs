//! Input rules: the conversions, the undo step and the history boundary.

use crate::attr::Attrs;
use crate::change::Change;
use crate::commands::structure::markup_of;
use crate::commands::{
    InputRule, InputRuleMatch, chain, input_rule_field, input_rules, insert_text, run_command,
    split_block, undo_input_rule,
};
use crate::history::{HistoryConfig, history, undo};
use crate::node::Markup;
use crate::selection::Selection;
use crate::slice::{Slice, Token};
use crate::state::{EditorState, Extension, TransactionSpec};

use super::support::*;

fn type_text(state: &EditorState, text: &str) -> EditorState {
    let command = insert_text(text);
    run_command(state, &command)
        .expect("typing applies")
        .expect("the transaction resolves")
        .state()
        .clone()
}

fn type_all(state: &EditorState, text: &str) -> EditorState {
    let mut current = state.clone();
    for ch in text.chars() {
        current = type_text(&current, &ch.to_string());
    }
    current
}

/// Replace the block's own open and close tokens, which is what a block
/// conversion is at the token level.
fn retype_block(m: &InputRuleMatch<'_>, markup: Markup) -> Vec<Change> {
    let before = m.block_start - 1;
    let after = m.block_start + m.block.content_size();
    vec![
        Change::replace(
            before,
            before + 1,
            Slice::from_tokens(&[Token::Open(markup.clone())]),
        ),
        Change::delete(m.from, m.to),
        Change::replace(
            after,
            after + 1,
            Slice::from_tokens(&[Token::Close(markup)]),
        ),
    ]
}

fn markdown_rules() -> Vec<InputRule> {
    vec![
        InputRule::block_start("- ", |m| {
            let list = m.schema.node_id("bullet_list")?;
            let item = m.schema.node_id("list_item")?;
            let before = m.block_start - 1;
            let after = m.block_start + m.block.content_size() + 1;
            Some(TransactionSpec::new().changes([
                Change::insert(
                    before,
                    Slice::from_tokens(&[
                        Token::Open(markup_of(m.schema, list, &Attrs::empty())),
                        Token::Open(markup_of(m.schema, item, &Attrs::empty())),
                    ]),
                ),
                Change::delete(m.from, m.to),
                Change::insert(
                    after,
                    Slice::from_tokens(&[
                        Token::Close(markup_of(m.schema, item, &Attrs::empty())),
                        Token::Close(markup_of(m.schema, list, &Attrs::empty())),
                    ]),
                ),
            ]))
        }),
        InputRule::new(
            |before| {
                let hashes = before.chars().take_while(|c| *c == '#').count();
                (hashes > 0 && hashes <= 6 && before.chars().count() == hashes + 1)
                    .then_some(hashes + 1)
                    .filter(|_| before.ends_with(' '))
            },
            |m| {
                let level = m.text.chars().take_while(|c| *c == '#').count() as i64;
                let heading = m.schema.node_id("heading")?;
                let markup = markup_of(m.schema, heading, &crate::attrs! {"level" => level});
                Some(TransactionSpec::new().changes(retype_block(m, markup)))
            },
        ),
        InputRule::block_start("```", |m| {
            let code = m.schema.node_id("code_block")?;
            let markup = markup_of(m.schema, code, &Attrs::empty());
            Some(TransactionSpec::new().changes(retype_block(m, markup)))
        }),
    ]
}

fn rule_state() -> EditorState {
    let schema = shared_schema();
    state(
        doc(&schema, [n(&schema, "paragraph", [])]),
        Extension::all([
            input_rules(markdown_rules()),
            history(HistoryConfig::default()),
        ]),
    )
}

#[test]
fn a_bullet_marker_wraps_the_paragraph_in_a_list() {
    let schema = shared_schema();
    let after = type_all(&rule_state(), "- ");
    assert_eq!(
        schema.describe(after.doc()),
        r#"doc(bullet_list(list_item(paragraph())))"#
    );
    let typed = type_text(&after, "x");
    assert_eq!(
        schema.describe(typed.doc()),
        r#"doc(bullet_list(list_item(paragraph("x"))))"#
    );
}

#[test]
fn a_hash_marker_makes_a_heading_of_the_right_level() {
    let schema = shared_schema();
    let after = type_all(&rule_state(), "## ");
    assert_eq!(
        schema.describe(after.doc()),
        r#"doc(heading[level=Int(2)]())"#
    );
}

#[test]
fn a_fence_makes_a_code_block_and_rules_do_not_fire_inside_it() {
    let schema = shared_schema();
    let after = type_all(&rule_state(), "```");
    assert_eq!(schema.describe(after.doc()), r#"doc(code_block())"#);
    // Input rules are suppressed in code, so the marker stays literal.
    let inside = type_all(&after, "- ");
    assert_eq!(schema.describe(inside.doc()), r#"doc(code_block("- "))"#);
}

#[test]
fn undo_input_rule_keeps_the_typed_text() {
    let schema = shared_schema();
    let after = type_all(&rule_state(), "- ");
    assert!(
        after
            .field(input_rule_field())
            .expect("configured")
            .is_some()
    );
    let command = undo_input_rule();
    let back = run_command(&after, &command)
        .expect("there is a rule to undo")
        .expect("the transaction resolves")
        .state()
        .clone();
    assert_eq!(schema.describe(back.doc()), r#"doc(paragraph("- "))"#);
    assert_eq!(back.selection(), &Selection::cursor(3));
    // The record is consumed, so a second press does nothing.
    assert!(
        back.field(input_rule_field())
            .expect("configured")
            .is_none()
    );
}

#[test]
fn undoing_after_a_rule_is_a_step_of_its_own() {
    let schema = shared_schema();
    let after = type_all(&rule_state(), "- x");
    assert_eq!(
        schema.describe(after.doc()),
        r#"doc(bullet_list(list_item(paragraph("x"))))"#
    );
    let step = |state: &EditorState| {
        state
            .update([undo(state).expect("something to undo")])
            .expect("undo resolves")
            .state()
            .clone()
    };
    // The typed character comes back first...
    let once = step(&after);
    assert_eq!(
        schema.describe(once.doc()),
        r#"doc(bullet_list(list_item(paragraph())))"#
    );
    // ...then the conversion together with the space that triggered it...
    let twice = step(&once);
    assert_eq!(schema.describe(twice.doc()), r#"doc(paragraph("-"))"#);
    // ...and finally the marker itself.
    let thrice = step(&twice);
    assert_eq!(schema.describe(thrice.doc()), r#"doc(paragraph())"#);
}

#[test]
fn a_rule_does_not_fire_for_a_non_typing_transaction() {
    let schema = shared_schema();
    let start = rule_state();
    let moved = start
        .update([TransactionSpec::new()
            .changes([crate::tests::support::insert_text(&schema, 1, "- ")])
            .user_event("input.paste")])
        .expect("resolves")
        .state()
        .clone();
    assert_eq!(schema.describe(moved.doc()), r#"doc(paragraph("- "))"#);
}

#[test]
fn a_rule_composes_with_a_command_chain() {
    let schema = shared_schema();
    let after = type_all(&rule_state(), "- a");
    let enter = chain([split_block()]);
    let split = run_command(&after, &enter)
        .expect("applies")
        .expect("resolves")
        .state()
        .clone();
    assert_eq!(
        schema.describe(split.doc()),
        r#"doc(bullet_list(list_item(paragraph("a"), paragraph())))"#
    );
}
