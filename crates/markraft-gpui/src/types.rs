//! The node and mark types the view draws, resolved once per schema.
//!
//! The view renders whatever the host's schema declares, so every role is
//! optional: a single-line editor whose schema has only `doc > paragraph` finds
//! none of them and draws plain text. What a missing role costs is written on
//! the field.

use markraft_core::projection::{Ancestor, Line};
use markraft_core::{Attrs, DocTypeNames, MarkTypeId, NodeTypeId, Schema};

/// The roles the view, its key bindings and its extensions know about, as the
/// ids one schema gives them.
///
/// A host builds this once — with [`DocTypes::from_schema_names`] from a
/// document kind's own table, or field by field — and hands it to the view in
/// [`Setup`](crate::Setup).
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
    /// Source text kept verbatim. Without it such a block is drawn as ordinary
    /// text rather than in the monospaced style that marks it as unparsed.
    pub raw_block: Option<NodeTypeId>,
    /// A hard line break. The view finds breaks through the projection's
    /// `line_break` group rather than here, so this names the type for hosts
    /// and extensions that insert one.
    pub hard_break: Option<NodeTypeId>,
    /// An image. The view draws every inline atom the same way, so this names
    /// the type for hosts and extensions that insert one.
    pub image: Option<NodeTypeId>,
    /// Strong emphasis. Without it ⌘B does nothing.
    pub strong: Option<MarkTypeId>,
    /// Emphasis. Without it ⌘I does nothing.
    pub em: Option<MarkTypeId>,
    /// A code span. Without it ⌘E does nothing, no pill is drawn, and an emoji
    /// shortcode is replaced inside one.
    pub code: Option<MarkTypeId>,
    /// Strikethrough. Without it ⌘⇧S does nothing.
    pub strikethrough: Option<MarkTypeId>,
    /// Underline. Without it ⌘U does nothing.
    pub underline: Option<MarkTypeId>,
    /// A link, carrying an `href` attribute. Without it links cannot be set,
    /// followed or pasted as links.
    pub link: Option<MarkTypeId>,
}

impl DocTypes {
    /// No role at all: the view draws plain text and every format binding falls
    /// through to the host. This is what a single-line editor runs on.
    pub fn none() -> DocTypes {
        DocTypes::default()
    }

    /// Resolve every name in `names` against `schema`. A name the schema does
    /// not declare leaves its role unset.
    pub fn from_schema_names(schema: &Schema, names: &DocTypeNames) -> DocTypes {
        let node = |name: Option<&str>| name.and_then(|name| schema.node_id(name));
        let mark = |name: Option<&str>| name.and_then(|name| schema.mark_id(name));
        DocTypes {
            paragraph: node(names.paragraph),
            heading: node(names.heading),
            blockquote: node(names.blockquote),
            code_block: node(names.code_block),
            bullet_list: node(names.bullet_list),
            ordered_list: node(names.ordered_list),
            list_item: node(names.list_item),
            task_item: node(names.task_item),
            horizontal_rule: node(names.horizontal_rule),
            raw_block: node(names.raw_block),
            hard_break: node(names.hard_break),
            image: node(names.image),
            strong: mark(names.strong),
            em: mark(names.em),
            code: mark(names.code),
            strikethrough: mark(names.strikethrough),
            underline: mark(names.underline),
            link: mark(names.link),
        }
    }

    /// Whether `ty` is one of the two list types.
    pub(crate) fn is_list(&self, ty: NodeTypeId) -> bool {
        Some(ty) == self.bullet_list || Some(ty) == self.ordered_list
    }

    /// Whether `ty` is one of the two list item types.
    pub(crate) fn is_item(&self, ty: NodeTypeId) -> bool {
        Some(ty) == self.list_item || Some(ty) == self.task_item
    }

    /// The heading level of a line's own block, when it is a heading.
    pub(crate) fn heading_level(&self, line: &Line) -> Option<u8> {
        let own = line.ancestors.last()?;
        (Some(own.node_type) == self.heading).then(|| {
            own.attrs
                .get("level")
                .and_then(|value| value.as_int())
                .unwrap_or(1)
                .clamp(1, 6) as u8
        })
    }

    /// Whether the line's own block is a code block.
    pub(crate) fn is_code_block(&self, line: &Line) -> bool {
        line.node_type()
            .is_some_and(|ty| Some(ty) == self.code_block)
    }

    /// The language attribute of a code block line.
    pub(crate) fn code_language<'a>(&self, line: &'a Line) -> Option<&'a str> {
        let own = line.ancestors.last()?;
        (Some(own.node_type) == self.code_block).then(|| {
            own.attrs
                .get("language")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
        })
    }

    /// The innermost list item ancestor of a line, with the list holding it.
    pub(crate) fn item_of<'a>(&self, line: &'a Line) -> Option<(&'a Ancestor, &'a Ancestor)> {
        let index = line
            .ancestors
            .iter()
            .rposition(|ancestor| self.is_item(ancestor.node_type))?;
        let list = line.ancestors.get(index.checked_sub(1)?)?;
        self.is_list(list.node_type)
            .then(|| (&line.ancestors[index], list))
    }

    /// How many list levels a line sits in, counting from zero for a top-level
    /// item. Used for the marker shape, which cycles with depth.
    pub(crate) fn list_depth(&self, line: &Line) -> usize {
        line.ancestors
            .iter()
            .filter(|ancestor| self.is_list(ancestor.node_type))
            .count()
            .saturating_sub(1)
    }

    /// How many block quotes a line sits in.
    pub(crate) fn quote_depth(&self, line: &Line) -> usize {
        line.ancestors
            .iter()
            .filter(|ancestor| Some(ancestor.node_type) == self.blockquote)
            .count()
    }

    /// Whether a task item's box is ticked.
    pub(crate) fn task_checked(attrs: &Attrs) -> bool {
        attrs
            .get("checked")
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
    }
}
