//! The data that describes a document kind: node types, mark types, groups,
//! attributes and content rules.
//!
//! Specs are plain values with no behaviour; [`Schema::new`](super::Schema::new)
//! validates them and compiles them into interned types.

use crate::attr::AttrSpec;

/// Declaration of one node type.
///
/// `content` is a content expression (see [`ContentExpr`](super::ContentExpr));
/// an empty string makes the type a leaf.
///
/// `marks` selects the mark types allowed on this type's **inline content**:
/// `"_"` for all, `""` for none, or a space-separated list of mark type and
/// mark group names. Marks belong to inline content, and a node's own marks are
/// governed by its parent, so a schema forbids emphasis inside code by giving
/// `code_block` an empty mark list — not by restricting the text type, which is
/// shared by every textblock. When `marks` is `None`, types with inline content
/// allow every mark and all other types allow none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeTypeSpec {
    /// Unique name, also used in JSON.
    pub name: String,
    /// Space-separated group names this type belongs to.
    pub group: String,
    /// Content expression.
    pub content: String,
    /// Allowed marks, see the type documentation.
    pub marks: Option<String>,
    /// Whether this is an inline type. Block otherwise.
    pub inline: bool,
    /// Attribute declarations, in the order given.
    pub attrs: Vec<AttrSpec>,
    /// Marks this as the schema's text type. Exactly one type may set it.
    pub text: bool,
    /// The node counts as a single opaque unit for selection and editing even
    /// though it may have content.
    pub atom: bool,
    /// The node can be selected as a whole.
    pub selectable: bool,
    /// The node can be dragged.
    pub draggable: bool,
    /// The node's identity should be preserved when its content is moved or
    /// pasted elsewhere.
    pub defining: bool,
    /// Editing operations should not cross this node's boundaries.
    pub isolating: bool,
    /// The node holds code; consumers use this to disable smart behaviour.
    pub code: bool,
}

impl NodeTypeSpec {
    /// A block container with the given content expression.
    pub fn new(name: impl Into<String>, content: impl Into<String>) -> Self {
        NodeTypeSpec {
            name: name.into(),
            group: String::new(),
            content: content.into(),
            marks: None,
            inline: false,
            attrs: Vec::new(),
            text: false,
            atom: false,
            selectable: false,
            draggable: false,
            defining: false,
            isolating: false,
            code: false,
        }
    }

    /// A block leaf (no content).
    pub fn leaf(name: impl Into<String>) -> Self {
        NodeTypeSpec::new(name, "")
    }

    /// The schema's text type.
    pub fn text(name: impl Into<String>) -> Self {
        let mut spec = NodeTypeSpec::new(name, "");
        spec.text = true;
        spec.inline = true;
        spec
    }

    /// Set the space-separated group list.
    pub fn group(mut self, group: impl Into<String>) -> Self {
        self.group = group.into();
        self
    }

    /// Mark the type as inline.
    pub fn inline(mut self, inline: bool) -> Self {
        self.inline = inline;
        self
    }

    /// Set the allowed mark expression.
    pub fn marks(mut self, marks: impl Into<String>) -> Self {
        self.marks = Some(marks.into());
        self
    }

    /// Append an attribute declaration.
    pub fn attr(mut self, attr: AttrSpec) -> Self {
        self.attrs.push(attr);
        self
    }

    /// Set the `atom` flag.
    pub fn atom(mut self, atom: bool) -> Self {
        self.atom = atom;
        self
    }

    /// Set the `selectable` flag.
    pub fn selectable(mut self, selectable: bool) -> Self {
        self.selectable = selectable;
        self
    }

    /// Set the `draggable` flag.
    pub fn draggable(mut self, draggable: bool) -> Self {
        self.draggable = draggable;
        self
    }

    /// Set the `defining` flag.
    pub fn defining(mut self, defining: bool) -> Self {
        self.defining = defining;
        self
    }

    /// Set the `isolating` flag.
    pub fn isolating(mut self, isolating: bool) -> Self {
        self.isolating = isolating;
        self
    }

    /// Set the `code` flag.
    pub fn code(mut self, code: bool) -> Self {
        self.code = code;
        self
    }
}

/// Declaration of one mark type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkTypeSpec {
    /// Unique name, also used in JSON.
    pub name: String,
    /// Space-separated group names.
    pub group: String,
    /// Attribute declarations.
    pub attrs: Vec<AttrSpec>,
    /// Mark types this one pushes out of a mark set. `None` means "only other
    /// marks of the same type", `Some("_")` means all marks, `Some("")` means
    /// none, otherwise a space-separated list of type and group names.
    pub excludes: Option<String>,
    /// Whether the mark applies to content typed directly at its end.
    pub inclusive: bool,
    /// Sorting rank within a mark set; lower ranks come first.
    pub rank: u8,
}

impl MarkTypeSpec {
    /// A mark type with no attributes and default behaviour.
    pub fn new(name: impl Into<String>) -> Self {
        MarkTypeSpec {
            name: name.into(),
            group: String::new(),
            attrs: Vec::new(),
            excludes: None,
            inclusive: true,
            rank: 50,
        }
    }

    /// Set the space-separated group list.
    pub fn group(mut self, group: impl Into<String>) -> Self {
        self.group = group.into();
        self
    }

    /// Append an attribute declaration.
    pub fn attr(mut self, attr: AttrSpec) -> Self {
        self.attrs.push(attr);
        self
    }

    /// Set the `excludes` expression.
    pub fn excludes(mut self, excludes: impl Into<String>) -> Self {
        self.excludes = Some(excludes.into());
        self
    }

    /// Set the `inclusive` flag.
    pub fn inclusive(mut self, inclusive: bool) -> Self {
        self.inclusive = inclusive;
        self
    }

    /// Set the sorting rank.
    pub fn rank(mut self, rank: u8) -> Self {
        self.rank = rank;
        self
    }
}

/// The full description of a document kind.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SchemaSpec {
    /// Node type declarations, in priority order. The order decides which type
    /// a content expression picks as a default.
    pub nodes: Vec<NodeTypeSpec>,
    /// Mark type declarations.
    pub marks: Vec<MarkTypeSpec>,
    /// Name of the top (document) node type. Defaults to the first node type
    /// named `doc`, or the first declared type.
    pub top_node: Option<String>,
}

impl SchemaSpec {
    /// An empty spec.
    pub fn new() -> Self {
        SchemaSpec::default()
    }

    /// Append a node type.
    pub fn node(mut self, node: NodeTypeSpec) -> Self {
        self.nodes.push(node);
        self
    }

    /// Append a mark type.
    pub fn mark(mut self, mark: MarkTypeSpec) -> Self {
        self.marks.push(mark);
        self
    }

    /// Set the top node type name.
    pub fn top_node(mut self, name: impl Into<String>) -> Self {
        self.top_node = Some(name.into());
        self
    }
}
