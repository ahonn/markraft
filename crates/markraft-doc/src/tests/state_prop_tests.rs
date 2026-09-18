//! Property tests over the state layer, driven by the deterministic generator
//! in [`super::support`].

use super::support::*;
use crate::change::{Change, ChangeSet};
use crate::fit::Fit;
use crate::fragment::Fragment;
use crate::history::{HistoryConfig, history, redo, undo, undo_depth};
use crate::node::Node;
use crate::schema::Schema;
use crate::selection::Selection;
use crate::slice::Slice;
use crate::state::{EditorState, Extension, TransactionSpec};

/// How many random documents each property walks through.
const ROUNDS: usize = 200;
/// How many transactions each document sees.
const STEPS: usize = 6;

fn textblocks(schema: &Schema, document: &Node) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    document.descendants(&mut |node, pos, _, _| {
        if node.is_textblock(schema) {
            out.push((pos + 1, pos + 1 + node.content_size()));
        }
        true
    });
    out
}

fn random_inline_change(schema: &Schema, rng: &mut Rng, document: &Node) -> Option<Change> {
    let blocks = textblocks(schema, document);
    if blocks.is_empty() {
        return None;
    }
    let (start, end) = *rng.pick(&blocks);
    let a = rng.range(start, end);
    let b = rng.range(start, end);
    let (from, to) = (a.min(b), a.max(b));
    if from < to && rng.one_in(2) {
        Some(Change::delete(from, to))
    } else {
        Some(Change::insert(
            from,
            Slice::from_fragment(Fragment::from_node(t(schema, "zz"))),
        ))
    }
}

fn random_changes(schema: &Schema, rng: &mut Rng, document: &Node) -> Option<Vec<Change>> {
    if rng.one_in(4) {
        random_structural_change(schema, rng, document)
    } else {
        Some(vec![random_inline_change(schema, rng, document)?])
    }
}

fn is_inline_position(schema: &Schema, document: &Node, pos: usize) -> bool {
    document.resolve(pos).is_ok_and(|resolved| {
        schema
            .node_type(resolved.parent().type_id())
            .has_inline_content()
    })
}

#[test]
fn every_transaction_leaves_a_valid_document_and_selection() {
    let schema = shared_schema();
    for seed in 0..ROUNDS as u64 {
        let mut rng = Rng::new(seed + 1);
        let mut current = state(random_doc(&schema, &mut rng), Extension::none());
        for step in 0..STEPS {
            let Some(changes) = random_changes(&schema, &mut rng, current.doc()) else {
                continue;
            };
            let spec = TransactionSpec::new().changes(changes);
            let Ok(tr) = current.update([spec]) else {
                continue;
            };
            let next = tr.state().clone();
            next.doc().check(&schema).unwrap_or_else(|error| {
                panic!("seed {seed} step {step}: invalid document: {error}")
            });
            next.selection()
                .check(next.doc(), &schema)
                .unwrap_or_else(|error| {
                    panic!("seed {seed} step {step}: invalid selection: {error}")
                });
            current = next;
        }
    }
}

#[test]
fn undo_restores_and_redo_replays_a_random_sequence() {
    let schema = shared_schema();
    for seed in 0..ROUNDS as u64 {
        let mut rng = Rng::new(seed + 1_000);
        let start = state(
            random_doc(&schema, &mut rng),
            history(HistoryConfig::default()),
        );
        let mut current = start.clone();
        // Every step is far enough apart in time that nothing merges, so one
        // recorded transaction is one entry.
        let mut history_line: Vec<EditorState> = vec![start.clone()];
        for step in 0..STEPS {
            let Some(changes) = random_changes(&schema, &mut rng, current.doc()) else {
                continue;
            };
            let spec = TransactionSpec::new()
                .changes(changes)
                .user_event("input.type")
                .time(step as u64 * 10_000);
            let Ok(tr) = current.update([spec]) else {
                continue;
            };
            if !tr.doc_changed() {
                continue;
            }
            current = tr.state().clone();
            history_line.push(current.clone());
        }
        assert_eq!(
            undo_depth(&current),
            history_line.len() - 1,
            "seed {seed}: one entry per recorded transaction"
        );

        let mut undone = current.clone();
        for (index, expected) in history_line.iter().enumerate().rev().skip(1) {
            let spec = undo(&undone).unwrap_or_else(|| panic!("seed {seed}: nothing to undo"));
            undone = undone.update([spec]).expect("undo applies").state().clone();
            assert_eq!(
                undone.doc(),
                expected.doc(),
                "seed {seed}: undo back to step {index}"
            );
            assert_eq!(
                undone.selection(),
                expected.selection(),
                "seed {seed}: undo restores the selection of step {index}"
            );
            undone.doc().check(&schema).unwrap();
        }
        assert_eq!(undo_depth(&undone), 0);
        assert!(undo(&undone).is_none());

        let mut redone = undone;
        for (index, expected) in history_line.iter().enumerate().skip(1) {
            let spec = redo(&redone).unwrap_or_else(|| panic!("seed {seed}: nothing to redo"));
            redone = redone.update([spec]).expect("redo applies").state().clone();
            assert_eq!(
                redone.doc(),
                expected.doc(),
                "seed {seed}: redo forward to step {index}"
            );
            assert_eq!(
                redone.selection(),
                expected.selection(),
                "seed {seed}: redo restores the selection of step {index}"
            );
        }
        assert_eq!(redone.doc(), current.doc());
    }
}

#[test]
fn selections_map_into_valid_positions() {
    let schema = shared_schema();
    for seed in 0..ROUNDS as u64 {
        let mut rng = Rng::new(seed + 2_000);
        let document = random_doc(&schema, &mut rng);
        let size = document.content_size();
        let a = rng.below(size + 1);
        let b = rng.below(size + 1);
        let (from, to) = (a.min(b), a.max(b));
        let change = if rng.one_in(2) {
            Change::replace(
                from,
                to,
                Slice::from_fragment(Fragment::from_node(t(&schema, "qq"))),
            )
            .with_fit(Fit::Auto)
        } else {
            Change::delete(from, to).with_fit(Fit::Auto)
        };
        let Ok(changes) = ChangeSet::create(&schema, &document, [change]) else {
            continue;
        };
        let Ok(after) = changes.apply(&document) else {
            continue;
        };
        let desc = changes.desc();

        let spots = textblock_positions(&schema, &document);
        for (_, pos) in spots.iter().take(12) {
            let mapped = Selection::cursor(*pos).map(&schema, &after, &desc);
            mapped
                .check(&after, &schema)
                .unwrap_or_else(|error| panic!("seed {seed}: invalid mapped selection: {error}"));
            let head = mapped.head(&after);
            assert!(
                matches!(mapped, Selection::All) || is_inline_position(&schema, &after, head),
                "seed {seed}: a text selection left inline content at {head}"
            );
        }

        for pos in 0..=size {
            if !Selection::is_selectable(&schema, &document, pos) {
                continue;
            }
            let mapped = Selection::node(pos).map(&schema, &after, &desc);
            mapped
                .check(&after, &schema)
                .unwrap_or_else(|error| panic!("seed {seed}: invalid mapped selection: {error}"));
            match mapped {
                Selection::Node { pos } => assert!(
                    Selection::is_selectable(&schema, &after, pos),
                    "seed {seed}: a node selection landed on an unselectable node"
                ),
                other => assert!(
                    matches!(other, Selection::Text { .. } | Selection::All),
                    "seed {seed}: unexpected fallback {other:?}"
                ),
            }
        }
    }
}
