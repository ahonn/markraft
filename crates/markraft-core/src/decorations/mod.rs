//! Decorations: presentation attached to a document without changing it.
//!
//! A decoration says something about a stretch of the document — a search hit,
//! a spelling squiggle, a collaborator's caret, a placeholder — that belongs to
//! the view rather than to the content. Nothing here draws anything: a
//! [`DecorationSet`] is data a view layer reads.
//!
//! # Kinds
//!
//! * [`Decoration::Inline`] puts attributes on inline content. A view asks for
//!   [`Decoration::inline_pieces`] to get the parts that fall inside a single
//!   inline node, which is what it can actually style.
//! * [`Decoration::Node`] covers exactly one node.
//! * [`Decoration::Widget`] is a point, with a side saying which way it leans
//!   when content is inserted at it.
//! * [`Decoration::Tag`] applies to every node of one type, which is how a
//!   theme says "draw every blockquote like this" without walking the document.
//!
//! Every kind carries a [`DecorationSpec`]: an open [`Attrs`] bag plus an
//! optional opaque payload a view can downcast to whatever it needs.
//!
//! # Keeping them current
//!
//! A set is mapped through a change with [`DecorationSet::map`]; the exact
//! rules are in [`range_set`]. A set that lives in a
//! [`StateField`](crate::StateField) maps itself in the field's `update`; a set
//! that is derived from the state is registered in the [`decorations`] facet as
//! a [`DecorationSource::Computed`] instead and never needs mapping at all.

pub mod range_set;

use std::any::Any;
use std::sync::{Arc, LazyLock};

use crate::attr::Attrs;
use crate::change::ChangeDesc;
use crate::node::Node;
use crate::schema::{NodeTypeId, Schema};
use crate::state::{EditorState, Facet};

pub use range_set::{PointItem, PointSet, RangeItem, RangeSet};

/// What a decoration tells a view.
#[derive(Clone, Default)]
pub struct DecorationSpec {
    /// Open attribute bag. A view decides what the names mean.
    pub attrs: Attrs,
    /// An opaque value for a view that needs more than attributes.
    pub payload: Option<Arc<dyn Any + Send + Sync>>,
}

impl DecorationSpec {
    /// A spec holding only attributes.
    pub fn new(attrs: Attrs) -> DecorationSpec {
        DecorationSpec {
            attrs,
            payload: None,
        }
    }

    /// Attach an opaque payload.
    pub fn with_payload(mut self, payload: Arc<dyn Any + Send + Sync>) -> DecorationSpec {
        self.payload = Some(payload);
        self
    }

    /// The payload, downcast to `T`.
    pub fn payload<T: Send + Sync + 'static>(&self) -> Option<&T> {
        self.payload.as_ref()?.downcast_ref::<T>()
    }
}

impl std::fmt::Debug for DecorationSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecorationSpec")
            .field("attrs", &self.attrs)
            .field("payload", &self.payload.is_some())
            .finish()
    }
}

impl PartialEq for DecorationSpec {
    fn eq(&self, other: &DecorationSpec) -> bool {
        self.attrs == other.attrs
            && match (&self.payload, &other.payload) {
                (None, None) => true,
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                _ => false,
            }
    }
}

impl Eq for DecorationSpec {}

/// One decoration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decoration {
    /// Attributes over a stretch of inline content.
    Inline {
        /// Start of the stretch.
        from: usize,
        /// End of the stretch.
        to: usize,
        /// Whether content inserted at `from` joins the decoration.
        inclusive_start: bool,
        /// Whether content inserted at `to` joins the decoration.
        inclusive_end: bool,
        /// What to tell the view.
        spec: DecorationSpec,
    },
    /// A decoration covering exactly one node.
    Node {
        /// Position directly before the node.
        from: usize,
        /// Position directly after the node.
        to: usize,
        /// What to tell the view.
        spec: DecorationSpec,
    },
    /// A decoration at one position.
    Widget {
        /// The position.
        pos: usize,
        /// Which side of `pos` the widget leans to.
        side: i32,
        /// What to tell the view.
        spec: DecorationSpec,
    },
    /// A decoration on every node of one type.
    Tag {
        /// The type to decorate.
        node_type: NodeTypeId,
        /// What to tell the view.
        spec: DecorationSpec,
    },
}

impl Decoration {
    /// An inline decoration whose ends both push inserted content out.
    pub fn inline(from: usize, to: usize, spec: DecorationSpec) -> Decoration {
        Decoration::Inline {
            from: from.min(to),
            to: from.max(to),
            inclusive_start: false,
            inclusive_end: false,
            spec,
        }
    }

    /// An inline decoration with explicit inclusivity.
    pub fn inline_with(
        from: usize,
        to: usize,
        inclusive_start: bool,
        inclusive_end: bool,
        spec: DecorationSpec,
    ) -> Decoration {
        Decoration::Inline {
            from: from.min(to),
            to: from.max(to),
            inclusive_start,
            inclusive_end,
            spec,
        }
    }

    /// A decoration covering the node starting at `from`.
    pub fn node(from: usize, to: usize, spec: DecorationSpec) -> Decoration {
        Decoration::Node {
            from: from.min(to),
            to: from.max(to),
            spec,
        }
    }

    /// A widget at `pos`.
    pub fn widget(pos: usize, side: i32, spec: DecorationSpec) -> Decoration {
        Decoration::Widget {
            pos,
            side: if side < 0 { -1 } else { 1 },
            spec,
        }
    }

    /// A decoration on every node of `node_type`.
    pub fn tag(node_type: NodeTypeId, spec: DecorationSpec) -> Decoration {
        Decoration::Tag { node_type, spec }
    }

    /// What the decoration tells the view.
    pub fn spec(&self) -> &DecorationSpec {
        match self {
            Decoration::Inline { spec, .. }
            | Decoration::Node { spec, .. }
            | Decoration::Widget { spec, .. }
            | Decoration::Tag { spec, .. } => spec,
        }
    }

    /// The range the decoration covers, or `None` for a
    /// [`Decoration::Tag`].
    pub fn range(&self) -> Option<(usize, usize)> {
        match self {
            Decoration::Inline { from, to, .. } | Decoration::Node { from, to, .. } => {
                Some((*from, *to))
            }
            Decoration::Widget { pos, .. } => Some((*pos, *pos)),
            Decoration::Tag { .. } => None,
        }
    }

    /// The parts of an inline decoration that fall inside a single inline node.
    ///
    /// A decoration that spans two textblocks, or several differently marked
    /// text nodes, cannot be drawn as one run; this is the split a view needs.
    /// Other kinds answer with their own range.
    pub fn inline_pieces(&self, doc: &Node, schema: &Schema) -> Vec<(usize, usize)> {
        let (from, to) = match self {
            Decoration::Inline { from, to, .. } => (*from, *to),
            _ => return self.range().into_iter().collect(),
        };
        if from >= to {
            return vec![(from, to)];
        }
        let mut out = Vec::new();
        doc.nodes_between(from, to, &mut |node, pos, _, _| {
            if !schema.node_type(node.type_id()).is_inline() {
                return true;
            }
            let start = pos.max(from);
            let end = (pos + node.node_size()).min(to);
            if start < end {
                out.push((start, end));
            }
            false
        });
        out
    }
}

/// The inline, node, widget and tag decorations of one layer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DecorationSet {
    inline: RangeSet<DecorationSpec>,
    nodes: RangeSet<DecorationSpec>,
    widgets: PointSet<DecorationSpec>,
    tags: Vec<(NodeTypeId, DecorationSpec)>,
}

impl DecorationSet {
    /// The empty set.
    pub fn new() -> DecorationSet {
        DecorationSet::default()
    }

    /// A set holding `decorations`.
    pub fn from_decorations(decorations: impl IntoIterator<Item = Decoration>) -> DecorationSet {
        DecorationSet::new().add(decorations)
    }

    /// A copy of this set with `decorations` added.
    pub fn add(&self, decorations: impl IntoIterator<Item = Decoration>) -> DecorationSet {
        let mut inline = Vec::new();
        let mut nodes = Vec::new();
        let mut widgets = Vec::new();
        let mut tags = self.tags.clone();
        for decoration in decorations {
            match decoration {
                Decoration::Inline {
                    from,
                    to,
                    inclusive_start,
                    inclusive_end,
                    spec,
                } => inline
                    .push(RangeItem::new(from, to, spec).inclusive(inclusive_start, inclusive_end)),
                Decoration::Node { from, to, spec } => nodes.push(RangeItem::new(from, to, spec)),
                Decoration::Widget { pos, side, spec } => {
                    widgets.push(PointItem::new(pos, side, spec))
                }
                Decoration::Tag { node_type, spec } => tags.push((node_type, spec)),
            }
        }
        DecorationSet {
            inline: self.inline.add(inline),
            nodes: self.nodes.add(nodes),
            widgets: self.widgets.add(widgets),
            tags,
        }
    }

    /// A copy of this set without the decorations `drop` accepts.
    pub fn remove(&self, drop: impl Fn(&Decoration) -> bool) -> DecorationSet {
        DecorationSet {
            inline: self.inline.remove(|item| {
                drop(&Decoration::inline_with(
                    item.from,
                    item.to,
                    item.inclusive_start,
                    item.inclusive_end,
                    item.value.clone(),
                ))
            }),
            nodes: self
                .nodes
                .remove(|item| drop(&Decoration::node(item.from, item.to, item.value.clone()))),
            widgets: self
                .widgets
                .remove(|item| drop(&Decoration::widget(item.pos, item.side, item.value.clone()))),
            tags: self
                .tags
                .iter()
                .filter(|(ty, spec)| !drop(&Decoration::tag(*ty, spec.clone())))
                .cloned()
                .collect(),
        }
    }

    /// Combine two sets. `other`'s decorations come after this set's.
    pub fn union(&self, other: &DecorationSet) -> DecorationSet {
        self.add(other.all())
    }

    /// The inline decorations.
    pub fn inline(&self) -> &RangeSet<DecorationSpec> {
        &self.inline
    }

    /// The node decorations.
    pub fn nodes(&self) -> &RangeSet<DecorationSpec> {
        &self.nodes
    }

    /// The widgets.
    pub fn widgets(&self) -> &PointSet<DecorationSpec> {
        &self.widgets
    }

    /// The per-node-type decorations.
    pub fn tags(&self) -> &[(NodeTypeId, DecorationSpec)] {
        &self.tags
    }

    /// Every decoration in the set.
    pub fn all(&self) -> Vec<Decoration> {
        let mut out: Vec<Decoration> = Vec::new();
        for item in self.inline.iter() {
            out.push(Decoration::inline_with(
                item.from,
                item.to,
                item.inclusive_start,
                item.inclusive_end,
                item.value.clone(),
            ));
        }
        for item in self.nodes.iter() {
            out.push(Decoration::node(item.from, item.to, item.value.clone()));
        }
        for item in self.widgets.iter() {
            out.push(Decoration::widget(item.pos, item.side, item.value.clone()));
        }
        for (ty, spec) in &self.tags {
            out.push(Decoration::tag(*ty, spec.clone()));
        }
        out
    }

    /// The decorations that touch `from..to`. Tags always match.
    pub fn find(&self, from: usize, to: usize) -> Vec<Decoration> {
        let mut out: Vec<Decoration> = Vec::new();
        for item in self.inline.find(from, to) {
            out.push(Decoration::inline_with(
                item.from,
                item.to,
                item.inclusive_start,
                item.inclusive_end,
                item.value.clone(),
            ));
        }
        for item in self.nodes.find(from, to) {
            out.push(Decoration::node(item.from, item.to, item.value.clone()));
        }
        for item in self.widgets.find(from, to) {
            out.push(Decoration::widget(item.pos, item.side, item.value.clone()));
        }
        for (ty, spec) in &self.tags {
            out.push(Decoration::tag(*ty, spec.clone()));
        }
        out
    }

    /// Whether the set holds nothing.
    pub fn is_empty(&self) -> bool {
        self.inline.is_empty()
            && self.nodes.is_empty()
            && self.widgets.is_empty()
            && self.tags.is_empty()
    }

    /// Move every decoration through `changes`.
    pub fn map(&self, changes: &ChangeDesc) -> DecorationSet {
        DecorationSet {
            inline: self.inline.map(changes),
            nodes: self.nodes.map(changes),
            widgets: self.widgets.map(changes),
            tags: self.tags.clone(),
        }
    }
}

/// Where a decoration set comes from.
#[derive(Clone)]
pub enum DecorationSource {
    /// A set that does not depend on the state. It is the provider's job to
    /// keep it mapped.
    Static(DecorationSet),
    /// A set derived from the state whenever a view asks, which never needs
    /// mapping.
    Computed(Arc<dyn Fn(&EditorState) -> DecorationSet + Send + Sync>),
}

impl std::fmt::Debug for DecorationSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecorationSource::Static(set) => f.debug_tuple("Static").field(set).finish(),
            DecorationSource::Computed(_) => f.write_str("Computed"),
        }
    }
}

impl DecorationSource {
    /// A source computed from the state.
    pub fn computed(
        f: impl Fn(&EditorState) -> DecorationSet + Send + Sync + 'static,
    ) -> DecorationSource {
        DecorationSource::Computed(Arc::new(f))
    }

    /// The set this source describes for `state`.
    pub fn resolve(&self, state: &EditorState) -> DecorationSet {
        match self {
            DecorationSource::Static(set) => set.clone(),
            DecorationSource::Computed(f) => f(state),
        }
    }
}

static DECORATIONS: LazyLock<Facet<DecorationSource>> = LazyLock::new(Facet::list);

/// The facet a view layer reads decorations from.
pub fn decorations() -> &'static Facet<DecorationSource> {
    &DECORATIONS
}

/// Every configured source, resolved and merged into one set.
pub fn collect_decorations(state: &EditorState) -> DecorationSet {
    let mut out = DecorationSet::new();
    for source in state.facet(decorations()) {
        out = out.union(&source.resolve(state));
    }
    out
}
