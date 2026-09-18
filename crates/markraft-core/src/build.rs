//! Schema-driven node construction.

use crate::attr::Attrs;
use crate::error::NodeError;
use crate::fragment::Fragment;
use crate::mark::{Mark, MarkSet};
use crate::node::{Markup, Node};
use crate::schema::{NodeTypeId, Schema};

/// How deep [`Schema::create_and_fill`] may recurse while inventing required
/// children before it gives up.
const MAX_FILL_DEPTH: usize = 16;

impl Schema {
    /// Look up a node type by name.
    pub fn node_type_named(&self, name: &str) -> Result<NodeTypeId, NodeError> {
        self.node_id(name)
            .ok_or_else(|| NodeError::Json(format!("unknown node type `{name}`")))
    }

    /// Create a node of the given type.
    ///
    /// Attributes are completed with the type's defaults and validated;
    /// content is *not* validated, so that intermediate shapes can be built.
    /// Use [`Node::check`] to validate a finished document.
    ///
    /// Text nodes carry their text rather than content, so this rejects the
    /// schema's text type; use [`Schema::text`] for those.
    pub fn create(
        &self,
        ty: NodeTypeId,
        attrs: Attrs,
        marks: MarkSet,
        content: Fragment,
    ) -> Result<Node, NodeError> {
        if self.node_type(ty).is_text() {
            return Err(NodeError::InvalidText(
                "use `Schema::text` to build text nodes".into(),
            ));
        }
        let attrs = self.build_node_attrs(ty, &attrs)?;
        let markup = Markup { ty, attrs, marks };
        if self.node_type(ty).is_leaf() {
            Ok(Node::leaf(markup))
        } else {
            Ok(Node::container(markup, content))
        }
    }

    /// Create a node by type name, with default attributes and no marks.
    pub fn node(
        &self,
        name: &str,
        content: impl IntoIterator<Item = Node>,
    ) -> Result<Node, NodeError> {
        let ty = self.node_type_named(name)?;
        self.create(
            ty,
            Attrs::empty(),
            MarkSet::empty(),
            Fragment::from_nodes(content),
        )
    }

    /// Create a node by type name with explicit attributes.
    pub fn node_with(
        &self,
        name: &str,
        attrs: Attrs,
        content: impl IntoIterator<Item = Node>,
    ) -> Result<Node, NodeError> {
        let ty = self.node_type_named(name)?;
        self.create(ty, attrs, MarkSet::empty(), Fragment::from_nodes(content))
    }

    /// Create a document node holding `content`.
    pub fn doc(&self, content: impl IntoIterator<Item = Node>) -> Result<Node, NodeError> {
        self.create(
            self.top_type(),
            Attrs::empty(),
            MarkSet::empty(),
            Fragment::from_nodes(content),
        )
    }

    /// Create a text leaf.
    ///
    /// # Panics
    ///
    /// Panics when the schema declares no text type.
    pub fn text(&self, text: &str) -> Node {
        self.text_marked(text, MarkSet::empty())
    }

    /// Create a text leaf carrying `marks`.
    ///
    /// # Panics
    ///
    /// Panics when the schema declares no text type.
    pub fn text_marked(&self, text: &str, marks: MarkSet) -> Node {
        let ty = self.text_type().expect("schema declares no text type");
        Node::text_leaf(
            Markup {
                ty,
                attrs: Attrs::empty(),
                marks,
            },
            text,
        )
    }

    /// Create a mark by name.
    pub fn mark(&self, name: &str, attrs: Attrs) -> Result<Mark, NodeError> {
        let ty = self
            .mark_id(name)
            .ok_or_else(|| NodeError::Json(format!("unknown mark type `{name}`")))?;
        let attrs = self.build_mark_attrs(ty, &attrs)?;
        Ok(Mark { ty, attrs })
    }

    /// Create a node of `ty` holding `content`, inserting whatever required
    /// children the content rule asks for.
    ///
    /// Returns `None` when no valid content can be produced, which is how the
    /// fitter learns that a container cannot stand on its own.
    pub fn create_and_fill(
        &self,
        ty: NodeTypeId,
        attrs: Attrs,
        marks: MarkSet,
        content: Fragment,
    ) -> Option<Node> {
        self.create_and_fill_at(ty, attrs, marks, content, 0)
    }

    fn create_and_fill_at(
        &self,
        ty: NodeTypeId,
        attrs: Attrs,
        marks: MarkSet,
        content: Fragment,
        depth: usize,
    ) -> Option<Node> {
        if depth > MAX_FILL_DEPTH {
            return None;
        }
        let start = self.content_match(ty);
        let mut full = content;
        if !full.is_empty() {
            let types: Vec<NodeTypeId> = full.iter().map(|n| n.type_id()).collect();
            let before = self.fill_before(start, &types, false)?;
            full = self.fill_nodes(&before, depth)?.append(&full);
        }
        let matched = start.match_types(full.iter().map(|n| n.type_id()))?;
        let after = self.fill_before(matched, &[], true)?;
        let full = full.append(&self.fill_nodes(&after, depth)?);
        self.create(ty, attrs, marks, full).ok()
    }

    fn fill_nodes(&self, types: &[NodeTypeId], depth: usize) -> Option<Fragment> {
        let mut out = Vec::with_capacity(types.len());
        for ty in types {
            out.push(self.create_and_fill_at(
                *ty,
                Attrs::empty(),
                MarkSet::empty(),
                Fragment::empty(),
                depth + 1,
            )?);
        }
        Some(Fragment::from_nodes(out))
    }

    /// A compact, readable rendering of a node, for tests and diagnostics.
    pub fn describe(&self, node: &Node) -> String {
        let mut out = String::new();
        self.describe_into(node, &mut out);
        out
    }

    fn describe_into(&self, node: &Node, out: &mut String) {
        if let Some(text) = node.text() {
            out.push('"');
            out.push_str(text);
            out.push('"');
        } else {
            out.push_str(self.node_type(node.type_id()).name());
        }
        if !node.attrs().is_empty() {
            out.push('[');
            for (i, (name, value)) in node.attrs().iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(name);
                out.push('=');
                out.push_str(&format!("{value:?}"));
            }
            out.push(']');
        }
        if !node.marks().is_empty() {
            out.push('{');
            for (i, mark) in node.marks().iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(self.mark_type(mark.ty).name());
            }
            out.push('}');
        }
        if node.is_container() {
            out.push('(');
            for (i, child) in node.children().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                self.describe_into(child, out);
            }
            out.push(')');
        }
    }
}
