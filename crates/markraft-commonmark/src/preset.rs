//! The CommonMark/GFM serialiser rules.
//!
//! A paragraph's, a heading's and a table cell's text is written as the inline
//! source it is (see [`crate::serialize`]); everything else follows the rules
//! here.
//!
//! # Spellings this preset fixes
//!
//! * A heading is ATX, or setext when a level 1 or 2 heading holds a line
//!   break, which only a setext heading can. A heading of level 3 or more has
//!   no way to hold one; the canonicalising correction turns it into a space,
//!   and so does the writer.
//! * A thematic break is `---`, which is what an author writes, except where a
//!   reader would take those three dashes for something else: directly under a
//!   line of text they are that line's setext underline, and after a `-` marker
//!   `- ---` is a run of four dashes and so a thematic break in its own right.
//!   `***` is used in both of those places.
//! * An empty paragraph has no CommonMark spelling: consecutive blanks are only
//!   separators, so empty paragraphs write as nothing and may collapse on the
//!   next read. A lone `<br>` HTML block *reads* as an empty paragraph.
//! * A code block is always fenced, with a fence longer than any run of the
//!   fence character inside it.
//! * A table is a pipe table whose columns are padded to a uniform display
//!   width, so the source lines up in a fixed-width editor. Re-padding on the
//!   way out is a cosmetic change, which is all this codec promises about
//!   spelling. See [`table`].
//!
//! # Spelling semantic content
//!
//! The mark rules are for [`spell`](crate::serialize::spell), which writes
//! content that has marks but no spelling — pasted HTML. Text carrying
//! [`SYNTAX`](md::SYNTAX) there is spelling already and goes out as it stands.
//! Otherwise emphasis,
//! strong and strikethrough use `*`, `**` and `~~` — `_` and `__` for the
//! first two under an underscore [`HouseStyle`](crate::HouseStyle), except
//! where a letter or digit borders the run —, underline `<u>`…`</u>`, a
//! hard break a trailing `\`, and a link whose text is its own URL the bare
//! URL wherever a reader gives that link back.

use std::sync::Arc;

use markraft_core::{Mark, Node, Schema};

use crate::escape::{code_span_delimiters, escape_text, link_destination, link_title};
use crate::house::{HardBreak, HouseStyleHandle};
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

fn attr_bool(node: &Node, name: &str, default: bool) -> bool {
    node.attrs()
        .get(name)
        .and_then(|value| value.as_bool())
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

/// The CommonMark/GFM node rules, keyed by schema type name. A hard break is
/// spelled in `house`'s [`HardBreak`] as it is written.
pub fn commonmark_node_rules(house: &HouseStyleHandle) -> NodeRules {
    let mut rules = NodeRules::new();
    rules.insert(
        md::DOC.to_string(),
        rule(|state, node, _, _| state.render_content(node)),
    );
    rules.insert(
        md::PARAGRAPH.to_string(),
        rule(|state, node, parent, index| {
            // Empty paragraphs have no CommonMark spelling. Writing a `<br>` HTML
            // block would be a Markraft-only encoding; leave them blank. A sole
            // empty container (empty list item, empty quote, empty document)
            // already writes as nothing via close_block.
            if node.content_size() > 0 {
                state.render_inline(node);
                state.close_block(node);
                return;
            }
            // One opening an item that goes on — Return at the end of an item's
            // first paragraph, which splits the item — leaves the
            // marker alone on its line and the item's next block on the line
            // after it: an item may open on an empty line, but a marker and a
            // blank line end it.
            let opens_item = index == 0
                && parent.is_some_and(|item| {
                    let name = state.schema().node_type(item.type_id()).name();
                    (name == md::LIST_ITEM || name == md::TASK_ITEM) && item.child_count() > 1
                });
            if opens_item {
                state.write("");
                state.ensure_newline();
                return;
            }
            state.close_block(node);
        }),
    );
    rules.insert(md::HEADING.to_string(), rule(heading));
    rules.insert(md::BLOCKQUOTE.to_string(), rule(blockquote));
    rules.insert(
        md::FOOTNOTE_DEFINITION.to_string(),
        rule(footnote_definition),
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
            state.write(thematic_break(state, node));
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
            state.text(&crate::textblock::image_spelling(node.attrs()), false);
        }),
    );
    rules.insert(
        md::EMOJI.to_string(),
        rule(|state, node, _, _| {
            state.text(
                &crate::shortcode::spelling(attr_str(node, "code", "")),
                false,
            );
        }),
    );
    rules.insert(
        md::RAW_INLINE.to_string(),
        rule(|state, node, _, _| {
            state.text(attr_str(node, "source", ""), false);
        }),
    );
    rules.insert(
        md::WIKI_LINK.to_string(),
        rule(|state, node, _, _| {
            // The attributes hold the source's own bytes, so nothing here
            // escapes or normalises them. A `!` in the text before a link that
            // is not an embed is given up by `SerializerState::text` itself,
            // which would otherwise read the two together as one.
            let link = crate::wiki::WikiLink {
                target: attr_str(node, "target", "").to_string(),
                alias: attr_str(node, "alias", "").to_string(),
                embed: attr_bool(node, "embed", false),
            };
            state.text(&link.source(), false);
        }),
    );
    let break_house = house.clone();
    rules.insert(
        md::LINE_BREAK.to_string(),
        rule(move |state, node, parent, index| {
            hard_break(state, node, parent, index, break_house.get().hard_break)
        }),
    );
    rules
}

/// The spelling of a thematic break that reads as one where it sits.
///
/// A break keeps the character it was written with: `___`
/// reads as a break anywhere, and `***` does except after a `*` list marker,
/// where the line is four stars — a break of its own — and `---` is written.
/// For a break of dashes `---` is the usual spelling, and the one already in
/// a user's files. Two places need another:
///
/// * after a `-` list marker — a thematic break is three or more of one
///   character with nothing else on the line, so `- ---` is four dashes
///   rather than an item holding a break — `***`, which the reader takes back
///   as a break of dashes (see the parse rule);
/// * directly under a line that already has text on it, where `---` is that
///   paragraph's setext underline. Only a tight list and a callout's marker
///   line put a block there. `- - -` is still dashes, and an underline has no
///   spaces in it.
fn thematic_break(state: &SerializerState<'_>, node: &Node) -> &'static str {
    let out = state.out();
    let line = &out[out.rfind('\n').map_or(0, |index| index + 1)..];
    let marker = line.trim();
    match node.attrs().get("mark").and_then(|value| value.as_str()) {
        Some("_") => return "___",
        Some("*") if state.closed().is_some() || !marker.starts_with('*') => return "***",
        Some("*") => return "---",
        _ => {}
    }
    // A block waiting to be separated from this one says what will sit above:
    // one line ending — a tight list — puts the break directly under the line
    // before, where three dashes would underline it instead.
    if state.closed().is_some() {
        return if state.flush_size() > 1 {
            "---"
        } else {
            "- - -"
        };
    }
    // Nothing above, so what shares the line is a list marker, if any.
    if !marker.is_empty() {
        return if marker.chars().all(|c| c == '-') {
            "***"
        } else {
            "---"
        };
    }
    // This line is bare, so the one above is what three dashes would underline.
    // A callout writes its marker one line above its first block rather than a
    // blank line above it, which is the one place a block starts here with text
    // directly over it.
    let above = out[..out.len() - line.len()].trim_end_matches('\n');
    let previous = &above[above.rfind('\n').map_or(0, |index| index + 1)..];
    if out.ends_with('\n') && !previous.trim_matches([' ', '\t', '>']).is_empty() {
        "- - -"
    } else {
        "---"
    }
}

/// A block quote, with its callout marker on the first quoted line when it has
/// one.
///
/// The marker and the body's first line are one source line apart, not a blank
/// line apart, so the separation is written here rather than left to the block
/// flush a paragraph would ask for. A quote whose whole content is the empty
/// paragraph standing for an empty container writes nothing after the marker,
/// so `> [!note]` stays one line and reads back as itself.
///
/// The marker is written unescaped because the codec spells it; text that only
/// *looks* like one at the start of an ordinary quote goes out with a
/// backslash before its `[`, so no edit can turn a quote into a callout behind
/// the user's back.
/// `[^label]: ` before the first line, and the content column four columns in,
/// which is where CommonMark reads a definition's later blocks.
fn footnote_definition(state: &mut SerializerState<'_>, node: &Node, _: Option<&Node>, _: usize) {
    let label = attr_str(node, md::FOOTNOTE_LABEL_ATTR, "");
    // An empty definition ends at its colon, with no space after it.
    let marker = if is_empty_container(state, node) {
        format!("[^{label}]:")
    } else {
        format!("[^{label}]: ")
    };
    state.wrap_block("    ", Some(&marker), node, |state| {
        state.render_content(node)
    });
}

fn blockquote(state: &mut SerializerState<'_>, node: &Node, _: Option<&Node>, _: usize) {
    let callout = crate::callout::Callout {
        kind: attr_str(node, "callout", "").to_string(),
        fold: attr_str(node, "fold", "").to_string(),
        title: attr_str(node, "title", "").to_string(),
    };
    state.wrap_block("> ", None, node, |state| {
        if callout.kind.is_empty() {
            // Only the first line can be a marker; text there that spells one
            // is kept text with a backslash, which the parser puts in the
            // tree too.
            if node
                .first_child()
                .is_some_and(|first| crate::textblock::looks_like_callout(state.schema(), first))
            {
                state.write_prefix("\\");
            }
            state.render_content(node);
            return;
        }
        state.text(&callout.marker(), false);
        if is_empty_container(state, node) {
            return;
        }
        // The marker line is a line of text, so the body may follow it directly
        // only where its first block can interrupt a paragraph; anything else
        // would be read as more of the marker's own line and the callout would
        // come back as something else.
        // A paragraph needs no rule of its own: it *is* the rest of the marker's
        // line, which is how the compact form is written and read back.
        let compact = node.maybe_child(0).is_some_and(|first| {
            state.schema().node_id(md::PARAGRAPH) == Some(first.type_id())
                || interrupts_paragraph(state.schema(), first)
        });
        state.close_block(node);
        state.flush_close(if compact { 1 } else { 2 });
        state.render_content(node);
    });
}

/// Whether a container holds nothing but the empty paragraph that stands for
/// having no content, which writes as nothing at all.
fn is_empty_container(state: &SerializerState<'_>, node: &Node) -> bool {
    node.child_count() == 1
        && node.children().all(|child| {
            child.content_size() == 0
                && state.schema().node_id(md::PARAGRAPH) == Some(child.type_id())
        })
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
                    // A cell is inline source whose pipes the guard has
                    // escaped. It holds no line break; were one to reach
                    // here, `<br>` is what GFM renders as the break.
                    Some(cell) => state.capture_inline(cell, "<br>"),
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

/// A heading: ATX, or setext when a level 1 or 2 heading holds a line break.
fn heading(state: &mut SerializerState<'_>, node: &Node, _: Option<&Node>, _: usize) {
    let level = attr_int(node, "level", 1).clamp(1, 6) as usize;
    let breaks = state
        .schema()
        .node_id(md::LINE_BREAK)
        .is_some_and(|ty| node.children().any(|child| child.type_id() == ty));
    if breaks && level <= 2 {
        let before = state.out().len();
        state.render_inline(node);
        if state.out().len() > before {
            state.text(if level == 1 { "\n===" } else { "\n---" }, false);
            state.close_block(node);
            return;
        }
    }
    let previous = state.set_single_line(true);
    state.write(&format!("{} ", "#".repeat(level)));
    state.render_inline(node);
    state.set_single_line(previous);
    state.close_block(node);
}

/// A hard break in spelled content, spelled as `spelling` says: by default a
/// trailing `\\`, which survives an editor that strips trailing whitespace
/// where the two-space spelling does not.
fn hard_break(
    state: &mut SerializerState<'_>,
    node: &Node,
    parent: Option<&Node>,
    index: usize,
    spelling: HardBreak,
) {
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
    let marker = spelling.marker();
    state.text(&format!("{marker}\n"), false);
}

/// The marker character a list uses, taken from its attributes.
fn list_marker_char(node: &Node, attr: &str, default: &str) -> String {
    attr_str(node, attr, default).to_string()
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
    let bullet = list_marker_char(node, "bullet_char", "-");
    let marker = format!("{bullet} ");
    let delim = " ".repeat(marker.chars().count());
    let tight = written_tight(state.schema(), node);
    state.render_list(node, &delim, tight, &|index| {
        item_marker(node, index, &marker)
    });
}

fn ordered_list(state: &mut SerializerState<'_>, node: &Node, _: Option<&Node>, _: usize) {
    let start = attr_int(node, "start", 1).max(0);
    let delimiter = list_marker_char(node, "delimiter", ".");
    // A list written `1.` `1.` `1.` keeps that spelling.
    let same = node
        .attrs()
        .get("same_ordinal")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let step = i64::from(!same);
    // Pad ordinals so every item shares one content column. Without that, a
    // list that crosses a digit boundary (`9.` → `10.`) under-indents later
    // items' nested blocks and CommonMark reads them as outside the list.
    let last = start + (node.child_count().max(1) as i64 - 1) * step;
    let width = last.to_string().len();
    let delim = " ".repeat(width + delimiter.chars().count() + 1);
    let tight = written_tight(state.schema(), node);
    state.render_list(node, &delim, tight, &|index| {
        let ordinal = (start + index as i64 * step).to_string();
        let marker = format!(
            "{ordinal}{delimiter}{}",
            " ".repeat(1 + width.saturating_sub(ordinal.len()))
        );
        item_marker(node, index, &marker)
    });
}

/// Whether `list` is written tight: what its `tight` attribute asks for, as
/// far as its content lets the writer honour it. See [`writable_tight`].
pub(crate) fn written_tight(schema: &Schema, list: &Node) -> bool {
    tight_attr(list) && writable_tight(schema, list)
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
///
/// An empty paragraph writes as nothing, so it has no say: Backspace in an
/// empty item leaves one in the item before, and the list stays as it was
/// written until something is typed there.
fn writable_tight(schema: &Schema, list: &Node) -> bool {
    let paragraph = schema.node_id(md::PARAGRAPH);
    list.children().all(|item| {
        let last = item.child_count().saturating_sub(1);
        let mut previous: Option<&Node> = None;
        item.children().enumerate().all(|(index, block)| {
            if Some(block.type_id()) == paragraph && block.content_size() == 0 {
                return true;
            }
            let ok = (index == last || !runs_until_a_blank_line(schema, block))
                && previous.is_none_or(|before| {
                    !is_open_paragraph(schema, before) || interrupts_paragraph(schema, block)
                });
            previous = Some(block);
            ok
        })
    })
}

/// Whether the block swallows the line after it unless that line is blank.
fn runs_until_a_blank_line(schema: &Schema, node: &Node) -> bool {
    let named = |name: &str| schema.node_id(name) == Some(node.type_id());
    named(md::TABLE) || named(md::RAW_BLOCK)
}

fn is_open_paragraph(schema: &Schema, node: &Node) -> bool {
    schema.node_id(md::PARAGRAPH) == Some(node.type_id()) && node.content_size() > 0
}

/// Whether the block's own first line can interrupt the paragraph above it.
fn interrupts_paragraph(schema: &Schema, node: &Node) -> bool {
    let named = |name: &str| schema.node_id(name) == Some(node.type_id());
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

/// The CommonMark/GFM mark rules, keyed by schema type name, which
/// [`spell`](crate::serialize::spell) writes semantic content with.
///
/// Emphasis and strong are written in the delimiter `house` holds when the
/// rules are made; the codecs build a serialiser per write, so a style set
/// later is followed by the next write.
pub fn commonmark_mark_rules(house: &HouseStyleHandle) -> MarkRules {
    mark_rules_for(house.get().emphasis)
}

/// [`commonmark_mark_rules`] with emphasis and strong written in `emphasis`.
pub(crate) fn mark_rules_for(emphasis: char) -> MarkRules {
    let mut rules = MarkRules::new();
    rules.insert(md::LINK.to_string(), autolink_link_rule());
    rules.insert(
        md::FOOTNOTE_REFERENCE.to_string(),
        MarkRule::fixed("[^", "]"),
    );
    for spec in crate::styles::STYLES {
        let rule = match (spec.run, spec.underscore_run) {
            (Some(run), Some(underscores)) if emphasis == '_' => underscore_rule(underscores, run),
            (Some(run), _) => {
                let delimiter = run.chars().next().expect("a run has a character");
                emphasis_rule(run, delimiter)
            }
            (None, _) => MarkRule::fixed(spec.tags.0, spec.tags.1),
        };
        rules.insert(spec.mark.to_string(), rule);
    }
    rules.insert(md::CODE.to_string(), code_rule());
    rules.insert(md::MATH.to_string(), math_rule());
    // Text already spelled — a soft break, an empty link's `[](…)` — goes out
    // as it stands.
    rules.insert(
        md::SYNTAX.to_string(),
        MarkRule {
            open: Arc::new(|_, _| String::new()),
            close: Arc::new(|_, _| String::new()),
            mixable: false,
            expel_enclosing_whitespace: false,
            escape: false,
            lead: None,
            trail: None,
        },
    );
    rules
}

/// A mark written as a delimiter run.
fn emphasis_rule(run: &'static str, delimiter: char) -> MarkRule {
    MarkRule {
        open: Arc::new(move |_, _| run.to_string()),
        close: Arc::new(move |_, _| run.to_string()),
        mixable: true,
        expel_enclosing_whitespace: true,
        escape: true,
        lead: Some(delimiter),
        trail: Some(delimiter),
    }
}

/// A mark written as a run of `_`, or as the `*` run `asterisks` where a
/// letter or digit borders it: `_` neither opens after one nor closes before
/// one, so `foo_bar_baz` is plain text where `foo*bar*baz` is not.
///
/// The choice is made when the mark opens, from the character written before
/// it and the one the run will be followed by, and remembered as the mark's
/// alternative spelling so the closing run matches.
fn underscore_rule(run: &'static str, asterisks: &'static str) -> MarkRule {
    let open: MarkStringFn = Arc::new(move |state: &mut SerializerState<'_>, target| {
        let before = (!state.at_line_start())
            .then(|| state.char_before_run('_'))
            .flatten();
        let after = char_after_run(target);
        let intraword = [before, after]
            .into_iter()
            .flatten()
            .any(char::is_alphanumeric);
        state.set_tagged(target.mark.ty, intraword);
        if intraword { asterisks } else { run }.to_string()
    });
    let close: MarkStringFn = Arc::new(move |state: &mut SerializerState<'_>, target| {
        if state.tagged(target.mark.ty) {
            asterisks
        } else {
            run
        }
        .to_string()
    });
    MarkRule {
        open,
        close,
        mixable: true,
        expel_enclosing_whitespace: true,
        escape: true,
        lead: Some('_'),
        trail: Some('_'),
    }
}

/// The character right after the run of `target.mark` that starts at
/// `target.index`, as far as the content shows it: the first character of
/// the text after the run, or the whitespace that leaves the run. `None` at
/// the end of the block or before something that is not text.
fn char_after_run(target: &MarkTarget<'_>) -> Option<char> {
    let parent = target.parent;
    let mut index = target.index;
    while index + 1 < parent.child_count() && parent.child(index + 1).marks().contains(target.mark)
    {
        index += 1;
    }
    // Whitespace ending the run moves out of it, so the run closes before it.
    let last = parent
        .maybe_child(index)?
        .text()
        .and_then(|t| t.chars().next_back());
    if last.is_some_and(char::is_whitespace) {
        return last;
    }
    parent.maybe_child(index + 1)?.text()?.chars().next()
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

/// A formula, written `$…$`, or `$$…$$` for display math. Its content is TeX
/// and goes out as it stands; a formula no fence can hold — one with a `$` in
/// it, or with a space inside an inline fence — does not read back, which the
/// commands that spell one check for.
fn math_rule() -> MarkRule {
    let fence = |target: &MarkTarget<'_>| {
        let display = target
            .mark
            .attrs
            .get(md::MATH_DISPLAY_ATTR)
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        if display { "$$" } else { "$" }.to_string()
    };
    MarkRule {
        open: Arc::new(move |_, target| fence(target)),
        close: Arc::new(move |_, target| fence(target)),
        mixable: false,
        expel_enclosing_whitespace: false,
        escape: false,
        lead: Some('$'),
        trail: Some('$'),
    }
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
/// The marked run has to be a single text leaf carrying the link: another
/// *non-style* construct between the URL and what surrounds it would need
/// brackets. Style marks are fine — their delimiters sit outside the URL. What surrounds it is then
/// exactly what the output already holds and what the nodes after it will
/// write, which is what [`crate::autolink::writes_bare`] is asked about.
fn writes_bare_url(state: &SerializerState<'_>, target: &MarkTarget<'_>) -> bool {
    let Some(node) = target.parent.maybe_child(target.index) else {
        return false;
    };
    if !node.marks().contains(target.mark) {
        return false;
    }
    let Some(text) = node.text() else {
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
    let Some(after) = following_text(target.parent, target.index + 1) else {
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
fn following_text(parent: &Node, from: usize) -> Option<String> {
    let mut out = String::new();
    for index in from..parent.child_count() {
        let child = parent.child(index);
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

/// A serialiser for `schema` with the CommonMark/GFM rules, spelling new
/// syntax in `house`'s style.
pub fn commonmark_serializer(schema: &Schema, house: &HouseStyleHandle) -> MarkdownSerializer {
    MarkdownSerializer::new(
        schema.clone(),
        commonmark_node_rules(house),
        commonmark_mark_rules(house),
    )
}
