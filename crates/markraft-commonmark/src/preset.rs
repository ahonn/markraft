//! The CommonMark/GFM serialiser rules.
//!
//! # Spellings this preset fixes
//!
//! * Headings are always ATX. The tree has no paragraph/underline ambiguity to
//!   preserve, and a setext heading cannot hold more than two levels.
//! * A hard break is a trailing `\`, which survives an editor that strips
//!   trailing whitespace where the two-space spelling does not. A hard break
//!   with nothing after it, or one inside a heading, cannot be expressed in
//!   CommonMark: the first is dropped and the second becomes a space.
//! * A thematic break is `---`, which is what an author writes, except where a
//!   reader would take those three dashes for something else: directly under a
//!   line of text they are that line's setext underline, and after a `-` marker
//!   `- ---` is a run of four dashes and so a thematic break in its own right.
//!   `***` is used in both of those places.
//! * An empty paragraph is a line holding only `<br>`.
//! * A link whose text is its own URL is the bare URL — what an author typed
//!   and what GFM's autolink extension reads back — rather than `[url](url)`
//!   or `<url>`, wherever a reader would still give that link back. Where it
//!   would not, the brackets stay. [`inline_link_mark_rule`] is the rule for a
//!   dialect that has no autolink extension at all.
//! * A code block is always fenced, with a fence longer than any run of the
//!   fence character inside it.
//! * Two lists of the same type in a row would be read as one list, so the
//!   second one takes a different bullet character or ordered delimiter.
//! * A table is a pipe table whose columns are padded to a uniform display
//!   width, so the source lines up in a fixed-width editor. Re-padding on the
//!   way out is a cosmetic change, which is all this codec promises about
//!   spelling. See [`table`].

use std::sync::Arc;

use markraft_core::{Mark, Node, Schema};

use crate::escape::{
    code_span_delimiters, escape_label, escape_pipes, escape_text, flanks, link_destination,
    link_title,
};
use crate::schema as md;
use crate::serialize::{
    MarkRule, MarkRules, MarkStringFn, MarkTarget, MarkdownSerializer, NodeRule, NodeRules,
    SerializerState,
};
use crate::table::{Alignment, alignments_of, cell_width};

fn rule(
    f: impl Fn(&mut SerializerState<'_>, &Node, Option<&Node>, usize) + Send + Sync + 'static,
) -> NodeRule {
    Arc::new(f)
}

fn attr_str<'a>(node: &'a Node, name: &str, default: &'a str) -> &'a str {
    node.attrs()
        .get(name)
        .and_then(|value| value.as_str())
        .unwrap_or(default)
}

fn attr_int(node: &Node, name: &str, default: i64) -> i64 {
    node.attrs()
        .get(name)
        .and_then(|value| value.as_int())
        .unwrap_or(default)
}

/// The concatenated text of a node's children: what a code block holds, and
/// what a raw block holds.
fn text_content(node: &Node) -> String {
    node.children().filter_map(|child| child.text()).collect()
}

/// The CommonMark/GFM node rules, keyed by schema type name.
pub fn commonmark_node_rules() -> NodeRules {
    let mut rules = NodeRules::new();
    rules.insert(
        md::DOC.to_string(),
        rule(|state, node, _, _| state.render_content(node)),
    );
    rules.insert(
        md::PARAGRAPH.to_string(),
        rule(|state, node, parent, _| {
            if node.content_size() == 0 {
                // An empty paragraph has no CommonMark spelling; a `<br>` on a
                // line of its own is an HTML block that every renderer shows as
                // a blank line, and this codec reads back as an empty paragraph.
                //
                // Unless it is all its parent holds: then it *is* the empty
                // container — an empty list item, an empty quote, an empty
                // document — which CommonMark writes as nothing at all, and
                // which the importer fills back in.
                if parent.is_none_or(|parent| parent.child_count() > 1) {
                    state.write("<br>");
                }
            } else {
                state.render_inline(node);
            }
            state.close_block(node);
        }),
    );
    rules.insert(
        md::HEADING.to_string(),
        rule(|state, node, _, _| {
            let level = attr_int(node, "level", 1).clamp(1, 6) as usize;
            let previous = state.set_single_line(true);
            state.write(&format!("{} ", "#".repeat(level)));
            state.render_inline(node);
            escape_trailing_hashes(state);
            state.set_single_line(previous);
            state.close_block(node);
        }),
    );
    rules.insert(
        md::BLOCKQUOTE.to_string(),
        rule(|state, node, _, _| {
            state.wrap_block("> ", None, node, |state| state.render_content(node));
        }),
    );
    rules.insert(md::CODE_BLOCK.to_string(), rule(code_block));
    rules.insert(md::BULLET_LIST.to_string(), rule(bullet_list));
    rules.insert(md::ORDERED_LIST.to_string(), rule(ordered_list));
    let item: NodeRule = rule(|state, node, _, _| state.render_content(node));
    rules.insert(md::LIST_ITEM.to_string(), item.clone());
    rules.insert(md::TASK_ITEM.to_string(), item);
    rules.insert(
        md::HORIZONTAL_RULE.to_string(),
        rule(|state, node, _, _| {
            state.write(thematic_break(state));
            state.close_block(node);
        }),
    );
    rules.insert(
        md::RAW_BLOCK.to_string(),
        rule(|state, node, _, _| {
            // The text is the source, written as it stands: one output line per
            // line of it, each carrying the prefix of the containers it sits
            // in, and a blank line on either side like any other block.
            //
            // A block whose text an edit removed has no source left to write,
            // so it writes nothing at all — not even the separation around it —
            // and the next parse no longer sees a block there.
            let source = text_content(node);
            if source.is_empty() {
                return;
            }
            state.text(&source, false);
            state.close_block(node);
        }),
    );
    rules.insert(md::TABLE.to_string(), rule(table));
    rules.insert(
        md::TEXT.to_string(),
        rule(|state, node, _, _| state.text(node.text().unwrap_or_default(), true)),
    );
    rules.insert(
        md::IMAGE.to_string(),
        rule(|state, node, _, _| {
            let written = format!(
                "![{}]({}{})",
                escape_label(attr_str(node, "alt", "")),
                link_destination(attr_str(node, "src", "")),
                link_title(attr_str(node, "title", "")),
            );
            state.text(&written, false);
        }),
    );
    rules.insert(
        md::RAW_INLINE.to_string(),
        rule(|state, node, _, _| {
            state.text(attr_str(node, "source", ""), false);
        }),
    );
    rules.insert(
        md::INLINE_SPAN.to_string(),
        rule(|state, node, _, _| {
            // HTML tags cannot merge into the neighbouring Markdown delimiter run.
            // Links retain Markdown spelling so an empty label remains a link node.
            let rules = crate::html::commonmark_html_mark_rules();
            let mut closing = Vec::new();
            for mark in node.marks().iter() {
                let name = state.schema().mark_type(mark.ty).name();
                let (open, close) = if name == md::LINK {
                    let value = |key| mark.attrs.get(key).and_then(|v| v.as_str()).unwrap_or("");
                    (
                        "[".to_string(),
                        format!(
                            "]({}{})",
                            link_destination(value("href")),
                            link_title(value("title"))
                        ),
                    )
                } else if let Some(rule) = rules.get(name) {
                    rule(mark)
                } else {
                    continue;
                };
                state.text(&open, false);
                closing.push(close);
            }
            state.render_inline(node);
            for close in closing.iter().rev() {
                state.text(close, false);
            }
        }),
    );
    rules.insert(
        md::SOFT_BREAK.to_string(),
        rule(|state, _, _, _| {
            state.text(if state.is_single_line() { " " } else { "\n" }, false);
        }),
    );
    rules.insert(md::HARD_BREAK.to_string(), rule(hard_break));
    rules
}

/// The spelling of a thematic break that reads as one where it sits.
///
/// `---` is the usual spelling, and the one already in a user's files. Two
/// places need `***` instead:
///
/// * after a marker of the same character — a thematic break is three or more
///   of one character with nothing else on the line, so `- ---` is four dashes
///   rather than an item holding a break;
/// * directly under a line that already has text on it, where `---` is that
///   paragraph's setext underline. Only a tight list writes a block there.
fn thematic_break(state: &SerializerState<'_>) -> &'static str {
    // A block waiting to be separated from this one says what will sit above:
    // one line ending — a tight list — puts the break directly under the line
    // before, where three dashes would underline it instead.
    if state.closed().is_some() {
        return if state.flush_size() > 1 { "---" } else { "***" };
    }
    // Nothing above, so what shares the line is a list marker, if any.
    let out = state.out();
    let marker = out[out.rfind('\n').map_or(0, |index| index + 1)..].trim();
    if !marker.is_empty() && marker.chars().all(|c| c == '-') {
        "***"
    } else {
        "---"
    }
}

fn code_block(state: &mut SerializerState<'_>, node: &Node, _: Option<&Node>, _: usize) {
    let text = text_content(node);
    let language = attr_str(node, "language", "");
    let mut fence_char = attr_str(node, "fence_char", "`")
        .chars()
        .next()
        .unwrap_or('`');
    if fence_char != '~' && language.contains('`') {
        fence_char = '~';
    }
    let longest = text
        .split('\n')
        .map(|line| {
            line.trim_start_matches([' ', '\t'])
                .chars()
                .take_while(|c| *c == fence_char)
                .count()
        })
        .max()
        .unwrap_or(0);
    let width = (attr_int(node, "fence_length", 3).max(3) as usize).max(longest + 1);
    let fence: String = std::iter::repeat_n(fence_char, width).collect();
    state.write(&format!("{fence}{language}"));
    state.ensure_newline();
    if !text.is_empty() {
        // The closing newline is the block's, not the content's: writing it
        // here is what keeps `"a"` and `"a\n"` two different code blocks.
        state.text(&format!("{text}\n"), false);
    }
    state.write(&fence);
    state.close_block(node);
}

/// The narrowest column a delimiter cell still reads well in. GFM needs only
/// one dash, but `---` is what an author writes and what every other writer
/// produces.
const MIN_COLUMN_WIDTH: usize = 3;

/// A GFM pipe table: the header row, the delimiter row that carries the
/// alignments, and the body.
///
/// Every column is padded to one width so the source lines up. The width is a
/// *display* width, so a CJK or emoji cell — two columns per character in a
/// fixed-width font — does not pull the pipes out of line.
fn table(state: &mut SerializerState<'_>, node: &Node, _: Option<&Node>, _: usize) {
    let alignments = alignments_of(node);
    if alignments.is_empty() {
        state.close_block(node);
        return;
    }
    let rows: Vec<Vec<String>> = node
        .children()
        .map(|row| {
            (0..alignments.len())
                .map(|column| match row.maybe_child(column) {
                    // A hard break has no spelling inside a row, which is one
                    // source line; `<br>` is what GFM renders as the break the
                    // author made, and comes back as a raw inline primitive.
                    Some(cell) => escape_pipes(&state.capture_inline(cell, "<br>")),
                    None => String::new(),
                })
                .collect()
        })
        .collect();
    let widths: Vec<usize> = (0..alignments.len())
        .map(|column| {
            rows.iter()
                .filter_map(|row| row.get(column))
                .map(|cell| cell_width(cell))
                .max()
                .unwrap_or(0)
                .max(MIN_COLUMN_WIDTH)
        })
        .collect();
    let mut lines: Vec<String> = Vec::with_capacity(rows.len() + 1);
    let mut rows = rows.into_iter();
    // A table with no rows cannot be built, and one with only a header is a
    // table with no body — both are what the header line below writes.
    lines.push(pipe_row(&rows.next().unwrap_or_default(), &widths));
    lines.push(delimiter_row(&alignments, &widths));
    lines.extend(rows.map(|row| pipe_row(&row, &widths)));
    state.text(&lines.join("\n"), false);
    state.close_block(node);
}

/// `| a   | b |`, each cell padded out to its column's width.
fn pipe_row(cells: &[String], widths: &[usize]) -> String {
    let mut line = String::from("|");
    for (index, width) in widths.iter().enumerate() {
        let cell = cells.get(index).map(String::as_str).unwrap_or_default();
        line.push(' ');
        line.push_str(cell);
        line.push_str(&" ".repeat(width.saturating_sub(cell_width(cell))));
        line.push_str(" |");
    }
    line
}

/// `| --- | :-: |`: the row that separates the header from the body and says
/// how each column is aligned.
fn delimiter_row(alignments: &[Alignment], widths: &[usize]) -> String {
    let mut line = String::from("|");
    for (alignment, width) in alignments.iter().zip(widths) {
        let (left, right) = match alignment {
            Alignment::None => ("", ""),
            Alignment::Left => (":", ""),
            Alignment::Center => (":", ":"),
            Alignment::Right => ("", ":"),
        };
        let dashes = width.saturating_sub(left.len() + right.len()).max(1);
        line.push(' ');
        line.push_str(left);
        line.push_str(&"-".repeat(dashes));
        line.push_str(right);
        line.push_str(" |");
    }
    line
}

fn hard_break(state: &mut SerializerState<'_>, node: &Node, parent: Option<&Node>, index: usize) {
    if state.is_single_line() {
        state.text(state.line_break(), false);
        return;
    }
    // CommonMark has no way to end a block with a line break, so a trailing run
    // of them is dropped rather than written as a stray backslash.
    if let Some(parent) = parent
        && (index + 1..parent.child_count()).all(|i| parent.child(i).type_id() == node.type_id())
    {
        return;
    }
    state.text("\\\n", false);
}

/// The marker character a list uses, changed when the list before it used the
/// same one and a reader would join the two.
fn distinct_marker(
    state: &SerializerState<'_>,
    node: &Node,
    attr: &str,
    default: &str,
    choices: &[&str],
) -> String {
    let current = attr_str(node, attr, default).to_string();
    let joined = state.closed().is_some_and(|previous| {
        previous.type_id() == node.type_id() && attr_str(previous, attr, default) == current
    });
    if !joined {
        return current;
    }
    choices
        .iter()
        .find(|choice| **choice != current)
        .unwrap_or(&default)
        .to_string()
}

/// A list item's marker, with the check box of a task item after it. The box is
/// part of the item's first paragraph, so the continuation indent does not
/// count it.
fn item_marker(node: &Node, index: usize, marker: &str) -> String {
    let Some(child) = node.maybe_child(index) else {
        return marker.to_string();
    };
    let checked = child
        .attrs()
        .get("checked")
        .and_then(|value| value.as_bool());
    match checked {
        Some(true) => format!("{marker}[x] "),
        Some(false) => format!("{marker}[ ] "),
        None => marker.to_string(),
    }
}

fn bullet_list(state: &mut SerializerState<'_>, node: &Node, _: Option<&Node>, _: usize) {
    let bullet = distinct_marker(state, node, "bullet_char", "-", &["-", "*", "+"]);
    let marker = format!("{bullet} ");
    let delim = " ".repeat(marker.chars().count());
    let tight = tight_attr(node) && writable_tight(state, node);
    state.render_list(node, &delim, tight, &|index| {
        item_marker(node, index, &marker)
    });
}

fn ordered_list(state: &mut SerializerState<'_>, node: &Node, _: Option<&Node>, _: usize) {
    let start = attr_int(node, "start", 1).max(0);
    let delimiter = distinct_marker(state, node, "delimiter", ".", &[".", ")"]);
    let last = start + node.child_count().max(1) as i64 - 1;
    let width = last.to_string().len();
    let delim = " ".repeat(width + 2);
    let tight = tight_attr(node) && writable_tight(state, node);
    state.render_list(node, &delim, tight, &|index| {
        let ordinal = (start + index as i64).to_string();
        // Pad on the right, never on the left: a marker that begins with a
        // space would push the enclosing item's content column along with it,
        // and the lines below would no longer line up with this list.
        let marker = format!(
            "{ordinal}{delimiter}{}",
            " ".repeat(1 + width.saturating_sub(ordinal.len()))
        );
        item_marker(node, index, &marker)
    });
}

fn tight_attr(node: &Node) -> bool {
    node.attrs()
        .get("tight")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

/// Whether a list can be written tight, which needs every block inside an item
/// to start a block of its own without a blank line before it.
///
/// A paragraph swallows the line after it unless what follows is a construct
/// that may interrupt a paragraph, so a list whose item holds two paragraphs is
/// written loose however its `tight` attribute reads. The alternative — a blank
/// line inside a list the model calls tight — would not survive being read back.
///
/// A table and a raw block are stricter still, because each runs on until a
/// blank line: the first line after a table that is not blank is read as one
/// more of its rows, and an HTML block ends where a blank line does. Either has
/// to be the last block of its item; what the *next* item's marker starts is a
/// new item, not more of the block. What may come before them is the ordinary
/// rule's business: neither can interrupt a paragraph either.
fn writable_tight(state: &SerializerState<'_>, list: &Node) -> bool {
    list.children().all(|item| {
        let last = item.child_count().saturating_sub(1);
        let mut previous: Option<&Node> = None;
        item.children().enumerate().all(|(index, block)| {
            let ok = (index == last || !runs_until_a_blank_line(state, block))
                && previous.is_none_or(|before| {
                    !is_open_paragraph(state, before) || interrupts_paragraph(state, block)
                });
            previous = Some(block);
            ok
        })
    })
}

/// Whether the block swallows the line after it unless that line is blank.
fn runs_until_a_blank_line(state: &SerializerState<'_>, node: &Node) -> bool {
    let named = |name: &str| state.schema().node_id(name) == Some(node.type_id());
    named(md::TABLE) || named(md::RAW_BLOCK)
}

fn is_open_paragraph(state: &SerializerState<'_>, node: &Node) -> bool {
    state.schema().node_id(md::PARAGRAPH) == Some(node.type_id()) && node.content_size() > 0
}

/// Whether the block's own first line can interrupt the paragraph above it.
fn interrupts_paragraph(state: &SerializerState<'_>, node: &Node) -> bool {
    let named = |name: &str| state.schema().node_id(name) == Some(node.type_id());
    if named(md::HEADING) || named(md::CODE_BLOCK) || named(md::BLOCKQUOTE) {
        return true;
    }
    if named(md::HORIZONTAL_RULE) {
        return true;
    }
    // A list only interrupts a paragraph when its first item has content, and
    // an ordered one also has to start at 1.
    let first_item_has_content = node
        .first_child()
        .is_some_and(|item| item.content_size() > 2);
    if named(md::BULLET_LIST) {
        return first_item_has_content;
    }
    if named(md::ORDERED_LIST) {
        return first_item_has_content && attr_int(node, "start", 1) == 1;
    }
    false
}

/// An ATX heading whose text ends in `#` would lose it to the optional closing
/// sequence, so the run is escaped once it has been written.
fn escape_trailing_hashes(state: &mut SerializerState<'_>) {
    let out = state.out();
    let line = out.rfind('\n').map_or(0, |index| index + 1);
    let tail = &out[line..];
    let hashes = tail.len() - tail.trim_end_matches('#').len();
    if hashes == 0 {
        return;
    }
    let head = &tail[..tail.len() - hashes];
    if !head.ends_with([' ', '\t']) {
        return;
    }
    let at = out.len() - hashes;
    state.out_mut().insert(at, '\\');
}

/// The CommonMark/GFM mark rules, keyed by schema type name.
pub fn commonmark_mark_rules() -> MarkRules {
    let mut rules = MarkRules::new();
    rules.insert(md::LINK.to_string(), autolink_link_rule());
    rules.insert(md::STRONG.to_string(), emphasis_rule("**", '*', "strong"));
    rules.insert(md::EM.to_string(), emphasis_rule("*", '*', "em"));
    rules.insert(
        md::STRIKETHROUGH.to_string(),
        emphasis_rule("~~", '~', "del"),
    );
    let mut underline = MarkRule::fixed("<u>", "</u>");
    underline.lead = Some('<');
    underline.trail = Some('>');
    rules.insert(md::UNDERLINE.to_string(), underline);
    rules.insert(md::CODE.to_string(), code_rule());
    rules
}

/// The link written `[text](href "title")`, whatever its text says.
///
/// [`commonmark_mark_rules`] writes a link whose text is its own URL as the
/// bare URL, because GFM's autolink extension reads that back as the same
/// link. A serialiser for strict CommonMark, where a bare URL is only text,
/// takes this rule instead.
pub fn inline_link_mark_rule() -> MarkRule {
    let close: MarkStringFn = Arc::new(|_, target: &MarkTarget<'_>| closing_brackets(target));
    MarkRule {
        open: Arc::new(|_, _| "[".to_string()),
        close,
        // A link may be opened inside or outside the styling around it —
        // `[**a**](u)` and `**[a](u)**` render the same — and letting the
        // serialiser choose keeps a mark that is already open from closing and
        // reopening across a link boundary.
        mixable: true,
        expel_enclosing_whitespace: false,
        escape: true,
        lead: Some('['),
        trail: Some(')'),
    }
}

/// The link of [`inline_link_mark_rule`], written bare where the URL is its
/// own text and a reader gives the link back from that alone.
fn autolink_link_rule() -> MarkRule {
    let open: MarkStringFn = Arc::new(|state: &mut SerializerState<'_>, target| {
        // Flush the pending block separation first: until it is out, the
        // output still ends with the block before this one, and where the URL
        // lands is half of what decides whether a reader links it back.
        state.write("");
        let bare = writes_bare_url(state, target);
        state.set_tagged(target.mark.ty, bare);
        if bare { String::new() } else { "[".to_string() }
    });
    let close: MarkStringFn = Arc::new(|state: &mut SerializerState<'_>, target| {
        if state.tagged(target.mark.ty) {
            String::new()
        } else {
            closing_brackets(target)
        }
    });
    MarkRule {
        open,
        close,
        ..inline_link_mark_rule()
    }
}

fn mark_str<'a>(mark: &'a Mark, name: &str) -> &'a str {
    mark.attrs
        .get(name)
        .and_then(|value| value.as_str())
        .unwrap_or_default()
}

fn closing_brackets(target: &MarkTarget<'_>) -> String {
    format!(
        "]({}{})",
        link_destination(mark_str(target.mark, "href")),
        link_title(mark_str(target.mark, "title"))
    )
}

/// Whether the link opening here is the one a reader builds from its own text.
///
/// The marked run has to be a single text leaf carrying the link and nothing
/// else: another mark writes its delimiter between the URL and what surrounds
/// it, and a bare URL is only a URL where it sits. What surrounds it is then
/// exactly what the output already holds and what the nodes after it will
/// write, which is what [`crate::autolink::writes_bare`] is asked about.
fn writes_bare_url(state: &SerializerState<'_>, target: &MarkTarget<'_>) -> bool {
    let Some(text) = target
        .parent
        .maybe_child(target.index)
        .filter(|node| node.marks().len() == 1)
        .and_then(|node| node.text())
    else {
        return false;
    };
    // The run ends at this leaf, or the URL is not all of the link's text.
    if target
        .parent
        .maybe_child(target.index + 1)
        .is_some_and(|next| next.marks().contains(target.mark))
    {
        return false;
    }
    let before = (!state.at_line_start())
        .then(|| state.out().chars().next_back())
        .flatten();
    let Some(after) = following_text(state, target.parent, target.index + 1) else {
        return false;
    };
    crate::autolink::writes_bare(
        text,
        mark_str(target.mark, "href"),
        mark_str(target.mark, "title"),
        before,
        &after,
    )
}

/// The text written directly after the marked run, escaped as it will be
/// written and cut at the first whitespace: everything a reader could still
/// pull into a bare URL.
///
/// `None` where what follows is not plain text — an image, a hard break, a
/// marked run — and so cannot be shown to stay out of the URL.
fn following_text(state: &SerializerState<'_>, parent: &Node, from: usize) -> Option<String> {
    let mut out = String::new();
    for index in from..parent.child_count() {
        let child = parent.child(index);
        // A source line ending is whitespace wherever it is written.
        if state.schema().node_type(child.type_id()).name() == md::SOFT_BREAK {
            break;
        }
        let text = child.text().filter(|_| child.marks().is_empty())?;
        out.push_str(&escape_text(text, false));
        if out.contains(char::is_whitespace) {
            break;
        }
    }
    let end = out.find(char::is_whitespace).unwrap_or(out.len());
    out.truncate(end);
    Some(out)
}

fn code_rule() -> MarkRule {
    let text_of = |target: &MarkTarget<'_>| {
        target
            .parent
            .maybe_child(target.index)
            .and_then(|node| node.text())
            .unwrap_or_default()
            .to_string()
    };
    MarkRule {
        open: Arc::new(move |_, target| code_span_delimiters(&text_of(target)).0),
        close: Arc::new(move |_, target| code_span_delimiters(&text_of(target)).1),
        mixable: false,
        expel_enclosing_whitespace: false,
        escape: false,
        lead: Some('`'),
        trail: Some('`'),
    }
}

/// A mark written as a delimiter run where the run can flank, and as an HTML
/// tag where CommonMark would refuse to read the run as emphasis.
fn emphasis_rule(run: &'static str, delimiter: char, tag: &'static str) -> MarkRule {
    MarkRule {
        open: Arc::new(
            move |state: &mut SerializerState<'_>, target: &MarkTarget<'_>| {
                let merges = state.after_mark_close() && state.out().ends_with(delimiter);
                // A simultaneous ** + * opening is parsed as em outside strong.
                // Use a tag when the model requires the opposite nesting.
                let reversed = tag == "strong"
                    && target.parent.maybe_child(target.index).is_some_and(|node| {
                        state
                            .schema()
                            .mark_id(md::EM)
                            .is_some_and(|ty| node.marks().contains_type(ty))
                    });
                let plain = !merges && !reversed && emphasis_flanks(state, target, delimiter);
                state.set_tagged(target.mark.ty, !plain);
                if plain {
                    run.to_string()
                } else {
                    format!("<{tag}>")
                }
            },
        ),
        close: Arc::new(
            move |state: &mut SerializerState<'_>, target: &MarkTarget<'_>| {
                if state.tagged(target.mark.ty) {
                    format!("</{tag}>")
                } else {
                    run.to_string()
                }
            },
        ),
        mixable: true,
        expel_enclosing_whitespace: true,
        escape: true,
        lead: Some(delimiter),
        trail: Some(delimiter),
    }
}

/// Whether a delimiter run around the marked stretch starting at
/// `target.index` would both open and close emphasis where it sits.
fn emphasis_flanks(state: &SerializerState<'_>, target: &MarkTarget<'_>, delimiter: char) -> bool {
    let parent = target.parent;
    let start = target.index;
    let mut end = start;
    while parent
        .maybe_child(end)
        .is_some_and(|child| child.marks().contains(target.mark))
    {
        end += 1;
    }
    if end == start {
        return false;
    }
    // Whitespace at the edges of the run is expelled before the delimiter is
    // written, so what decides the flanking is the first and last node that
    // contributes a character at all.
    let first = (start..end).find_map(|i| {
        parent
            .maybe_child(i)
            .and_then(|n| edge_char(state, n, target.mark, true))
    });
    let last = (start..end).rev().find_map(|i| {
        parent
            .maybe_child(i)
            .and_then(|n| edge_char(state, n, target.mark, false))
    });
    let before = state.char_before_run(delimiter);
    let after = lead_after(state, parent, end, target, delimiter);
    flanks(first, last, before, after, delimiter)
}

/// The first or last character the node contributes to the output, as the
/// escaper would write it.
///
/// A mark written *inside* `outer` puts its own delimiter at the edge instead
/// of the node's text: the run around `` `code` `` touches a backtick, not the
/// `c`.
fn edge_char(
    state: &SerializerState<'_>,
    node: &Node,
    outer: &Mark,
    leading: bool,
) -> Option<char> {
    if let Some(edge) = state.edge_inside(node, outer, leading) {
        return Some(edge);
    }
    match node.text() {
        Some(text) => {
            let trimmed = text.trim_matches([' ', '\t']);
            if leading {
                let first = trimmed.chars().next()?;
                escape_text(&first.to_string(), false).chars().next()
            } else {
                trimmed.chars().next_back()
            }
        }
        // Every inline atom this preset writes starts with `!` and ends with
        // `)`; both are punctuation, which is all the flanking rule asks.
        None => Some(if leading { '!' } else { ')' }),
    }
}

/// The first character written after the marked stretch ends.
fn lead_after(
    state: &SerializerState<'_>,
    parent: &Node,
    end: usize,
    target: &MarkTarget<'_>,
    delimiter: char,
) -> Option<char> {
    let last = parent.maybe_child(end.checked_sub(1)?)?;
    match parent.maybe_child(end) {
        // The next node opens whatever marks it has that this one did not.
        Some(next) => next
            .marks()
            .iter()
            .find(|mark| !last.marks().contains(mark))
            .and_then(|mark| state.mark_lead(mark.ty))
            .or_else(|| edge_char(state, next, target.mark, true)),
        // Nothing follows, so what comes next is the closing delimiter of the
        // mark this one sits inside — skipping the ones spelled with the same
        // character, which merge with this one into a single longer run that a
        // reader gives back as both marks.
        None => last
            .marks()
            .as_slice()
            .iter()
            .rev()
            .skip_while(|mark| *mark != target.mark)
            .skip(1)
            .filter_map(|mark| state.mark_trail(mark.ty))
            .find(|character| *character != delimiter),
    }
}

/// A serialiser for `schema` with the CommonMark/GFM rules.
pub fn commonmark_serializer(schema: &Schema) -> MarkdownSerializer {
    MarkdownSerializer::new(
        schema.clone(),
        commonmark_node_rules(),
        commonmark_mark_rules(),
    )
}
