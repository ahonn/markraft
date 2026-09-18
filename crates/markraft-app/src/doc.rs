//! The kind of document this application edits, and the vocabulary its user
//! interface names formats with.
//!
//! One CommonMark schema is shared by every note editor, the query field's
//! flattening and the vault's codec, so a document read from disk can be handed
//! to any of them. [`Block`] and [`Inline`] are the application's own names for
//! the formats its toolbar and slash menu offer; each resolves to a command from
//! the editor's catalogue.

use markraft_doc::commands::{Command, command, replace_selection};
use markraft_doc::projection::Projection;
use markraft_doc::{
    Attrs, EditorState, Extension, Fragment, MarkSet, MarkTypeId, Node, NodeTypeId, Schema, Slice,
};
use markraft_markdown::{commonmark_extensions, commonmark_schema, schema as md};
use std::sync::LazyLock;

static SCHEMA: LazyLock<Schema> = LazyLock::new(commonmark_schema);

/// The document kind every note is written in.
pub fn schema() -> &'static Schema {
    &SCHEMA
}

/// The input rules and corrections a CommonMark editor wants.
pub fn extensions() -> Extension {
    commonmark_extensions(schema())
}

/// The smallest document the schema allows: one empty paragraph.
pub fn empty() -> Node {
    let paragraph = schema()
        .node(md::PARAGRAPH, [])
        .expect("an empty paragraph is valid");
    schema()
        .doc([paragraph])
        .expect("one paragraph is a valid document")
}

/// Read a note's body. Nothing is rejected: what the model cannot interpret is
/// kept verbatim, so a file another editor wrote survives a round trip.
pub fn from_markdown(source: &str) -> Node {
    markraft_markdown::from_markdown(schema(), source).unwrap_or_else(|_| empty())
}

pub fn to_markdown(doc: &Node) -> String {
    markraft_markdown::to_markdown(schema(), doc)
}

pub fn plain_text(doc: &Node) -> String {
    markraft_markdown::to_plain_text(schema(), doc)
}

fn node(name: &str) -> NodeTypeId {
    schema()
        .node_id(name)
        .unwrap_or_else(|| panic!("the CommonMark schema declares {name}"))
}

fn mark(name: &str) -> MarkTypeId {
    schema()
        .mark_id(name)
        .unwrap_or_else(|| panic!("the CommonMark schema declares {name}"))
}

/// A block format the user interface offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Block {
    Paragraph,
    Heading(u8),
    Quote,
    Code,
    Ordered,
    Bullet,
    Task,
    Divider,
}

impl Block {
    /// The command the toolbar and the slash menu run for this format.
    pub fn command(self) -> Command {
        let schema = schema();
        match self {
            Block::Paragraph => {
                markraft_gpui::commands::toggle_block(schema, node(md::PARAGRAPH), Attrs::empty())
            }
            Block::Heading(level) => markraft_gpui::commands::toggle_block(
                schema,
                node(md::HEADING),
                Attrs::from_pairs([("level", i64::from(level))]),
            ),
            Block::Code => {
                markraft_gpui::commands::toggle_block(schema, node(md::CODE_BLOCK), Attrs::empty())
            }
            Block::Quote => markraft_gpui::commands::toggle_quote(schema),
            Block::Ordered => markraft_gpui::commands::toggle_list(
                schema,
                node(md::ORDERED_LIST),
                node(md::LIST_ITEM),
            ),
            Block::Bullet => markraft_gpui::commands::toggle_list(
                schema,
                node(md::BULLET_LIST),
                node(md::LIST_ITEM),
            ),
            Block::Task => markraft_gpui::commands::toggle_list(
                schema,
                node(md::BULLET_LIST),
                node(md::TASK_ITEM),
            ),
            // A closed slice of block content splits the textblock around it, so
            // the rule always lands on a line of its own.
            Block::Divider => command(|state| {
                let rule = crate::doc::schema()
                    .node(md::HORIZONTAL_RULE, [])
                    .expect("a horizontal rule takes no content");
                let slice = Slice::from_fragment(Fragment::from_node(rule));
                replace_selection(slice)(state)
            }),
        }
    }

    /// The format of the block at `pos`, or `None` when the position sits in
    /// nothing the interface has a name for.
    fn at(state: &EditorState, projection: &Projection, pos: usize) -> Option<Block> {
        let index = projection.line_at(pos)?;
        let line = projection.line(index)?;
        let own = line.ancestors.last()?;
        if own.node_type == node(md::HORIZONTAL_RULE) {
            return Some(Block::Divider);
        }
        if own.node_type == node(md::CODE_BLOCK) {
            return Some(Block::Code);
        }
        if own.node_type == node(md::HEADING) {
            let level = own
                .attrs
                .get("level")
                .and_then(|value| value.as_int())
                .unwrap_or(1)
                .clamp(1, 6) as u8;
            return Some(Block::Heading(level));
        }
        // The innermost wrapper decides: a paragraph in a quote in a list item is
        // a quote, and one in an item is that item's list.
        for (index, ancestor) in line.ancestors.iter().enumerate().rev() {
            let ty = ancestor.node_type;
            if ty == node(md::BLOCKQUOTE) {
                return Some(Block::Quote);
            }
            if ty == node(md::TASK_ITEM) {
                return Some(Block::Task);
            }
            if ty == node(md::LIST_ITEM) {
                let list = line.ancestors.get(index.checked_sub(1)?)?;
                return Some(if list.node_type == node(md::ORDERED_LIST) {
                    Block::Ordered
                } else {
                    Block::Bullet
                });
            }
        }
        let _ = state;
        Some(Block::Paragraph)
    }

    /// The format every block the selection touches shares, or `None` when they
    /// differ.
    pub fn active(state: &EditorState, projection: &Projection) -> Option<Block> {
        let doc = state.doc();
        let selection = state.selection();
        let (from, to) = (selection.from(doc), selection.to(doc));
        let first = projection.line_at(from)?;
        let last = match projection.line_at(to) {
            // A range that stops at a later block's start leaves that block alone.
            Some(index) if index > first && projection.lines()[index].from == to => index - 1,
            Some(index) => index,
            None => projection.line_count().saturating_sub(1),
        };
        let format = Block::at(state, projection, projection.lines()[first].from)?;
        (first..=last.max(first))
            .all(|index| {
                Block::at(state, projection, projection.lines()[index].from) == Some(format)
            })
            .then_some(format)
    }
}

/// An inline format the user interface offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Inline {
    Bold,
    Italic,
    Code,
    Strikethrough,
    Underline,
}

impl Inline {
    pub fn mark(self) -> MarkTypeId {
        mark(match self {
            Inline::Bold => md::STRONG,
            Inline::Italic => md::EM,
            Inline::Code => md::CODE,
            Inline::Strikethrough => md::STRIKETHROUGH,
            Inline::Underline => md::UNDERLINE,
        })
    }

    pub fn command(self) -> Command {
        markraft_doc::commands::toggle_mark(self.mark(), Attrs::empty())
    }

    pub fn is_active(self, marks: &MarkSet) -> bool {
        marks.contains_type(self.mark())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use markraft_doc::projection::projection_of;
    use markraft_doc::{EditorStateConfig, Selection, TransactionSpec};

    fn state_of(source: &str) -> EditorState {
        EditorState::create(
            EditorStateConfig::new(schema().clone())
                .doc(from_markdown(source))
                .extensions(Extension::all([
                    markraft_doc::projection::projection(),
                    markraft_doc::history(Default::default()),
                    extensions(),
                ])),
        )
        .expect("a valid state")
    }

    fn at(state: &EditorState, line: usize) -> EditorState {
        let projection = projection_of(state);
        let pos = projection.lines()[line].from;
        state
            .update([TransactionSpec::new().selection(Selection::cursor(pos))])
            .expect("a selection")
            .state()
            .clone()
    }

    fn active(state: &EditorState) -> Option<Block> {
        Block::active(state, &projection_of(state))
    }

    #[test]
    fn every_block_format_is_recognised_where_it_nests() {
        for (source, expected) in [
            ("plain", Block::Paragraph),
            ("## head", Block::Heading(2)),
            ("> quoted", Block::Quote),
            ("```\ncode\n```", Block::Code),
            ("- item", Block::Bullet),
            ("1. item", Block::Ordered),
            ("- [ ] task", Block::Task),
            ("***", Block::Divider),
            // The innermost wrapper wins.
            ("- > quoted", Block::Quote),
            ("- - nested", Block::Bullet),
        ] {
            assert_eq!(active(&state_of(source)), Some(expected), "{source}");
        }
    }

    #[test]
    fn a_mixed_selection_has_no_one_format() {
        let state = state_of("# head\n\nplain");
        assert_eq!(active(&at(&state, 0)), Some(Block::Heading(1)));
        assert_eq!(active(&at(&state, 1)), Some(Block::Paragraph));
        let all = state
            .update([TransactionSpec::new().selection(Selection::All)])
            .expect("a selection")
            .state()
            .clone();
        assert_eq!(active(&all), None);
    }

    #[test]
    fn a_block_command_toggles_back_to_a_paragraph() {
        let state = state_of("text");
        let heading = markraft_doc::commands::run_command(&state, &Block::Heading(1).command())
            .expect("the heading applies")
            .expect("a transaction")
            .state()
            .clone();
        assert_eq!(to_markdown(heading.doc()), "# text");
        let back = markraft_doc::commands::run_command(&heading, &Block::Heading(1).command())
            .expect("the heading toggles")
            .expect("a transaction")
            .state()
            .clone();
        assert_eq!(to_markdown(back.doc()), "text");
    }
}
