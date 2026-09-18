//! The editing behaviour that belongs to Markdown rather than to the model.
//!
//! [`commonmark_extensions`] bundles two things:
//!
//! * **Input rules** — the conversions a Markdown writer expects while typing:
//!   `# ` through `###### `, `- `/`* `/`+ `, `1. `, `> `, `---`,
//!   `[ ] `/`[x] ` at the start of a bullet item, and a fence. The fence waits
//!   for the space that ends its info string — ```` ``` ```` opens a code block
//!   with no language and ```` ```rust ```` opens one with `rust` — because the
//!   two cannot be told apart before it.
//! * **Corrections** — a list merge, and the repair that puts a required child
//!   back into an emptied container. Two lists of the same type and attributes
//!   sitting next to each other are one list as far as CommonMark is concerned,
//!   so the model is brought in line rather than left describing something the
//!   source cannot express.
//!
//! Nothing else: key bindings, clipboard handling and the rest belong to a
//! host, which composes them with this.

use markraft_core::commands::structure::markup_of;
use markraft_core::commands::{InputRule, InputRuleMatch, input_rules};
use markraft_core::{
    Attrs, Change, Correction, Extension, Fragment, MarkSet, Markup, Node, NodeTypeId, Schema,
    Slice, Token, attrs, corrections, fill_required_content,
};

use crate::schema as md;

/// Input rules and corrections for the CommonMark preset.
pub fn commonmark_extensions(schema: &Schema) -> Extension {
    Extension::all([
        input_rules(commonmark_input_rules()),
        corrections(commonmark_corrections(schema)),
    ])
}

/// The corrections the preset needs: merge adjacent identical lists, and fill
/// in the block a container's content rule requires — an emptied table gets a
/// row back, and an emptied row a cell, so neither can be left describing
/// something the format cannot write.
pub fn commonmark_corrections(schema: &Schema) -> Vec<Correction> {
    let containers = [
        md::DOC,
        md::BLOCKQUOTE,
        md::LIST_ITEM,
        md::TASK_ITEM,
        md::BULLET_LIST,
        md::ORDERED_LIST,
        md::TABLE,
        md::TABLE_ROW,
    ];
    let mut out = Vec::new();
    for name in containers {
        let Some(ty) = schema.node_id(name) else {
            continue;
        };
        out.push(fill_required_content(ty));
        out.push(Correction::on_child_list(ty, merge_adjacent_lists));
    }
    out
}

/// Join two sibling lists that a reader would read as one.
///
/// Only one pair per round: two merges of neighbouring pairs would overlap, and
/// the correction loop runs to a fixed point anyway.
fn merge_adjacent_lists(cx: &markraft_core::CorrectionContext<'_>) -> Vec<Change> {
    let list_types = [md::BULLET_LIST, md::ORDERED_LIST];
    let schema = cx.start_state.schema();
    let ids: Vec<NodeTypeId> = list_types
        .iter()
        .filter_map(|name| schema.node_id(name))
        .collect();
    let mut pos = cx.content_start;
    let mut previous: Option<&Node> = None;
    for child in cx.node.children() {
        if let Some(previous) = previous
            && ids.contains(&child.type_id())
            && previous.type_id() == child.type_id()
            && previous.attrs() == child.attrs()
        {
            return vec![Change::delete(pos - 1, pos + 1)];
        }
        pos += child.node_size();
        previous = Some(child);
    }
    Vec::new()
}

/// The conversions a Markdown writer expects while typing.
pub fn commonmark_input_rules() -> Vec<InputRule> {
    vec![
        heading_rule(),
        bullet_rule(),
        ordered_rule(),
        quote_rule(),
        code_rule(),
        divider_rule(),
        task_rule(),
    ]
}

/// Replace the block's own open and close tokens and drop the marker text,
/// which is what a block conversion is at the token level.
fn retype_block(m: &InputRuleMatch<'_>, markup: Markup) -> Vec<Change> {
    let before = m.block_start - 1;
    let after = m.block_start + m.block.content_size();
    vec![
        Change::replace(
            before,
            before + 1,
            Slice::from_tokens(&[Token::Open(markup.clone())]),
        ),
        Change::delete(m.from, m.to),
        Change::replace(
            after,
            after + 1,
            Slice::from_tokens(&[Token::Close(markup)]),
        ),
    ]
}

/// Wrap the block in `markups`, outermost first, and drop the marker text.
fn wrap_block(m: &InputRuleMatch<'_>, markups: &[Markup]) -> Vec<Change> {
    let before = m.block_start - 1;
    let after = m.block_start + m.block.content_size() + 1;
    let opens: Vec<Token> = markups.iter().cloned().map(Token::Open).collect();
    let closes: Vec<Token> = markups.iter().rev().cloned().map(Token::Close).collect();
    vec![
        Change::insert(before, Slice::from_tokens(&opens)),
        Change::delete(m.from, m.to),
        Change::insert(after, Slice::from_tokens(&closes)),
    ]
}

fn heading_rule() -> InputRule {
    InputRule::new(
        |before| {
            let hashes = before.chars().take_while(|c| *c == '#').count();
            ((1..=6).contains(&hashes) && before.chars().count() == hashes + 1)
                .then_some(hashes + 1)
                .filter(|_| before.ends_with(' '))
        },
        |m| {
            let level = m.text.chars().take_while(|c| *c == '#').count() as i64;
            let heading = m.schema.node_id(md::HEADING)?;
            let markup = markup_of(m.schema, heading, &attrs! {"level" => level});
            Some(spec(retype_block(m, markup)))
        },
    )
}

fn bullet_rule() -> InputRule {
    InputRule::new(
        |before| matches!(before, "- " | "* " | "+ ").then_some(2),
        |m| {
            let bullet = m.text.get(..1).unwrap_or("-");
            let list = m.schema.node_id(md::BULLET_LIST)?;
            let item = m.schema.node_id(md::LIST_ITEM)?;
            let markups = [
                markup_of(m.schema, list, &attrs! {"bullet_char" => bullet}),
                markup_of(m.schema, item, &Attrs::empty()),
            ];
            Some(spec(wrap_block(m, &markups)))
        },
    )
}

fn ordered_rule() -> InputRule {
    InputRule::new(
        |before| {
            let digits = before.chars().take_while(|c| c.is_ascii_digit()).count();
            let delimited = before[digits..].starts_with(['.', ')']);
            ((1..=9).contains(&digits) && delimited && before.chars().count() == digits + 2)
                .then_some(digits + 2)
                .filter(|_| before.ends_with(' '))
        },
        |m| {
            let digits = m.text.chars().take_while(|c| c.is_ascii_digit()).count();
            let start: i64 = m.text[..digits].parse().ok()?;
            let delimiter = m.text.get(digits..digits + 1).unwrap_or(".");
            let list = m.schema.node_id(md::ORDERED_LIST)?;
            let item = m.schema.node_id(md::LIST_ITEM)?;
            let markups = [
                markup_of(
                    m.schema,
                    list,
                    &attrs! {"start" => start, "delimiter" => delimiter},
                ),
                markup_of(m.schema, item, &Attrs::empty()),
            ];
            Some(spec(wrap_block(m, &markups)))
        },
    )
}

fn quote_rule() -> InputRule {
    InputRule::block_start("> ", |m| {
        let quote = m.schema.node_id(md::BLOCKQUOTE)?;
        let markups = [markup_of(m.schema, quote, &Attrs::empty())];
        Some(spec(wrap_block(m, &markups)))
    })
}

fn code_rule() -> InputRule {
    InputRule::new(
        |before| {
            let ticks = before.chars().take_while(|c| *c == '`').count();
            let info = &before[ticks.min(before.len())..];
            (ticks == 3 && before.ends_with(' ') && !info.contains('`'))
                .then(|| before.chars().count())
        },
        |m| {
            let language = m.text[3..].trim().to_string();
            let code = m.schema.node_id(md::CODE_BLOCK)?;
            let markup = markup_of(m.schema, code, &attrs! {"language" => language});
            Some(spec(retype_block(m, markup)))
        },
    )
}

fn divider_rule() -> InputRule {
    InputRule::block_start("---", |m| {
        let divider = m.schema.node_id(md::HORIZONTAL_RULE)?;
        let paragraph = m.schema.node_id(md::PARAGRAPH)?;
        let before = m.block_start - 1;
        let after = m.block_start + m.block.content_size();
        // The rule replaces the whole block with the break and an empty
        // paragraph to carry on typing in.
        let rule_node = m
            .schema
            .create(divider, Attrs::empty(), MarkSet::empty(), Fragment::empty())
            .ok()?;
        let next = m
            .schema
            .create(
                paragraph,
                Attrs::empty(),
                MarkSet::empty(),
                Fragment::empty(),
            )
            .ok()?;
        Some(spec(vec![Change::replace(
            before,
            after + 1,
            Slice::from_fragment(Fragment::from_nodes([rule_node, next])),
        )]))
    })
}

fn task_rule() -> InputRule {
    InputRule::new(
        |before| matches!(before, "[ ] " | "[x] " | "[X] ").then_some(4),
        |m| {
            let item_type = m.schema.node_id(md::LIST_ITEM)?;
            let task_type = m.schema.node_id(md::TASK_ITEM)?;
            let resolved = m.doc.resolve(m.block_start).ok()?;
            let depth = resolved.depth();
            if depth < 1 || resolved.index(depth - 1) != 0 {
                return None;
            }
            let item = resolved.node(depth - 1);
            if item.type_id() != item_type {
                return None;
            }
            let checked = m.text.contains(['x', 'X']);
            let markup = markup_of(m.schema, task_type, &attrs! {"checked" => checked});
            let before = resolved.before(depth - 1);
            let after = before + item.node_size() - 1;
            Some(spec(vec![
                Change::replace(
                    before,
                    before + 1,
                    Slice::from_tokens(&[Token::Open(markup.clone())]),
                ),
                Change::delete(m.from, m.to),
                Change::replace(
                    after,
                    after + 1,
                    Slice::from_tokens(&[Token::Close(markup)]),
                ),
            ]))
        },
    )
}

fn spec(changes: Vec<Change>) -> markraft_core::TransactionSpec {
    markraft_core::TransactionSpec::new().changes(changes)
}
