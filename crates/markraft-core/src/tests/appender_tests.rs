//! Transaction appenders: reacting to a transaction with another one.

use std::sync::Arc;

use super::support::*;
use crate::history::{HistoryConfig, history, undo, undo_depth};
use crate::schema::Schema;
use crate::state::{
    EditorState, Extension, MAX_APPENDED_TRANSACTIONS, TransactionAppenderFn, TransactionSpec,
    appended, appenders_diverged, transaction_appender,
};

/// An appender that adds `text` at the end of the first paragraph, optionally
/// reacting to its own output as well.
fn adder(text: &'static str, react_to_appended: bool) -> TransactionAppenderFn {
    Arc::new(move |tr| {
        if !react_to_appended && tr.annotation(appended()).is_some() {
            return None;
        }
        if !tr.is_user_event("input.type") && !react_to_appended {
            return None;
        }
        let schema = tr.start_state().schema().clone();
        let at = tr.new_doc().content_size() - 1;
        Some(
            TransactionSpec::new()
                .changes([insert_text(&schema, at, text)])
                .user_event("insert"),
        )
    })
}

fn sample(schema: &Schema, extensions: Extension) -> EditorState {
    state(
        doc(schema, [n(schema, "paragraph", [t(schema, "hello")])]),
        extensions,
    )
}

#[test]
fn an_appender_that_declines_leaves_the_dispatch_alone() {
    let schema = shared_schema();
    let quiet: TransactionAppenderFn = Arc::new(|_| None);
    let start = sample(&schema, transaction_appender().of(quiet));
    let all = start
        .update_with_appended([TransactionSpec::new()
            .changes([insert_text(&schema, 3, "A")])
            .user_event("input.type")])
        .unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(
        schema.describe(all[0].new_doc()),
        r#"doc(paragraph("heAllo"))"#
    );
}

#[test]
fn plain_update_does_not_run_appenders() {
    let schema = shared_schema();
    let start = sample(&schema, transaction_appender().of(adder("!", false)));
    let tr = start
        .update([TransactionSpec::new()
            .changes([insert_text(&schema, 3, "A")])
            .user_event("input.type")])
        .unwrap();
    assert_eq!(schema.describe(tr.new_doc()), r#"doc(paragraph("heAllo"))"#);
}

#[test]
fn an_appended_transaction_is_a_separate_transaction_on_the_result_state() {
    let schema = shared_schema();
    let start = sample(&schema, transaction_appender().of(adder("!", false)));
    let all = start
        .update_with_appended([TransactionSpec::new()
            .changes([insert_text(&schema, 3, "A")])
            .user_event("input.type")])
        .unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(
        schema.describe(all[0].new_doc()),
        r#"doc(paragraph("heAllo"))"#
    );
    assert_eq!(
        schema.describe(all[1].new_doc()),
        r#"doc(paragraph("heAllo!"))"#
    );
    // Each one starts from the state the one before it produced.
    assert_eq!(all[1].start_state().doc(), all[0].new_doc());

    let info = all[1].annotation(appended()).expect("an appended marker");
    assert_eq!(info.trigger_user_event.as_deref(), Some("input.type"));
    assert_eq!(info.depth, 1);
    assert!(all[0].annotation(appended()).is_none());
}

#[test]
fn an_appended_transaction_undoes_with_the_one_that_triggered_it() {
    let schema = shared_schema();
    let start = sample(
        &schema,
        Extension::all([
            history(HistoryConfig::default()),
            transaction_appender().of(adder("!", false)),
        ]),
    );
    let all = start
        .update_with_appended([TransactionSpec::new()
            .changes([insert_text(&schema, 3, "A")])
            .user_event("input.type")
            .time(0)])
        .unwrap();
    let after = all
        .last()
        .expect("at least one transaction")
        .state()
        .clone();
    assert_eq!(schema.describe(after.doc()), r#"doc(paragraph("heAllo!"))"#);
    assert_eq!(undo_depth(&after), 1, "one entry for the pair");

    let undone = after
        .update([undo(&after).expect("something to undo")])
        .unwrap()
        .state()
        .clone();
    assert_eq!(schema.describe(undone.doc()), r#"doc(paragraph("hello"))"#);
    assert_eq!(undo_depth(&undone), 0);
}

#[test]
fn an_appended_transaction_does_not_break_the_typing_run_it_belongs_to() {
    let schema = shared_schema();
    let start = sample(
        &schema,
        Extension::all([
            history(HistoryConfig::default()),
            transaction_appender().of(adder("!", false)),
        ]),
    );
    let all = start
        .update_with_appended([TransactionSpec::new()
            .changes([insert_text(&schema, 3, "A")])
            .user_event("input.type")
            .time(0)])
        .unwrap();
    let after = all
        .last()
        .expect("at least one transaction")
        .state()
        .clone();
    // The next keystroke still joins the entry the first one opened.
    let all = after
        .update_with_appended([TransactionSpec::new()
            .changes([insert_text(&schema, 4, "B")])
            .user_event("input.type")
            .time(10)])
        .unwrap();
    let after = all
        .last()
        .expect("at least one transaction")
        .state()
        .clone();
    assert_eq!(undo_depth(&after), 1);
    let undone = after
        .update([undo(&after).expect("something to undo")])
        .unwrap()
        .state()
        .clone();
    assert_eq!(schema.describe(undone.doc()), r#"doc(paragraph("hello"))"#);
}

#[test]
fn add_to_history_false_propagates_to_what_is_appended() {
    let schema = shared_schema();
    let start = sample(
        &schema,
        Extension::all([
            history(HistoryConfig::default()),
            transaction_appender().of(adder("!", false)),
        ]),
    );
    let all = start
        .update_with_appended([TransactionSpec::new()
            .changes([insert_text(&schema, 3, "A")])
            .user_event("input.type")
            .add_to_history(false)
            .time(0)])
        .unwrap();
    assert_eq!(all.len(), 2);
    let after = all
        .last()
        .expect("at least one transaction")
        .state()
        .clone();
    assert_eq!(schema.describe(after.doc()), r#"doc(paragraph("heAllo!"))"#);
    assert_eq!(
        undo_depth(&after),
        0,
        "neither the trigger nor what it appended is recorded"
    );
}

#[test]
fn a_chain_that_will_not_stop_is_cut_at_the_bound() {
    let schema = shared_schema();
    let start = sample(&schema, transaction_appender().of(adder("!", true)));
    let all = start
        .update_with_appended([TransactionSpec::new()
            .changes([insert_text(&schema, 3, "A")])
            .user_event("input.type")])
        .unwrap();
    assert_eq!(all.len(), 1 + MAX_APPENDED_TRANSACTIONS);
    let last = all.last().expect("at least one transaction");
    assert_eq!(last.annotation(appenders_diverged()), Some(&true));
    assert_eq!(
        last.annotation(appended()).map(|info| info.depth),
        Some(MAX_APPENDED_TRANSACTIONS)
    );
    assert_eq!(
        schema.describe(last.new_doc()),
        r#"doc(paragraph("heAllo!!!!!!!!"))"#
    );
}
