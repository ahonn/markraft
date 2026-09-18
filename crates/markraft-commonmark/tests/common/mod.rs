//! Shared helpers: the codec under test and the HTML judge.
//!
//! Each integration test compiles this module separately, so not every binary
//! uses every helper.
#![allow(dead_code)]

use markraft_commonmark::{
    MarkdownParser, MarkdownSerializer, commonmark_options, commonmark_schema,
    commonmark_serializer,
};
use markraft_core::{Node, Schema};

/// The codec under test.
pub struct Codec {
    pub schema: Schema,
    pub parser: MarkdownParser,
    pub serializer: MarkdownSerializer,
}

impl Codec {
    pub fn new() -> Codec {
        let schema = commonmark_schema();
        Codec {
            parser: MarkdownParser::commonmark(schema.clone()),
            serializer: commonmark_serializer(&schema),
            schema,
        }
    }

    pub fn parse(&self, source: &str) -> Node {
        self.parser
            .parse(source)
            .unwrap_or_else(|error| panic!("parsing {source:?} failed: {error}"))
    }

    pub fn write(&self, doc: &Node) -> String {
        self.serializer.serialize(doc)
    }

    /// Parse and write again, which is the normalisation this codec promises.
    pub fn normalize(&self, source: &str) -> String {
        self.write(&self.parse(source))
    }

    pub fn describe(&self, doc: &Node) -> String {
        self.schema.describe(doc)
    }
}

/// Render Markdown the way the judge sees it.
pub fn html(source: &str) -> String {
    let mut options = commonmark_options();
    // Raw HTML has to reach the output, or a `raw_block` and the source it came
    // from would both collapse to the same placeholder and hide a difference.
    options.render.r#unsafe = true;
    normalize_html(&comrak::markdown_to_html(source, &options))
}

/// Collapse whitespace outside `<pre>` blocks.
///
/// CommonMark lets a renderer put a soft line break in the output as a newline
/// or as a space, and this codec does not record where the author wrapped their
/// prose, so whitespace between inline content is not a difference worth
/// judging. Inside `<pre>` every byte counts and is left alone.
pub fn normalize_html(html: &str) -> String {
    let mut out = String::new();
    let mut rest = html;
    while let Some(start) = rest.find("<pre") {
        out.push_str(&collapse(&rest[..start]));
        let end = rest[start..]
            .find("</pre>")
            .map_or(rest.len(), |offset| start + offset + "</pre>".len());
        out.push_str(&rest[start..end]);
        rest = &rest[end..];
    }
    out.push_str(&collapse(rest));
    out
}

fn collapse(html: &str) -> String {
    html.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether the codec's normalisation of `source` renders the same HTML.
pub fn judge(codec: &Codec, source: &str) -> Result<(), String> {
    let written = codec.normalize(source);
    let expected = html(source);
    let actual = html(&written);
    if expected == actual {
        return Ok(());
    }
    Err(format!(
        "source:   {source:?}\nwritten:  {written:?}\nexpected: {expected}\nactual:   {actual}"
    ))
}

/// A deterministic generator, the same xorshift the model's tests use.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(6364136223846793005).wrapping_add(1) | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next_u64() % n as u64) as usize
        }
    }

    pub fn range(&mut self, from: usize, to: usize) -> usize {
        from + self.below(to - from + 1)
    }

    pub fn one_in(&mut self, n: usize) -> bool {
        self.below(n) == 0
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}
