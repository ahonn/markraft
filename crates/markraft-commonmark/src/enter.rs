//! Blocks a whole line spells, made when Enter ends it.
//!
//! Two Markdown blocks open with a line that says nothing until it is
//! finished: a fence, whose info string runs to the end of the line, and a
//! table's header row, which only becomes a table once a delimiter row follows
//! it. Neither can be an input rule on a character — ```` ```r ```` may still
//! be becoming ```` ```rust ```` — so [`block_from_line`] makes them when Enter
//! ends the line, the way Typora does. The input rule that opens a fence on a
//! space stays as it is.
//!
//! A thematic break of stars or underscores is the same: `***` may still be
//! opening `***bold italic***`, so only Enter makes it a divider. Three dashes
//! have an input rule of their own.
//!
//! A footnote definition, too, ends on Enter the way Typora ends it: at the
//! end of its last paragraph, the next paragraph goes after the definition
//! rather than into it.
//!
//! A block a line makes goes where the line was, unless the container holding
//! the line needs a paragraph first. A task item is one: its check box can
//! only start a paragraph, a heading or a quote, so a fence ended on the check
//! box line leaves that line an empty paragraph — `- [ ] ` on a line of its own —
//! and puts the code block under it, still inside the item. The item stays a
//! task, the block is where it was typed, and `- [ ] ` followed by an indented
//! fence reads back as exactly that.

use markraft_core::commands::structure::can_replace_with;
use markraft_core::commands::{Command, command};
use markraft_core::{
    Attrs, Change, EditorState, Fragment, MarkSet, Node, Selection, Slice, TransactionSpec, attrs,
};

use crate::from_markdown;
use crate::schema as md;
use crate::textblock::{Item, Items};
use markraft_core::protocol::event;

/// Enter at the end of a paragraph whose whole text opens a block: a fence
/// becomes an empty code block in that language, and a row of `|`-separated
/// cells a table with that header and one empty row, the caret in it. Does
/// not apply anywhere else, so Enter carries on as usual. A thematic break
/// becomes a divider with an empty paragraph after it to carry on typing in.
pub fn block_from_line() -> Command {
    command(|state| {
        if let Some(spec) = out_of_footnote(state) {
            return Some(spec);
        }
        let line = Line::at_caret(state)?;
        let block = fence(state, &line.text)
            .or_else(|| table(state, &line.text))
            .or_else(|| divider(state, &line.text))?;
        line.replace_with(state, block)
    })
}

/// An empty paragraph after the footnote definition whose last paragraph the
/// caret ends, the caret in it. Not for an empty paragraph, which is a
/// definition's own, nor anywhere but the end of the last one.
fn out_of_footnote(state: &EditorState) -> Option<TransactionSpec> {
    let schema = state.schema();
    if !state.selection().is_cursor() {
        return None;
    }
    let resolved = state.resolved_head()?;
    let depth = resolved.depth().checked_sub(1)?;
    let paragraph = resolved.parent();
    let definition = resolved.node(depth);
    if paragraph.type_id() != schema.node_id(md::PARAGRAPH)?
        || definition.type_id() != schema.node_id(md::FOOTNOTE_DEFINITION)?
        || paragraph.content_size() == 0
        || resolved.parent_offset() != paragraph.content_size()
        || resolved.index(depth) + 1 != definition.child_count()
    {
        return None;
    }
    let at = resolved.after(depth);
    let empty = schema
        .create(
            schema.node_id(md::PARAGRAPH)?,
            Attrs::empty(),
            MarkSet::empty(),
            Fragment::empty(),
        )
        .ok()?;
    let spec = TransactionSpec::new().changes(vec![Change::replace(
        at,
        at,
        Slice::from_fragment(Fragment::from_node(empty)),
    )]);
    let applied = state.update([spec]).ok()?;
    let selection = Selection::cursor(at + 1);
    selection.check(applied.new_doc(), schema).ok()?;
    Some(
        TransactionSpec::new()
            .change_set(applied.changes().clone())
            .selection(selection)
            .user_event(event::INPUT)
            .scroll_into_view(),
    )
}

/// The paragraph the caret ends, and its text.
struct Line {
    /// The position before the paragraph.
    before: usize,
    /// The position after it.
    after: usize,
    text: String,
    /// The node holding the paragraph, and the paragraph's index in it.
    parent: Node,
    index: usize,
    /// The paragraph itself, emptied: what stays when its container needs a
    /// paragraph before the new block.
    emptied: Node,
}

impl Line {
    fn at_caret(state: &EditorState) -> Option<Line> {
        let schema = state.schema();
        if !state.selection().is_cursor() {
            return None;
        }
        let resolved = state.resolved_head()?;
        let depth = resolved.depth();
        let paragraph = resolved.node(depth);
        if paragraph.type_id() != schema.node_id(md::PARAGRAPH)?
            || resolved.parent_offset() != paragraph.content_size()
        {
            return None;
        }
        let items = Items::from_nodes(schema, paragraph.children());
        // A line of its own: no atom, no line break.
        if items.0.iter().any(|item| !matches!(item, Item::Char(_))) {
            return None;
        }
        let container = depth.checked_sub(1)?;
        Some(Line {
            before: resolved.before(depth),
            after: resolved.after(depth),
            text: items.text(),
            parent: resolved.node(container).clone(),
            index: resolved.index(container),
            emptied: paragraph.copy(Fragment::empty()),
        })
    }

    /// Put `blocks` where the paragraph is, with the caret in their first
    /// textblock after `skip` others.
    ///
    /// Where the paragraph's container cannot hold `blocks` in its place — a
    /// task item, whose check box cannot be written before a code block — the
    /// paragraph stays, emptied, with `blocks` after it. Where
    /// even that does not fit, the line is left as it is.
    fn replace_with(
        &self,
        state: &EditorState,
        (blocks, skip): (Fragment, usize),
    ) -> Option<TransactionSpec> {
        let schema = state.schema();
        let fits = |blocks: &Fragment| {
            let types: Vec<_> = blocks.iter().map(Node::type_id).collect();
            can_replace_with(schema, &self.parent, self.index, self.index + 1, &types)
        };
        let (blocks, skip) = if fits(&blocks) {
            (blocks, skip)
        } else {
            let kept = Fragment::from_nodes(
                std::iter::once(self.emptied.clone()).chain(blocks.iter().cloned()),
            );
            if !fits(&kept) {
                return None;
            }
            (kept, skip + 1)
        };
        let spec = TransactionSpec::new().changes(vec![Change::replace(
            self.before,
            self.after,
            Slice::from_fragment(blocks),
        )]);
        let applied = state.update([spec]).ok()?;
        let doc = applied.new_doc();
        let mut caret = None;
        let mut seen = 0;
        doc.nodes_between(self.before, doc.content_size(), &mut |node, pos, _, _| {
            if caret.is_some() {
                return false;
            }
            if node.is_textblock(schema) {
                if seen == skip {
                    caret = Some(pos + 1);
                }
                seen += 1;
                return false;
            }
            true
        });
        let selection = Selection::cursor(caret?);
        selection.check(doc, schema).ok()?;
        Some(
            TransactionSpec::new()
                .change_set(applied.changes().clone())
                .selection(selection)
                .user_event(event::INPUT)
                .scroll_into_view(),
        )
    }
}

/// An empty code block, when `text` is a fence: three or more backticks or
/// tildes and an info string whose first word is the language. A backtick
/// fence's info string holds no backtick, or the line is inline code.
fn fence(state: &EditorState, text: &str) -> Option<(Fragment, usize)> {
    let text = text.trim_end_matches([' ', '\t']);
    let fence_char = text.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let length = text.chars().take_while(|c| *c == fence_char).count();
    let info = text[length..].trim();
    if length < 3 || (fence_char == '`' && info.contains('`')) {
        return None;
    }
    let language = info.split_whitespace().next().unwrap_or_default();
    let schema = state.schema();
    let code = schema
        .create(
            schema.node_id(md::CODE_BLOCK)?,
            attrs! {
                "language" => language,
                "fence_char" => fence_char.to_string(),
                "fence_length" => length as i64,
            },
            MarkSet::empty(),
            Fragment::empty(),
        )
        .ok()?;
    Some((Fragment::from_node(code), 0))
}

/// A table with `text` as its header row and one empty row, when `text` is a
/// row of cells between pipes: it starts and ends with an unescaped `|`.
fn table(state: &EditorState, text: &str) -> Option<(Fragment, usize)> {
    let cells = header_cells(text)?;
    let row = |cell: &str| format!("|{}", format!(" {cell} |").repeat(cells));
    let source = format!("{text}\n{}\n{}\n", row("---"), row(""));
    let doc = from_markdown(state.schema(), &source).ok()?;
    let table = doc.maybe_child(0).filter(|_| doc.child_count() == 1)?;
    let header = table.maybe_child(0)?;
    if table.type_id() != state.schema().node_id(md::TABLE)? || header.child_count() != cells {
        return None;
    }
    // The caret goes to the first cell of the empty row, after the header's.
    Some((Fragment::from_node(table.clone()), cells))
}

/// A divider and an empty paragraph after it, when `text` is a thematic break:
/// three or more of one of `*`, `_` or `-`, with nothing else but spaces.
fn divider(state: &EditorState, text: &str) -> Option<(Fragment, usize)> {
    if !is_thematic_break(text) {
        return None;
    }
    let schema = state.schema();
    let create = |name| {
        schema
            .create(
                schema.node_id(name)?,
                Attrs::empty(),
                MarkSet::empty(),
                Fragment::empty(),
            )
            .ok()
    };
    let mark = text.chars().find(|c| !matches!(c, ' ' | '\t'))?;
    let divider = schema
        .create(
            schema.node_id(md::HORIZONTAL_RULE)?,
            attrs! { "mark" => mark.to_string() },
            MarkSet::empty(),
            Fragment::empty(),
        )
        .ok()?;
    let paragraph = create(md::PARAGRAPH)?;
    Some((Fragment::from_nodes([divider, paragraph]), 0))
}

fn is_thematic_break(text: &str) -> bool {
    let mut marks = text.chars().filter(|c| !matches!(c, ' ' | '\t'));
    let Some(first) = marks.next().filter(|c| matches!(c, '*' | '_' | '-')) else {
        return false;
    };
    let mut count = 1;
    for mark in marks {
        if mark != first {
            return false;
        }
        count += 1;
    }
    count >= 3
}

/// How many cells a header row `|a|b|` has, or `None` when `text` is not one.
fn header_cells(text: &str) -> Option<usize> {
    let text = text.trim_end_matches([' ', '\t']);
    let inner = text.strip_prefix('|')?.strip_suffix('|')?;
    if inner.ends_with('\\') && !inner.ends_with("\\\\") {
        return None;
    }
    let mut cells = 1;
    let mut backslashes = 0;
    for character in inner.chars() {
        if character == '|' && backslashes % 2 == 0 {
            cells += 1;
        }
        backslashes = if character == '\\' {
            backslashes + 1
        } else {
            0
        };
    }
    Some(cells)
}

#[cfg(test)]
mod tests {
    use super::{header_cells, is_thematic_break};

    #[test]
    fn a_thematic_break_is_three_of_one_mark() {
        for text in ["***", "___", "---", "* * *", "_____", "- - -"] {
            assert!(is_thematic_break(text), "{text:?}");
        }
        for text in ["**", "*-*", "**a", "", "==="] {
            assert!(!is_thematic_break(text), "{text:?}");
        }
    }

    #[test]
    fn a_header_row_is_cells_between_pipes() {
        assert_eq!(header_cells("|a|"), Some(1));
        assert_eq!(header_cells("|a|b|"), Some(2));
        assert_eq!(header_cells("| a | b | "), Some(2));
        assert_eq!(header_cells(r"|a\|b|"), Some(1));
        assert_eq!(header_cells("|a|b"), None);
        assert_eq!(header_cells("a|b|"), None);
        assert_eq!(header_cells(r"|a\|"), None);
    }
}
