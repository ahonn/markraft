//! The CommonMark spec examples, judged by the HTML they render to.
//!
//! `tests/data/commonmark-spec-0.31.2.json` is the example set published with
//! CommonMark 0.31.2, vendored so the suite does not reach the network.
//!
//! For each example the judge asks whether `serialize(parse(md))` renders the
//! same HTML as `md` did. Where it does not, the example number is on
//! [`ALLOWED`] with the reason. The test asserts the failing set is *exactly*
//! that list, so a regression and an improvement both show up.

mod common;

use std::collections::BTreeSet;

use common::{Codec, judge};
use serde::Deserialize;

#[derive(Deserialize)]
struct Example {
    markdown: String,
    example: usize,
    section: String,
}

/// Spec examples whose HTML this codec deliberately does not reproduce.
///
/// Every entry is a consequence of a decision the crate documents, not a bug
/// waiting to be fixed.
const ALLOWED: &[(usize, &str)] = &[
    // A mark set holds one mark per type, so emphasis cannot nest inside
    // emphasis of the same kind; the inner run merges into the outer one.
    (369, "nested emphasis of the same type"),
    (373, "nested emphasis of the same type"),
    (389, "nested emphasis of the same type"),
    (407, "nested emphasis of the same type"),
    (408, "nested emphasis of the same type"),
    (409, "nested emphasis of the same type"),
    (417, "nested emphasis of the same type"),
    (418, "nested emphasis of the same type"),
    (419, "nested emphasis of the same type"),
    (425, "nested emphasis of the same type"),
    (426, "nested emphasis of the same type"),
    (427, "nested emphasis of the same type"),
    (432, "nested emphasis of the same type"),
    (461, "nested emphasis of the same type"),
    (463, "nested emphasis of the same type"),
    (464, "nested emphasis of the same type"),
    (465, "nested emphasis of the same type"),
    (466, "nested emphasis of the same type"),
    (468, "nested emphasis of the same type"),
    // Mark ranks fix the nesting, so strong that covers only part of an
    // emphasis run splits the emphasis into two runs that render the same.
    (413, "mark ranks fix the nesting order"),
    // Inline HTML is not a modelled construct: an unpaired tag degrades to the
    // source text, which is then escaped so it survives another round trip.
    (148, "inline HTML degrades to text"),
    (187, "inline HTML degrades to text"),
    (201, "inline HTML degrades to text"),
    (344, "inline HTML degrades to text"),
    (475, "inline HTML degrades to text"),
    (476, "inline HTML degrades to text"),
    (477, "inline HTML degrades to text"),
    (491, "inline HTML degrades to text"),
    (494, "inline HTML degrades to text"),
    (524, "inline HTML degrades to text"),
    (536, "inline HTML degrades to text"),
    (613, "inline HTML degrades to text"),
    (614, "inline HTML degrades to text"),
    (615, "inline HTML degrades to text"),
    (616, "inline HTML degrades to text"),
    (617, "inline HTML degrades to text"),
    (623, "inline HTML degrades to text"),
    (625, "inline HTML degrades to text"),
    (626, "inline HTML degrades to text"),
    (627, "inline HTML degrades to text"),
    (628, "inline HTML degrades to text"),
    (629, "inline HTML degrades to text"),
    (630, "inline HTML degrades to text"),
    (631, "inline HTML degrades to text"),
    (642, "inline HTML degrades to text"),
    (643, "inline HTML degrades to text"),
    // A link with an empty label has no inline content to carry the mark, so it
    // travels as the source text it reads as.
    (484, "a link with no label"),
    (487, "a link with no label"),
];

#[test]
fn commonmark_spec_examples_render_the_same() {
    let raw = include_str!("data/commonmark-spec-0.31.2.json");
    let examples: Vec<Example> = serde_json::from_str(raw).expect("the vendored spec parses");
    assert!(examples.len() > 600, "the spec set looks truncated");

    let codec = Codec::new();
    let allowed: BTreeSet<usize> = ALLOWED.iter().map(|(number, _)| *number).collect();
    let mut failed: BTreeSet<usize> = BTreeSet::new();
    let mut report = String::new();
    for example in &examples {
        if let Err(message) = judge(&codec, &example.markdown) {
            failed.insert(example.example);
            if !allowed.contains(&example.example) {
                report.push_str(&format!(
                    "\n--- example {} ({})\n{message}\n",
                    example.example, example.section
                ));
            }
        }
    }

    let unexpected: Vec<usize> = failed.difference(&allowed).copied().collect();
    let fixed: Vec<usize> = allowed.difference(&failed).copied().collect();
    assert!(
        unexpected.is_empty() && fixed.is_empty(),
        "{} of {} examples match.\nnewly failing: {unexpected:?}\nno longer failing (remove from ALLOWED): {fixed:?}{report}",
        examples.len() - failed.len(),
        examples.len(),
    );
}

#[test]
fn every_spec_example_parses_and_normalises_to_a_fixed_point() {
    let raw = include_str!("data/commonmark-spec-0.31.2.json");
    let examples: Vec<Example> = serde_json::from_str(raw).expect("the vendored spec parses");
    let codec = Codec::new();
    for example in &examples {
        let once = codec.normalize(&example.markdown);
        let twice = codec.normalize(&once);
        assert_eq!(
            once, twice,
            "example {} ({}) does not settle:\n{:?}",
            example.example, example.section, example.markdown
        );
    }
}
