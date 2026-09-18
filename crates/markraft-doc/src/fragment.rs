//! Sequences of sibling nodes.

use std::sync::Arc;

use crate::node::Node;

/// An immutable sequence of sibling nodes with a cached total token size.
///
/// Cloning is a reference-count bump. Building a fragment normalises inline
/// content: empty text nodes are dropped and adjacent text nodes with identical
/// markup are merged, which keeps the document representation canonical.
#[derive(Debug, Clone, Default)]
pub struct Fragment {
    nodes: Option<Arc<Vec<Node>>>,
    size: usize,
}

impl Fragment {
    /// The empty fragment.
    pub fn empty() -> Fragment {
        Fragment {
            nodes: None,
            size: 0,
        }
    }

    /// Build a fragment from nodes, normalising adjacent text.
    pub fn from_nodes(nodes: impl IntoIterator<Item = Node>) -> Fragment {
        let mut out: Vec<Node> = Vec::new();
        let mut size = 0;
        for node in nodes {
            if node.is_text() && node.text_len() == 0 {
                continue;
            }
            if node.is_text()
                && let Some(last) = out.last()
                && last.is_text()
                && last.same_markup(&node)
            {
                let merged = last.with_text(&format!(
                    "{}{}",
                    last.text().expect("text node"),
                    node.text().expect("text node")
                ));
                size += node.node_size();
                let index = out.len() - 1;
                out[index] = merged;
                continue;
            }
            size += node.node_size();
            out.push(node);
        }
        if out.is_empty() {
            Fragment::empty()
        } else {
            Fragment {
                nodes: Some(Arc::new(out)),
                size,
            }
        }
    }

    /// A fragment holding a single node.
    pub fn from_node(node: Node) -> Fragment {
        Fragment::from_nodes([node])
    }

    /// The summed token size of the nodes in this fragment.
    pub fn size(&self) -> usize {
        self.size
    }

    /// The number of child nodes.
    pub fn child_count(&self) -> usize {
        self.nodes.as_ref().map_or(0, |v| v.len())
    }

    /// Whether the fragment holds no nodes.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_none()
    }

    /// The child at `index`.
    ///
    /// # Panics
    ///
    /// Panics when `index` is out of bounds; use [`Fragment::maybe_child`] to
    /// get an option instead.
    pub fn child(&self, index: usize) -> &Node {
        &self.as_slice()[index]
    }

    /// The child at `index`, if any.
    pub fn maybe_child(&self, index: usize) -> Option<&Node> {
        self.as_slice().get(index)
    }

    /// The first child, if any.
    pub fn first_child(&self) -> Option<&Node> {
        self.as_slice().first()
    }

    /// The last child, if any.
    pub fn last_child(&self) -> Option<&Node> {
        self.as_slice().last()
    }

    /// The children as a slice.
    pub fn as_slice(&self) -> &[Node] {
        self.nodes.as_ref().map_or(&[], |v| v.as_slice())
    }

    /// Iterate over the children.
    pub fn iter(&self) -> std::slice::Iter<'_, Node> {
        self.as_slice().iter()
    }

    /// Find the child containing content offset `pos`.
    ///
    /// Returns `(index, offset)` where `offset` is the content offset at which
    /// the child at `index` starts. When `pos` falls exactly on a child
    /// boundary, `offset == pos` and `index` is the index of the following
    /// child.
    ///
    /// # Panics
    ///
    /// Panics when `pos` is greater than [`Fragment::size`].
    pub fn find_index(&self, pos: usize) -> (usize, usize) {
        assert!(pos <= self.size, "offset {pos} out of range");
        if pos == 0 {
            return (0, 0);
        }
        if pos == self.size {
            return (self.child_count(), self.size);
        }
        let mut offset = 0;
        for (i, child) in self.iter().enumerate() {
            let end = offset + child.node_size();
            if end >= pos {
                if end == pos {
                    return (i + 1, end);
                }
                return (i, offset);
            }
            offset = end;
        }
        (self.child_count(), self.size)
    }

    /// The content between two content offsets, cutting into nodes as needed.
    ///
    /// Partially covered containers keep their markup and are cut recursively;
    /// partially covered text nodes are split on character boundaries.
    ///
    /// # Panics
    ///
    /// Panics when the range is reversed or reaches past [`Fragment::size`].
    pub fn cut(&self, from: usize, to: usize) -> Fragment {
        assert!(
            from <= to && to <= self.size,
            "cut {from}..{to} out of range"
        );
        if from == 0 && to == self.size {
            return self.clone();
        }
        let mut out = Vec::new();
        let mut pos = 0;
        for child in self.iter() {
            let end = pos + child.node_size();
            if end > from && pos < to {
                if child.is_text() {
                    let start = from.saturating_sub(pos);
                    let stop = (to - pos).min(child.text_len());
                    out.push(child.cut_text(start, stop));
                } else if pos >= from && end <= to {
                    out.push(child.clone());
                } else {
                    // A container overlapping one of the ends: cut its content.
                    let inner_from = from.saturating_sub(pos + 1);
                    let inner_to = (to - pos - 1).min(child.content_size());
                    out.push(child.copy(child.content().cut(inner_from, inner_to)));
                }
            }
            pos = end;
            if pos >= to {
                break;
            }
        }
        Fragment::from_nodes(out)
    }

    /// Concatenate two fragments, merging text across the seam.
    pub fn append(&self, other: &Fragment) -> Fragment {
        if self.is_empty() {
            return other.clone();
        }
        if other.is_empty() {
            return self.clone();
        }
        Fragment::from_nodes(self.iter().chain(other.iter()).cloned())
    }

    /// Return a copy with the child at `index` replaced.
    ///
    /// # Panics
    ///
    /// Panics when `index` is out of bounds.
    pub fn replace_child(&self, index: usize, node: Node) -> Fragment {
        let mut nodes = self.as_slice().to_vec();
        nodes[index] = node;
        Fragment::from_nodes(nodes)
    }

    /// Whether the two fragments are the same allocation.
    pub fn ptr_eq(&self, other: &Fragment) -> bool {
        match (&self.nodes, &other.nodes) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        }
    }
}

impl PartialEq for Fragment {
    fn eq(&self, other: &Self) -> bool {
        self.ptr_eq(other) || (self.size == other.size && self.as_slice() == other.as_slice())
    }
}

impl Eq for Fragment {}

impl FromIterator<Node> for Fragment {
    fn from_iter<T: IntoIterator<Item = Node>>(iter: T) -> Self {
        Fragment::from_nodes(iter)
    }
}

impl<'a> IntoIterator for &'a Fragment {
    type Item = &'a Node;
    type IntoIter = std::slice::Iter<'a, Node>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
