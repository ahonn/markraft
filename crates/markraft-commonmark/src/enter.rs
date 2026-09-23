//! Blocks a whole line spells, made when Enter ends it.
//!
//! Two Markdown blocks open with a line that says nothing until it is
//! finished: a fence, whose info string runs to the end of the line, and a
//! table's header row, which only becomes a table once a delimiter row follows
//! it. Neither can be an input rule on a character — ```` ```r ```` may still
//! be becoming ```` ```rust ```` — so [`block_from_line`] makes them when Enter
//! ends the line, the way Typora does. The input rule that opens a fence on a
//! space stays as it is.

use markraft_core::commands::{Command, command};
use markraft_core::{
    Change, EditorState, Fragment, MarkSet, Node, Selection, Slice, TransactionSpec, attrs,
};

use crate::from_markdown;
use crate::schema as md;
use crate::textblock::{Item, Items};

/// Enter at the end of a paragraph whose whole text opens a block: a fence
/// becomes an empty code block in that language, and a row of `|`-separated
/// cells a table with that header and one empty row, the caret in it. Does
/// not apply anywhere else, so Enter carries on as usual.
pub fn block_from_line() -> Command {
    command(|state| {
        let line = Line::at_caret(state)?;
        let block = fence(state, &line.text).or_else(|| table(state, &line.text))?;
        line.replace_with(state, block)
    })
}

/// The paragraph the caret ends, and its text.
struct Line {
    /// The position before the paragraph.
    before: usize,
    /// The position after it.
    after: usize,
    text: String,
}

impl Line {
    fn at_caret(state: &EditorState) -> Option<Line> {
        let doc = state.doc();
        let schema = state.schema();
        if !state.selection().is_cursor() {
            return None;
        }
        let resolved = doc.resolve(state.selection().head(doc)).ok()?;
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
        Some(Line {
            before: resolved.before(depth),
            after: resolved.after(depth),
            text: items.text(),
        })
    }

    /// Put `block` where the paragraph is, with the caret in its first
    /// textblock after `skip` others.
    fn replace_with(
        &self,
        state: &EditorState,
        (block, skip): (Node, usize),
    ) -> Option<TransactionSpec> {
        let spec = TransactionSpec::new().changes(vec![Change::replace(
            self.before,
            self.after,
            Slice::from_fragment(Fragment::from_node(block)),
        )]);
        let applied = state.update([spec]).ok()?;
        let doc = applied.new_doc();
        let schema = state.schema();
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
                .user_event("input")
                .scroll_into_view(),
        )
    }
}

/// An empty code block, when `text` is a fence: three or more backticks or
/// tildes and an info string whose first word is the language. A backtick
/// fence's info string holds no backtick, or the line is inline code.
fn fence(state: &EditorState, text: &str) -> Option<(Node, usize)> {
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
    Some((code, 0))
}

/// A table with `text` as its header row and one empty row, when `text` is a
/// row of cells between pipes: it starts and ends with an unescaped `|`.
fn table(state: &EditorState, text: &str) -> Option<(Node, usize)> {
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
    Some((table.clone(), cells))
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
    use super::header_cells;

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
