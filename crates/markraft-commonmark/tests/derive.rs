//! [`derive`] judged against comrak's own HTML, and checked for stability.
//!
//! `derive` reads a textblock's inline source through comrak and maps comrak's
//! positions back onto the text. What it says a text *means* — which visible
//! characters carry which style — has to agree with what comrak renders for the
//! same source, or the positions went wrong somewhere. The judge is the HTML,
//! walked element by element, not the AST `derive` itself was built from.
//!
//! The inputs are the CommonMark 0.31.2 spec examples, the GFM table examples,
//! the shared corpus of hard inputs, and random edits to all of them.
//!
//! # Which examples are judged
//!
//! Each example is first brought into the shape the inline source model gives
//! a document: every image, wiki link and raw HTML tag other than `<u>`/`</u>`
//! becomes one U+FFFC, since those are atoms in the tree and `derive` only ever
//! sees their placeholder. The example is then judged when every top-level
//! block of the result is a paragraph, a heading or a table, whose inline
//! source can be cut out of the document without knowing any container's
//! prefixes. Everything else — lists, block quotes, code and HTML blocks,
//! thematic breaks — holds no inline source of its own or needs the parser
//! to strip container prefixes, and is counted, not judged. The counts are
//! asserted, so a change in comrak or in the rule shows up here.
//!
//! The stability tests take every textblock the parser builds instead,
//! container-held ones included, with their prefixes stripped as the tree
//! holds them.
//!
//! Examples that are judged but do not agree are listed in [`SKIPPED`] with
//! the reason; the test fails if the list is stale in either direction.

mod common;

use common::{ATOM, html_is_read, leading_definition_lines, options, with_atoms};

use std::collections::BTreeMap;

use comrak::nodes::{AstNode, NodeValue, Sourcepos};
use comrak::{Arena, parse_document};
use markraft_commonmark::derive::{
    BlockKind, DeriveContext, Derived, Guarded, Style, derive, guard,
};
use scraper::{Html, Node as HtmlNode};
use serde::Deserialize;

#[derive(Deserialize)]
struct Example {
    markdown: String,
    example: usize,
}

/// The shared corpus as examples, numbered from 10 000 so the numbers
/// never meet the spec's.
fn corpus_examples() -> Vec<Example> {
    common::CORPUS
        .iter()
        .enumerate()
        .map(|(index, markdown)| Example {
            markdown: markdown.to_string(),
            example: 10_000 + index,
        })
        .collect()
}

/// Judged examples whose meaning `derive` does not reproduce, and why.
const SKIPPED: &[(usize, &str)] = &[];

/// Stands for a hard line break in a semantic sequence.
const BREAK: char = '\u{2028}';

fn spec_examples() -> Vec<Example> {
    let raw = include_str!("data/commonmark-spec-0.31.2.json");
    serde_json::from_str(raw).expect("the vendored spec parses")
}

fn table_examples() -> Vec<Example> {
    let raw = include_str!("data/gfm-tables-0.29.json");
    serde_json::from_str(raw).expect("the vendored examples parse")
}

// -- cutting inline source out of a document --------------------------------

/// One textblock's inline source, cut out of an example.
struct Block {
    kind: BlockKind,
    text: String,
}

/// The textblocks of a document whose top-level blocks are all paragraphs,
/// headings and tables, with the link reference definitions it holds; `None`
/// for any other document.
fn textblocks(markdown: &str) -> Option<(Vec<Block>, String)> {
    let arena = Arena::new();
    let root = parse_document(&arena, markdown, &options());
    let lines: Vec<&str> = markdown.split('\n').collect();
    let mut covered = vec![false; lines.len() + 1];
    let mut blocks = Vec::new();
    let mut definitions = Vec::new();
    for block in root.children() {
        let (value, pos) = {
            let data = block.data.borrow();
            (data.value.clone(), data.sourcepos)
        };
        let last = pos.end.line.min(lines.len());
        covered[pos.start.line..=last].fill(true);
        match value {
            NodeValue::Paragraph => {
                let text = paragraph_source(&lines, pos, pos.end.line, &mut definitions);
                blocks.push(Block {
                    kind: BlockKind::Paragraph,
                    text,
                });
            }
            // A setext heading is a paragraph with an underline, definitions
            // and all.
            NodeValue::Heading(heading) if heading.setext => {
                let text = paragraph_source(&lines, pos, pos.end.line - 1, &mut definitions);
                blocks.push(Block {
                    kind: BlockKind::Heading,
                    text,
                });
            }
            NodeValue::Heading(_) => blocks.push(Block {
                kind: BlockKind::Heading,
                text: children_source(block, &lines),
            }),
            NodeValue::Table(_) => {
                for row in block.children() {
                    for cell in row.children() {
                        let pos = cell.data.borrow().sourcepos;
                        let text = lines
                            .get(pos.start.line.wrapping_sub(1))
                            .and_then(|line| {
                                line.get(pos.start.column.checked_sub(1)?..pos.end.column)
                            })
                            .unwrap_or_default()
                            .trim();
                        // A cell the row was short of has the position of the
                        // row's closing pipe; it is empty.
                        let text = if text == "|" { "" } else { text }.to_string();
                        blocks.push(Block {
                            kind: BlockKind::TableCell,
                            text,
                        });
                    }
                }
            }
            _ => return None,
        }
    }
    for (index, line) in lines.iter().enumerate() {
        if !covered[index + 1] && !line.trim().is_empty() {
            definitions.push(line.to_string());
        }
    }
    Some((blocks, definitions.join("\n")))
}

/// The inline source of a paragraph whose content ends on line `last`, with
/// the definitions at its start moved to `definitions`.
fn paragraph_source(
    lines: &[&str],
    pos: Sourcepos,
    last: usize,
    definitions: &mut Vec<String>,
) -> String {
    let mut source: Vec<&str> = lines[pos.start.line - 1..last].to_vec();
    source[0] = &source[0][pos.start.column - 1..];
    // Definitions at the start of a paragraph are dropped from it but stay
    // inside its position.
    let leading = leading_definition_lines(&source);
    definitions.extend(source.drain(..leading).map(str::to_string));
    source.join("\n").trim_end_matches([' ', '\t']).to_string()
}

/// The source from a heading's first inline child to its last.
fn children_source<'a>(node: &'a AstNode<'a>, lines: &[&str]) -> String {
    let (Some(first), Some(last)) = (node.first_child(), node.last_child()) else {
        return String::new();
    };
    let start = first.data.borrow().sourcepos.start;
    let end = last.data.borrow().sourcepos.end;
    let mut out = Vec::new();
    for line in start.line..=end.line {
        let text = lines[line - 1];
        let from = if line == start.line {
            start.column - 1
        } else {
            0
        };
        let to = if line == end.line {
            end.column.min(text.len())
        } else {
            text.len()
        };
        out.push(&text[from..to]);
    }
    out.join("\n")
}

// -- meaning --------------------------------------------------------------

/// A style as the judge compares it: a link's destination as HTML carries it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Seen {
    Strong,
    Em,
    Del,
    Code,
    Link(String, String),
    U,
    Mark,
    Sup,
    Sub,
    Kbd,
    Math,
}

/// What a reader sees, one entry per visible character, with the styles over
/// it. Whitespace carries no style and collapses to one space, because a
/// renderer may put a soft break in the output either way.
type Meaning = Vec<(char, Vec<Seen>)>;

fn settle(raw: Meaning) -> Meaning {
    let mut out: Meaning = Vec::new();
    for (character, mut styles) in raw {
        if character.is_whitespace() && character != BREAK {
            if out
                .last()
                .is_none_or(|(last, _)| *last == ' ' || *last == BREAK)
            {
                continue;
            }
            out.push((' ', Vec::new()));
            continue;
        }
        if character == BREAK {
            if out.last().is_some_and(|(last, _)| *last == ' ') {
                out.pop();
            }
            styles.clear();
        }
        styles.sort();
        styles.dedup();
        out.push((character, styles));
    }
    if out.last().is_some_and(|(last, _)| *last == ' ') {
        out.pop();
    }
    out
}

fn href(url: &str) -> String {
    let mut escaped = String::new();
    comrak::html::escape_href(&mut escaped, url, false).expect("writing to a string");
    escaped.replace("&amp;", "&").replace("&#x27;", "'")
}

fn derived_meaning(text: &str, derived: &Derived) -> Meaning {
    let seen = |offset: usize| -> Vec<Seen> {
        derived
            .styles_at(offset)
            .into_iter()
            .flat_map(|style| match style {
                Style::Strong => vec![Seen::Strong],
                Style::Emphasis => vec![Seen::Em],
                Style::Strikethrough => vec![Seen::Del],
                Style::Code => vec![Seen::Code],
                Style::Link { href: url, title } => vec![Seen::Link(href(url), title.clone())],
                Style::Underline => vec![Seen::U],
                Style::Highlight => vec![Seen::Mark],
                Style::Superscript => vec![Seen::Sup],
                Style::Subscript => vec![Seen::Sub],
                Style::Keyboard => vec![Seen::Kbd],
                Style::Math { .. } => vec![Seen::Math],
                // comrak writes a reference as a link to the note, raised.
                Style::FootnoteReference { label } => {
                    vec![Seen::Sup, Seen::Link(format!("#fn-{label}"), String::new())]
                }
            })
            .collect()
    };
    let mut out = Vec::new();
    let mut line_start = true;
    for (index, character) in text.chars().enumerate() {
        // A reader removes the indentation of a paragraph's lines before it
        // reads them, code spans included. The tree never holds it (the
        // parser strips it as comrak does), so derive leaves it as typed.
        let indentation = line_start && matches!(character, ' ' | '\t');
        line_start = character == '\n' || indentation;
        if indentation {
            continue;
        }
        if let Some(conceal) = derived.conceal_at(index) {
            if conceal.range.start == index {
                for shown in conceal.display.chars() {
                    out.push((shown, seen(index)));
                }
            }
            continue;
        }
        if derived.hard_breaks.contains(&index) {
            out.push((BREAK, Vec::new()));
        } else {
            out.push((character, seen(index)));
        }
    }
    settle(out)
}

/// The meaning of every paragraph, heading and table cell in `html`, in
/// document order.
fn rendered_meanings(html: &str) -> Vec<Meaning> {
    let document = Html::parse_fragment(html);
    let mut out = Vec::new();
    for node in document.root_element().descendants() {
        let Some(element) = scraper::ElementRef::wrap(node) else {
            continue;
        };
        let name = element.value().name();
        let block = matches!(
            name,
            "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "th" | "td"
        );
        if block {
            let mut raw = Vec::new();
            walk_html(element, &mut Vec::new(), &mut raw);
            out.push(settle(raw));
        }
    }
    out
}

fn walk_html(node: scraper::ElementRef<'_>, styles: &mut Vec<Seen>, out: &mut Meaning) {
    for child in node.children() {
        match child.value() {
            HtmlNode::Text(text) => {
                for character in text.chars() {
                    out.push((character, styles.clone()));
                }
            }
            HtmlNode::Element(element) => {
                let style = match element.name() {
                    _ if element.attr("data-math-style").is_some() => Some(Seen::Math),
                    // A raw tag reaches the rendering as written, alias and all.
                    "strong" | "b" => Some(Seen::Strong),
                    "em" | "i" => Some(Seen::Em),
                    "del" | "s" | "strike" => Some(Seen::Del),
                    "code" => Some(Seen::Code),
                    "u" | "ins" => Some(Seen::U),
                    "kbd" => Some(Seen::Kbd),
                    "mark" => Some(Seen::Mark),
                    "sup" => Some(Seen::Sup),
                    "sub" => Some(Seen::Sub),
                    "a" => Some(Seen::Link(
                        element.attr("href").unwrap_or_default().to_string(),
                        element.attr("title").unwrap_or_default().to_string(),
                    )),
                    "br" => {
                        out.push((BREAK, Vec::new()));
                        None
                    }
                    _ => None,
                };
                let pushed = style.is_some();
                styles.extend(style);
                if let Some(child) = scraper::ElementRef::wrap(child) {
                    walk_html(child, styles, out);
                }
                if pushed {
                    styles.pop();
                }
            }
            _ => {}
        }
    }
}

fn show(meaning: &Meaning) -> String {
    let mut out = String::new();
    let mut last: Option<&Vec<Seen>> = None;
    for (character, styles) in meaning {
        if last != Some(styles) {
            out.push_str(&format!("{styles:?}"));
            last = Some(styles);
        }
        out.push(*character);
    }
    out
}

// -- the differential -----------------------------------------------------

/// How the examples of one data set came out.
#[derive(Debug, Default, PartialEq, Eq)]
struct Tally {
    judged: usize,
    out_of_scope: usize,
    failed: BTreeMap<usize, String>,
}

fn judge_examples(examples: &[Example]) -> Tally {
    let mut tally = Tally::default();
    for example in examples {
        let markdown = with_atoms(&example.markdown);
        let Some((blocks, definitions)) = textblocks(&markdown) else {
            tally.out_of_scope += 1;
            continue;
        };
        tally.judged += 1;
        let ctx = DeriveContext::new().with_definitions(definitions);
        let expected = rendered_meanings(&comrak::markdown_to_html(&markdown, &options()));
        let actual: Vec<Meaning> = blocks
            .iter()
            .map(|block| derived_meaning(&block.text, &derive(block.kind, &block.text, &ctx)))
            .collect();
        if expected != actual {
            let mut report = format!("source: {:?}\n", example.markdown);
            for (index, block) in blocks.iter().enumerate() {
                report.push_str(&format!(
                    "  {:?} {:?}\n    derive:   {}\n    rendered: {}\n",
                    block.kind,
                    block.text,
                    show(&actual[index]),
                    expected.get(index).map(show).unwrap_or_default(),
                ));
            }
            if expected.len() != actual.len() {
                report.push_str(&format!(
                    "  {} rendered blocks for {} textblocks\n",
                    expected.len(),
                    actual.len()
                ));
            }
            tally.failed.insert(example.example, report);
        }
    }
    tally
}

#[test]
fn derive_means_what_comrak_renders_for_the_spec_tables_and_corpus() {
    let spec = judge_examples(&spec_examples());
    let tables = judge_examples(&table_examples());
    let corpus = judge_examples(&corpus_examples());
    let counts = [
        (spec.judged, spec.out_of_scope),
        (tables.judged, tables.out_of_scope),
        (corpus.judged, corpus.out_of_scope),
    ];
    let mut failed = spec.failed;
    failed.extend(tables.failed);
    failed.extend(corpus.failed);
    let skipped: BTreeMap<usize, &str> = SKIPPED.iter().copied().collect();
    let unexpected: Vec<&String> = failed
        .iter()
        .filter(|(example, _)| !skipped.contains_key(example))
        .map(|(_, report)| report)
        .collect();
    let stale: Vec<&usize> = skipped
        .keys()
        .filter(|example| !failed.contains_key(example))
        .collect();
    assert!(
        unexpected.is_empty() && stale.is_empty(),
        "{} examples disagree:\n{}\nlisted in SKIPPED but agreeing: {stale:?}",
        unexpected.len(),
        unexpected
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    );
    // (judged, out of scope) for the spec, the table examples and the corpus.
    assert_eq!(
        counts,
        [(430, 222), (7, 1), (50, 47)],
        "the examples in scope changed"
    );
}

// -- stability --------------------------------------------------------------

/// Every textblock the three data sets hold, as the parser holds it — in
/// containers too, whose prefixes it strips — with the definitions its
/// document keeps.
fn all_textblocks() -> Vec<(Block, String)> {
    let codec = common::Codec::new();
    let raw = codec.schema.node_id("raw_block");
    let mut out = Vec::new();
    for example in spec_examples()
        .into_iter()
        .chain(table_examples())
        .chain(corpus_examples())
    {
        let doc = codec.parse(&example.markdown);
        let mut blocks = Vec::new();
        let mut definitions = Vec::new();
        doc.descendants(&mut |node, _, _, _| {
            let name = codec.schema.node_type(node.type_id()).name();
            let kind = match name {
                "paragraph" => Some(BlockKind::Paragraph),
                "heading" => Some(BlockKind::Heading),
                "table_cell" => Some(BlockKind::TableCell),
                _ => None,
            };
            if let Some(kind) = kind {
                blocks.push(Block {
                    kind,
                    text: textblock_text(&codec, node),
                });
                return false;
            }
            if Some(node.type_id()) == raw {
                let text: String = node.children().filter_map(|leaf| leaf.text()).collect();
                if text.trim_start().starts_with('[') {
                    definitions.push(text);
                }
                return false;
            }
            true
        });
        let definitions = definitions.join("\n");
        out.extend(blocks.into_iter().map(|block| (block, definitions.clone())));
    }
    out
}

/// A textblock's text as `derive` reads it: its characters, U+FFFC for each
/// atom and `\n` for each line break.
fn textblock_text(codec: &common::Codec, node: &markraft_core::Node) -> String {
    let line_break = codec.schema.node_id("line_break");
    node.children()
        .map(|child| match child.text() {
            Some(text) => text.to_string(),
            None if Some(child.type_id()) == line_break => "\n".to_string(),
            None => ATOM.to_string(),
        })
        .collect()
}

/// `derived` with every offset moved from the original text into the guarded
/// one, and the guard's own backslashes added as the escapes they read as —
/// which is exactly what deriving the guarded text has to say.
///
/// Inside a code span a backslash is content, so an insertion that lands in
/// one — a line of a multi-line code span that would otherwise open a block —
/// is not an escape; `code` is what deriving the guarded text found. One that
/// lands in a concealed run, such as a link's destination, is part of it.
fn through_guard(
    derived: &Derived,
    guarded: &Guarded,
    code: &[std::ops::Range<usize>],
) -> Vec<String> {
    // A range ends before any backslash inserted at its end.
    let end = |offset: usize| offset + guarded.insertions.partition_point(|&at| at < offset);
    let map = |range: &std::ops::Range<usize>| guarded.to_guarded(range.start)..end(range.end);
    let mut out: Vec<String> = derived
        .styles
        .iter()
        .map(|span| format!("{:?} {:?}", map(&span.range), span.style))
        .collect();
    // An insertion inside a run that is concealed anyway — a line of a link
    // destination — is part of that run.
    let concealed: Vec<_> = derived.conceals.iter().map(|c| map(&c.range)).collect();
    for (index, at) in guarded.insertions.iter().enumerate() {
        let inserted = at + index;
        let absorbed = code
            .iter()
            .chain(&concealed)
            .any(|range| range.contains(&inserted));
        if !absorbed {
            out.push(format!("conceal {:?} \"\"", inserted..inserted + 1));
        }
    }
    for conceal in &derived.conceals {
        out.push(format!(
            "conceal {:?} {:?}",
            map(&conceal.range),
            conceal.display
        ));
    }
    for at in &derived.hard_breaks {
        out.push(format!("break {}", guarded.to_guarded(*at)));
    }
    out.sort();
    out
}

fn summary(derived: &Derived) -> Vec<String> {
    let mut out: Vec<String> = derived
        .styles
        .iter()
        .map(|span| format!("{:?} {:?}", span.range, span.style))
        .collect();
    for conceal in &derived.conceals {
        out.push(format!("conceal {:?} {:?}", conceal.range, conceal.display));
    }
    for at in &derived.hard_breaks {
        out.push(format!("break {at}"));
    }
    out.sort();
    out
}

/// Characters edits are drawn from: every delimiter this reader knows, the
/// characters that start blocks, and a little prose.
const ALPHABET: &[char] = &[
    '*', '*', '_', '~', '`', '[', ']', '(', ')', '<', '>', '\\', '&', ';', '#', '|', '!', ':', '/',
    '"', '-', '+', '=', '=', '^', '$', '$', '1', '.', ' ', ' ', '\n', 'a', 'b', 'x', 'é', '中',
    '，', ATOM,
];

/// `text` after a few random single-character edits.
fn mutate(rng: &mut common::Rng, text: &str) -> String {
    let mut chars: Vec<char> = text.chars().collect();
    for _ in 0..rng.range(1, 4) {
        let at = rng.below(chars.len() + 1);
        match rng.below(3) {
            0 if at < chars.len() => {
                chars.remove(at);
            }
            1 if at < chars.len() => chars[at] = *rng.pick(ALPHABET),
            _ => chars.insert(at, *rng.pick(ALPHABET)),
        }
    }
    chars.into_iter().collect()
}

/// Guarding is a fixed point, and deriving guarded text says what deriving the
/// text said, plus the guard's backslashes as escapes. Together they make the
/// correction that applies both idempotent.
#[test]
fn guard_and_derive_are_idempotent() {
    let mut rng = common::Rng::new(7);
    let mut checked = 0;
    for (block, definitions) in all_textblocks() {
        let ctx = DeriveContext::new().with_definitions(definitions);
        let mut texts = vec![block.text.clone()];
        texts.extend((0..8).map(|_| mutate(&mut rng, &block.text)));
        for text in texts {
            if block.kind == BlockKind::TableCell && text.contains('\n') {
                continue;
            }
            let guarded = guard(block.kind, &text);
            let again = guard(block.kind, &guarded.text);
            assert!(
                again.insertions.is_empty(),
                "{:?} {text:?} guarded to {:?}, which guards again at {:?}",
                block.kind,
                guarded.text,
                again.insertions
            );
            let derived = derive(block.kind, &text, &ctx);
            assert_eq!(
                derived,
                derive(block.kind, &text, &ctx),
                // Its hash maps iterate in a different order each run, so this
                // holds only while nothing it says depends on that order.
                "derive is a function of its input"
            );
            let tree = derive(block.kind, &guarded.text, &ctx);
            // A cell's reader drops the backslash of every `\|` before it reads
            // the cell, code spans included, so there it is always spelling.
            let code: Vec<_> = tree
                .styles
                .iter()
                .filter(|span| span.style == Style::Code && block.kind != BlockKind::TableCell)
                .map(|span| span.range.clone())
                .collect();
            assert_eq!(
                summary(&tree),
                through_guard(&derived, &guarded, &code),
                "{:?} {text:?} guarded to {:?}",
                block.kind,
                guarded.text
            );
            checked += 1;
        }
    }
    assert!(checked > 8_000, "only {checked} texts checked");
}

/// The differential again, over random edits of every textblock: what
/// `derive` says a text means is what comrak renders for the guarded text as a
/// block of its own.
#[test]
fn derive_means_what_comrak_renders_after_random_edits() {
    let mut rng = common::Rng::new(11);
    let mut judged = 0;
    let mut excluded: BTreeMap<&str, usize> = BTreeMap::new();
    let mut failures = Vec::new();
    for (block, _) in all_textblocks() {
        for _ in 0..6 {
            let text = mutate(&mut rng, &block.text);
            let guarded = guard(block.kind, &text);
            let markdown = as_document(block.kind, &guarded.text);
            if let Some(reason) = unjudged(block.kind, &markdown) {
                *excluded.entry(reason).or_default() += 1;
                continue;
            }
            judged += 1;
            let rendered = rendered_meanings(&comrak::markdown_to_html(&markdown, &options()));
            let rendered = rendered.into_iter().next().unwrap_or_default();
            // The text as the tree holds it once the guard has run. How the
            // unguarded text relates to it is the idempotence test's to check.
            let tree = &guarded.text;
            let derived = derived_meaning(tree, &derive(block.kind, tree, &DeriveContext::new()));
            if rendered != derived {
                failures.push(format!(
                    "{:?} {text:?}\n  derive:   {}\n  rendered: {}",
                    block.kind,
                    show(&derived),
                    show(&rendered)
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {judged} edits disagree:\n{}",
        failures.len(),
        failures.join("\n")
    );
    assert!(judged > 4_000, "only {judged} edits judged");
    let excluded_total: usize = excluded.values().sum();
    assert!(excluded_total < judged / 5, "excluded: {excluded:?}");
}

/// A document holding exactly one block of `kind` with `guarded` as its
/// inline source.
fn as_document(kind: BlockKind, guarded: &str) -> String {
    match kind {
        BlockKind::Paragraph => guarded.to_string(),
        BlockKind::Heading if guarded.contains('\n') => format!("{guarded}\n==="),
        BlockKind::Heading => format!("# {guarded}"),
        BlockKind::TableCell => format!("| {guarded} |\n|-|"),
    }
}

/// Why an edited text is not judged, if it is not.
fn unjudged(kind: BlockKind, markdown: &str) -> Option<&'static str> {
    let arena = Arena::new();
    let root = parse_document(&arena, markdown, &options());
    let blocks: Vec<_> = root.children().collect();
    let one = match (kind, blocks.as_slice()) {
        (_, []) => true,
        (BlockKind::Paragraph, [block]) => {
            matches!(block.data.borrow().value, NodeValue::Paragraph)
        }
        (BlockKind::Heading, [block]) => {
            matches!(block.data.borrow().value, NodeValue::Heading(_))
        }
        (BlockKind::TableCell, [block]) => {
            matches!(block.data.borrow().value, NodeValue::Table(_))
                && block.children().count() == 1
                && block
                    .first_child()
                    .is_some_and(|row| row.children().count() == 1)
        }
        _ => false,
    };
    // A blank line, leading indentation, a line ending in a cell: text no
    // backslash can protect. See the guard module. A blank line can hide
    // behind a single block: what follows it may be a link reference
    // definition, which is no block at all.
    let blank_line = markdown.split('\n').any(|line| line.trim().is_empty());
    if !one || (blank_line && !markdown.trim().is_empty()) {
        return Some("unguardable");
    }
    for node in root.descendants() {
        let value = node.data.borrow().value.clone();
        match value {
            // In the tree these are atoms, never text; typed as text they are
            // the parser's to turn into atoms, not derive's to read.
            NodeValue::Image(_) | NodeValue::WikiLink(_) => return Some("atom"),
            NodeValue::HtmlInline(_) if !html_is_read(node) => return Some("atom"),
            // comrak nests an autolink in a link's text, and an HTML reader
            // closes the outer `<a>` at the inner one, so the judge cannot see
            // what comrak meant.
            NodeValue::Link(_) if node.ancestors().skip(1).any(is_link) => {
                return Some("nested link");
            }
            _ => {}
        }
    }
    None
}

fn is_link<'a>(node: &'a AstNode<'a>) -> bool {
    matches!(node.data.borrow().value, NodeValue::Link(_))
}

// -- what the differential leaves out -----------------------------------------

/// A cell's text that holds a line ending — which no table row can — is read a
/// line at a time, each line a cell of its own, and every span keeps an id of
/// its own across the lines.
#[test]
fn a_cells_lines_are_read_one_at_a_time() {
    let derived = derive(BlockKind::TableCell, "**a**\nb *c*", &DeriveContext::new());
    let styles: Vec<_> = derived
        .styles
        .iter()
        .map(|span| (span.range.clone(), span.style.clone()))
        .collect();
    assert_eq!(styles, [(0..5, Style::Strong), (8..11, Style::Emphasis)]);
    let conceals: Vec<_> = derived
        .conceals
        .iter()
        .map(|conceal| (conceal.range.clone(), conceal.span))
        .collect();
    assert_eq!(conceals, [(0..2, 0), (3..5, 0), (8..9, 1), (10..11, 1)]);
}
