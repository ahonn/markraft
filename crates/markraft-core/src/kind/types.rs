//! The node and mark types the view draws, resolved once per schema.
//!
//! The view renders whatever the host's schema declares, so every role is
//! optional: a single-line editor whose schema has only `doc > paragraph` finds
//! none of them and draws plain text. What a missing role costs is written on
//! the field.

use crate::commands::{ColumnAlignment, TableTypes};
use crate::kind::TABLE_ALIGNMENTS_ATTR;
use crate::projection::{Ancestor, Line};
use crate::{Attrs, EditorState, MarkTypeId, Node, NodeTypeId, Schema, kind::DocTypeNames};

/// The conventional schema names of the two roles [`DocTypeNames`] has no
/// entry for.
const RAW_INLINE: &str = "raw_inline";
const WIKI_LINK: &str = "wiki_link";
const EMOJI: &str = "emoji";

/// Which attributes of a block quote make it a callout, and where its header
/// reads from.
///
/// A callout is a block quote carrying a type and a title in attributes — the
/// shape the convention gave it — rather than a node type of its own, so the view
/// cannot find one by node type the way it finds every other role. A host that
/// wants callouts drawn names those two attributes here; one that leaves
/// [`DocTypes::callout`] unset gets ordinary block quotes, whatever its
/// attributes are called.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CalloutAttrs {
    /// The attribute naming the callout's type — `note`, `warning`, anything
    /// at all. A quote whose value here is empty is an ordinary quote.
    pub kind: &'static str,
    /// The attribute holding the title written beside the type. A callout with
    /// no title is headed by its type instead.
    pub title: &'static str,
}

/// The roles the view, its key bindings and its extensions know about, as the
/// ids one schema gives them.
///
/// A host builds this once — with [`DocTypes::from_schema_names`] from a
/// document kind's own table, or field by field — and hands it to the view in
/// the view's setup.
#[derive(Clone, Debug, Default)]
pub struct DocTypes {
    /// The default textblock. Without it a block toggle cannot return to a
    /// plain block, so ⌘⌥0 and the second press of every block binding do
    /// nothing, and literal multi-line text is inserted as one block.
    pub paragraph: Option<NodeTypeId>,
    /// A heading, carrying a `level` attribute. Without it the heading
    /// bindings do nothing and no line is drawn larger.
    pub heading: Option<NodeTypeId>,
    /// A block quote. Without it ⌘⇧B does nothing and no quote bar is drawn.
    pub blockquote: Option<NodeTypeId>,
    /// A footnote definition, carrying a label. Its content is indented under
    /// the label, and clicking the label goes back to the first reference.
    /// Without it a definition is drawn as the blocks it holds.
    pub footnote_definition: Option<NodeTypeId>,
    /// A code block, carrying a `language` attribute. Without it ⌘⌥C does
    /// nothing, no code chrome or highlighting is drawn, and Tab, ⌘A and a
    /// paste behave inside one exactly as they do anywhere else.
    pub code_block: Option<NodeTypeId>,
    /// A bullet list. Without it ⌘⇧8 and ⌘⇧9 do nothing.
    pub bullet_list: Option<NodeTypeId>,
    /// An ordered list. Without it ⌘⇧7 does nothing and no ordinals are drawn.
    pub ordered_list: Option<NodeTypeId>,
    /// A plain list item. Without it Enter, Backspace, Tab and ⇧Tab keep their
    /// plain-block behaviour inside one and no bullet is drawn.
    pub list_item: Option<NodeTypeId>,
    /// A list item with a check box, carrying a `checked` attribute. Without it
    /// ⌘⇧9 and ⌘⏎ do nothing and no box is drawn.
    pub task_item: Option<NodeTypeId>,
    /// A thematic break. Without it nothing draws the rule's line.
    pub horizontal_rule: Option<NodeTypeId>,
    /// A block the codec keeps verbatim — an HTML block, a comment. Source text
    /// kept verbatim as the block's text; edited in place; drawn in the code
    /// font and muted. Without it such a block is drawn as ordinary prose, and
    /// Enter, Tab, ⌘A and a paste behave inside one exactly as they do
    /// anywhere else.
    pub raw_block: Option<NodeTypeId>,
    /// A table, carrying an `alignments` attribute. All three table roles have
    /// to be present for any of them to do anything: without them a table's
    /// cells are drawn as ordinary stacked blocks with no grid, Tab and Enter
    /// keep their plain-block behaviour inside one, and nothing stops a general
    /// edit from leaving one row narrower than the rest.
    pub table: Option<NodeTypeId>,
    /// One row of a table. See [`DocTypes::table`].
    pub table_row: Option<NodeTypeId>,
    /// One cell of a table row, which is a textblock. See [`DocTypes::table`].
    pub table_cell: Option<NodeTypeId>,
    /// A line break. The view finds breaks through the projection, which reads
    /// each type's declared [`BreakKind`](crate::BreakKind), rather
    /// than here, so this names the type for hosts and extensions that insert
    /// one.
    pub hard_break: Option<NodeTypeId>,
    /// An image, carrying `src`, `alt` and `title` attributes. Without it an
    /// image is drawn as a bare object-replacement character, which is blank.
    pub image: Option<NodeTypeId>,
    /// One inline HTML primitive, kept verbatim in a `source` attribute. The
    /// view draws that source as it stands and never renders it. Without it
    /// such an atom is drawn as a bare object-replacement character, which is
    /// blank.
    pub raw_inline: Option<NodeTypeId>,
    /// A wiki link, carrying `target`, `alias` and `embed` attributes. The view
    /// draws its alias, or its target, in the link colour, and a click on one
    /// asks the host to follow it — only the host knows what a target names.
    /// Without it such an atom is drawn as a bare object-replacement character,
    /// which is blank.
    pub wiki_link: Option<NodeTypeId>,
    /// An emoji shortcode, carrying the `code` between its colons. The view
    /// draws the emoji the code names. Without it such an atom is drawn as a
    /// bare object-replacement character, which is blank.
    pub emoji: Option<NodeTypeId>,
    /// Strong emphasis. Without it ⌘B does nothing.
    pub strong: Option<MarkTypeId>,
    /// Emphasis. Without it ⌘I does nothing.
    pub em: Option<MarkTypeId>,
    /// A code span. Without it ⌘E does nothing, no pill is drawn, and an emoji
    /// shortcode is replaced inside one.
    pub code: Option<MarkTypeId>,
    /// Strikethrough. Without it ⌘⇧S does nothing.
    pub strikethrough: Option<MarkTypeId>,
    /// Underline. Present for HTML paste; Markdown write strips it.
    pub underline: Option<MarkTypeId>,
    /// A key or key combination, drawn as a pill in the prose face, as a code
    /// span is in the code face. Without it the keys are drawn as prose.
    pub keyboard: Option<MarkTypeId>,
    /// Highlighted text, drawn over the view's highlight colour.
    pub highlight: Option<MarkTypeId>,
    /// Superscript, drawn smaller and raised in the slot full-size text would
    /// take. Without it the text is drawn on the line like its neighbours.
    pub superscript: Option<MarkTypeId>,
    /// Subscript, drawn smaller and lowered in the slot full-size text would
    /// take. Without it the text is drawn on the line like its neighbours.
    pub subscript: Option<MarkTypeId>,
    /// A formula. Its TeX source is drawn as it stands, in the code font, and
    /// no emoji shortcode is replaced inside one. Without it a formula is drawn
    /// as prose.
    pub math: Option<MarkTypeId>,
    /// A link, carrying an `href` attribute. Without it links cannot be set,
    /// followed or pasted as links.
    pub link: Option<MarkTypeId>,
    /// A reference to a footnote, carrying the label of its definition. Drawn
    /// raised in the link colour; ⌘-click goes to the definition. Without it
    /// a reference is drawn as the text it is.
    pub footnote_reference: Option<MarkTypeId>,
    /// The mark on the characters that spell rather than say, carrying `span`
    /// and `display` (see [`DocTypeNames::syntax`]). Without it such a run is
    /// drawn like any other text, so the spelling stays visible.
    pub syntax: Option<MarkTypeId>,
    /// Which attributes of a [`DocTypes::blockquote`] spell a callout. Unset —
    /// which is what [`DocTypes::from_schema_names`] leaves it, since no role
    /// table names these — no quote is given a header or an accent of its own.
    pub callout: Option<CalloutAttrs>,
}

impl DocTypes {
    /// No role at all: the view draws plain text and every format binding falls
    /// through to the host. This is what a single-line editor runs on.
    pub fn none() -> DocTypes {
        DocTypes::default()
    }

    /// Resolve every name in `names` against `schema`. A name the schema does
    /// not declare leaves its role unset.
    ///
    /// [`DocTypes::raw_inline`], [`DocTypes::wiki_link`] and [`DocTypes::emoji`]
    /// have no entry in
    /// [`DocTypeNames`], so they are
    /// looked up under the names the CommonMark preset gives them. A schema
    /// that spells them differently sets those fields itself; leaving
    /// [`DocTypes::raw_inline`] unset costs the source text an inline primitive
    /// is drawn as.
    ///
    /// [`DocTypes::callout`] is left unset: a role table names node and mark
    /// types, and a callout is spelled in a block quote's *attributes*. A host
    /// that wants them drawn sets that field after this call.
    pub fn from_schema_names(schema: &Schema, names: &DocTypeNames) -> DocTypes {
        let node = |name: Option<&str>| name.and_then(|name| schema.node_id(name));
        let mark = |name: Option<&str>| name.and_then(|name| schema.mark_id(name));
        DocTypes {
            paragraph: node(names.paragraph),
            heading: node(names.heading),
            blockquote: node(names.blockquote),
            footnote_definition: node(names.footnote_definition),
            code_block: node(names.code_block),
            bullet_list: node(names.bullet_list),
            ordered_list: node(names.ordered_list),
            list_item: node(names.list_item),
            task_item: node(names.task_item),
            horizontal_rule: node(names.horizontal_rule),
            raw_block: node(names.raw_block),
            table: node(names.table),
            table_row: node(names.table_row),
            table_cell: node(names.table_cell),
            hard_break: node(names.hard_break),
            image: node(names.image),
            raw_inline: node(Some(RAW_INLINE)),
            wiki_link: node(Some(WIKI_LINK)),
            emoji: node(Some(EMOJI)),
            strong: mark(names.strong),
            em: mark(names.em),
            code: mark(names.code),
            strikethrough: mark(names.strikethrough),
            underline: mark(names.underline),
            keyboard: mark(names.keyboard),
            highlight: mark(names.highlight),
            superscript: mark(names.superscript),
            subscript: mark(names.subscript),
            math: mark(names.math),
            link: mark(names.link),
            footnote_reference: mark(names.footnote_reference),
            syntax: mark(names.syntax),
            callout: None,
        }
    }

    /// Whether `ty` is one of the two list types.
    pub fn is_list(&self, ty: NodeTypeId) -> bool {
        Some(ty) == self.bullet_list || Some(ty) == self.ordered_list
    }

    /// Whether `ty` is one of the two list item types.
    pub fn is_item(&self, ty: NodeTypeId) -> bool {
        Some(ty) == self.list_item || Some(ty) == self.task_item
    }

    /// The heading level of a line's own block, when it is a heading.
    pub fn heading_level(&self, line: &Line) -> Option<u8> {
        let own = line.ancestors().last()?;
        (Some(own.node_type) == self.heading).then(|| {
            own.attrs
                .get("level")
                .and_then(|value| value.as_int())
                .unwrap_or(1)
                .clamp(1, 6) as u8
        })
    }

    /// Whether the line's own block is a code block.
    pub fn is_code_block(&self, line: &Line) -> bool {
        line.node_type()
            .is_some_and(|ty| Some(ty) == self.code_block)
    }

    /// Whether the line's own block is a raw block, whose source is its text
    /// and is drawn as the source it is.
    pub fn is_raw_block(&self, line: &Line) -> bool {
        line.node_type()
            .is_some_and(|ty| Some(ty) == self.raw_block)
    }

    /// Whether the line's own block holds its text verbatim — a code block or a
    /// raw block. The two are drawn differently, but a key pressed inside one
    /// does what it does inside the other: a line ending is a character, Tab is
    /// a tab, and nothing typed is read as markup.
    pub fn is_verbatim_block(&self, line: &Line) -> bool {
        self.is_code_block(line) || self.is_raw_block(line)
    }

    /// Whether `node`, sitting in a textblock of type `parent`, is a `<br>` in
    /// a table cell: the cell's line break, drawn as one,
    /// rather than inline HTML shown as its source.
    pub fn is_cell_break(&self, parent: NodeTypeId, node: &Node) -> bool {
        Some(parent) == self.table_cell
            && Some(node.type_id()) == self.raw_inline
            && node
                .attrs()
                .get("source")
                .and_then(|value| value.as_str())
                .is_some_and(|source| {
                    let tag = source.trim().to_ascii_lowercase();
                    matches!(tag.as_str(), "<br>" | "<br/>" | "<br />")
                })
    }

    /// Whether a block of type `ty` holds its text verbatim, as
    /// [`DocTypes::is_verbatim_block`] asks of a line.
    pub fn is_verbatim(&self, ty: NodeTypeId) -> bool {
        Some(ty) == self.code_block || Some(ty) == self.raw_block
    }

    /// Whether the cursor sits in a verbatim block, for the paths that ask
    /// about the document rather than about a laid-out line.
    pub fn in_verbatim_block_at(&self, state: &EditorState) -> bool {
        state.resolved_head().is_some_and(|resolved| {
            let ty = Some(resolved.parent().type_id());
            ty == self.code_block || ty == self.raw_block
        })
    }

    /// Whether the line is the first block of a ticked task item.
    pub fn in_checked_item(&self, line: &Line) -> bool {
        self.item_of(line).is_some_and(|(item, _)| {
            Some(item.node_type) == self.task_item && DocTypes::task_checked(&item.attrs)
        })
    }

    /// The language attribute of a code block line.
    pub fn code_language<'a>(&self, line: &'a Line) -> Option<&'a str> {
        let own = line.ancestors().last()?;
        (Some(own.node_type) == self.code_block).then(|| {
            own.attrs
                .get("language")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
        })
    }

    /// The index in [`Line::ancestors`] of the innermost list item a line sits
    /// in.
    pub fn item_index(&self, line: &Line) -> Option<usize> {
        line.ancestors()
            .iter()
            .rposition(|ancestor| self.is_item(ancestor.node_type))
    }

    /// The innermost list item ancestor of a line, with the list holding it.
    pub fn item_of<'a>(&self, line: &'a Line) -> Option<(&'a Ancestor, &'a Ancestor)> {
        let index = self.item_index(line)?;
        let list = line.ancestors().get(index.checked_sub(1)?)?;
        self.is_list(list.node_type)
            .then(|| (&line.ancestors()[index], list))
    }

    /// How many list levels a line sits in, counting from zero for a top-level
    /// item. Used for the marker shape, which cycles with depth.
    pub fn list_depth(&self, line: &Line) -> usize {
        line.ancestors()
            .iter()
            .filter(|ancestor| self.is_list(ancestor.node_type))
            .count()
            .saturating_sub(1)
    }

    /// The innermost footnote definition a line sits in.
    pub fn footnote_of<'a>(&self, line: &'a Line) -> Option<&'a Ancestor> {
        let ty = self.footnote_definition?;
        line.ancestors()
            .iter()
            .rev()
            .find(|ancestor| ancestor.node_type == ty)
    }

    /// Whether a line is the first of its footnote definition, where the label
    /// is drawn: every block between the definition and the line is the first
    /// of its parent.
    pub fn starts_footnote(&self, line: &Line) -> bool {
        let Some(ty) = self.footnote_definition else {
            return false;
        };
        let Some(at) = line
            .ancestors()
            .iter()
            .rposition(|ancestor| ancestor.node_type == ty)
        else {
            return false;
        };
        at + 1 < line.ancestors().len()
            && line.ancestors()[at + 1..]
                .iter()
                .all(|ancestor| ancestor.index == 0)
    }

    /// How many block quotes a line sits in.
    pub fn quote_depth(&self, line: &Line) -> usize {
        line.ancestors()
            .iter()
            .filter(|ancestor| Some(ancestor.node_type) == self.blockquote)
            .count()
    }

    /// The three table types, for the catalogue's table commands.
    ///
    /// `None` unless the schema declares all three: every one of those commands
    /// maintains the shape all three describe, so a partial set cannot keep it.
    pub fn table_types(&self) -> Option<TableTypes> {
        Some(TableTypes::new(
            self.table?,
            self.table_row?,
            self.table_cell?,
            TABLE_ALIGNMENTS_ATTR,
        ))
    }

    /// Where a line sits in a table: the position before the table node, which
    /// identifies the grid, and the line's row and column within it.
    ///
    /// The projection gives a cell one line of its own, so this doubles as the
    /// test for "is this line a table cell".
    pub fn table_cell_of(&self, line: &Line) -> Option<(usize, usize, usize)> {
        let cell = line.ancestors().last()?;
        if Some(cell.node_type) != self.table_cell {
            return None;
        }
        let row = line.ancestors().iter().nth_back(1)?;
        let table_index = line.depth().checked_sub(3)?;
        let table = &line.ancestors()[table_index];
        (Some(row.node_type) == self.table_row && Some(table.node_type) == self.table).then_some((
            line.ancestor_before(table_index),
            row.index,
            cell.index,
        ))
    }

    /// Whether a line sits in a table's header row, which is its first row.
    pub fn is_table_header(&self, line: &Line) -> bool {
        matches!(self.table_cell_of(line), Some((_, 0, _)))
    }

    /// The alignments the table holding `line` declares, one per column.
    ///
    /// Read off the projection's own ancestor rather than the document: the
    /// layout pass holds lines, not the tree, and the ancestor carries the
    /// table's attributes verbatim. A missing, short or over-long attribute is
    /// padded and trimmed to `columns`, exactly as
    /// [`column_alignments`](crate::commands::column_alignments) does,
    /// so a caller never has to bounds-check the result.
    pub fn column_alignments(&self, line: &Line, columns: usize) -> Vec<ColumnAlignment> {
        let declared = line
            .ancestors()
            .iter()
            .nth_back(2)
            .filter(|table| Some(table.node_type) == self.table)
            .and_then(|table| table.attrs.get(TABLE_ALIGNMENTS_ATTR))
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let mut alignments: Vec<ColumnAlignment> = declared
            .split(',')
            .filter(|name| !name.is_empty())
            .map(ColumnAlignment::from_name)
            .collect();
        alignments.resize(columns, ColumnAlignment::None);
        alignments
    }

    /// Whether a task item's box is ticked.
    pub fn task_checked(attrs: &Attrs) -> bool {
        attrs
            .get("checked")
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
    }
}
