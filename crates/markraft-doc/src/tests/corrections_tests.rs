//! Corrections: per-node-type observers that repair a transaction's result.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::support::*;
use crate::change::Change;
use crate::corrections::{
    Correction, CorrectionContext, MAX_CORRECTION_ROUNDS, corrections, corrections_diverged,
    fill_required_content,
};
use crate::fragment::Fragment;
use crate::node::Node;
use crate::schema::Schema;
use crate::slice::Slice;
use crate::state::{Extension, TransactionSpec};

/// The text a textblock holds, ignoring anything that is not a text leaf.
fn text_of(node: &Node) -> String {
    node.children().filter_map(|child| child.text()).collect()
}

/// Replace the last token of a textblock's content with `text`.
fn replace_last(cx: &CorrectionContext<'_>, text: &str) -> Vec<Change> {
    let end = cx.content_start + cx.node.content_size();
    vec![Change::replace(
        end - 1,
        end,
        Slice::from_fragment(Fragment::from_node(cx.start_state.schema().text(text))),
    )]
}

fn list_doc(schema: &Schema) -> Node {
    doc(
        schema,
        [n(
            schema,
            "bullet_list",
            [n(
                schema,
                "list_item",
                [n(schema, "paragraph", [t(schema, "a")])],
            )],
        )],
    )
}

#[test]
fn a_correction_fills_a_container_that_lost_its_required_child() {
    let schema = shared_schema();
    let item = schema.node_id("list_item").unwrap();
    let start = state(
        list_doc(&schema),
        corrections([fill_required_content(item)]),
    );
    // Deleting the paragraph outright leaves `list_item` with no content, which
    // its `paragraph block*` rule forbids.
    assert!(
        crate::change::ChangeSet::create(&schema, start.doc(), [Change::delete(2, 5)])
            .unwrap()
            .apply(start.doc())
            .unwrap()
            .check(&schema)
            .is_err()
    );

    let tr = start
        .update([TransactionSpec::new().changes([Change::delete(2, 5)])])
        .unwrap();
    assert_eq!(
        schema.describe(tr.new_doc()),
        "doc(bullet_list(list_item(paragraph())))"
    );
    tr.new_doc().check(&schema).unwrap();
    assert_eq!(
        tr.changes().length_after(),
        tr.new_doc().content_size(),
        "the correction's changes composed with the transaction's own"
    );
    tr.state().doc().check(&schema).unwrap();
}

#[test]
fn corrections_do_not_run_for_remote_transactions() {
    let schema = shared_schema();
    let item = schema.node_id("list_item").unwrap();
    let start = state(
        list_doc(&schema),
        corrections([fill_required_content(item)]),
    );
    let tr = start
        .update([TransactionSpec::new()
            .changes([Change::delete(2, 5)])
            .remote(true)])
        .unwrap();
    assert_eq!(
        schema.describe(tr.new_doc()),
        "doc(bullet_list(list_item()))"
    );
}

#[test]
fn a_correction_only_sees_the_types_it_watches_and_only_when_touched() {
    let schema = shared_schema();
    let heading = schema.node_id("heading").unwrap();
    let paragraph = schema.node_id("paragraph").unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let probe = hits.clone();
    let start = state(
        doc(
            &schema,
            [
                n(&schema, "paragraph", [t(&schema, "one")]),
                n(&schema, "paragraph", [t(&schema, "two")]),
            ],
        ),
        Extension::all([
            corrections([Correction::on_content(paragraph, move |_| {
                hits.fetch_add(1, Ordering::SeqCst);
                Vec::new()
            })]),
            corrections([Correction::on_content(heading, |_| {
                panic!("no heading was touched")
            })]),
        ]),
    );
    let _ = start
        .update([TransactionSpec::new().changes([insert_text(&schema, 2, "X")])])
        .unwrap();
    assert_eq!(probe.load(Ordering::SeqCst), 1);
}

#[test]
fn a_corrections_changes_are_mapped_into_the_result_document() {
    let schema = shared_schema();
    let paragraph = schema.node_id("paragraph").unwrap();
    let start = state(
        doc(&schema, [n(&schema, "paragraph", [t(&schema, "ab")])]),
        corrections([Correction::on_content(paragraph, |cx| {
            // Append a marker at the end of the paragraph's content, addressed
            // in the document the transaction produced. Idempotent, so the
            // fixed-point loop settles after one round.
            if text_of(cx.node).ends_with('!') {
                return Vec::new();
            }
            let at = cx.content_start + cx.node.content_size();
            vec![insert_text(cx.start_state.schema(), at, "!")]
        })]),
    );
    let tr = start
        .update([TransactionSpec::new().changes([insert_text(&schema, 2, "X")])])
        .unwrap();
    assert_eq!(schema.describe(tr.new_doc()), r#"doc(paragraph("aXb!"))"#);
    // Composition is exact: applying the transaction's own change set to the
    // start document reproduces the corrected result.
    assert_eq!(&tr.changes().apply(start.doc()).unwrap(), tr.new_doc());
}

#[test]
fn corrections_run_until_they_have_nothing_left_to_ask_for() {
    let schema = shared_schema();
    let paragraph = schema.node_id("paragraph").unwrap();
    // The first correction's output is the second one's input, so settling
    // takes two productive rounds.
    let start = state(
        doc(&schema, [n(&schema, "paragraph", [t(&schema, "x")])]),
        corrections([
            Correction::on_content(paragraph, |cx| {
                if text_of(cx.node).ends_with('@') {
                    replace_last(cx, "#")
                } else {
                    Vec::new()
                }
            }),
            Correction::on_content(paragraph, |cx| {
                if text_of(cx.node).ends_with('#') {
                    let at = cx.content_start + cx.node.content_size();
                    vec![insert_text(cx.start_state.schema(), at, "!")]
                } else {
                    Vec::new()
                }
            }),
        ]),
    );
    let tr = start
        .update([TransactionSpec::new().changes([insert_text(&schema, 2, "@")])])
        .unwrap();
    assert_eq!(schema.describe(tr.new_doc()), r#"doc(paragraph("x#!"))"#);
    assert_eq!(tr.annotation(corrections_diverged()), None);
    // The rounds composed into the transaction's own change set, so undoing it
    // undoes the corrections with it.
    assert_eq!(&tr.changes().apply(start.doc()).unwrap(), tr.new_doc());
    let back = tr.changes().invert(start.doc()).unwrap();
    assert_eq!(&back.apply(tr.new_doc()).unwrap(), start.doc());
}

#[test]
fn corrections_that_fight_each_other_stop_at_the_bound() {
    let schema = shared_schema();
    let paragraph = schema.node_id("paragraph").unwrap();
    let start = state(
        doc(&schema, [n(&schema, "paragraph", [t(&schema, "x")])]),
        corrections([
            Correction::on_content(paragraph, |cx| {
                if text_of(cx.node).ends_with('a') {
                    replace_last(cx, "b")
                } else {
                    Vec::new()
                }
            }),
            Correction::on_content(paragraph, |cx| {
                if text_of(cx.node).ends_with('b') {
                    replace_last(cx, "a")
                } else {
                    Vec::new()
                }
            }),
        ]),
    );
    let tr = start
        .update([TransactionSpec::new().changes([insert_text(&schema, 2, "a")])])
        .unwrap();
    assert_eq!(
        tr.annotation(corrections_diverged()),
        Some(&true),
        "the pair never settles, so the loop reports it"
    );
    // Whatever the bounded run produced is still applied, and is still exact.
    let text = schema.describe(tr.new_doc());
    assert!(
        text == r#"doc(paragraph("xa"))"# || text == r#"doc(paragraph("xb"))"#,
        "{text}"
    );
    assert_eq!(&tr.changes().apply(start.doc()).unwrap(), tr.new_doc());
    const { assert!(MAX_CORRECTION_ROUNDS >= 2) };
}

#[test]
fn several_corrections_extensions_share_one_fixed_point_loop() {
    let schema = shared_schema();
    let paragraph = schema.node_id("paragraph").unwrap();
    let start = state(
        doc(&schema, [n(&schema, "paragraph", [t(&schema, "x")])]),
        Extension::all([
            corrections([Correction::on_content(paragraph, |cx| {
                if text_of(cx.node).ends_with('@') {
                    replace_last(cx, "#")
                } else {
                    Vec::new()
                }
            })]),
            corrections([Correction::on_content(paragraph, |cx| {
                if text_of(cx.node).ends_with('#') {
                    let at = cx.content_start + cx.node.content_size();
                    vec![insert_text(cx.start_state.schema(), at, "!")]
                } else {
                    Vec::new()
                }
            })]),
        ]),
    );
    let tr = start
        .update([TransactionSpec::new().changes([insert_text(&schema, 2, "@")])])
        .unwrap();
    assert_eq!(schema.describe(tr.new_doc()), r#"doc(paragraph("x#!"))"#);
}
