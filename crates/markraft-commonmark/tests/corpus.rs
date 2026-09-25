//! A corpus of hard inputs, judged the same way as the spec.
//!
//! The inputs exercise escaping, entities, link destinations, code fences,
//! emphasis flanking, the underline HTML import, CJK and emoji. Only the inputs
//! are kept: their original expectations assumed one block per source line,
//! which this model does not.

mod common;

use common::{CORPUS, Codec, judge};

/// Corpus entries whose HTML this codec deliberately does not reproduce, each
/// with its reason.
const ALLOWED: &[(&str, &str)] = &[
    // A lone `<br>` block reads as an empty paragraph (see `parse`), and an
    // empty document writes as nothing.
    ("<br>", "an empty document writes as nothing"),
];

#[test]
fn the_old_corpus_renders_the_same() {
    let codec = Codec::new();
    let allowed: Vec<&str> = ALLOWED.iter().map(|(source, _)| *source).collect();
    let mut unexpected = Vec::new();
    let mut fixed = Vec::new();
    for source in CORPUS {
        match judge(&codec, source) {
            Ok(()) if allowed.contains(source) => fixed.push(*source),
            Ok(()) => {}
            Err(message) if allowed.contains(source) => {
                let _ = message;
            }
            Err(message) => unexpected.push(message),
        }
    }
    assert!(
        unexpected.is_empty() && fixed.is_empty(),
        "newly failing:\n{}\nno longer failing (remove from ALLOWED): {fixed:?}",
        unexpected.join("\n")
    );
}

#[test]
fn the_old_corpus_normalises_to_a_fixed_point() {
    let codec = Codec::new();
    for source in CORPUS {
        let once = codec.normalize(source);
        let twice = codec.normalize(&once);
        assert_eq!(once, twice, "{source:?} does not settle: {once:?}");
    }
}

/// Destinations round-tripped three times, to catch escapes that grow on
/// every pass: spelled once from a link mark, then read and written again.
#[test]
fn link_destinations_survive_repeated_round_trips() {
    let codec = Codec::new();
    for url in [
        "https://example.com/?q=&copy;",
        "https://example.com/?q=&amp;&#32;",
        r"https://example.com/a\(b)",
        "https://example.com/<tag>",
        "https://example.com/a b&copy;",
        "https://example.com/a(b",
        "a\nb",
        "a\rb",
        "a\u{1}b",
        "a\u{7f}b",
        "  a b  ",
        "",
    ] {
        let doc = codec
            .schema
            .doc([codec
                .schema
                .node(
                    "paragraph",
                    [codec.schema.text_marked(
                        "label",
                        markraft_core::MarkSet::from_marks(
                            &codec.schema,
                            [codec
                                .schema
                                .mark("link", markraft_core::attrs! {"href" => url})
                                .expect("the link mark exists")],
                        ),
                    )],
                )
                .expect("a paragraph")])
            .expect("a document");
        // A link mark with no spelling of its own is what pasted HTML reads
        // as; spelling it is what gives it one. What must not change is the
        // destination — then or on any pass after it.
        let spelled = markraft_commonmark::serialize::spell(&codec.serializer, doc.child(0));
        let mut current = codec.parse(&spelled);
        let canonical = current.clone();
        assert_eq!(
            destination(&codec, &current).as_deref(),
            Some(url),
            "the first write changed the destination {url:?}"
        );
        for pass in 0..3 {
            let written = codec.write(&current);
            current = codec.parse(&written);
            assert_eq!(
                current, canonical,
                "pass {pass} changed the destination {url:?}: {written:?}"
            );
        }
    }
}

/// The href of the first link mark in `doc`.
fn destination(codec: &Codec, doc: &markraft_core::Node) -> Option<String> {
    let mut out = None;
    doc.nodes_between(0, doc.content_size(), &mut |node, _, _, _| {
        for mark in node.marks().iter() {
            if codec.schema.mark_type(mark.ty).name() == "link" {
                out = mark
                    .attrs
                    .get("href")
                    .and_then(|value| value.as_str())
                    .map(str::to_owned);
            }
        }
        true
    });
    out
}
