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

/// Collapse whitespace outside `<pre>` blocks, and spell every void element
/// one way.
///
/// CommonMark lets a renderer put a soft line break in the output as a newline
/// or as a space, and this codec does not record where the author wrapped their
/// prose, so whitespace between inline content is not a difference worth
/// judging. Inside `<pre>` every byte counts and is left alone.
///
/// A void element is written `<br>` or `<br />` at a writer's whim, and an
/// `<img>` with no `alt` renders exactly as one with an empty `alt`. Neither
/// is visible to a reader, and this codec changes both when it reads a tag as
/// the node it stands for. Everything else in a tag — the element name, every
/// other attribute and every value — is compared as written.
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
    // Collapse only text whitespace. Attribute values and comments are data:
    // globally splitting whitespace would hide corruption of `title="a  b"`.
    let mut out = String::new();
    let mut pending_space = false;
    // A reader strips the whitespace that starts the line a `<br>` begins, so
    // whether a writer puts a line ending after the tag is not visible either.
    let mut after_break = false;
    let mut rest = html;
    while !rest.is_empty() {
        if rest.starts_with('<') {
            let end = if rest.starts_with("<!--") {
                rest.find("-->").map(|index| index + 3)
            } else {
                let mut quote = None;
                rest.char_indices().find_map(|(index, character)| {
                    match (quote, character) {
                        (Some(q), c) if q == c => quote = None,
                        (None, '\'' | '"') => quote = Some(character),
                        (None, '>') => return Some(index + 1),
                        _ => {}
                    }
                    None
                })
            }
            .unwrap_or(rest.len());
            let tag = canonical_tag(&rest[..end]);
            if pending_space && !out.is_empty() && !after_break {
                out.push(' ');
            }
            pending_space = false;
            after_break = tag == "<br>";
            out.push_str(&tag);
            rest = &rest[end..];
        } else {
            let character = rest.chars().next().expect("nonempty remainder");
            rest = &rest[character.len_utf8()..];
            if character.is_ascii_whitespace() {
                pending_space = true;
            } else {
                if pending_space && !out.is_empty() && !after_break {
                    out.push(' ');
                }
                pending_space = false;
                after_break = false;
                out.push(character);
            }
        }
    }
    out
}

/// The elements HTML closes for the writer, where the `/` before the `>` is
/// decoration rather than markup.
const VOID_ELEMENTS: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

/// One void element's tag as a reader sees it: no optional `/`, and no `alt`
/// on an `<img>` that has nothing to say.
fn canonical_tag(tag: &str) -> String {
    let Some(body) = tag
        .strip_prefix('<')
        .and_then(|rest| rest.strip_suffix('>'))
    else {
        return tag.to_string();
    };
    let end = body
        .find(|c: char| !c.is_ascii_alphanumeric())
        .unwrap_or(body.len());
    let name = body[..end].to_ascii_lowercase();
    if !VOID_ELEMENTS.contains(&name.as_str()) {
        return tag.to_string();
    }
    let mut body = if name == "img" {
        body.replace(" alt=\"\"", "")
    } else {
        body.to_string()
    };
    // A `/` that closes the tag, rather than one inside an unquoted value.
    if let Some(rest) = body.strip_suffix('/')
        && (rest.len() == name.len() || rest.ends_with([' ', '\t', '\n', '"', '\'']))
    {
        body = rest.trim_end().to_string();
    }
    format!("<{body}>")
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
