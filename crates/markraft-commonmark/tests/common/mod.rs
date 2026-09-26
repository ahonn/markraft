//! Shared helpers: the codec under test and the HTML judge.
//!
//! Each integration test compiles this module separately, so not every binary
//! uses every helper.
#![allow(dead_code)]

use markraft_commonmark::{
    HouseStyle, HouseStyleHandle, MarkdownParser, MarkdownSerializer, commonmark_options,
    commonmark_schema, commonmark_serializer,
};
use markraft_core::{Node, Schema};

/// The codec under test.
pub struct Codec {
    pub schema: Schema,
    pub parser: MarkdownParser,
    pub serializer: MarkdownSerializer,
    /// The house style the serializer writes new syntax in.
    pub house: HouseStyleHandle,
}

impl Codec {
    pub fn new() -> Codec {
        Codec::in_house(HouseStyle::default())
    }

    /// The codec, writing new syntax in `style`.
    pub fn in_house(style: HouseStyle) -> Codec {
        let schema = commonmark_schema();
        let house = HouseStyleHandle::new(style);
        Codec {
            parser: MarkdownParser::commonmark(schema.clone()),
            serializer: commonmark_serializer(&schema, &house),
            schema,
            house,
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

// -- inline source ------------------------------------------------------------

/// A placeholder for an atom in a textblock's text: one character, which
/// CommonMark 0.31 classes as punctuation.
pub const ATOM: char = '\u{fffc}';

/// The application's comrak options, rendering raw HTML and every link
/// destination as written so a judge sees them.
pub fn options() -> comrak::Options<'static> {
    let mut options = commonmark_options();
    options.render.r#unsafe = true;
    options
}

/// `markdown` with every atom's source replaced by [`ATOM`].
pub fn with_atoms(markdown: &str) -> String {
    let arena = comrak::Arena::new();
    let root = comrak::parse_document(&arena, markdown, &options());
    let starts = line_starts(markdown);
    let lines: Vec<&str> = markdown.split('\n').collect();
    let mut ranges = Vec::new();
    collect_atoms(root, &starts, &lines, 0, markdown, &mut ranges);
    ranges.sort_by_key(|range| (range.start, range.end));
    let mut out = String::new();
    let mut at = 0;
    for range in ranges {
        if range.start < at {
            continue;
        }
        out.push_str(&markdown[at..range.start]);
        out.push(ATOM);
        at = range.end;
    }
    out.push_str(&markdown[at..]);
    out
}

/// Collect the byte ranges of the atoms under `node`, whose inline positions
/// are reported `shift` lines early.
///
/// comrak reports the inline positions of a paragraph that starts with link
/// reference definitions as though the definitions were not there: one line
/// early for each line they take.
fn collect_atoms<'a>(
    node: &'a comrak::nodes::AstNode<'a>,
    starts: &[usize],
    lines: &[&str],
    shift: usize,
    markdown: &str,
    out: &mut Vec<std::ops::Range<usize>>,
) {
    let (value, mut pos) = {
        let data = node.data.borrow();
        (data.value.clone(), data.sourcepos)
    };
    let mut shift = shift;
    if matches!(value, comrak::nodes::NodeValue::Paragraph) {
        let own = &lines[pos.start.line - 1..pos.end.line.min(lines.len())];
        shift = leading_definition_lines(own);
    } else {
        pos.start.line += shift;
        pos.end.line += shift;
    }
    let atom = match &value {
        comrak::nodes::NodeValue::Image(_) | comrak::nodes::NodeValue::WikiLink(_) => true,
        comrak::nodes::NodeValue::HtmlInline(html) => !is_u_tag(html) && !html_is_read(node),
        _ => false,
    };
    if atom {
        if let Some(range) = byte_range(starts, pos, markdown) {
            out.push(range);
        }
        return;
    }
    for child in node.children() {
        collect_atoms(child, starts, lines, shift, markdown, out);
    }
}

/// An inline HTML tag's lower-case name, whether it closes, and whether it
/// carries an `href`; `None` for anything that is not an opening or closing
/// tag, or closes itself.
fn tag_of(html: &str) -> Option<(String, bool, bool)> {
    let body = html.strip_prefix('<')?.strip_suffix('>')?;
    let (body, closing) = match body.strip_prefix('/') {
        Some(rest) => (rest, true),
        None => (body, false),
    };
    if body.ends_with('/') {
        return None;
    }
    let len = body
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .unwrap_or(body.len());
    let name = body[..len].to_ascii_lowercase();
    let href = body[len..].to_ascii_lowercase().contains("href");
    (!name.is_empty()).then_some((name, closing, href))
}

/// Whether an inline HTML node is read as something other than an atom: a
/// style tag — `<u>`, `<em>`, `<strong>`, `<del>`, `<mark>`, `<sup>`, `<sub>`,
/// `<kbd>`, the aliases `<ins>`, `<i>`, `<b>`, `<s>` and `<strike>`, and
/// `<a href>` — paired with its partner among its siblings, or a `<br>` a soft
/// break follows.
///
/// This restates the tags derive reads as styles (`Tag::style` in
/// `src/derive.rs`); the two lists must name the same tags.
pub fn html_is_read<'a>(node: &'a comrak::nodes::AstNode<'a>) -> bool {
    use comrak::nodes::NodeValue;
    let tag = |n: &'a comrak::nodes::AstNode<'a>| match &n.data.borrow().value {
        NodeValue::HtmlInline(html) => tag_of(html),
        _ => None,
    };
    let Some((name, _, _)) = tag(node) else {
        return false;
    };
    if name == "br" {
        return node
            .next_sibling()
            .is_some_and(|next| matches!(next.data.borrow().value, NodeValue::SoftBreak));
    }
    let Some(parent) = node.parent() else {
        return false;
    };
    let style = [
        "u", "em", "strong", "del", "mark", "sup", "sub", "kbd", "ins", "i", "b", "s", "strike",
        "a",
    ];
    let mut open: Vec<(String, *const comrak::nodes::AstNode<'a>)> = Vec::new();
    for sibling in parent.children() {
        let Some((name, closing, href)) = tag(sibling) else {
            continue;
        };
        if closing {
            if let Some(at) = open.iter().rposition(|(open, _)| *open == name) {
                let (_, opener) = open.remove(at);
                if std::ptr::eq(opener, node) || std::ptr::eq(sibling, node) {
                    return true;
                }
            }
        } else if style.contains(&name.as_str()) && (name != "a" || href) {
            open.push((name, sibling));
        }
    }
    false
}

/// Whether an inline HTML tag is `<u>` or `</u>`, which stays text when it
/// has no partner: half of a style being typed.
pub fn is_u_tag(html: &str) -> bool {
    let body = html.trim_start_matches('<').trim_end_matches('>').trim();
    body.eq_ignore_ascii_case("u") || body.eq_ignore_ascii_case("/u")
}

/// The byte offset of each line's start.
pub fn line_starts(text: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(text.match_indices('\n').map(|(at, _)| at + 1))
        .collect()
}

/// The bytes of `text` a comrak position covers, reading its columns as byte
/// offsets from the start of its line (which also reads a column past a line's
/// end the way comrak means it), or `None` when they do not fit.
pub fn byte_range(
    starts: &[usize],
    pos: comrak::nodes::Sourcepos,
    text: &str,
) -> Option<std::ops::Range<usize>> {
    let from = starts.get(pos.start.line.checked_sub(1)?)? + pos.start.column.checked_sub(1)?;
    let to = starts.get(pos.end.line.checked_sub(1)?)? + pos.end.column;
    (from < to && to <= text.len() && text.is_char_boundary(from) && text.is_char_boundary(to))
        .then_some(from..to)
}

/// How many whole lines at the start of `lines` a reader takes for link
/// reference definitions: the longest run that reads as nothing else.
pub fn leading_definition_lines(lines: &[&str]) -> usize {
    (1..=lines.len())
        .rev()
        .find(|count| {
            let arena = comrak::Arena::new();
            comrak::parse_document(&arena, &lines[..*count].join("\n"), &options())
                .first_child()
                .is_none()
        })
        .unwrap_or(0)
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

/// Hard Markdown inputs, kept without expectations; see `tests/corpus.rs`.
pub const CORPUS: &[&str] = &[
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
