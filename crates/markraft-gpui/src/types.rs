//! The node and mark types the view draws, resolved once per schema.
//!
//! The view renders whatever the schema declares, so every lookup is optional: a
//! single-line editor whose schema has only `doc > paragraph` simply finds none
//! of the block types and draws plain text.

use markraft_doc::projection::{Ancestor, Line};
use markraft_doc::{Attrs, MarkTypeId, NodeTypeId, Schema};
use markraft_markdown::schema as md;

/// The types a [`Schema`] declares under the CommonMark names.
#[derive(Clone, Debug)]
pub(crate) struct DocTypes {
    pub paragraph: Option<NodeTypeId>,
    pub heading: Option<NodeTypeId>,
    pub blockquote: Option<NodeTypeId>,
    pub code_block: Option<NodeTypeId>,
    pub bullet_list: Option<NodeTypeId>,
    pub ordered_list: Option<NodeTypeId>,
    pub list_item: Option<NodeTypeId>,
    pub task_item: Option<NodeTypeId>,
    pub horizontal_rule: Option<NodeTypeId>,
    pub raw_block: Option<NodeTypeId>,
    pub strong: Option<MarkTypeId>,
    pub em: Option<MarkTypeId>,
    pub code: Option<MarkTypeId>,
    pub strikethrough: Option<MarkTypeId>,
    pub underline: Option<MarkTypeId>,
    pub link: Option<MarkTypeId>,
}

impl DocTypes {
    /// Resolve every name against `schema`.
    pub(crate) fn of(schema: &Schema) -> DocTypes {
        DocTypes {
            paragraph: schema.node_id(md::PARAGRAPH),
            heading: schema.node_id(md::HEADING),
            blockquote: schema.node_id(md::BLOCKQUOTE),
            code_block: schema.node_id(md::CODE_BLOCK),
            bullet_list: schema.node_id(md::BULLET_LIST),
            ordered_list: schema.node_id(md::ORDERED_LIST),
            list_item: schema.node_id(md::LIST_ITEM),
            task_item: schema.node_id(md::TASK_ITEM),
            horizontal_rule: schema.node_id(md::HORIZONTAL_RULE),
            raw_block: schema.node_id(md::RAW_BLOCK),
            strong: schema.mark_id(md::STRONG),
            em: schema.mark_id(md::EM),
            code: schema.mark_id(md::CODE),
            strikethrough: schema.mark_id(md::STRIKETHROUGH),
            underline: schema.mark_id(md::UNDERLINE),
            link: schema.mark_id(md::LINK),
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
