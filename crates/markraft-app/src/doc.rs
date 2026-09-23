//! The kind of document this application edits, and the vocabulary its user
//! interface names formats with.
//!
//! One CommonMark schema is shared by every note editor, the query field's
//! flattening and the vault's codec, so a document read from disk can be handed
//! to any of them. [`types`] and [`codecs`] are what the editor view needs of
//! that kind: which types play the roles it draws, and how the clipboard reads
//! and writes it. [`Block`] and [`Inline`] are the application's own names for
//! the formats its toolbar and slash menu offer; each resolves to a command from
//! the editor's catalogue.

use markraft_commonmark::{
    CommandRefusal, CommonMarkCodecs, CommonMarkSpelling, commonmark_doc_type_names,
    commonmark_extensions, commonmark_schema, holds_definitions, schema as md,
};
use markraft_core::commands::{Command, command, replace_selection};
use markraft_core::kind::Codecs;
use markraft_core::projection::{Line, Projection};
use markraft_core::{
    Attrs, EditorState, Extension, Fragment, MarkSet, MarkTypeId, Node, NodeTypeId, Schema, Slice,
};
use markraft_gpui::{CalloutAttrs, DocTypes};
use markraft_gpui::{Formatting, LinkSetter, MarkToggle, SplitWrap};
use std::sync::{Arc, LazyLock};
use unicode_segmentation::UnicodeSegmentation;

static SCHEMA: LazyLock<Schema> = LazyLock::new(commonmark_schema);
static TYPES: LazyLock<DocTypes> = LazyLock::new(|| DocTypes {
    // The preset spells a callout in a block quote's own attributes, so no role
    // table can name them and the view has to be told.
    callout: Some(CalloutAttrs {
        kind: "callout",
        title: "title",
    }),
    ..DocTypes::from_schema_names(schema(), &commonmark_doc_type_names())
});
static CODECS: LazyLock<Arc<dyn Codecs>> =
    LazyLock::new(|| Arc::new(CommonMarkCodecs::new(schema().clone())));
static SPELLING: LazyLock<Arc<dyn markraft_core::kind::SourceSpelling>> =
    LazyLock::new(|| Arc::new(CommonMarkSpelling::new(schema().clone())));

/// The document kind every note is written in.
pub fn schema() -> &'static Schema {
    &SCHEMA
}

/// Which of the schema's types play the roles the editor view draws and binds
/// keys to.
pub fn types() -> &'static DocTypes {
    &TYPES
}

/// How the editor's clipboard reads and writes this document kind.
pub fn codecs() -> Arc<dyn Codecs> {
    CODECS.clone()
}

/// The input rules and corrections a CommonMark editor wants.
pub fn extensions() -> Extension {
    commonmark_extensions(schema())
}

/// How this document kind spells the parts of itself a focused line reveals.
pub fn spelling() -> Arc<dyn markraft_core::kind::SourceSpelling> {
    SPELLING.clone()
}

/// How this document kind toggles an inline mark.
///
/// Markdown keeps the characters that spell a mark in the document, so a toggle
/// edits those rather than the mark alone; the model's own `toggle_mark` would
/// leave the two disagreeing. Where Markdown cannot spell the result, the
/// editor is told why in the words `refusal` gives it.
pub fn mark_toggle(refusal: fn(&CommandRefusal) -> String) -> MarkToggle {
    Arc::new(move |ty, _| worded(markraft_commonmark::toggle_style(ty), refusal))
}

/// How this document kind links and unlinks: by editing the link's source.
pub fn link_setter(refusal: fn(&CommandRefusal) -> String) -> LinkSetter {
    Arc::new(move |_, url| {
        let command = match url {
            Some(url) => markraft_commonmark::set_link(url, ""),
            None => markraft_commonmark::unlink(),
        };
        worded(command, refusal)
    })
}

/// How this document kind splits a block: every style open at the caret is
/// closed before the cut and opened again after it.
pub fn split_wrap() -> SplitWrap {
    Arc::new(markraft_commonmark::keeping_styles)
}

/// What Enter makes of a line that spells a whole block's opening: a fence, a
/// table's header row.
pub fn enter_rule() -> Command {
    markraft_commonmark::block_from_line()
}

/// A kind's formatting command, its refusal put in the application's words.
fn worded(
    command: markraft_commonmark::FormatCommand,
    refusal: fn(&CommandRefusal) -> String,
) -> Formatting {
    Arc::new(move |state| command(state).map_err(|error| refusal(&error)))
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

/// Construct semantic test fixtures. File loading uses SourceDocument instead.
#[cfg(test)]
pub fn from_markdown(source: &str) -> Node {
    markraft_commonmark::from_markdown(schema(), source).unwrap_or_else(|_| empty())
}

pub fn to_markdown(doc: &Node) -> String {
    markraft_commonmark::to_markdown(schema(), doc)
}

pub fn plain_text(doc: &Node) -> String {
    markraft_commonmark::to_plain_text(schema(), doc)
}

/// How many characters — or, with `words`, words — `doc` holds as a reader
/// sees it, laid out as `projection` lays it out.
///
/// Each line counts what it reads as ([`Codecs::to_text`]), not the Markdown
/// it holds: `**bold**` is four characters, `&amp;` one. A table puts each of
/// its cells on a line of its own, and those breaks are the grid rather than
/// anything anyone typed, so they are not characters — they still part words,
/// as the break between two blocks does. Link reference definitions are
/// where links go rather than anything read, so their blocks count nothing.
pub fn count(doc: &Node, projection: &Projection, words: bool) -> usize {
    let codecs = codecs();
    let units = |text: &str| {
        if words {
            text.unicode_words().count()
        } else {
            text.graphemes(true).count()
        }
    };
    let mut total = 0;
    let mut previous: Option<Option<usize>> = None;
    for line in projection.lines() {
        // Reference definitions are where links go, not text a reader sees,
        // and no break stands for the block they fill.
        if line.block_before().is_some_and(|block| {
            doc.node_at(block)
                .is_some_and(|node| holds_definitions(schema(), &node))
        }) {
            continue;
        }
        let text = doc
            .slice(line.from(), line.to())
            .map(|slice| codecs.to_text(&slice))
            .unwrap_or_default();
        total += units(&text);
        let table = table_of(line);
        // The break this line opened with, unless it fell between two cells of
        // one table, or the count is of words, which no break adds to.
        if !words && previous.is_some_and(|before| table.is_none() || before != table) {
            total += 1;
        }
        previous = Some(table);
    }
    total
}

/// The line a note is named after: the first one that reads as text.
///
/// A block the model keeps verbatim — an HTML block, a table — contributes what
/// a reader would see in it rather than its markup, so a note opening with
/// `<div class="note">` is not named after the tag. A note with nothing to read
/// has no title line.
pub fn title_line(doc: &Node) -> Option<String> {
    doc.children().find_map(title_of)
}

fn title_of(block: &Node) -> Option<String> {
    let text = if schema().node_type(block.type_id()).name() == md::RAW_BLOCK {
        strip_tags(&plain_text(block))
    } else {
        plain_text(block)
    };
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(one_line)
}

/// A title is one line of prose, and it also names the note's file, so the tabs a
/// table's cells are laid out with — and any other control character the text carries —
/// read as a single space rather than travelling into a window title or a file name.
fn one_line(line: &str) -> String {
    let mut title = String::with_capacity(line.len());
    for character in line.chars() {
        if character.is_control() {
            if !title.ends_with(' ') {
                title.push(' ');
            }
        } else {
            title.push(character);
        }
    }
    title.trim_end().to_owned()
}

/// The table a projected line sits in, named by where that table starts in the
/// document, or `None` for a line outside one. A table lays its cells out one to a
/// line, so this is what tells a break between two cells from a break between blocks.
pub fn table_of(line: &Line) -> Option<usize> {
    let table = node(md::TABLE);
    line.ancestors()
        .iter()
        .position(|ancestor| ancestor.node_type == table)
        .map(|index| line.ancestor_before(index))
}

/// The readable text of raw markup: everything outside `<…>`. Nothing is
/// rendered, so an entity stays as it was written.
fn strip_tags(source: &str) -> String {
    let mut text = String::with_capacity(source.len());
    let mut depth = 0usize;
    for character in source.chars() {
        match character {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => text.push(character),
            _ => {}
        }
    }
    text
}

/// Only unformatted, whitespace-only paragraphs are disposable blank notes.
/// Atoms and other block structures carry content even without readable text.
pub fn is_blank(doc: &Node) -> bool {
    doc.children().all(|block| {
        schema().node_type(block.type_id()).name() == md::PARAGRAPH
            && block.marks().is_empty()
            && block.children().all(|child| {
                child.is_text()
                    && child.marks().is_empty()
                    && child.text().is_some_and(|text| text.trim().is_empty())
            })
    })
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
    /// An Obsidian callout, which is a quote carrying a type. The interface
    /// offers the default `note`; changing an existing one's type, fold or
    /// title is not something v1 does.
    Callout,
    Code,
    Ordered,
    Bullet,
    Task,
    Divider,
}

impl Block {
    /// The command the toolbar and the slash menu run for this format.
    pub fn command(self) -> Command {
        let types = types();
        match self {
            Block::Paragraph => {
                markraft_gpui::commands::toggle_block(types, node(md::PARAGRAPH), Attrs::empty())
            }
            Block::Heading(level) => markraft_gpui::commands::toggle_block(
                types,
                node(md::HEADING),
                Attrs::from_pairs([("level", i64::from(level))]),
            ),
            Block::Code => {
                markraft_gpui::commands::toggle_block(types, node(md::CODE_BLOCK), Attrs::empty())
            }
            Block::Quote => markraft_gpui::commands::toggle_quote(types),
            Block::Callout => markraft_gpui::commands::toggle_wrap(
                node(md::BLOCKQUOTE),
                Attrs::from_pairs([("callout", "note"), ("fold", ""), ("title", "")]),
            ),
            Block::Ordered => markraft_gpui::commands::toggle_list(
                types,
                node(md::ORDERED_LIST),
                node(md::LIST_ITEM),
            ),
            Block::Bullet => markraft_gpui::commands::toggle_list(
                types,
                node(md::BULLET_LIST),
                node(md::LIST_ITEM),
            ),
            Block::Task => markraft_gpui::commands::toggle_list(
                types,
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
        let own = line.ancestors().last()?;
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
        for (index, ancestor) in line.ancestors().iter().enumerate().rev() {
            let ty = ancestor.node_type;
            if ty == node(md::BLOCKQUOTE) {
                // A callout is a quote with a type on it, and it is the kind
                // the toolbar shows so that asking for a plain quote takes the
                // marker off rather than doing nothing.
                let callout = ancestor
                    .attrs
                    .get("callout")
                    .and_then(|value| value.as_str())
                    .is_some_and(|kind| !kind.is_empty());
                return Some(if callout {
                    Block::Callout
                } else {
                    Block::Quote
                });
            }
            if ty == node(md::TASK_ITEM) {
                return Some(Block::Task);
            }
            if ty == node(md::LIST_ITEM) {
                let list = line.ancestors().get(index.checked_sub(1)?)?;
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
            Some(index) if index > first && projection.lines()[index].from() == to => index - 1,
            Some(index) => index,
            None => projection.line_count().saturating_sub(1),
        };
        let format = Block::at(state, projection, projection.lines()[first].from())?;
        (first..=last.max(first))
            .all(|index| {
                Block::at(state, projection, projection.lines()[index].from()) == Some(format)
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
    /// Present when HTML paste carries underline; Markdown write drops it.
    #[allow(dead_code)]
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

    pub fn is_active(self, marks: &MarkSet) -> bool {
        marks.contains_type(self.mark())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use markraft_core::projection::projection_of;
    use markraft_core::{EditorStateConfig, Selection, TransactionSpec};

    fn state_of(source: &str) -> EditorState {
        EditorState::create(
            EditorStateConfig::new(schema().clone())
                .doc(from_markdown(source))
                .extensions(Extension::all([
                    markraft_core::projection::projection(),
                    markraft_core::history::history(Default::default()),
                    extensions(),
                ])),
        )
        .expect("a valid state")
    }

    fn at(state: &EditorState, line: usize) -> EditorState {
        let projection = projection_of(state);
        let pos = projection.lines()[line].from();
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
            ("> [!note]\n> callout", Block::Callout),
            ("- > quoted", Block::Quote),
            ("- - nested", Block::Bullet),
        ] {
            assert_eq!(active(&state_of(source)), Some(expected), "{source}");
        }
    }

    #[test]
    fn a_title_line_reads_through_markup_and_gives_up_on_a_note_with_no_text() {
        for (source, expected) in [
            ("# Heading\n\nbody", Some("Heading")),
            ("  spaced  \n\nbody", Some("spaced")),
            // Raw HTML names the note after what it renders, not after its tags.
            ("<div class=\"card\">\n\nBody text\n", Some("Body text")),
            ("<p>Inline text</p>", Some("Inline text")),
            ("<hr/>", None),
            ("***", None),
            // A table's cells are laid out with tabs between them; a title is one line
            // of prose, and it also names a file, so no control character survives it.
            ("| a | b |\n| - | - |\n| c | d |", Some("a b")),
        ] {
            assert_eq!(
                title_line(&from_markdown(source)).as_deref(),
                expected,
                "{source}"
            );
        }
        assert_eq!(title_line(&empty()), None);
    }

    /// What the footer's character count leans on: a table lays each cell out on a line
    /// of its own, and every one of those lines names the same table, so the breaks
    /// between them can be told from the break between two blocks.
    #[test]
    fn the_cells_of_one_table_share_a_line_of_their_own_and_name_it() {
        let state = state_of("intro\n\n| a | b |\n| - | - |\n| c | d |\n\nafter");
        let projection = projection_of(&state);
        let tables: Vec<_> = projection.lines().iter().map(table_of).collect();
        assert_eq!(projection.line_count(), 6, "one line for each of the cells");
        assert_eq!(tables[0], None);
        assert_eq!(tables[5], None);
        let table = tables[1].expect("the first cell sits in a table");
        assert!(
            tables[1..5].iter().all(|line| *line == Some(table)),
            "every cell names the one table: {tables:?}"
        );
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
        for level in 1..=6 {
            let state = state_of("text");
            let heading =
                markraft_core::commands::run_command(&state, &Block::Heading(level).command())
                    .expect("the heading applies")
                    .expect("a transaction")
                    .state()
                    .clone();
            assert_eq!(
                to_markdown(heading.doc()),
                format!("{} text", "#".repeat(usize::from(level)))
            );
            assert_eq!(active(&heading), Some(Block::Heading(level)));
            let back =
                markraft_core::commands::run_command(&heading, &Block::Heading(level).command())
                    .expect("the heading toggles")
                    .expect("a transaction")
                    .state()
                    .clone();
            assert_eq!(to_markdown(back.doc()), "text");
        }
    }

    #[test]
    fn the_callout_command_converts_a_quote_and_takes_the_marker_off_again() {
        let run = |state: &EditorState, block: Block| {
            markraft_core::commands::run_command(state, &block.command())
                .expect("the command applies")
                .expect("a transaction")
                .state()
                .clone()
        };
        // From plain text: one quote, carrying the default type.
        let callout = run(&state_of("text"), Block::Callout);
        assert_eq!(to_markdown(callout.doc()), "> [!note]\n> text");
        assert_eq!(active(&callout), Some(Block::Callout));
        // Asking again lifts it, the way every other block command toggles.
        assert_eq!(to_markdown(run(&callout, Block::Callout).doc()), "text");
        // An existing quote is retyped rather than wrapped a second time, and
        // the quote command takes the marker off it.
        let quote = run(&state_of("text"), Block::Quote);
        let converted = run(&quote, Block::Callout);
        assert_eq!(to_markdown(converted.doc()), "> [!note]\n> text");
        assert_eq!(to_markdown(run(&converted, Block::Quote).doc()), "> text");
    }

    /// The footer counts what a reader sees: delimiters, escapes and an
    /// entity's spelling are no characters, and a line break inside a block is
    /// one, as the break between two blocks is.
    #[test]
    fn a_note_counts_the_characters_a_reader_sees() {
        let count_of = |source: &str, words: bool| {
            let state = state_of(source);
            count(state.doc(), &projection_of(&state), words)
        };
        assert_eq!(count_of("**bold** and *em*", false), "bold and em".len());
        assert_eq!(
            count_of(r"a \*b\* &amp; [c](https://x.y)", false),
            "a *b* & c".len()
        );
        assert_eq!(count_of("one\n\n`two`", false), "one\ntwo".len());
        assert_eq!(count_of("a\\\nb", false), "a\nb".len());
        // A table's cells part words, but add no characters of their own.
        assert_eq!(count_of("| a | **b** |\n| - | - |", false), 2);
        assert_eq!(count_of("**bold** words, *here*", true), 3);
        // Reference definitions are no text, and add no break of their own.
        let referenced = "a [b][r]\n\n[r]: https://x.y\n\nc";
        assert_eq!(count_of(referenced, false), "a b\nc".len());
        assert_eq!(count_of(referenced, true), 3);
        assert_eq!(count_of("[r]: https://x.y", false), 0);
    }
}
