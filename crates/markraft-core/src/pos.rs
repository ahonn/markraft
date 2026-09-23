//! Resolved positions: a document offset plus the context it sits in.

use crate::error::NodeError;
use crate::fragment::Fragment;
use crate::mark::MarkSet;
use crate::node::Node;
use crate::schema::Schema;

#[derive(Debug, Clone)]
struct PathEntry {
    node: Node,
    index: usize,
    /// Absolute position before `node.child(index)`, or the position itself
    /// when the offset falls exactly on a child boundary.
    child_before: usize,
}

/// A document position together with its ancestor chain.
///
/// `depth` counts wrapping containers, with the document at depth 0. Text
/// leaves are not ancestors: a position inside a text leaf resolves to the
/// leaf's parent with a non-zero [`ResolvedPos::text_offset`].
#[derive(Debug, Clone)]
pub struct ResolvedPos {
    pos: usize,
    path: Vec<PathEntry>,
    parent_offset: usize,
}

impl Node {
    /// Resolve a document position.
    ///
    /// `pos` is an offset into this node's content, so valid positions run from
    /// `0` to [`Node::content_size`].
    pub fn resolve(&self, pos: usize) -> Result<ResolvedPos, NodeError> {
        if pos > self.content_size() {
            return Err(NodeError::PosOutOfRange {
                pos,
                size: self.content_size(),
            });
        }
        let mut path: Vec<PathEntry> = Vec::new();
        let mut start = 0usize;
        let mut parent_offset = pos;
        let mut node = self.clone();
        loop {
            let (index, offset) = node.content().find_index_unchecked(parent_offset);
            let rem = parent_offset - offset;
            path.push(PathEntry {
                node: node.clone(),
                index,
                child_before: start + offset,
            });
            if rem == 0 {
                break;
            }
            let child = node.child(index).clone();
            if child.is_text() {
                break;
            }
            parent_offset = rem - 1;
            start += offset + 1;
            node = child;
        }
        Ok(ResolvedPos {
            pos,
            path,
            parent_offset,
        })
    }
}

impl ResolvedPos {
    /// The position itself.
    pub fn pos(&self) -> usize {
        self.pos
    }

    /// The number of containers wrapping this position, with the document at
    /// depth 0.
    pub fn depth(&self) -> usize {
        self.path.len() - 1
    }

    /// The offset of this position inside its parent's content.
    pub fn parent_offset(&self) -> usize {
        self.parent_offset
    }

    /// The offset into the text leaf this position falls inside, or 0 when the
    /// position is at a node boundary.
    pub fn text_offset(&self) -> usize {
        self.pos - self.path[self.depth()].child_before
    }

    /// The container this position points into.
    pub fn parent(&self) -> &Node {
        self.node(self.depth())
    }

    /// The document this position was resolved in.
    pub fn doc(&self) -> &Node {
        &self.path[0].node
    }

    /// The ancestor at `depth`.
    ///
    /// # Panics
    ///
    /// Panics when `depth` exceeds [`ResolvedPos::depth`].
    pub fn node(&self, depth: usize) -> &Node {
        &self.path[depth].node
    }

    /// The index into `node(depth)`'s content that this position sits at.
    pub fn index(&self, depth: usize) -> usize {
        self.path[depth].index
    }

    /// The index after this position in `node(depth)`'s content.
    pub fn index_after(&self, depth: usize) -> usize {
        self.index(depth) + usize::from(depth != self.depth() || self.text_offset() != 0)
    }

    /// The position at the start of `node(depth)`'s content.
    pub fn start(&self, depth: usize) -> usize {
        if depth == 0 {
            0
        } else {
            self.path[depth - 1].child_before + 1
        }
    }

    /// The position at the end of `node(depth)`'s content.
    pub fn end(&self, depth: usize) -> usize {
        self.start(depth) + self.node(depth).content_size()
    }

    /// The position directly before `node(depth)`.
    ///
    /// # Panics
    ///
    /// Panics at depth 0, which has no position before it, and beyond
    /// [`ResolvedPos::depth`].
    pub fn before(&self, depth: usize) -> usize {
        assert!(depth > 0, "there is no position before the top node");
        self.path[depth - 1].child_before
    }

    /// The position directly after `node(depth)`.
    ///
    /// # Panics
    ///
    /// Panics at depth 0, which has no position after it, and beyond
    /// [`ResolvedPos::depth`].
    pub fn after(&self, depth: usize) -> usize {
        assert!(depth > 0, "there is no position after the top node");
        self.before(depth) + self.node(depth).node_size()
    }

    /// The node directly before this position, cut to the part before it when
    /// the position falls inside a text leaf.
    pub fn node_before(&self) -> Option<Node> {
        let index = self.index(self.depth());
        let offset = self.text_offset();
        if offset > 0 {
            return Some(self.parent().child(index).cut_text_unchecked(0, offset));
        }
        if index == 0 {
            None
        } else {
            Some(self.parent().child(index - 1).clone())
        }
    }

    /// The node directly after this position, cut to the part after it when
    /// the position falls inside a text leaf.
    pub fn node_after(&self) -> Option<Node> {
        let parent = self.parent();
        let index = self.index(self.depth());
        if index >= parent.child_count() {
            return None;
        }
        let child = parent.child(index);
        let offset = self.text_offset();
        if offset > 0 {
            Some(child.cut_text_unchecked(offset, child.text_len()))
        } else {
            Some(child.clone())
        }
    }

    /// The marks that content inserted at this position should carry.
    ///
    /// Marks are taken from the node before the position, dropping
    /// non-inclusive marks that do not continue into the node after it. At the
    /// start of a parent the node after the position is used instead. Marks the
    /// parent does not allow in its content are dropped, so the result is
    /// always a mark set that may be stored here.
    pub fn marks(&self, schema: &Schema) -> MarkSet {
        let inherited = self.inherited_marks(schema);
        self.local_marks(schema)
            .iter()
            .fold(inherited, |marks, mark| marks.add(schema, mark.clone()))
    }

    /// Effective marks supplied by enclosing inline containers.
    pub fn inherited_marks(&self, schema: &Schema) -> MarkSet {
        (1..=self.depth()).fold(MarkSet::empty(), |marks, depth| {
            let node = self.node(depth);
            if schema.node_type(node.type_id()).is_inline() {
                node.marks()
                    .iter()
                    .fold(marks, |marks, mark| marks.add(schema, mark.clone()))
            } else {
                marks
            }
        })
    }

    pub(crate) fn local_marks(&self, schema: &Schema) -> MarkSet {
        let parent = self.parent();
        let parent_ty = schema.node_type(parent.type_id());
        if parent.content_size() == 0 {
            return MarkSet::empty();
        }
        if self.text_offset() > 0 {
            return parent
                .child(self.index(self.depth()))
                .marks()
                .filter(|mark| parent_ty.allows_mark_in_content(mark.ty));
        }
        let index = self.index(self.depth());
        let before = index.checked_sub(1).and_then(|i| parent.maybe_child(i));
        let after = parent.maybe_child(index);
        let (main, other) = match before {
            Some(before) => (before, after),
            None => match after {
                Some(after) => (after, None),
                None => return MarkSet::empty(),
            },
        };
        main.marks().filter(|mark| {
            parent_ty.allows_mark_in_content(mark.ty)
                && (schema.mark_type(mark.ty).is_inclusive()
                    || other.is_some_and(|other| other.marks().contains(mark)))
        })
    }

    /// The deepest depth at which this position and `pos` share an ancestor.
    pub fn shared_depth(&self, pos: usize) -> usize {
        let mut depth = self.depth();
        while depth > 0 {
            if self.start(depth) <= pos && self.end(depth) >= pos {
                return depth;
            }
            depth -= 1;
        }
        0
    }

    /// Whether both positions point into the same parent node.
    pub fn same_parent(&self, other: &ResolvedPos) -> bool {
        self.pos - self.parent_offset == other.pos - other.parent_offset
    }

    /// The lower of the two positions.
    pub fn min<'a>(&'a self, other: &'a ResolvedPos) -> &'a ResolvedPos {
        if self.pos <= other.pos { self } else { other }
    }

    /// The higher of the two positions.
    pub fn max<'a>(&'a self, other: &'a ResolvedPos) -> &'a ResolvedPos {
        if self.pos >= other.pos { self } else { other }
    }

    /// The ancestor chain from the document down to [`ResolvedPos::parent`].
    pub fn ancestors(&self) -> impl Iterator<Item = &Node> {
        self.path.iter().map(|entry| &entry.node)
    }

    /// The smallest range of sibling blocks that covers both positions.
    ///
    /// Returns `None` when the two positions have no common block ancestor
    /// satisfying `pred`.
    pub fn block_range(
        &self,
        schema: &Schema,
        other: &ResolvedPos,
        pred: Option<&dyn Fn(&Node) -> bool>,
    ) -> Option<NodeRange> {
        if other.pos < self.pos {
            return other.block_range(schema, self, pred);
        }
        let inline_parent = schema
            .node_type(self.parent().type_id())
            .has_inline_content();
        let start_depth = self
            .depth()
            .checked_sub(usize::from(inline_parent || self.pos == other.pos))?;
        let mut depth = start_depth as isize;
        while depth >= 0 {
            let d = depth as usize;
            let ty = schema.node_type(self.node(d).type_id());
            if !ty.is_inline()
                && !ty.has_inline_content()
                && other.pos <= self.end(d)
                && pred.is_none_or(|pred| pred(self.node(d)))
            {
                return Some(NodeRange {
                    from: self.clone(),
                    to: other.clone(),
                    depth: d,
                });
            }
            depth -= 1;
        }
        None
    }
}

/// A run of sibling nodes inside one parent, as produced by
/// [`ResolvedPos::block_range`].
#[derive(Debug, Clone)]
pub struct NodeRange {
    from: ResolvedPos,
    to: ResolvedPos,
    depth: usize,
}

impl NodeRange {
    /// The depth of the parent that holds the range.
    pub fn depth(&self) -> usize {
        self.depth
    }

    /// The parent node holding the range.
    pub fn parent(&self) -> &Node {
        self.from.node(self.depth)
    }

    /// The position before the first node in the range.
    pub fn start(&self) -> usize {
        self.offset_of(self.start_index())
    }

    /// The position after the last node in the range.
    pub fn end(&self) -> usize {
        self.offset_of(self.end_index())
    }

    /// The document position of child `index` in the range's parent.
    fn offset_of(&self, index: usize) -> usize {
        let parent = self.parent();
        self.from.start(self.depth)
            + parent.content().as_slice()[..index]
                .iter()
                .map(Node::node_size)
                .sum::<usize>()
    }

    /// The index of the first node in the range.
    pub fn start_index(&self) -> usize {
        self.from.index(self.depth)
    }

    /// The index after the last node in the range.
    pub fn end_index(&self) -> usize {
        self.to.index_after(self.depth)
    }

    /// The nodes in the range.
    pub fn content(&self) -> Fragment {
        let parent = self.parent();
        Fragment::from_nodes(
            parent.content().as_slice()[self.start_index()..self.end_index()]
                .iter()
                .cloned(),
        )
    }

    /// The resolved start position.
    pub fn resolved_from(&self) -> &ResolvedPos {
        &self.from
    }

    /// The resolved end position.
    pub fn resolved_to(&self) -> &ResolvedPos {
        &self.to
    }
}
