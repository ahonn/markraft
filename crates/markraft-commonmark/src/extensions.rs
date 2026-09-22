//! The editing behaviour that belongs to Markdown rather than to the model.
//!
//! [`commonmark_extensions`] bundles two things:
//!
//! * **Input rules** — the conversions a Markdown writer expects while typing:
//!   `# ` through `###### `, `- `/`* `/`+ `, `1. `, `> `, `---`,
//!   `[ ] `/`[x] ` at the start of a bullet item, and a fence. The fence waits
//!   for the space that ends its info string — ```` ``` ```` opens a code block
//!   with no language and ```` ```rust ```` opens one with `rust` — because the
//!   two cannot be told apart before it. A typed `[[Note]]` becomes the wiki
//!   link atom on its closing `]]`, because text spelling one is not one and a
//!   source-preserving save would read the two back as different documents.
//!   `[!note] ` at the start of a block quote turns it into a callout, for the
//!   same reason and on the same space a check box waits for.
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
use markraft_core::projection::OBJECT_REPLACEMENT;
use markraft_core::{
    Attrs, Change, Correction, Extension, Fragment, Mark, MarkSet, Markup, Node, NodeTypeId,
    Schema, Selection, Slice, Token, attrs, corrections, fill_required_content,
};

use crate::schema as md;

/// Input rules and corrections for the CommonMark preset.
pub fn commonmark_extensions(schema: &Schema) -> Extension {
    Extension::all([
        input_rules(commonmark_input_rules()),
        corrections(commonmark_corrections(schema)),
    ])
}

/// The corrections the preset needs: merge adjacent identical lists, fill in
/// the block a container's content rule requires, and re-derive Method-B
/// delimiter leaves after inline edits.
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
    out.extend(crate::normalize::method_b_normalize_corrections(schema));
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
        wiki_link_rule(),
        callout_rule(),
        // Longest delimiter first: `**bold**` must not be read as emphasis
        // around `*bold*`.
        inline_style_rule("~~", md::STRIKETHROUGH),
        inline_style_rule("**", md::STRONG),
        inline_style_rule("*", md::EM),
        inline_style_rule("`", md::CODE),
    ]
}

/// `**bold**`, `*em*`, `~~struck~~` and `` `code` `` become the mark and its
/// Method-B delimiter leaves as soon as the closing run is typed.
///
/// A rule rather than a reparse of the block: the tree cannot tell a delimiter
/// a writer just typed from one that was written `\*` in the source, so the only
/// characters safe to read as spelling are the ones the caret has just closed.
fn inline_style_rule(delim: &'static str, mark_name: &'static str) -> InputRule {
    InputRule::new(
        move |before| {
            let head = before.strip_suffix(delim)?;
            let start = head.rfind(delim)?;
            let content = &head[start + delim.len()..];
            if content.is_empty() || content.contains(delim) {
                return None;
            }
            // A longer run of the same character is a longer delimiter still
            // being typed: `**bold*` is on its way to strong, not emphasis
            // around `bold`.
            let repeated = delim.chars().next()?;
            if head[..start].ends_with(repeated)
                || content.starts_with(repeated)
                || content.ends_with(repeated)
            {
                return None;
            }
            // CommonMark will not read a run that flanks whitespace as emphasis,
            // and neither does the writer who typed it.
            if content.starts_with(char::is_whitespace) || content.ends_with(char::is_whitespace) {
                return None;
            }
            Some(before[start..].chars().count())
        },
        move |m| {
            let ty = m.schema.mark_id(mark_name)?;
            let code = m.schema.mark_id(md::CODE);
            let width = delim.chars().count();
            let (inner_from, inner_to) = (m.from + width, m.to - width);
            if inner_from >= inner_to {
                return None;
            }
            // Inside a code span every character is literal, and an atom is one
            // token that no delimiter may be pushed into.
            let in_code = code.is_some_and(|code| {
                m.doc
                    .resolve(m.from)
                    .is_ok_and(|resolved| resolved.marks(m.schema).contains_type(code))
            });
            if in_code || m.text.contains(OBJECT_REPLACEMENT) {
                return None;
            }
            // A code span holds no other marks, so one that would swallow some
            // is left as the characters the writer typed.
            if Some(ty) == code
                && m.doc
                    .slice(inner_from, inner_to)
                    .is_ok_and(|slice| slice.content().iter().any(|node| !node.marks().is_empty()))
            {
                return None;
            }
            let mark = Mark::new(ty);
            let leaf = |text: &str| -> Option<Slice> {
                let leaf = crate::inline::syntax_text(m.schema, text).ok()?;
                let leaf = leaf.mark(leaf.marks().add(m.schema, mark.clone()));
                Some(Slice::from_fragment(Fragment::from_node(leaf)))
            };
            Some(
                spec(vec![
                    Change::replace(m.from, inner_from, leaf(delim)?),
                    Change::add_mark(inner_from, inner_to, mark.clone()),
                    Change::replace(inner_to, m.to, leaf(delim)?),
                ])
                // The caret stands past the closing delimiter, where the style is
                // over: what is typed next is ordinary text.
                .selection(Selection::cursor_with_marks(m.to, MarkSet::empty())),
            )
        },
    )
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

/// `[!note] ` at the start of a block quote turns it into a callout.
///
/// The space is what completes the marker, the way it completes a check box:
/// before it, `[!note]-` may still be growing a fold marker. Only an ordinary
/// quote converts, and only from its own first block, which is where a reader
/// looks for the marker too. A title is not typed here — it is not text once
/// the quote is a callout, and v1 has no way to edit one.
fn callout_rule() -> InputRule {
    InputRule::new(
        |before| {
            let marker = before.strip_suffix(' ')?;
            crate::callout::read_callout(marker)
                .filter(|callout| callout.title.is_empty())
                .map(|_| before.chars().count())
        },
        |m| {
            let quote_type = m.schema.node_id(md::BLOCKQUOTE)?;
            let callout = crate::callout::read_callout(m.text.trim_end_matches(' '))?;
            let resolved = m.doc.resolve(m.block_start).ok()?;
            let depth = resolved.depth();
            // The marker opens the quote, so it sits in its first block.
            if depth < 1 || resolved.index(depth - 1) != 0 {
                return None;
            }
            let quote = resolved.node(depth - 1);
            if quote.type_id() != quote_type {
                return None;
            }
            // A quote that is already a callout has its marker in hand.
            let named = quote
                .attrs()
                .get("callout")
                .and_then(|value| value.as_str())
                .is_some_and(|kind| !kind.is_empty());
            if named {
                return None;
            }
            let in_code = m.schema.mark_id(md::CODE).is_some_and(|code| {
                m.doc
                    .resolve(m.from)
                    .is_ok_and(|resolved| resolved.marks(m.schema).contains_type(code))
            });
            if in_code {
                return None;
            }
            let markup = markup_of(
                m.schema,
                quote_type,
                &attrs! {
                    "callout" => callout.kind,
                    "fold" => callout.fold,
                    "title" => "",
                },
            );
            let before = resolved.before(depth - 1);
            let after = before + quote.node_size() - 1;
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

/// `[[Note]]` becomes the atom as soon as the closing `]]` is typed.
///
/// It has to: text spelling a wiki link is not a wiki link, and a save would
/// read the two back as different documents and refuse the edit. A code span
/// keeps what is typed in it literal, and so does a code block, which the rule
/// runner excludes already.
fn wiki_link_rule() -> InputRule {
    InputRule::new(
        |before| {
            if !before.ends_with("]]") {
                return None;
            }
            let mut start = before.rfind("[[")?;
            // An embed's `!` belongs to the link it opens.
            if before[..start].ends_with('!') {
                start -= 1;
            }
            let (_, len) = crate::wiki::read_wiki_link(&before[start..])?;
            (start + len == before.len()).then(|| before[start..].chars().count())
        },
        |m| {
            let ty = m.schema.node_id(md::WIKI_LINK)?;
            let (link, _) = crate::wiki::read_wiki_link(m.text)?;
            // An atom stands for one token, so what the rule replaces has to be
            // text and nothing else.
            if m.text
                .contains(markraft_core::projection::OBJECT_REPLACEMENT)
            {
                return None;
            }
            let in_code = m.schema.mark_id(md::CODE).is_some_and(|code| {
                m.doc
                    .resolve(m.from)
                    .is_ok_and(|resolved| resolved.marks(m.schema).contains_type(code))
            });
            if in_code {
                return None;
            }
            let node = m
                .schema
                .create(
                    ty,
                    attrs! {
                        "target" => link.target,
                        "alias" => link.alias,
                        "embed" => link.embed,
                    },
                    MarkSet::empty(),
                    Fragment::empty(),
                )
                .ok()?;
            Some(spec(vec![Change::replace(
                m.from,
                m.to,
                Slice::from_fragment(Fragment::from_node(node)),
            )]))
        },
    )
}

fn spec(changes: Vec<Change>) -> markraft_core::TransactionSpec {
    markraft_core::TransactionSpec::new().changes(changes)
}
