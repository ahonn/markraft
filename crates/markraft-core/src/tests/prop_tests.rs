//! Property tests over randomly generated documents and change sets.
//!
//! The generator is a small deterministic LCG/xorshift in [`super::support`],
//! so failures are reproducible from the seed printed in the assertion.

use super::support::*;
use crate::change::apply::doc_token_run;
use crate::change::{Change, ChangeSet, TrackMode};
use crate::error::ChangeError;
use crate::fit::Fit;
use crate::fragment::Fragment;
use crate::node::Node;
use crate::schema::Schema;
use crate::slice::Slice;

/// How many random documents each property walks through.
const ROUNDS: usize = 2000;

fn random_slice(schema: &Schema, rng: &mut Rng) -> Slice {
    match rng.below(5) {
        0 => Slice::empty(),
        1 => Slice::from_fragment(Fragment::from_node(t(schema, "zz"))),
        2 => Slice::from_fragment(Fragment::from_node(random_block(schema, rng, 1))),
        _ => {
            let source = random_doc(schema, rng);
            let size = source.content_size();
            let a = rng.below(size + 1);
            let b = rng.below(size + 1);
            source.slice(a.min(b), a.max(b)).expect("in range")
        }
    }
}

/// A random change that fitting is allowed to repair.
fn random_change(schema: &Schema, rng: &mut Rng, d: &Node) -> Change {
    let size = d.content_size();
    let a = rng.below(size + 1);
    let b = rng.below(size + 1);
    let (from, to) = (a.min(b), a.max(b));
    match rng.below(6) {
        0 => Change::add_mark(from, to, m(schema, "strong")),
        1 => Change::add_mark(from, to, m(schema, "em")),
        2 => Change::remove_mark(from, to, m(schema, "strong")),
        _ => Change::replace(from, to, random_slice(schema, rng)).with_fit(Fit::Auto),
    }
}

/// A random change that stays inside one textblock, so it is always balanced
/// and needs no repair.
fn random_inline_change(schema: &Schema, rng: &mut Rng, d: &Node) -> Option<Change> {
    let mut blocks = Vec::new();
    d.descendants(&mut |node, pos, _, _| {
        if node.is_textblock(schema) && node.content_size() > 0 {
            blocks.push((pos + 1, pos + 1 + node.content_size()));
        }
        true
    });
    if blocks.is_empty() {
        return None;
    }
    let (start, end) = *rng.pick(&blocks);
    let a = rng.range(start, end);
    let b = rng.range(start, end);
    let (from, to) = (a.min(b), a.max(b));
    Some(match rng.below(4) {
        0 => Change::add_mark(from, to, m(schema, "strong")),
        1 => Change::remove_mark(from, to, m(schema, "em")),
        _ => Change::replace(
            from,
            to,
            Slice::from_fragment(Fragment::from_node(t(
                schema,
                rng.pick::<&str>(&["q", "qrs"]),
            ))),
        ),
    })
}

fn make(schema: &Schema, d: &Node, changes: Vec<Change>) -> Option<ChangeSet> {
    match ChangeSet::create(schema, d, changes) {
        Ok(set) => Some(set),
        // Random ranges can collide after fitting widens them, and some
        // replacements simply cannot be made to fit.
        Err(ChangeError::Overlapping { .. })
        | Err(ChangeError::FitConflict { .. })
        | Err(ChangeError::Unfittable(_)) => None,
        Err(err) => panic!("unexpected change error: {err}"),
    }
}

#[test]
fn applying_a_fitted_change_yields_a_valid_document() {
    let schema = test_schema();
    let mut valid = 0;
    for seed in 0..ROUNDS as u64 {
        let mut rng = Rng::new(seed + 1);
        let d = random_doc(&schema, &mut rng);
        d.check(&schema)
            .expect("the generator produces valid documents");
        let change = random_change(&schema, &mut rng, &d);
        let Some(set) = make(&schema, &d, vec![change.clone()]) else {
            continue;
        };
        let out = set
            .apply(&d)
            .unwrap_or_else(|err| panic!("seed {seed}: apply failed: {err} ({change:?})"));
        out.check(&schema).unwrap_or_else(|err| {
            panic!(
                "seed {seed}: invalid result {}: {err}",
                schema.describe(&out)
            )
        });
        assert_eq!(out.content_size(), set.length_after());
        valid += 1;
    }
    assert!(valid > ROUNDS / 2, "only {valid} of {ROUNDS} rounds ran");
}

#[test]
fn invert_round_trips() {
    let schema = test_schema();
    let mut ran = 0;
    for seed in 0..ROUNDS as u64 {
        let mut rng = Rng::new(seed + 7919);
        let d = random_doc(&schema, &mut rng);
        let change = random_change(&schema, &mut rng, &d);
        let Some(set) = make(&schema, &d, vec![change.clone()]) else {
            continue;
        };
        let Ok(out) = set.apply(&d) else { continue };
        let inverse = set.invert(&d).expect("invertible");
        let back = inverse
            .apply(&out)
            .unwrap_or_else(|err| panic!("seed {seed}: inverse failed: {err}"));
        assert_eq!(
            back,
            d,
            "seed {seed}: inverting {change:?} gave {} instead of {}",
            schema.describe(&back),
            schema.describe(&d)
        );
        ran += 1;
    }
    assert!(ran > ROUNDS / 2, "only {ran} of {ROUNDS} rounds ran");
}

#[test]
fn compose_matches_sequential_application() {
    let schema = test_schema();
    let mut ran = 0;
    for seed in 0..ROUNDS as u64 {
        let mut rng = Rng::new(seed + 104729);
        let d = random_doc(&schema, &mut rng);
        let Some(a) = make(&schema, &d, vec![random_change(&schema, &mut rng, &d)]) else {
            continue;
        };
        let Ok(mid) = a.apply(&d) else { continue };
        let Some(b) = make(&schema, &mid, vec![random_change(&schema, &mut rng, &mid)]) else {
            continue;
        };
        let Ok(expected) = b.apply(&mid) else {
            continue;
        };
        let composed = a.compose(&b).expect("composable");
        assert_eq!(composed.length_before(), a.length_before());
        assert_eq!(composed.length_after(), b.length_after());
        let actual = composed
            .apply(&d)
            .unwrap_or_else(|err| panic!("seed {seed}: composed apply failed: {err}"));
        assert_eq!(
            actual,
            expected,
            "seed {seed}: compose gave {} instead of {}",
            schema.describe(&actual),
            schema.describe(&expected)
        );
        ran += 1;
    }
    assert!(ran > ROUNDS / 3, "only {ran} of {ROUNDS} rounds ran");
}

#[test]
fn transform_converges_for_inline_edits() {
    let schema = test_schema();
    let mut ran = 0;
    for seed in 0..ROUNDS as u64 {
        let mut rng = Rng::new(seed + 15485863);
        let d = random_doc(&schema, &mut rng);
        let (Some(ca), Some(cb)) = (
            random_inline_change(&schema, &mut rng, &d),
            random_inline_change(&schema, &mut rng, &d),
        ) else {
            continue;
        };
        let (Some(a), Some(b)) = (
            make(&schema, &d, vec![ca.clone()]),
            make(&schema, &d, vec![cb.clone()]),
        ) else {
            continue;
        };
        let (a2, b2) = a.transform(&d, &b, true).expect("transformable");
        let left = a2
            .apply(&b.apply(&d).expect("applies"))
            .unwrap_or_else(|err| panic!("seed {seed}: a' failed: {err}"));
        let right = b2
            .apply(&a.apply(&d).expect("applies"))
            .unwrap_or_else(|err| panic!("seed {seed}: b' failed: {err}"));
        assert_eq!(
            left,
            right,
            "seed {seed}: {ca:?} and {cb:?} diverged: {} vs {}",
            schema.describe(&left),
            schema.describe(&right)
        );
        ran += 1;
    }
    assert!(ran > ROUNDS / 2, "only {ran} of {ROUNDS} rounds ran");
}

/// The span of the starting document a change set touches.
fn touched_span(cs: &ChangeSet) -> Option<(usize, usize)> {
    let mut lo = usize::MAX;
    let mut hi = 0usize;
    for change in cs.iter_changes() {
        let (from, to) = match change {
            crate::change::ChangeRange::Replaced { from_a, to_a, .. }
            | crate::change::ChangeRange::Marked { from_a, to_a, .. } => (from_a, to_a),
        };
        lo = lo.min(from);
        hi = hi.max(to);
    }
    if lo == usize::MAX {
        None
    } else {
        Some((lo, hi))
    }
}

#[test]
fn transform_always_produces_applicable_valid_change_sets() {
    let schema = test_schema();
    let mut ran = 0;
    let mut diverged = 0;
    for seed in 0..ROUNDS as u64 {
        let mut rng = Rng::new(seed + 32452843);
        let d = random_doc(&schema, &mut rng);
        let (Some(a), Some(b)) = (
            make(&schema, &d, vec![random_change(&schema, &mut rng, &d)]),
            make(&schema, &d, vec![random_change(&schema, &mut rng, &d)]),
        ) else {
            continue;
        };
        let (Ok(after_a), Ok(after_b)) = (a.apply(&d), b.apply(&d)) else {
            continue;
        };
        let (a2, b2) = a
            .transform(&d, &b, true)
            .unwrap_or_else(|err| panic!("seed {seed}: transform failed: {err}"));
        // The hard guarantee: a rebased set always applies and always leaves a
        // valid document behind, however the two changes conflict.
        let left = a2
            .apply(&after_b)
            .unwrap_or_else(|err| panic!("seed {seed}: a' does not apply: {err}"));
        let right = b2
            .apply(&after_a)
            .unwrap_or_else(|err| panic!("seed {seed}: b' does not apply: {err}"));
        left.check(&schema)
            .unwrap_or_else(|err| panic!("seed {seed}: a' left an invalid document: {err}"));
        right
            .check(&schema)
            .unwrap_or_else(|err| panic!("seed {seed}: b' left an invalid document: {err}"));

        // Changes that do not touch the same stretch of document always
        // converge. Overlapping ones may not; see `transform_diverges_only_on_        // overlapping_conflicts`.
        let disjoint = match (touched_span(&a), touched_span(&b)) {
            (Some((a_lo, a_hi)), Some((b_lo, b_hi))) => a_hi < b_lo || b_hi < a_lo,
            _ => true,
        };
        if left != right {
            diverged += 1;
            assert!(
                !disjoint,
                "seed {seed}: changes over disjoint ranges diverged: {} vs {}",
                schema.describe(&left),
                schema.describe(&right)
            );
        }
        ran += 1;
    }
    assert!(ran > ROUNDS / 2, "only {ran} of {ROUNDS} rounds ran");
    assert!(
        diverged * 10 < ran,
        "{diverged} of {ran} rounds diverged, which is more than the conflict \
         rate this property expects"
    );
}

#[test]
fn map_pos_follows_the_content_it_points_at() {
    let schema = test_schema();
    let mut ran = 0;
    for seed in 0..ROUNDS as u64 {
        let mut rng = Rng::new(seed + 49979687);
        let d = random_doc(&schema, &mut rng);
        // Pick a character in the document and a change that leaves it alone.
        let mut chars = Vec::new();
        d.descendants(&mut |node, pos, _, _| {
            if node.is_text() {
                for i in 0..node.text_len() {
                    chars.push(pos + i);
                }
            }
            true
        });
        if chars.is_empty() {
            continue;
        }
        let at = *rng.pick(&chars);
        let expected = d.text_between(&schema, at, at + 1, None, None);
        let size = d.content_size();
        // Change a range strictly before or strictly after the character.
        let change = if rng.one_in(2) && at > 0 {
            let to = rng.below(at + 1).min(at);
            let from = rng.below(to + 1);
            Change::replace(from, to, random_slice(&schema, &mut rng)).with_fit(Fit::Auto)
        } else {
            let from = rng.range(at + 1, size);
            let to = rng.range(from, size);
            Change::replace(from, to, random_slice(&schema, &mut rng)).with_fit(Fit::Auto)
        };
        let Some(set) = make(&schema, &d, vec![change]) else {
            continue;
        };
        // Fitting may widen the range over the character; skip those rounds.
        if set.touches(at, at + 1) {
            continue;
        }
        let Ok(out) = set.apply(&d) else { continue };
        let from = set.map_pos(at, -1, TrackMode::Simple).expect("mapped");
        let to = set.map_pos(at + 1, 1, TrackMode::Simple).expect("mapped");
        assert_eq!(to - from, 1, "seed {seed}: the character changed size");
        assert_eq!(
            out.text_between(&schema, from, to, None, None),
            expected,
            "seed {seed}: the mapped position points at other content"
        );
        ran += 1;
    }
    assert!(ran > ROUNDS / 5, "only {ran} of {ROUNDS} rounds ran");
}

#[test]
fn slicing_and_replacing_a_range_with_itself_is_a_no_op() {
    let schema = test_schema();
    for seed in 0..100u64 {
        let mut rng = Rng::new(seed + 86028121);
        let d = random_doc(&schema, &mut rng);
        let size = d.content_size();
        for _ in 0..8 {
            let a = rng.below(size + 1);
            let b = rng.below(size + 1);
            let (from, to) = (a.min(b), a.max(b));
            let slice = d.slice(from, to).expect("in range");
            assert_eq!(slice.size(), to - from, "seed {seed}: slice {from}..{to}");
            // The slice's tokens are exactly the document's tokens there.
            assert_eq!(
                slice.tokens(),
                doc_token_run(&d, from, to).expect("in range"),
                "seed {seed}: slice {from}..{to} does not match the document run"
            );
            // Replacing the range with its own content changes nothing.
            let set = ChangeSet::create(&schema, &d, vec![Change::replace(from, to, slice)])
                .expect("valid");
            assert_eq!(
                set.apply(&d).expect("applies"),
                d,
                "seed {seed}: replacing {from}..{to} with itself changed the document"
            );
        }
    }
}

#[test]
fn slice_tokens_round_trip_through_from_tokens() {
    let schema = test_schema();
    for seed in 0..100u64 {
        let mut rng = Rng::new(seed + 2147483647);
        let d = random_doc(&schema, &mut rng);
        let size = d.content_size();
        for _ in 0..8 {
            let a = rng.below(size + 1);
            let b = rng.below(size + 1);
            let slice = d.slice(a.min(b), a.max(b)).expect("in range");
            let tokens = slice.tokens();
            let rebuilt = Slice::from_tokens(&tokens);
            assert_eq!(rebuilt.tokens(), tokens, "seed {seed}");
            assert_eq!(rebuilt.size(), slice.size(), "seed {seed}");
            // Cutting and re-joining reproduces the run.
            let split = rng.below(slice.size() + 1);
            let joined = slice.cut(0, split).concat(&slice.cut(split, slice.size()));
            assert_eq!(joined.tokens(), tokens, "seed {seed}: split at {split}");
        }
    }
}

#[test]
fn size_and_text_invariants_hold() {
    let schema = test_schema();
    for seed in 0..ROUNDS as u64 {
        let mut rng = Rng::new(seed + 1000003);
        let d = random_doc(&schema, &mut rng);

        // node_size is content_size plus the node's own two tokens.
        let mut all_text = String::new();
        d.descendants(&mut |node, _, _, _| {
            if node.is_container() {
                assert_eq!(node.node_size(), node.content_size() + 2);
                let sum: usize = node.children().map(Node::node_size).sum();
                assert_eq!(sum, node.content_size());
            } else if let Some(text) = node.text() {
                assert_eq!(node.node_size(), text.chars().count());
                assert!(!text.is_empty(), "empty text nodes are normalised away");
            } else {
                assert_eq!(node.node_size(), 1);
            }
            true
        });
        d.descendants(&mut |node, _, _, _| {
            if let Some(text) = node.text() {
                all_text.push_str(text);
            }
            true
        });
        let size = d.content_size();
        assert_eq!(d.text_between(&schema, 0, size, None, None), all_text);

        // text_between splits at any position.
        for _ in 0..6 {
            let a = rng.below(size + 1);
            let b = rng.below(size + 1);
            let (from, to) = (a.min(b), a.max(b));
            let mid = rng.range(from, to);
            assert_eq!(
                d.text_between(&schema, from, to, None, None),
                format!(
                    "{}{}",
                    d.text_between(&schema, from, mid, None, None),
                    d.text_between(&schema, mid, to, None, None)
                ),
                "seed {seed}: text_between {from}..{mid}..{to}"
            );
        }

        // Resolving every position agrees with the tree.
        for pos in 0..=size {
            let r = d.resolve(pos).expect("in range");
            assert!(r.start(r.depth()) <= pos && pos <= r.end(r.depth()));
            assert_eq!(r.pos(), pos);
            assert_eq!(r.parent_offset(), pos - r.start(r.depth()));
            if r.depth() > 0 {
                assert_eq!(
                    r.after(r.depth()) - r.before(r.depth()),
                    r.parent().node_size()
                );
            }
        }
    }
}

#[test]
fn json_round_trips_for_random_documents() {
    let schema = test_schema();
    for seed in 0..100u64 {
        let mut rng = Rng::new(seed + 611953);
        let d = random_doc(&schema, &mut rng);
        let json = d.to_json(&schema);
        let back = Node::from_json(&schema, &json).expect("round trips");
        assert_eq!(back, d, "seed {seed}");
        let text = serde_json::to_string(&json).expect("serialises");
        let parsed: serde_json::Value = serde_json::from_str(&text).expect("parses");
        assert_eq!(
            Node::from_json(&schema, &parsed).expect("round trips"),
            d,
            "seed {seed}"
        );
    }
}

#[test]
fn the_content_automaton_agrees_with_a_reference_matcher() {
    // Each case pairs an expression with a predicate over sequences of the
    // symbols `a`, `b` and `c`, checked exhaustively up to length four.
    type Pred = fn(&[usize]) -> bool;
    let cases: Vec<(&str, Pred)> = vec![
        ("a*", |s| s.iter().all(|&x| x == 0)),
        ("a+", |s| !s.is_empty() && s.iter().all(|&x| x == 0)),
        ("a?", |s| s.is_empty() || (s.len() == 1 && s[0] == 0)),
        ("a b", |s| s == [0, 1]),
        ("a* b", |s| {
            !s.is_empty() && s[s.len() - 1] == 1 && s[..s.len() - 1].iter().all(|&x| x == 0)
        }),
        ("(a | b)+", |s| !s.is_empty() && s.iter().all(|&x| x < 2)),
        ("(a | b){2,3}", |s| {
            (2..=3).contains(&s.len()) && s.iter().all(|&x| x < 2)
        }),
        ("a{2}", |s| s == [0, 0]),
        ("a (b | c)*", |s| {
            !s.is_empty() && s[0] == 0 && s[1..].iter().all(|&x| x == 1 || x == 2)
        }),
        ("a*|b", |s| s.iter().all(|&x| x == 0) || s == [1]),
        ("(a* c)|b", |s| {
            s == [1]
                || (!s.is_empty()
                    && s[s.len() - 1] == 2
                    && s[..s.len() - 1].iter().all(|&x| x == 0))
        }),
        ("(a* b)*", |s| {
            s.is_empty() || (s[s.len() - 1] == 1 && s.iter().all(|&x| x == 0 || x == 1))
        }),
        ("(a | b)* c*", |s| {
            let split = s.iter().position(|&x| x == 2).unwrap_or(s.len());
            s[..split].iter().all(|&x| x < 2) && s[split..].iter().all(|&x| x == 2)
        }),
        ("a? b+ c", |s| {
            let rest = if s.first() == Some(&0) { &s[1..] } else { s };
            rest.len() >= 2
                && rest[rest.len() - 1] == 2
                && rest[..rest.len() - 1].iter().all(|&x| x == 1)
                && !rest[..rest.len() - 1].is_empty()
        }),
    ];

    for (expr, pred) in cases {
        let schema = Schema::new(
            crate::schema::SchemaSpec::new()
                .node(crate::schema::NodeTypeSpec::new("doc", expr))
                .node(crate::schema::NodeTypeSpec::new("a", "").group("g"))
                .node(crate::schema::NodeTypeSpec::new("b", "").group("g"))
                .node(crate::schema::NodeTypeSpec::new("c", "").group("g")),
        )
        .unwrap_or_else(|err| panic!("`{expr}` should compile: {err}"));
        let ids: Vec<_> = ["a", "b", "c"]
            .iter()
            .map(|n| schema.node_id(n).expect("known"))
            .collect();
        let doc_ty = schema.node_id("doc").expect("known");

        let mut sequences: Vec<Vec<usize>> = vec![Vec::new()];
        let mut frontier: Vec<Vec<usize>> = vec![Vec::new()];
        for _ in 0..4 {
            let mut next = Vec::new();
            for seq in &frontier {
                for symbol in 0..3 {
                    let mut extended = seq.clone();
                    extended.push(symbol);
                    next.push(extended);
                }
            }
            sequences.extend(next.iter().cloned());
            frontier = next;
        }

        for seq in sequences {
            let accepted = schema
                .content_match(doc_ty)
                .match_types(seq.iter().map(|&i| ids[i]))
                .is_some_and(|m| m.valid_end());
            assert_eq!(
                accepted,
                pred(&seq),
                "`{expr}` disagreed on {seq:?} (automaton said {accepted})"
            );
        }
    }
}

#[test]
fn compose_is_associative() {
    let schema = test_schema();
    let mut ran = 0;
    for seed in 0..ROUNDS as u64 {
        let mut rng = Rng::new(seed + 179424673);
        let d = random_doc(&schema, &mut rng);
        let Some(a) = make(&schema, &d, vec![random_change(&schema, &mut rng, &d)]) else {
            continue;
        };
        let Ok(d1) = a.apply(&d) else { continue };
        let Some(b) = make(&schema, &d1, vec![random_change(&schema, &mut rng, &d1)]) else {
            continue;
        };
        let Ok(d2) = b.apply(&d1) else { continue };
        let Some(c) = make(&schema, &d2, vec![random_change(&schema, &mut rng, &d2)]) else {
            continue;
        };
        if c.apply(&d2).is_err() {
            continue;
        }
        let left = a
            .compose(&b)
            .expect("composable")
            .compose(&c)
            .expect("composable");
        let right = a
            .compose(&b.compose(&c).expect("composable"))
            .expect("composable");
        assert_eq!(
            left.apply(&d).expect("applies"),
            right.apply(&d).expect("applies"),
            "seed {seed}: composition is not associative"
        );
        ran += 1;
    }
    assert!(ran > ROUNDS / 5, "only {ran} of {ROUNDS} rounds ran");
}

#[test]
fn change_sets_round_trip_through_json_exactly() {
    let schema = test_schema();
    let mut ran = 0;
    for seed in 0..ROUNDS as u64 {
        let mut rng = Rng::new(seed + 433494437);
        let d = random_doc(&schema, &mut rng);
        let Some(cs) = make(&schema, &d, vec![random_change(&schema, &mut rng, &d)]) else {
            continue;
        };
        let json = cs.to_json();
        let back = ChangeSet::from_json(&schema, &json).expect("round trips");
        assert_eq!(back, cs, "seed {seed}: the change set changed shape");
        assert_eq!(back.to_json(), json, "seed {seed}");
        if let Ok(expected) = cs.apply(&d) {
            assert_eq!(back.apply(&d).expect("applies"), expected, "seed {seed}");
        }
        ran += 1;
    }
    assert!(ran > ROUNDS / 2, "only {ran} of {ROUNDS} rounds ran");
}

#[test]
fn inserted_runs_are_canonical() {
    let schema = test_schema();
    for seed in 0..ROUNDS as u64 {
        let mut rng = Rng::new(seed + 715827883);
        let d = random_doc(&schema, &mut rng);
        let Some(cs) = make(&schema, &d, vec![random_change(&schema, &mut rng, &d)]) else {
            continue;
        };
        for change in cs.iter_changes() {
            if let crate::change::ChangeRange::Replaced { inserted, .. } = change {
                let tokens = inserted.tokens();
                assert_eq!(
                    Slice::from_tokens(&tokens).tokens(),
                    tokens,
                    "seed {seed}: an inserted run is not in canonical form"
                );
            }
        }
    }
}

#[test]
fn transform_survives_concurrent_structural_edits() {
    // Splits, joins, wraps and lifts on overlapping ranges are the cases a
    // naive rebase turns into an unbalanced token run.
    let schema = test_schema();
    let mut ran = 0;
    let mut diverged = 0;
    let mut structural = 0;
    for seed in 0..ROUNDS as u64 {
        let mut rng = Rng::new(seed + 2038074743);
        let d = random_doc(&schema, &mut rng);
        let ca = random_structural_change(&schema, &mut rng, &d);
        let cb = if rng.one_in(3) {
            Some(vec![random_change(&schema, &mut rng, &d)])
        } else {
            random_structural_change(&schema, &mut rng, &d)
        };
        let (Some(ca), Some(cb)) = (ca, cb) else {
            continue;
        };
        let is_structural = cb.len() > 1 || cb.iter().all(|c| !c.is_mark_change());
        let (Some(a), Some(b)) = (make(&schema, &d, ca.clone()), make(&schema, &d, cb.clone()))
        else {
            continue;
        };
        let (Ok(after_a), Ok(after_b)) = (a.apply(&d), b.apply(&d)) else {
            continue;
        };
        // Both starting changes describe valid documents.
        after_a.check(&schema).expect("a leaves a valid document");
        after_b.check(&schema).expect("b leaves a valid document");

        let (a2, b2) = a
            .transform(&d, &b, true)
            .unwrap_or_else(|err| panic!("seed {seed}: transform failed: {err}"));
        let left = a2
            .apply(&after_b)
            .unwrap_or_else(|err| panic!("seed {seed}: a' does not apply: {err} ({ca:?})"));
        let right = b2
            .apply(&after_a)
            .unwrap_or_else(|err| panic!("seed {seed}: b' does not apply: {err} ({cb:?})"));
        left.check(&schema)
            .unwrap_or_else(|err| panic!("seed {seed}: a' left an invalid document: {err}"));
        right
            .check(&schema)
            .unwrap_or_else(|err| panic!("seed {seed}: b' left an invalid document: {err}"));
        if left != right {
            diverged += 1;
            let disjoint = match (touched_span(&a), touched_span(&b)) {
                (Some((a_lo, a_hi)), Some((b_lo, b_hi))) => a_hi < b_lo || b_hi < a_lo,
                _ => true,
            };
            assert!(
                !disjoint,
                "seed {seed}: structural edits over disjoint ranges diverged: {} vs {}",
                schema.describe(&left),
                schema.describe(&right)
            );
        }
        if is_structural {
            structural += 1;
        }
        ran += 1;
    }
    assert!(ran > ROUNDS / 4, "only {ran} of {ROUNDS} rounds ran");
    assert!(
        structural > ran / 2,
        "too few structural pairs: {structural}"
    );
    assert!(
        diverged * 4 < ran,
        "{diverged} of {ran} structural rounds diverged"
    );
}

/// One random edit of `d`, or `None` when the drawn edit does not apply.
///
/// The mix covers structural edits both repaired and taken at face value, text
/// typed and deleted inside a textblock, deletions and insertions anywhere
/// with no repair, and mark changes — so the result is sometimes valid and
/// sometimes not.
fn random_mutation(schema: &Schema, rng: &mut Rng, d: &Node) -> Option<Node> {
    let size = d.content_size();
    let a = rng.below(size + 1);
    let b = rng.below(size + 1);
    let (from, to) = (a.min(b), a.max(b));
    let changes: Vec<Change> = match rng.below(7) {
        0 => random_structural_change(schema, rng, d)?,
        1 => random_structural_change(schema, rng, d)?
            .into_iter()
            .map(|change| change.with_fit(Fit::No))
            .collect(),
        2 => vec![random_inline_change(schema, rng, d)?],
        3 => vec![Change::delete(from, to)],
        4 => vec![match rng.below(3) {
            0 => Change::add_mark(from, to, m(schema, "strong")),
            1 => Change::add_mark(from, to, link(schema, "https://example.com")),
            _ => Change::remove_mark(from, to, m(schema, "em")),
        }],
        5 => vec![Change::insert(
            from,
            Slice::from_fragment(Fragment::from_node(match rng.below(3) {
                0 => tm(schema, "s", &["strong"]),
                1 => n(schema, "horizontal_rule", []),
                _ => img(schema, "pic.png"),
            })),
        )],
        _ => vec![Change::replace(from, to, random_slice(schema, rng))],
    };
    let set = ChangeSet::create(schema, d, changes).ok()?;
    set.apply(d).ok()
}

#[test]
fn check_from_agrees_with_a_full_check() {
    let schema = test_schema();
    let (mut valid, mut invalid) = (0, 0);
    for seed in 0..ROUNDS as u64 {
        let mut rng = Rng::new(seed + 104_729);
        // Chain a few edits, always from the last valid document, since that
        // is what `check_from` assumes of its baseline.
        let mut current = random_doc(&schema, &mut rng);
        for step in 0..4 {
            let Some(next) = random_mutation(&schema, &mut rng, &current) else {
                continue;
            };
            let full = next.check(&schema);
            let incremental = next.check_from(&current, &schema);
            assert_eq!(
                full.is_ok(),
                incremental.is_ok(),
                "seed {seed} step {step}: check says {full:?}, check_from says {incremental:?} \
                 for {} from {}",
                schema.describe(&next),
                schema.describe(&current),
            );
            if full.is_ok() {
                valid += 1;
                current = next;
            } else {
                invalid += 1;
            }
        }
    }
    assert!(
        valid > ROUNDS && invalid > ROUNDS / 4,
        "too little coverage: {valid} valid, {invalid} invalid results"
    );
}

/// The cached description, next to one computed from the sections now.
fn assert_desc_is_fresh(set: &ChangeSet, what: &str, seed: u64) {
    let fresh = crate::change::ChangeDesc::of_sections(&set.sections, set.length_before());
    assert_eq!(
        set.desc(),
        &fresh,
        "seed {seed}: stale description after {what}"
    );
    assert_eq!(set.desc().length_after(), set.length_after());
}

#[test]
fn a_change_sets_cached_description_matches_its_sections() {
    let schema = test_schema();
    let mut ran = 0;
    for seed in 0..ROUNDS as u64 {
        let mut rng = Rng::new(seed + 15_485_863);
        let d = random_doc(&schema, &mut rng);
        assert_desc_is_fresh(&ChangeSet::empty(&schema, d.content_size()), "empty", seed);
        let Some(a) = make(&schema, &d, vec![random_change(&schema, &mut rng, &d)]) else {
            continue;
        };
        assert_desc_is_fresh(&a, "create", seed);
        let Ok(after_a) = a.apply(&d) else { continue };
        assert_desc_is_fresh(&a.invert(&d).expect("invertible"), "invert", seed);
        let json = ChangeSet::from_json(&schema, &a.to_json()).expect("round trip");
        assert_desc_is_fresh(&json, "from_json", seed);
        let size = d.content_size();
        let cut = rng.below(size + 1);
        let restricted = crate::state::filters::restrict_changes(&a, &[(0, cut)]);
        assert_desc_is_fresh(&restricted, "restrict_changes", seed);
        if let Some(b) = make(
            &schema,
            &after_a,
            vec![random_change(&schema, &mut rng, &after_a)],
        ) {
            assert_desc_is_fresh(&b, "create", seed);
            assert_desc_is_fresh(&a.compose(&b).expect("composable"), "compose", seed);
        }
        if let Some(c) = make(&schema, &d, vec![random_change(&schema, &mut rng, &d)])
            && let Ok((a_over_c, c_over_a)) = a.transform(&d, &c, true)
        {
            assert_desc_is_fresh(&a_over_c, "transform", seed);
            assert_desc_is_fresh(&c_over_a, "transform", seed);
        }
        ran += 1;
    }
    assert!(ran > ROUNDS / 2, "only {ran} of {ROUNDS} rounds ran");
}

/// The tokens that are content rather than structure: one per character and
/// one per non-text leaf.
fn leaf_tokens(fragment: &Fragment) -> usize {
    fragment
        .iter()
        .map(|node| {
            if node.is_text() {
                node.text_len()
            } else if node.is_leaf() {
                1
            } else {
                leaf_tokens(node.content())
            }
        })
        .sum()
}

#[test]
fn a_fitted_replacement_that_reports_nothing_dropped_keeps_all_its_content() {
    let schema = test_schema();
    let (mut kept, mut dropped) = (0, 0);
    for seed in 0..ROUNDS as u64 {
        let mut rng = Rng::new(seed + 32_452_843);
        let d = random_doc(&schema, &mut rng);
        let size = d.content_size();
        let a = rng.below(size + 1);
        let b = rng.below(size + 1);
        let (from, to) = (a.min(b), a.max(b));
        let slice = random_slice(&schema, &mut rng);
        let change = Change::replace(from, to, slice.clone()).with_fit(Fit::Auto);
        let Some(set) = make(&schema, &d, vec![change]) else {
            continue;
        };
        let out = set.apply(&d).expect("a fitted set applies");
        let removed = leaf_tokens(d.slice(from, to).expect("in range").content());
        let expected = leaf_tokens(d.content()) - removed + leaf_tokens(slice.content());
        let actual = leaf_tokens(out.content());
        if set.dropped_tokens() == 0 {
            assert_eq!(
                actual,
                expected,
                "seed {seed}: content went missing from {} without a report",
                schema.describe(&out)
            );
            kept += 1;
        } else {
            dropped += 1;
        }
    }
    assert!(
        kept > ROUNDS / 4 && dropped > 0,
        "{kept} kept, {dropped} dropped"
    );
}
