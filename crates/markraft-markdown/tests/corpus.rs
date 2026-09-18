//! The old flat-model codec's corpus, judged the same way as the spec.
//!
//! These are the inputs `markraft-core` was hardened against. Its *expectations*
//! were written for a model where one source line was one block and do not
//! carry over, but the inputs still exercise everything that was hard-won:
//! escaping, entities, link destinations, code fences, emphasis flanking, the
//! `<u>` convention, CJK and emoji.

mod common;

use common::{Codec, judge};

/// Every Markdown source the old codec's tests fed to `from_markdown`.
const CORPUS: &[&str] = &[
    // Plain text.
    "",
    "a",
    "ab",
    "aaa",
    "x",
    "xz",
    "text",
    "abcdef",
    "original",
    "read this",
    "a\nb",
    "one\ntwo",
    "first\nsecond\nthird",
    "abcd\nef",
    "tail",
    "**paste**",
    // Headings.
    "# Title\nbody text",
    "# a **bold** tail\n- item\n中文",
    "# a **bold** tail\n- item\n中文\n```\n\ncode\n```\nlast",
    "# h\n\n\npara\n\n- a\n\n- b\n\n> q\n> \n> r",
    "Title\n=====",
    "```rust\none\ntwo\n```\n# after",
    // Lists.
    "- item",
    "- first\n  - ",
    "- first\n  - child",
    "- root\n  - child",
    "- root\n  - child\n- sibling",
    "- root\n  - child\nparagraph",
    "- first\n- second\n  - child\n- third\n- last",
    "- parent\n  - child\n    - grandchild\n  - second child\n- root",
    "- [x] done\n- [ ] open\nuntouched",
    "- first\n- \n- [ ] \n1. \n",
    "- first\n    - child\n        - grandchild",
    "1. one\n2. two\nbreak\n1. again",
    "3) one\n9. two\nbreak\n1. again",
    "1. first\n    1. child\n        - grandchild\n    2. child two\n2. second\n- [ ] task\n  - [x] nested\n> quote\n> > nested quote",
    "-   lead",
    // Quotes and dividers.
    "> first\n> second\n",
    "\\> not a quote",
    "---\nafter",
    "***\nafter",
    "\\-\\-\\-",
    "text\n---\nmore",
    "> quote\nlazy\n- item\ncontinued",
    "> - item\n> - two\n- > quoted\n  - > deep",
    // Code.
    "```\n```",
    "```\nx\n```\nafter",
    "```rust\na\nb\n```",
    "```rust\nx\n```\nafter",
    "```rust\ntext\n```",
    "```rust\none\ntwo\n```",
    "```rust\n\nafter\n```",
    "before\n```rust\n\nafter\n```",
    "```rust\nlet x = 1;\n```",
    "```rust\none\n  two\nlast\n```",
    "```rust\ncode\n\nmore\n```",
    "```rust\nlet x = 1;\nprintln!(\"{x}\");\n```\nparagraph",
    "```rust\n# not a heading\n\n**not bold**\n```\nafter\n````\n```\n````",
    "~~~\n~~~",
    "    indented\n    more\n\ntext",
    "- item\n\n  ```rust\n  code\n  ```",
    // Emphasis.
    "ab**cd**\nef\ngh",
    "~~gone~~ and <u>**kept**</u>",
    "**bold** [link](https://example.com)",
    "# Title\n- **bold** and *italic* and `code`\n- [ ] todo\n- [x] done\n\n***both***",
    "# Title\n- **bold** and *italic* and `code`\n- [ ] todo\n- [x] done\n\n***both*** **`bold code`** \\*literal\\*",
    // Links.
    "see [the **docs**](https://example.com/a_(b)) and [x](<a b>)",
    "see [the docs](https://example.com) end",
    "[x](https://example.com/?q=&amp;copy;)",
    "[ref]: https://example.com",
    // Escaping, entities, whitespace.
    "a \\* b &amp; c &copy;",
    "1\\. not a list",
    "  lead",
    "one\ntwo  \nthree\\\nfour",
    "&amp; &#32; \\* a",
    "    four",
    "\tfour",
    "   \tfour",
    // Constructs with no model of their own.
    "| a | b |\n[text] (url)\n![alt](image.png)",
    "| a | b |\n|---|---|\n| c | d |",
    "<div>\nraw\n</div>",
    "![alt](image.png)",
    "term\n: definition",
    "[^1]: a footnote",
    "<br>",
    "$$\nx = 1\n$$",
    // Unicode.
    "中😀\ntext",
    "a😀b\nc",
    "e\u{301}x",
    "hello  é 👨‍👩‍👧‍👦\n中文",
    "中😀e\u{301}👨‍👩‍👧‍👦\n末",
    "a👨‍👩‍👧‍👦b",
    "a👨‍👩‍👧‍👦\nb",
    "\u{3000}你好",
    "\u{a0}hello",
    "  \u{3000}你好",
    "first\n\u{3000}second",
];

/// Corpus entries whose HTML this codec deliberately does not reproduce, for
/// the same reasons the spec suite lists.
const ALLOWED: &[(&str, &str)] = &[
    // `<br>` alone is the spelling this codec gives an empty paragraph, and an
    // empty paragraph that is a document's only block writes as nothing at all
    // — which is how CommonMark spells an empty document.
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

/// The destinations the old codec round-tripped three times, to catch escapes
/// that grow on every pass.
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
                        markraft_doc::MarkSet::from_marks(
                            &codec.schema,
                            [codec
                                .schema
                                .mark("link", markraft_doc::attrs! {"href" => url})
                                .expect("the link mark exists")],
                        ),
                    )],
                )
                .expect("a paragraph")])
            .expect("a document");
        let mut current = doc.clone();
        for pass in 0..3 {
            let written = codec.write(&current);
            current = codec.parse(&written);
            assert_eq!(
                current, doc,
                "pass {pass} changed the destination {url:?}: {written:?}"
            );
        }
    }
}
