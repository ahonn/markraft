//! The CommonMark/GFM parse rules: which comrak kind becomes which schema type.

use comrak::nodes::{
    LineColumn, ListDelimType, ListType, NodeCode, NodeCodeBlock, NodeFootnoteDefinition,
    NodeFootnoteReference, NodeHeading, NodeHtmlBlock, NodeLink, NodeList, NodeMath, NodeTable,
    NodeTaskItem, NodeValue, NodeWikiLink, Sourcepos, TableAlignment,
};
use markraft_core::kind::TABLE_ALIGNMENTS_ATTR;
use markraft_core::{Attrs, attrs};

use super::{
    ParseRule, ParseRules, ParseTarget, attrs_fn, fixed, inline_text, no_attrs, text_fn, type_fn,
};
use crate::schema as md;
use crate::table::{Alignment, format_alignments};

/// The rule set for the CommonMark/GFM preset.
///
/// Everything the preset does not name — HTML blocks,
/// and any construct a comrak extension this crate does not enable might
/// produce — falls through to [`ParseRule::Raw`], so it survives as source
/// text rather than being dropped.
pub fn commonmark_rules() -> ParseRules {
    ParseRules::new(md::RAW_BLOCK)
        .with(&NodeValue::FrontMatter(String::new()), ParseRule::Ignore)
        .with(
            &NodeValue::BlockQuote,
            ParseRule::block_with(md::BLOCKQUOTE, attrs_fn(blockquote_attrs)),
        )
        .with(&NodeValue::Paragraph, ParseRule::block(md::PARAGRAPH))
        .with(
            &NodeValue::ThematicBreak,
            ParseRule::block_with(md::HORIZONTAL_RULE, attrs_fn(thematic_break_attrs)),
        )
        .with(
            &NodeValue::Heading(NodeHeading::default()),
            ParseRule::block_with(md::HEADING, attrs_fn(heading_attrs)),
        )
        .with(
            &NodeValue::CodeBlock(Box::<NodeCodeBlock>::default()),
            ParseRule::text_block(
                md::CODE_BLOCK,
                attrs_fn(code_block_attrs),
                text_fn(|target| match &*target.value() {
                    NodeValue::CodeBlock(code) => code
                        .literal
                        .strip_suffix('\n')
                        .unwrap_or(&code.literal)
                        .to_string(),
                    _ => String::new(),
                }),
            ),
        )
        .with(
            &NodeValue::List(NodeList::default()),
            ParseRule::Block {
                node_type: type_fn(list_type),
                attrs: attrs_fn(list_attrs),
            },
        )
        .with(
            &NodeValue::Item(NodeList::default()),
            ParseRule::block(md::LIST_ITEM),
        )
        .with(
            &NodeValue::TaskItem(NodeTaskItem {
                symbol: None,
                symbol_sourcepos: empty_sourcepos(),
            }),
            ParseRule::block_with(md::TASK_ITEM, attrs_fn(task_attrs)),
        )
        .with(
            &NodeValue::Text(std::borrow::Cow::Borrowed("")),
            ParseRule::text(text_fn(|target| match &*target.value() {
                NodeValue::Text(text) => text.to_string(),
                _ => String::new(),
            })),
        )
        .with(
            &NodeValue::SoftBreak,
            ParseRule::atom_with(md::LINE_BREAK, no_attrs()),
        )
        .with(
            &NodeValue::LineBreak,
            ParseRule::Atom {
                node_type: fixed(md::LINE_BREAK),
                attrs: no_attrs(),
            },
        )
        .with(
            &NodeValue::Code(NodeCode::default()),
            ParseRule::Text {
                text: text_fn(|target| match &*target.value() {
                    NodeValue::Code(code) => code.literal.clone(),
                    _ => String::new(),
                }),
                marks: vec![md::CODE.to_string()],
            },
        )
        .with(&NodeValue::Emph, ParseRule::mark(md::EM))
        .with(&NodeValue::Strong, ParseRule::mark(md::STRONG))
        .with(
            &NodeValue::Strikethrough,
            ParseRule::mark(md::STRIKETHROUGH),
        )
        .with(&NodeValue::Underline, ParseRule::mark(md::UNDERLINE))
        .with(&NodeValue::Highlight, ParseRule::mark(md::HIGHLIGHT))
        .with(&NodeValue::Superscript, ParseRule::mark(md::SUPERSCRIPT))
        .with(&NodeValue::Subscript, ParseRule::mark(md::SUBSCRIPT))
        .with(
            &NodeValue::Math(NodeMath::default()),
            ParseRule::Text {
                text: text_fn(|target| match &*target.value() {
                    NodeValue::Math(math) => math.literal.clone(),
                    _ => String::new(),
                }),
                marks: vec![md::MATH.to_string()],
            },
        )
        .with(
            &NodeValue::Link(Box::<NodeLink>::default()),
            ParseRule::mark_with(md::LINK, attrs_fn(link_attrs)),
        )
        .with(
            &NodeValue::Image(Box::<NodeLink>::default()),
            ParseRule::atom_with(md::IMAGE, attrs_fn(image_attrs)),
        )
        .with(
            &NodeValue::WikiLink(NodeWikiLink::default()),
            ParseRule::atom_with(md::WIKI_LINK, attrs_fn(wiki_link_attrs)),
        )
        .with(
            &NodeValue::Table(Box::<NodeTable>::default()),
            ParseRule::block_with(md::TABLE, attrs_fn(table_attrs)),
        )
        // comrak's header flag is not read: GFM has exactly one header row and
        // it is always the first, which is what the schema says too.
        .with(&NodeValue::TableRow(false), ParseRule::block(md::TABLE_ROW))
        .with(&NodeValue::TableCell, ParseRule::block(md::TABLE_CELL))
        .with(
            &NodeValue::FootnoteDefinition(NodeFootnoteDefinition::default()),
            ParseRule::block_with(md::FOOTNOTE_DEFINITION, attrs_fn(footnote_attrs)),
        )
        // Named so a consumer can see the fallback is deliberate for these.
        .with(
            &NodeValue::FootnoteReference(Box::<NodeFootnoteReference>::default()),
            ParseRule::Raw {
                node_type: fixed(md::RAW_BLOCK),
            },
        )
        .with(
            &NodeValue::HtmlBlock(NodeHtmlBlock::default()),
            ParseRule::Raw {
                node_type: fixed(md::RAW_BLOCK),
            },
        )
        .with(
            &NodeValue::HtmlInline(String::new()),
            ParseRule::Raw {
                node_type: fixed(md::RAW_BLOCK),
            },
        )
}

fn empty_sourcepos() -> Sourcepos {
    Sourcepos {
        start: LineColumn::default(),
        end: LineColumn::default(),
    }
}

/// A table's `alignments`, one entry per column, from the delimiter row comrak
/// read.
fn table_attrs(target: ParseTarget<'_>) -> Attrs {
    let NodeValue::Table(table) = &*target.value() else {
        return Attrs::empty();
    };
    let alignments: Vec<Alignment> = table
        .alignments
        .iter()
        .map(|alignment| match alignment {
            TableAlignment::Left => Alignment::Left,
            TableAlignment::Center => Alignment::Center,
            TableAlignment::Right => Alignment::Right,
            TableAlignment::None => Alignment::None,
        })
        .collect();
    attrs! {TABLE_ALIGNMENTS_ATTR => format_alignments(&alignments)}
}

/// A block quote's callout marker, read from the source of its first line.
///
/// The marker has to come from the source and not from the paragraph comrak
/// built: a reader resolves `\[!note]` to the text `[!note]`, and strips the
/// indentation that tells `>  [!x]` apart from `> [!x]`. Everything the
/// recogniser refuses leaves the attributes empty, which is an ordinary quote.
fn blockquote_attrs(target: ParseTarget<'_>) -> Attrs {
    let empty = attrs! {"callout" => "", "fold" => "", "title" => ""};
    let Some(callout) = callout_marker(target) else {
        return empty;
    };
    attrs! {
        "callout" => callout.kind,
        "fold" => callout.fold,
        "title" => callout.title,
    }
}

/// The callout marker a block quote's first line spells, or `None` for an
/// ordinary quote.
pub(crate) fn callout_marker(target: ParseTarget<'_>) -> Option<crate::callout::Callout> {
    let quote = target.sourcepos();
    let line = target.cx.line(quote.start.line);
    // Where the quote's content begins: past the `>` and the one space after it
    // a reader strips. A first line indented further does not open a callout,
    // which is what Obsidian says too.
    let marker = line.as_bytes().get(quote.start.column)?;
    let content = quote.start.column + 1 + usize::from(*marker == b' ');
    let first = target.node.first_child()?;
    let paragraph = first.data.borrow();
    if !matches!(paragraph.value, NodeValue::Paragraph)
        || paragraph.sourcepos.start.line != quote.start.line
        || paragraph.sourcepos.start.column != content
    {
        return None;
    }
    crate::callout::read_callout(line.get(content - 1..)?)
}

fn footnote_attrs(target: ParseTarget<'_>) -> Attrs {
    match &*target.value() {
        NodeValue::FootnoteDefinition(definition) => {
            attrs! {md::FOOTNOTE_LABEL_ATTR => definition.name.as_str()}
        }
        _ => Attrs::empty(),
    }
}

fn heading_attrs(target: ParseTarget<'_>) -> Attrs {
    match &*target.value() {
        NodeValue::Heading(heading) => {
            attrs! {"level" => i64::from(heading.level.clamp(1, 6))}
        }
        _ => Attrs::empty(),
    }
}

fn code_block_attrs(target: ParseTarget<'_>) -> Attrs {
    match &*target.value() {
        NodeValue::CodeBlock(code) => {
            let language = code
                .info
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_string();
            let (fence_char, fence_length) = if code.fenced {
                (
                    char::from(code.fence_char).to_string(),
                    code.fence_length.max(3) as i64,
                )
            } else {
                ("`".to_string(), 3)
            };
            attrs! {
                "language" => language,
                "fence_char" => fence_char,
                "fence_length" => fence_length,
            }
        }
        _ => Attrs::empty(),
    }
}

fn list_type(target: ParseTarget<'_>) -> String {
    let ordered = matches!(
        &*target.value(),
        NodeValue::List(list) if list.list_type == ListType::Ordered
    );
    if ordered {
        md::ORDERED_LIST.to_string()
    } else {
        md::BULLET_LIST.to_string()
    }
}

/// The character a thematic break is written with, so it is written again.
///
/// First in an item of a `-` list, a break of dashes cannot be written — the
/// line would be one break of four dashes — so the writer spells it `***`,
/// and a break there is read as dashes whatever it is spelled with: the two
/// write the same.
fn thematic_break_attrs(target: ParseTarget<'_>) -> Attrs {
    let mut mark = target
        .source()
        .chars()
        .find(|c| matches!(c, '-' | '*' | '_'))
        .unwrap_or('-');
    let first_in_dash_item = target.node.previous_sibling().is_none()
        && target.node.parent().is_some_and(|item| {
            matches!(item.data.borrow().value, NodeValue::Item(_) | NodeValue::TaskItem(_))
                && item.parent().is_some_and(|list| {
                    matches!(&list.data.borrow().value,
                        NodeValue::List(list) if list.list_type == ListType::Bullet && list.bullet_char == b'-')
                })
        });
    if first_in_dash_item && mark == '*' {
        mark = '-';
    }
    attrs! { "mark" => mark.to_string() }
}

fn list_attrs(target: ParseTarget<'_>) -> Attrs {
    let NodeValue::List(list) = &*target.value() else {
        return Attrs::empty();
    };
    if list.list_type == ListType::Ordered {
        let delimiter = match list.delimiter {
            ListDelimType::Paren => ")",
            ListDelimType::Period => ".",
        };
        attrs! {
            "tight" => list.tight,
            "start" => list.start as i64,
            "delimiter" => delimiter,
        }
    } else {
        attrs! {
            "tight" => list.tight,
            "bullet_char" => char::from(list.bullet_char).to_string(),
        }
    }
}

fn task_attrs(target: ParseTarget<'_>) -> Attrs {
    match &*target.value() {
        NodeValue::TaskItem(task) => attrs! {"checked" => task.symbol.is_some()},
        _ => Attrs::empty(),
    }
}

fn link_attrs(target: ParseTarget<'_>) -> Attrs {
    match &*target.value() {
        NodeValue::Link(link) => attrs! {
            "href" => link.url.clone(),
            "title" => link.title.clone(),
        },
        _ => Attrs::empty(),
    }
}

/// A wiki link's parts, read from the source the node covers rather than from
/// what comrak made of it: comrak trims, unescapes and entity-resolves the
/// destination, and the atom has to write back the bytes it took.
///
/// A source this codec does not read as a wiki link never reaches here — the
/// conversion layer keeps it as the text a reader sees — so the empty parts
/// this falls back to stand for a node that cannot occur.
fn wiki_link_attrs(target: ParseTarget<'_>) -> Attrs {
    let source = target.cx.wrapped_source(target.sourcepos());
    let link = crate::wiki::whole_wiki_link(&source).unwrap_or_default();
    attrs! {
        "target" => link.target,
        "alias" => link.alias,
        "embed" => link.embed,
    }
}

fn image_attrs(target: ParseTarget<'_>) -> Attrs {
    let (src, title) = match &*target.value() {
        NodeValue::Image(link) => (link.url.clone(), link.title.clone()),
        _ => (String::new(), String::new()),
    };
    attrs! {
        "src" => src,
        "alt" => inline_text(target.node),
        "title" => title,
    }
}
