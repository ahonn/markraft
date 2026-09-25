//! The immutable document tree. The crate documentation describes its value
//! semantics and the coordinate frame; [`Node::node_size`] counts a node's own
//! open and close tokens, [`Node::content_size`] excludes them.

use std::ops::Range;
use std::sync::Arc;

use crate::attr::Attrs;
use crate::error::NodeError;
use crate::fragment::{Fragment, check_range};
use crate::mark::{Mark, MarkSet};
use crate::schema::{NodeTypeId, Schema};
use crate::slice::Slice;

/// Callback for [`Node::nodes_between`] and [`Node::descendants`].
///
/// Receives the node, the position of the token before it (relative to the
/// content of the node the walk started on), the node's parent and its index in
/// that parent. Returning `false` skips the node's content.
pub type NodeVisitor<'a> = dyn FnMut(&Node, usize, Option<&Node>, usize) -> bool + 'a;

/// A node's type, attributes and marks — everything about a node except its
/// content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Markup {
    /// The node type.
    pub ty: NodeTypeId,
    /// The node's attributes.
    pub attrs: Attrs,
    /// The node's marks.
    pub marks: MarkSet,
}

impl Markup {
    /// Markup with no attributes and no marks.
    pub fn new(ty: NodeTypeId) -> Markup {
        Markup {
            ty,
            attrs: Attrs::empty(),
            marks: MarkSet::empty(),
        }
    }

    /// Markup with attributes.
    pub fn with_attrs(ty: NodeTypeId, attrs: Attrs) -> Markup {
        Markup {
            ty,
            attrs,
            marks: MarkSet::empty(),
        }
    }

    /// Return a copy carrying `marks`.
    pub fn marked(&self, marks: MarkSet) -> Markup {
        Markup {
            ty: self.ty,
            attrs: self.attrs.clone(),
            marks,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Body {
    /// A text leaf. The string is never empty in a well-formed document.
    Text(String),
    /// A leaf that is not text.
    Leaf,
    /// A container and its children.
    Content(Fragment),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NodeData {
    markup: Markup,
    body: Body,
    size: usize,
}

/// A node in the document tree.
///
/// Cloning is a reference-count bump. Equality is structural; use
/// [`Node::ptr_eq`] for the cheap identity test that structural sharing makes
/// meaningful.
#[derive(Debug, Clone)]
pub struct Node(Arc<NodeData>);

impl Node {
    /// A container node with the given markup and content.
    pub fn container(markup: Markup, content: Fragment) -> Node {
        let size = content.size() + 2;
        Node(Arc::new(NodeData {
            markup,
            body: Body::Content(content),
            size,
        }))
    }

    /// A leaf node that is not text.
    pub fn leaf(markup: Markup) -> Node {
        Node(Arc::new(NodeData {
            markup,
            body: Body::Leaf,
            size: 1,
        }))
    }

    /// A text leaf.
    ///
    /// The caller is responsible for using the schema's text type; prefer
    /// [`Schema::text`](crate::Schema::text).
    pub fn text_leaf(markup: Markup, text: impl Into<String>) -> Node {
        let text = text.into();
        let size = text.chars().count();
        Node(Arc::new(NodeData {
            markup,
            body: Body::Text(text),
            size,
        }))
    }

    /// The node's markup.
    pub fn markup(&self) -> &Markup {
        &self.0.markup
    }

    /// The node's type.
    pub fn type_id(&self) -> NodeTypeId {
        self.0.markup.ty
    }

    /// The node's attributes.
    pub fn attrs(&self) -> &Attrs {
        &self.0.markup.attrs
    }

    /// The node's marks.
    pub fn marks(&self) -> &MarkSet {
        &self.0.markup.marks
    }

    /// The number of tokens this node occupies, including its own open and
    /// close tokens for containers.
    pub fn node_size(&self) -> usize {
        self.0.size
    }

    /// The number of tokens this node's content occupies. Zero for leaves.
    pub fn content_size(&self) -> usize {
        match &self.0.body {
            Body::Content(content) => content.size(),
            _ => 0,
        }
    }

    /// The node's content. Empty for leaves.
    pub fn content(&self) -> &Fragment {
        static EMPTY: std::sync::OnceLock<Fragment> = std::sync::OnceLock::new();
        match &self.0.body {
            Body::Content(content) => content,
            _ => EMPTY.get_or_init(Fragment::empty),
        }
    }

    /// The text of a text leaf.
    pub fn text(&self) -> Option<&str> {
        match &self.0.body {
            Body::Text(text) => Some(text),
            _ => None,
        }
    }

    /// The number of characters in a text leaf, zero otherwise.
    pub fn text_len(&self) -> usize {
        match &self.0.body {
            Body::Text(_) => self.0.size,
            _ => 0,
        }
    }

    /// Whether this is a text leaf.
    pub fn is_text(&self) -> bool {
        matches!(self.0.body, Body::Text(_))
    }

    /// Whether this node has no content: a text or non-text leaf.
    pub fn is_leaf(&self) -> bool {
        !matches!(self.0.body, Body::Content(_))
    }

    /// Whether this node can hold content, even if it currently holds none.
    pub fn is_container(&self) -> bool {
        matches!(self.0.body, Body::Content(_))
    }

    /// Whether the node is inline, per its type in `schema`.
    pub fn is_inline(&self, schema: &Schema) -> bool {
        schema.node_type(self.type_id()).is_inline()
    }

    /// Whether the node is block-level, per its type in `schema`.
    pub fn is_block(&self, schema: &Schema) -> bool {
        schema.node_type(self.type_id()).is_block()
    }

    /// Whether the node is a block node with inline content.
    pub fn is_textblock(&self, schema: &Schema) -> bool {
        schema.node_type(self.type_id()).is_textblock()
    }

    /// Whether the node behaves as a single opaque unit.
    pub fn is_atom(&self, schema: &Schema) -> bool {
        schema.node_type(self.type_id()).is_atom()
    }

    /// The number of direct children.
    pub fn child_count(&self) -> usize {
        self.content().child_count()
    }

    /// The child at `index`.
    ///
    /// # Panics
    ///
    /// Panics when `index` is out of bounds.
    pub fn child(&self, index: usize) -> &Node {
        self.content().child(index)
    }

    /// The child at `index`, if any.
    pub fn maybe_child(&self, index: usize) -> Option<&Node> {
        self.content().maybe_child(index)
    }

    /// Iterate over the direct children.
    pub fn children(&self) -> std::slice::Iter<'_, Node> {
        self.content().iter()
    }

    /// The first child, if any.
    pub fn first_child(&self) -> Option<&Node> {
        self.content().first_child()
    }

    /// The last child, if any.
    pub fn last_child(&self) -> Option<&Node> {
        self.content().last_child()
    }

    /// Whether the two handles point at the same allocation.
    ///
    /// Because edits share unchanged subtrees, this is a cheap way to tell that
    /// a subtree did not change. A `false` result does not imply inequality.
    pub fn ptr_eq(&self, other: &Node) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// Whether the two nodes have the same type, attributes and marks.
    pub fn same_markup(&self, other: &Node) -> bool {
        self.0.markup == other.0.markup
    }

    /// Whether this node has exactly the given markup.
    pub fn has_markup(&self, ty: NodeTypeId, attrs: &Attrs, marks: &MarkSet) -> bool {
        self.type_id() == ty && self.attrs() == attrs && self.marks() == marks
    }

    /// A copy of this node with different content.
    ///
    /// Leaves ignore the new content and are returned unchanged.
    pub fn copy(&self, content: Fragment) -> Node {
        match &self.0.body {
            Body::Content(_) => Node::container(self.0.markup.clone(), content),
            _ => self.clone(),
        }
    }

    /// A copy of this node with a different mark set.
    pub fn mark(&self, marks: MarkSet) -> Node {
        if *self.marks() == marks {
            return self.clone();
        }
        let markup = self.0.markup.marked(marks);
        match &self.0.body {
            Body::Text(text) => Node::text_leaf(markup, text.clone()),
            Body::Leaf => Node::leaf(markup),
            Body::Content(content) => Node::container(markup, content.clone()),
        }
    }

    /// A copy of this node with different attributes.
    pub fn with_attrs(&self, attrs: Attrs) -> Node {
        let markup = Markup {
            ty: self.type_id(),
            attrs,
            marks: self.marks().clone(),
        };
        match &self.0.body {
            Body::Text(text) => Node::text_leaf(markup, text.clone()),
            Body::Leaf => Node::leaf(markup),
            Body::Content(content) => Node::container(markup, content.clone()),
        }
    }

    /// A copy of a text leaf with different text.
    pub fn with_text(&self, text: &str) -> Node {
        debug_assert!(self.is_text(), "with_text on a non-text node");
        Node::text_leaf(self.0.markup.clone(), text)
    }

    /// A text leaf holding the characters in `from..to` of this text leaf.
    ///
    /// # Errors
    ///
    /// Returns [`NodeError::PosOutOfRange`] when the range is reversed
    /// (reporting `from`) or reaches past [`Node::text_len`] (reporting `to`).
    ///
    /// # Panics
    ///
    /// Panics when called on a node that is not a text leaf.
    pub fn cut_text(&self, from: usize, to: usize) -> Result<Node, NodeError> {
        assert!(self.is_text(), "cut_text on a non-text node");
        check_range(from, to, self.text_len())?;
        Ok(self.cut_text_unchecked(from, to))
    }

    /// [`Node::cut_text`] without the range check: the range is clamped to
    /// the text's length and never runs backwards.
    ///
    /// The clamp is kept for the crate-internal callers — [`tokens_cut`] and
    /// the mark helpers behind [`Node::add_mark`] — which compute split points
    /// against the surrounding run: an overshooting end yields a shorter piece
    /// instead of a panic, in release and debug builds alike.
    ///
    /// [`tokens_cut`]: crate::tokens_cut
    ///
    /// # Panics
    ///
    /// Panics when called on a node that is not a text leaf.
    pub(crate) fn cut_text_unchecked(&self, from: usize, to: usize) -> Node {
        let text = self.text().expect("cut_text on a non-text node");
        let len = self.text_len();
        let from = from.min(len);
        let to = to.clamp(from, len);
        if from == 0 && to == len {
            return self.clone();
        }
        let start = char_index(text, from);
        let end = char_index(text, to);
        Node::text_leaf(self.0.markup.clone(), &text[start..end])
    }

    /// A copy of this node holding only the content between two offsets.
    ///
    /// For a container the offsets are content offsets and the content is cut
    /// as [`Fragment::cut`] does; for a text leaf they are character offsets
    /// and the text is cut as [`Node::cut_text`] does. A non-text leaf has no
    /// content, so only `0..0` is in range and it returns the leaf itself.
    ///
    /// # Errors
    ///
    /// Returns [`NodeError::PosOutOfRange`] when the range is reversed
    /// (reporting `from`) or reaches past [`Node::content_size`], or
    /// [`Node::text_len`] for a text leaf (reporting `to`).
    pub fn cut(&self, from: usize, to: usize) -> Result<Node, NodeError> {
        let size = if self.is_text() {
            self.text_len()
        } else {
            self.content_size()
        };
        check_range(from, to, size)?;
        Ok(self.cut_unchecked(from, to))
    }

    /// [`Node::cut`] for a range the caller has already bounded. A reversed
    /// or out-of-range cut is a bug: it trips a debug assertion.
    pub(crate) fn cut_unchecked(&self, from: usize, to: usize) -> Node {
        if self.is_text() {
            debug_assert!(
                from <= to && to <= self.text_len(),
                "cut {from}..{to} out of range"
            );
            return self.cut_text_unchecked(from, to);
        }
        self.copy(self.content().cut_unchecked(from, to))
    }

    /// The innermost node starting at content offset `pos`.
    ///
    /// Returns `None` when `pos` is not at a node boundary or is out of range.
    pub fn node_at(&self, pos: usize) -> Option<Node> {
        if pos > self.content_size() {
            return None;
        }
        let mut node = self.clone();
        let mut pos = pos;
        loop {
            let (index, offset) = node.content().find_index_unchecked(pos);
            let child = node.content().maybe_child(index)?.clone();
            if offset == pos || child.is_text() {
                return Some(child);
            }
            pos -= offset + 1;
            node = child;
        }
    }

    /// The direct child that ends at content offset `pos`.
    pub fn node_before(&self, pos: usize) -> Option<&Node> {
        if pos == 0 || pos > self.content_size() {
            return None;
        }
        let (index, offset) = self.content().find_index_unchecked(pos);
        if offset != pos {
            return None;
        }
        self.content().maybe_child(index.checked_sub(1)?)
    }

    /// The direct child that starts at content offset `pos`.
    pub fn node_after(&self, pos: usize) -> Option<&Node> {
        if pos > self.content_size() {
            return None;
        }
        let (index, offset) = self.content().find_index_unchecked(pos);
        if offset != pos {
            return None;
        }
        self.content().maybe_child(index)
    }

    /// Call `f` for every node that overlaps the content range `from..to`,
    /// outer nodes before inner ones.
    ///
    /// `f` receives the node, its position (the offset of the token before it,
    /// relative to this node's content start), its parent and its index in that
    /// parent. Returning `false` skips the node's content.
    pub fn nodes_between(&self, from: usize, to: usize, f: &mut NodeVisitor<'_>) {
        nodes_between_inner(self, from, to, 0, f);
    }

    /// Call `f` for every descendant of this node.
    pub fn descendants(&self, f: &mut NodeVisitor<'_>) {
        self.nodes_between(0, self.content_size(), f);
    }

    /// Concatenate the text in the content range `from..to`.
    ///
    /// `block_separator` is inserted between block-level pieces, and
    /// `leaf_text` provides a replacement string for non-text leaves.
    pub fn text_between(
        &self,
        schema: &Schema,
        from: usize,
        to: usize,
        block_separator: Option<&str>,
        leaf_text: Option<&dyn Fn(&Node) -> String>,
    ) -> String {
        let mut text = String::new();
        let mut first = true;
        self.nodes_between(from, to, &mut |node, pos, _, _| {
            let piece = if let Some(node_text) = node.text() {
                let start = from.max(pos) - pos;
                let end = (to - pos).min(node.text_len());
                node_text[char_index(node_text, start)..char_index(node_text, end)].to_string()
            } else if node.is_leaf() {
                leaf_text.map(|f| f(node)).unwrap_or_default()
            } else {
                String::new()
            };
            let ty = schema.node_type(node.type_id());
            if ty.is_block()
                && (ty.is_textblock() || (node.is_leaf() && !piece.is_empty()))
                && let Some(sep) = block_separator
            {
                if first {
                    first = false;
                } else {
                    text.push_str(sep);
                }
            }
            text.push_str(&piece);
            true
        });
        text
    }

    /// The content between two positions as a [`Slice`].
    ///
    /// The slice's open depths record how many containers the cut passes
    /// through on each side, which is what paste and fitting use to decide
    /// whether the content merges with its new surroundings.
    pub fn slice(&self, from: usize, to: usize) -> Result<Slice, NodeError> {
        if from == to {
            return Ok(Slice::empty());
        }
        let resolved_from = self.resolve(from)?;
        let resolved_to = self.resolve(to)?;
        let depth = resolved_from.shared_depth(to);
        let start = resolved_from.start(depth);
        let node = resolved_from.node(depth);
        let content = node.content().cut_unchecked(from - start, to - start);
        Ok(Slice::new(
            content,
            resolved_from.depth() - depth,
            resolved_to.depth() - depth,
        ))
    }

    /// Copy a range while retaining the semantic scopes of inline containers.
    ///
    /// Unlike the token-oriented [`Node::slice`], inline ancestors are closed
    /// in the result, so clipboard insertion into plain text keeps their marks.
    /// Partial block ancestors remain open for the usual paste fitting.
    pub fn slice_with_schema(
        &self,
        schema: &Schema,
        from: usize,
        to: usize,
    ) -> Result<Slice, NodeError> {
        if from == to {
            return Ok(Slice::empty());
        }
        let start = self.resolve(from)?;
        let end = self.resolve(to)?;
        let mut depth = start.shared_depth(to);
        while depth > 0 && schema.node_type(start.node(depth).type_id()).is_inline() {
            depth -= 1;
        }
        let offset = start.start(depth);
        let content = start
            .node(depth)
            .content()
            .cut_unchecked(from - offset, to - offset);
        let open_start = (depth + 1..=start.depth())
            .filter(|&d| !schema.node_type(start.node(d).type_id()).is_inline())
            .count();
        let open_end = (depth + 1..=end.depth())
            .filter(|&d| !schema.node_type(end.node(d).type_id()).is_inline())
            .count();
        Ok(Slice::new(content, open_start, open_end))
    }

    /// Validate this node and its content against `schema`.
    ///
    /// Checks attributes, content expressions, the all-inline-or-all-block rule
    /// and, for every child, that its marks are ones this node's type allows in
    /// its content. This node's own marks are the business of *its* parent, so
    /// calling `check` on a document validates every mark in it except any on
    /// the top node itself.
    pub fn check(&self, schema: &Schema) -> Result<(), NodeError> {
        self.check_local(schema)?;
        for child in self.children() {
            child.check(schema)?;
        }
        Ok(())
    }

    /// Validate this node against `schema`, assuming `old` is a valid node it
    /// was derived from.
    ///
    /// Gives the same answer as [`Node::check`] whenever `old` passes
    /// [`Node::check`], but only visits what changed: a subtree this node
    /// shares with `old` (the same allocation, see [`Node::ptr_eq`]) is taken
    /// to be valid. At each level the children are matched against `old`'s
    /// from both ends by identity; the unmatched middle ones are paired with
    /// `old`'s unmatched middle ones by index and checked the same way, and
    /// any left over are checked in full. The node's own rules, which cover
    /// its whole child list, are always checked.
    ///
    /// Passing an `old` that is not valid makes the answer meaningless.
    pub fn check_from(&self, old: &Node, schema: &Schema) -> Result<(), NodeError> {
        if self.ptr_eq(old) {
            return Ok(());
        }
        self.check_local(schema)?;
        let new_children = self.content().as_slice();
        let old_children = old.content().as_slice();
        let (old_range, new_range) = unshared_middles(old_children, new_children);
        let new_middle = &new_children[new_range];
        let old_middle = &old_children[old_range];
        for (index, child) in new_middle.iter().enumerate() {
            match old_middle.get(index) {
                Some(previous) => child.check_from(previous, schema)?,
                None => child.check(schema)?,
            }
        }
        Ok(())
    }

    /// The rules that concern this node itself: its type, attributes and marks
    /// exist in `schema`, its body matches its type, and its direct children
    /// satisfy its content expression, carry only marks it allows and have
    /// their adjacent text merged. The children's own content is not visited.
    fn check_local(&self, schema: &Schema) -> Result<(), NodeError> {
        let ty = schema
            .try_node_type(self.type_id())
            .ok_or_else(|| NodeError::Json("node type does not belong to this schema".into()))?;
        schema.build_node_attrs(self.type_id(), self.attrs())?;
        for mark in self.marks().iter() {
            if schema.try_mark_type(mark.ty).is_none() {
                return Err(NodeError::Json(
                    "mark type does not belong to this schema".into(),
                ));
            }
            schema.build_mark_attrs(mark.ty, &mark.attrs)?;
        }
        if let Some(text) = self.text() {
            if !ty.is_text() {
                return Err(NodeError::InvalidText(format!(
                    "node type `{}` is not the schema's text type",
                    ty.name()
                )));
            }
            if text.is_empty() {
                return Err(NodeError::InvalidText(
                    "text nodes must not be empty".into(),
                ));
            }
        } else if ty.is_text() {
            return Err(NodeError::InvalidText(format!(
                "`{}` is the schema's text type but this node carries no text",
                ty.name()
            )));
        } else if self.is_leaf() && ty.is_leaf() {
            // Nothing further to check for non-text leaves.
        } else if self.is_leaf() != ty.is_leaf() {
            return Err(NodeError::InvalidContent {
                node: ty.name().to_string(),
                message: if ty.is_leaf() {
                    "leaf type used as a container".into()
                } else {
                    "container type used as a leaf".into()
                },
            });
        }
        if self.is_container() {
            for child in self.children() {
                if schema.try_node_type(child.type_id()).is_none() {
                    return Err(NodeError::Json(
                        "node type does not belong to this schema".into(),
                    ));
                }
                for mark in child.marks().iter() {
                    if !ty.allows_mark_in_content(mark.ty) {
                        return Err(NodeError::MarkNotAllowed {
                            mark: schema
                                .try_mark_type(mark.ty)
                                .map(|m| m.name().to_string())
                                .unwrap_or_else(|| "?".into()),
                            node: ty.name().to_string(),
                        });
                    }
                }
            }
            let mut m = schema.content_match(self.type_id());
            for child in self.children() {
                m = m
                    .match_type(child.type_id())
                    .ok_or_else(|| NodeError::InvalidContent {
                        node: ty.name().to_string(),
                        message: format!(
                            "`{}` is not allowed here",
                            schema.node_type(child.type_id()).name()
                        ),
                    })?;
            }
            if !m.valid_end() {
                return Err(NodeError::InvalidContent {
                    node: ty.name().to_string(),
                    message: format!(
                        "content `{}` is incomplete",
                        schema.content_expr(self.type_id()).source()
                    ),
                });
            }
            // Adjacent text nodes with identical markup must have been merged.
            let mut previous: Option<&Node> = None;
            for child in self.children() {
                if let Some(previous) = previous
                    && previous.is_text()
                    && child.is_text()
                    && previous.same_markup(child)
                {
                    return Err(NodeError::InvalidContent {
                        node: ty.name().to_string(),
                        message: "adjacent text nodes with identical marks are not merged".into(),
                    });
                }
                previous = Some(child);
            }
        }
        Ok(())
    }

    /// Add `mark` to every inline descendant in the content range `from..to`
    /// whose parent allows that mark in its content.
    ///
    /// This is the tree-level operation; the change system exposes the same
    /// effect as a mark change so that it can be mapped and inverted.
    pub fn add_mark(&self, schema: &Schema, from: usize, to: usize, mark: &Mark) -> Node {
        self.modify_marks(schema, from, to, &|set| set.add(schema, mark.clone()))
    }

    /// Remove `mark` from every inline descendant in the content range
    /// `from..to`.
    pub fn remove_mark(&self, schema: &Schema, from: usize, to: usize, mark: &Mark) -> Node {
        self.modify_marks(schema, from, to, &|set| set.remove(mark))
    }

    fn modify_marks(
        &self,
        schema: &Schema,
        from: usize,
        to: usize,
        f: &dyn Fn(&MarkSet) -> MarkSet,
    ) -> Node {
        if from >= to {
            return self.clone();
        }
        let mut out: Vec<Node> = Vec::with_capacity(self.child_count());
        let mut pos = 0;
        for child in self.children() {
            let end = pos + child.node_size();
            if end <= from || pos >= to {
                out.push(child.clone());
            } else if pos >= from && end <= to {
                out.push(apply_marks_under(schema, self.type_id(), child, f));
            } else if child.is_text() {
                let split_from = from.saturating_sub(pos);
                let split_to = (to - pos).min(child.text_len());
                if split_from > 0 {
                    out.push(child.cut_text_unchecked(0, split_from));
                }
                out.push(apply_marks_under(
                    schema,
                    self.type_id(),
                    &child.cut_text_unchecked(split_from, split_to),
                    f,
                ));
                if split_to < child.text_len() {
                    out.push(child.cut_text_unchecked(split_to, child.text_len()));
                }
            } else if child.is_container() {
                let inner_from = from.saturating_sub(pos + 1);
                let inner_to = (to.saturating_sub(pos + 1)).min(child.content_size());
                out.push(child.modify_marks(schema, inner_from, inner_to, f));
            } else {
                out.push(child.clone());
            }
            pos = end;
        }
        self.copy(Fragment::from_nodes(out))
    }
}

/// Apply a mark-set transformation to the inline content of a subtree.
///
/// Marks belong to inline nodes, so a block node is descended into rather than
/// marked. Whether a mark is *allowed* is the parent's business and is resolved
/// before a change is recorded — see
/// [`NodeType::allows_mark_in_content`](crate::NodeType::allows_mark_in_content)
/// and [`ChangeSet::create`](crate::ChangeSet::create).
pub(crate) fn apply_marks(schema: &Schema, node: &Node, f: &dyn Fn(&MarkSet) -> MarkSet) -> Node {
    if schema.node_type(node.type_id()).is_inline() {
        return node.mark(f(node.marks()));
    }
    if node.is_container() && node.content_size() > 0 {
        let content: Fragment = node
            .children()
            .map(|child| apply_marks(schema, child, f))
            .collect();
        return node.copy(content);
    }
    node.clone()
}

/// Like [`apply_marks`], but dropping marks `parent` does not allow in its
/// content. Used by the tree-level `add_mark`/`remove_mark` helpers, which have
/// no change set to pre-filter for them.
fn apply_marks_under(
    schema: &Schema,
    parent: NodeTypeId,
    node: &Node,
    f: &dyn Fn(&MarkSet) -> MarkSet,
) -> Node {
    if schema.node_type(node.type_id()).is_inline() {
        let parent_ty = schema.node_type(parent);
        return node.mark(f(node.marks()).filter(|m| parent_ty.allows_mark_in_content(m.ty)));
    }
    if node.is_container() && node.content_size() > 0 {
        let inner = node.type_id();
        let content: Fragment = node
            .children()
            .map(|child| apply_marks_under(schema, inner, child, f))
            .collect();
        return node.copy(content);
    }
    node.clone()
}

fn nodes_between_inner(
    node: &Node,
    from: usize,
    to: usize,
    node_start: usize,
    f: &mut NodeVisitor<'_>,
) {
    let mut pos = 0;
    for (index, child) in node.children().enumerate() {
        if pos >= to {
            break;
        }
        let end = pos + child.node_size();
        if end > from && f(child, node_start + pos, Some(node), index) && child.content_size() > 0 {
            let start = pos + 1;
            nodes_between_inner(
                child,
                from.saturating_sub(start),
                (to.saturating_sub(start)).min(child.content_size()),
                node_start + start,
                f,
            );
        }
        pos = end;
    }
}

/// The unmatched middles of two child lists, as ranges into `old` and `new`:
/// what is left once the children both lists share by identity
/// ([`Node::ptr_eq`]) are matched from the front and then from the back. Both
/// ranges start at the same index, since everything before it is shared.
pub(crate) fn unshared_middles(old: &[Node], new: &[Node]) -> (Range<usize>, Range<usize>) {
    let prefix = old.iter().zip(new).take_while(|(a, b)| a.ptr_eq(b)).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a.ptr_eq(b))
        .count();
    (prefix..old.len() - suffix, prefix..new.len() - suffix)
}

/// A pair of children [`diff_region`] walked into.
#[derive(Debug, Clone)]
pub(crate) struct DiffStep {
    /// The pair's index in both parents. Every child before it is shared, so
    /// the index is the same on both sides.
    pub(crate) index: usize,
    /// The child in the old tree.
    pub(crate) old: Node,
    /// The child in the new tree.
    pub(crate) new: Node,
}

/// The smallest run of sibling children that holds every difference between
/// two trees, as [`diff_region`] finds it.
#[derive(Debug, Clone)]
pub(crate) struct DiffRegion {
    /// The pairs walked into, from the top down. The last pair (or the two
    /// tops, when the path is empty) is the region's parent; the pairs above
    /// it are its ancestors, each differing from its counterpart in that one
    /// child only.
    pub(crate) path: Vec<DiffStep>,
    /// The region within the old parent's children.
    pub(crate) old: Range<usize>,
    /// The region within the new parent's children. It starts where
    /// [`DiffRegion::old`] does.
    pub(crate) new: Range<usize>,
}

/// Where two trees differ, as the path down to the smallest run of siblings
/// that holds every difference.
///
/// At each level, starting from the tops, the children are matched from both
/// ends by identity ([`unshared_middles`]). When the unmatched middle is
/// exactly one child on each side, and that child is a block container that
/// is not a textblock and keeps its markup, the walk enters the pair and goes
/// on. Otherwise it stops, and the middles are the region: a change of type or
/// attributes concerns everything inside a node, and a textblock's inline
/// content is rebuilt as a whole. Identical trees give an empty path and empty
/// middles.
pub(crate) fn diff_region(old: &Node, new: &Node, schema: &Schema) -> DiffRegion {
    let mut path = Vec::new();
    let (mut old, mut new) = (old.clone(), new.clone());
    loop {
        let (old_range, new_range) =
            unshared_middles(old.content().as_slice(), new.content().as_slice());
        let pair = match (old_range.len(), new_range.len()) {
            (1, 1) => {
                let next_old = old.child(old_range.start).clone();
                let next_new = new.child(new_range.start).clone();
                let ty = schema.node_type(next_new.type_id());
                (next_new.is_container()
                    && ty.is_block()
                    && !ty.is_textblock()
                    && next_new.same_markup(&next_old))
                .then_some((next_old, next_new))
            }
            _ => None,
        };
        let Some((next_old, next_new)) = pair else {
            return DiffRegion {
                path,
                old: old_range,
                new: new_range,
            };
        };
        path.push(DiffStep {
            index: old_range.start,
            old: next_old.clone(),
            new: next_new.clone(),
        });
        old = next_old;
        new = next_new;
    }
}

/// Byte index of the `n`th character of `text`, clamped to its length.
pub(crate) fn char_index(text: &str, n: usize) -> usize {
    text.char_indices()
        .nth(n)
        .map(|(i, _)| i)
        .unwrap_or(text.len())
}

impl PartialEq for Node {
    fn eq(&self, other: &Self) -> bool {
        self.ptr_eq(other) || (self.0.size == other.0.size && self.0 == other.0)
    }
}

impl Eq for Node {}
