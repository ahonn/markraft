//! The CommonMark spec examples, judged by the HTML they render to.
//!
//! `tests/data/commonmark-spec-0.31.2.json` is the example set published with
//! CommonMark 0.31.2, vendored so the suite does not reach the network.
//!
//! Each serialized example is compared directly with the official expected
//! HTML, using strict CommonMark parsing/rendering. There is no failure allowlist.
//! The application's GFM preset is checked independently for round-trip HTML
//! semantics and a stable canonical spelling.

mod common;

use common::{Codec, judge, normalize_html};
use serde::Deserialize;

#[derive(Deserialize)]
struct Example {
    markdown: String,
    html: String,
    example: usize,
    section: String,
}

#[test]
fn commonmark_spec_examples_render_the_same() {
    let raw = include_str!("data/commonmark-spec-0.31.2.json");
    let examples: Vec<Example> = serde_json::from_str(raw).expect("the vendored spec parses");
    assert_eq!(examples.len(), 652, "the official corpus must be complete");

    let mut codec = Codec::new();
    let mut options = comrak::Options::default();
    options.render.r#unsafe = true;
    codec.parser = codec.parser.with_options(options.clone());
    // The application preset is GFM. Strict CommonMark spells a del mark as
    // HTML, since its ~~ delimiter belongs to the GFM extension.
    let mut marks = markraft_commonmark::commonmark_mark_rules();
    marks.insert(
        markraft_commonmark::schema::STRIKETHROUGH.to_string(),
        markraft_commonmark::MarkRule::fixed("<del>", "</del>"),
    );
    codec.serializer = markraft_commonmark::MarkdownSerializer::new(
        codec.schema.clone(),
        markraft_commonmark::commonmark_node_rules(),
        marks,
    );
    let mut failed = Vec::new();
    let mut report = String::new();
    for example in &examples {
        let written = codec.normalize(&example.markdown);
        let expected = normalize_html(&example.html);
        let actual = normalize_html(&comrak::markdown_to_html(&written, &options));
        if expected != actual {
            let message = format!(
                "source: {:?}\nwritten: {written:?}\nexpected: {expected}\nactual: {actual}",
                example.markdown
            );
            failed.push(example.example);
            report.push_str(&format!(
                "\n--- example {} ({})\n{message}\n",
                example.example, example.section
            ));
        }
    }

    assert!(
        failed.is_empty(),
        "{} of {} examples match.\nFailing: {failed:?}{report}",
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
        judge(&codec, &example.markdown)
            .unwrap_or_else(|message| panic!("GFM preset example {}: {message}", example.example));
        let once = codec.normalize(&example.markdown);
        let twice = codec.normalize(&once);
        assert_eq!(
            once, twice,
            "example {} ({}) does not settle:\n{:?}",
            example.example, example.section, example.markdown
        );
    }
}
