//! Data-driven schema: node types, mark types, groups and content rules.
//!
//! A [`SchemaSpec`] is plain data; [`Schema::new`] validates it and compiles it
//! into interned types with small ids. Nothing in this crate hard-codes a
//! concrete document kind — Markdown, HTML or anything else is expressed as a
//! spec by a consumer.

mod content;
mod spec;

pub use content::{ContentExpr, ContentMatch};
pub use spec::{MarkTypeSpec, NodeTypeSpec, SchemaSpec};

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::attr::{AttrSpec, AttrValue, Attrs};
use crate::error::{NodeError, SchemaError};

/// Interned identifier of a node type within one [`Schema`].
///
/// Ids are assigned in spec order, so they are stable for a given spec but
/// meaningless across schemas. They can only be obtained from a schema, which
/// is what lets the lookups be infallible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeTypeId(pub(crate) u16);

impl NodeTypeId {
    /// The id as a dense index, for side tables keyed by node type.
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// Interned identifier of a mark type within one [`Schema`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MarkTypeId(pub(crate) u16);

impl MarkTypeId {
    /// The id as a dense index, for side tables keyed by mark type.
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// A compiled node type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeType {
    id: NodeTypeId,
    name: String,
    groups: Vec<String>,
    inline: bool,
    is_text: bool,
    attrs: Vec<AttrSpec>,
    /// `None` means every mark type is allowed.
    allowed_marks: Option<Vec<MarkTypeId>>,
    atom: bool,
    selectable: bool,
    draggable: bool,
    defining: bool,
    isolating: bool,
    code: bool,
    /// Whether the compiled content expression only mentions inline types.
    inline_content: bool,
    is_leaf: bool,
    default_attrs: Attrs,
    has_required_attrs: bool,
}

impl NodeType {
    /// The type's id.
    pub fn id(&self) -> NodeTypeId {
        self.id
    }

    /// The type's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The groups this type belongs to.
    pub fn groups(&self) -> &[String] {
        &self.groups
    }

    /// Whether nodes of this type are inline.
    pub fn is_inline(&self) -> bool {
        self.inline
    }

    /// Whether nodes of this type are block-level.
    pub fn is_block(&self) -> bool {
        !self.inline
    }

    /// Whether this is the schema's text type.
    pub fn is_text(&self) -> bool {
        self.is_text
    }

    /// Whether this type holds no content.
    pub fn is_leaf(&self) -> bool {
        self.is_leaf
    }

    /// Whether this type's content is inline.
    pub fn has_inline_content(&self) -> bool {
        self.inline_content
    }

    /// Whether this is a block node with inline content.
    pub fn is_textblock(&self) -> bool {
        !self.inline && self.inline_content
    }

    /// Whether the node behaves as a single opaque unit.
    pub fn is_atom(&self) -> bool {
        self.atom || self.is_leaf
    }

    /// Whether the node can be selected as a whole.
    pub fn is_selectable(&self) -> bool {
        self.selectable
    }

    /// Whether the node can be dragged.
    pub fn is_draggable(&self) -> bool {
        self.draggable
    }

    /// Whether the node's identity survives being moved or pasted.
    pub fn is_defining(&self) -> bool {
        self.defining
    }

    /// Whether edits should avoid crossing this node's boundaries.
    pub fn is_isolating(&self) -> bool {
        self.isolating
    }

    /// Whether the node holds code.
    pub fn is_code(&self) -> bool {
        self.code
    }

    /// The attribute declarations of this type.
    pub fn attrs(&self) -> &[AttrSpec] {
        &self.attrs
    }

    /// Attribute map holding every default value, used when a node is created
    /// without explicit attributes.
    pub fn default_attrs(&self) -> &Attrs {
        &self.default_attrs
    }

    /// Whether some attribute has no default and must be supplied.
    pub fn has_required_attrs(&self) -> bool {
        self.has_required_attrs
    }

    /// Whether `mark` may sit on this type's inline content.
    ///
    /// Marks are a property of inline content, and a node's own marks are
    /// governed by its *parent*: a schema says "no marks inside a code block"
    /// by giving `code_block` an empty mark list.
    pub fn allows_mark_in_content(&self, mark: MarkTypeId) -> bool {
        match &self.allowed_marks {
            None => true,
            Some(list) => list.contains(&mark),
        }
    }

    /// Whether this type belongs to `group`.
    pub fn in_group(&self, group: &str) -> bool {
        self.groups.iter().any(|g| g == group)
    }
}

/// A compiled mark type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkType {
    id: MarkTypeId,
    name: String,
    groups: Vec<String>,
    attrs: Vec<AttrSpec>,
    excludes: Vec<MarkTypeId>,
    inclusive: bool,
    rank: u8,
    default_attrs: Attrs,
    has_required_attrs: bool,
}

impl MarkType {
    /// The type's id.
    pub fn id(&self) -> MarkTypeId {
        self.id
    }

    /// The type's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The groups this type belongs to.
    pub fn groups(&self) -> &[String] {
        &self.groups
    }

    /// The attribute declarations of this type.
    pub fn attrs(&self) -> &[AttrSpec] {
        &self.attrs
    }

    /// Attribute map holding every default value.
    pub fn default_attrs(&self) -> &Attrs {
        &self.default_attrs
    }

    /// Whether some attribute has no default.
    pub fn has_required_attrs(&self) -> bool {
        self.has_required_attrs
    }

    /// Whether the mark applies to content typed directly at its end.
    pub fn is_inclusive(&self) -> bool {
        self.inclusive
    }

    /// Sorting rank within a mark set.
    pub fn rank(&self) -> u8 {
        self.rank
    }

    /// Whether adding this mark removes marks of type `other`.
    pub fn excludes(&self, other: MarkTypeId) -> bool {
        self.excludes.contains(&other)
    }
}

#[derive(Debug)]
struct SchemaData {
    nodes: Vec<NodeType>,
    marks: Vec<MarkType>,
    node_by_name: BTreeMap<String, NodeTypeId>,
    mark_by_name: BTreeMap<String, MarkTypeId>,
    content: Vec<ContentExpr>,
    top: NodeTypeId,
    text: Option<NodeTypeId>,
}

/// A compiled schema.
///
/// Cloning a `Schema` is a reference-count bump; two clones of the same schema
/// are interchangeable and compare equal with [`Schema::same`].
#[derive(Debug, Clone)]
pub struct Schema(Arc<SchemaData>);

impl Schema {
    /// Compile a spec, validating names, groups, content expressions and
    /// attribute declarations.
    pub fn new(spec: SchemaSpec) -> Result<Schema, SchemaError> {
        let mut node_by_name: BTreeMap<String, NodeTypeId> = BTreeMap::new();
        for (i, node) in spec.nodes.iter().enumerate() {
            let id = NodeTypeId(u16::try_from(i).map_err(|_| {
                SchemaError::Structure("a schema may hold at most 65536 node types".into())
            })?);
            if node_by_name.insert(node.name.clone(), id).is_some() {
                return Err(SchemaError::DuplicateName {
                    kind: "node",
                    name: node.name.clone(),
                });
            }
        }
        let mut mark_by_name: BTreeMap<String, MarkTypeId> = BTreeMap::new();
        for (i, mark) in spec.marks.iter().enumerate() {
            let id = MarkTypeId(u16::try_from(i).map_err(|_| {
                SchemaError::Structure("a schema may hold at most 65536 mark types".into())
            })?);
            if mark_by_name.insert(mark.name.clone(), id).is_some() {
                return Err(SchemaError::DuplicateName {
                    kind: "mark",
                    name: mark.name.clone(),
                });
            }
        }

        let marks = compile_marks(&spec, &mark_by_name)?;
        let mut nodes = compile_nodes(&spec, &mark_by_name)?;

        // Compile content expressions once every node id exists.
        let mut content = Vec::with_capacity(spec.nodes.len());
        for node in &spec.nodes {
            let expr = ContentExpr::compile(&node.content, &mut |name| {
                resolve_node_name(name, &spec, &node_by_name)
            })
            .map_err(|message| SchemaError::ContentExpr {
                node: node.name.clone(),
                expr: node.content.clone(),
                message,
            })?;
            content.push(expr);
        }

        // Derive leaf-ness and inline-content-ness from the compiled content.
        for (i, expr) in content.iter().enumerate() {
            let mentioned = expr.mentioned_types();
            let inline_content =
                !mentioned.is_empty() && mentioned.iter().all(|t| nodes[t.0 as usize].inline);
            let mixed = !mentioned.is_empty()
                && mentioned.iter().any(|t| nodes[t.0 as usize].inline)
                && mentioned.iter().any(|t| !nodes[t.0 as usize].inline);
            if mixed {
                return Err(SchemaError::InvalidSpec {
                    node: nodes[i].name.clone(),
                    message: "content mixes inline and block node types".into(),
                });
            }
            nodes[i].is_leaf = expr.is_empty();
            nodes[i].inline_content = inline_content;
        }

        // A node type that does not spell out its `marks` allows every mark on
        // its inline content, and none at all when it holds block content. This
        // has to wait until the content expressions are compiled.
        for (i, node) in spec.nodes.iter().enumerate() {
            if node.marks.is_none() {
                nodes[i].allowed_marks = if nodes[i].inline_content {
                    None
                } else {
                    Some(Vec::new())
                };
            }
        }

        let text = nodes.iter().find(|n| n.is_text).map(|n| n.id);
        if nodes.iter().filter(|n| n.is_text).count() > 1 {
            return Err(SchemaError::Structure(
                "a schema may declare at most one text node type".into(),
            ));
        }
        if let Some(text) = text {
            let text_type = &nodes[text.0 as usize];
            if !text_type.inline || !text_type.is_leaf {
                return Err(SchemaError::InvalidSpec {
                    node: text_type.name.clone(),
                    message: "the text type must be an inline leaf".into(),
                });
            }
        }

        let top =
            match &spec.top_node {
                Some(name) => *node_by_name
                    .get(name)
                    .ok_or_else(|| SchemaError::UnknownName {
                        kind: "node",
                        name: name.clone(),
                        context: "top_node".into(),
                    })?,
                None => *node_by_name.get("doc").unwrap_or(
                    &spec.nodes.first().map(|_| NodeTypeId(0)).ok_or_else(|| {
                        SchemaError::Structure("a schema needs a node type".into())
                    })?,
                ),
            };
        if nodes[top.0 as usize].inline {
            return Err(SchemaError::InvalidSpec {
                node: nodes[top.0 as usize].name.clone(),
                message: "the top node type must be a block type".into(),
            });
        }

        Ok(Schema(Arc::new(SchemaData {
            nodes,
            marks,
            node_by_name,
            mark_by_name,
            content,
            top,
            text,
        })))
    }

    /// Whether two handles refer to the same compiled schema.
    pub fn same(&self, other: &Schema) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// All node types, in spec order.
    pub fn node_types(&self) -> &[NodeType] {
        &self.0.nodes
    }

    /// All mark types, in spec order.
    pub fn mark_types(&self) -> &[MarkType] {
        &self.0.marks
    }

    /// The node type with the given id.
    ///
    /// # Panics
    ///
    /// Panics when the id comes from a different schema and is out of range.
    /// Use [`Schema::try_node_type`] when the id's origin is not known.
    pub fn node_type(&self, id: NodeTypeId) -> &NodeType {
        &self.0.nodes[id.0 as usize]
    }

    /// The node type with the given id, or `None` when the id does not belong
    /// to this schema.
    pub fn try_node_type(&self, id: NodeTypeId) -> Option<&NodeType> {
        self.0.nodes.get(id.0 as usize)
    }

    /// The mark type with the given id.
    ///
    /// # Panics
    ///
    /// Panics when the id comes from a different schema and is out of range.
    /// Use [`Schema::try_mark_type`] when the id's origin is not known.
    pub fn mark_type(&self, id: MarkTypeId) -> &MarkType {
        &self.0.marks[id.0 as usize]
    }

    /// The mark type with the given id, or `None` when the id does not belong
    /// to this schema.
    pub fn try_mark_type(&self, id: MarkTypeId) -> Option<&MarkType> {
        self.0.marks.get(id.0 as usize)
    }

    /// Look up a node type by name.
    pub fn node_id(&self, name: &str) -> Option<NodeTypeId> {
        self.0.node_by_name.get(name).copied()
    }

    /// Look up a mark type by name.
    pub fn mark_id(&self, name: &str) -> Option<MarkTypeId> {
        self.0.mark_by_name.get(name).copied()
    }

    /// The top (document) node type.
    pub fn top_type(&self) -> NodeTypeId {
        self.0.top
    }

    /// The text node type, if the schema declares one.
    pub fn text_type(&self) -> Option<NodeTypeId> {
        self.0.text
    }

    /// The compiled content expression of a node type.
    pub fn content_expr(&self, id: NodeTypeId) -> &ContentExpr {
        &self.0.content[id.0 as usize]
    }

    /// The start state of a node type's content automaton.
    pub fn content_match(&self, id: NodeTypeId) -> ContentMatch<'_> {
        self.0.content[id.0 as usize].start()
    }

    /// Whether `parent` may directly contain a child of type `child`
    /// somewhere in its content.
    pub fn can_contain(&self, parent: NodeTypeId, child: NodeTypeId) -> bool {
        self.0.content[parent.0 as usize]
            .mentioned_types()
            .contains(&child)
    }

    /// Whether a type can be created without explicit attributes, which is the
    /// requirement for the model to invent nodes of that type.
    pub fn is_creatable(&self, id: NodeTypeId) -> bool {
        let ty = self.node_type(id);
        !ty.has_required_attrs && !ty.is_text
    }

    /// Whether a type can act as a wrapper: creatable and not a leaf.
    pub fn is_wrappable(&self, id: NodeTypeId) -> bool {
        self.is_creatable(id) && !self.node_type(id).is_leaf
    }

    /// The chain of container types that `child` must be wrapped in to be
    /// allowed at `at`, outermost-first.
    ///
    /// Returns an empty vector when `child` fits directly.
    pub fn find_wrapping(
        &self,
        at: ContentMatch<'_>,
        child: NodeTypeId,
    ) -> Option<Vec<NodeTypeId>> {
        let creatable = |id: NodeTypeId| self.is_wrappable(id);
        let content_of = |id: NodeTypeId| Some(self.content_expr(id));
        at.find_wrapping(child, &creatable, &content_of)
    }

    /// The node types to insert at `at` so `after` becomes valid content.
    pub fn fill_before(
        &self,
        at: ContentMatch<'_>,
        after: &[NodeTypeId],
        to_end: bool,
    ) -> Option<Vec<NodeTypeId>> {
        let creatable = |id: NodeTypeId| self.is_creatable(id) && !self.node_type(id).is_text;
        at.fill_before(after, to_end, &creatable)
    }

    /// The type the model uses when it has to invent a child at `at`.
    pub fn default_type(&self, at: ContentMatch<'_>) -> Option<NodeTypeId> {
        let creatable = |id: NodeTypeId| self.is_creatable(id);
        at.default_type(&creatable)
    }

    /// Complete `attrs` with the type's defaults and reject unknown or
    /// mistyped values.
    pub fn build_node_attrs(&self, id: NodeTypeId, attrs: &Attrs) -> Result<Attrs, NodeError> {
        let ty = self.node_type(id);
        build_attrs(&ty.name, &ty.attrs, &ty.default_attrs, attrs)
    }

    /// Complete `attrs` with the mark type's defaults and reject unknown or
    /// mistyped values.
    pub fn build_mark_attrs(&self, id: MarkTypeId, attrs: &Attrs) -> Result<Attrs, NodeError> {
        let ty = self.mark_type(id);
        build_attrs(&ty.name, &ty.attrs, &ty.default_attrs, attrs)
    }
}

fn build_attrs(
    owner: &str,
    specs: &[AttrSpec],
    defaults: &Attrs,
    given: &Attrs,
) -> Result<Attrs, NodeError> {
    for (name, _) in given.iter() {
        if !specs.iter().any(|s| s.name == name) {
            return Err(NodeError::InvalidAttr {
                attr: name.to_string(),
                owner: owner.to_string(),
                message: "unknown attribute".into(),
            });
        }
    }
    let mut out: Vec<(String, AttrValue)> = Vec::with_capacity(specs.len());
    for spec in specs {
        let value = match given.get(&spec.name) {
            Some(value) => value.clone(),
            None => match defaults.get(&spec.name) {
                Some(value) => value.clone(),
                None => {
                    return Err(NodeError::InvalidAttr {
                        attr: spec.name.clone(),
                        owner: owner.to_string(),
                        message: "required attribute is missing".into(),
                    });
                }
            },
        };
        if !spec.kind.accepts(&value) {
            return Err(NodeError::InvalidAttr {
                attr: spec.name.clone(),
                owner: owner.to_string(),
                message: format!("expected {}", spec.kind),
            });
        }
        out.push((spec.name.clone(), value));
    }
    Ok(Attrs::from_pairs(out))
}

fn resolve_node_name(
    name: &str,
    spec: &SchemaSpec,
    node_by_name: &BTreeMap<String, NodeTypeId>,
) -> Option<Vec<NodeTypeId>> {
    if let Some(id) = node_by_name.get(name) {
        return Some(vec![*id]);
    }
    let members: Vec<NodeTypeId> = spec
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| node.group.split_whitespace().any(|g| g == name))
        .map(|(i, _)| NodeTypeId(i as u16))
        .collect();
    if members.is_empty() {
        None
    } else {
        Some(members)
    }
}

fn resolve_mark_names(
    list: &str,
    spec: &SchemaSpec,
    mark_by_name: &BTreeMap<String, MarkTypeId>,
    context: &str,
) -> Result<Vec<MarkTypeId>, SchemaError> {
    let mut out = Vec::new();
    for name in list.split_whitespace() {
        if let Some(id) = mark_by_name.get(name) {
            if !out.contains(id) {
                out.push(*id);
            }
            continue;
        }
        let members: Vec<MarkTypeId> = spec
            .marks
            .iter()
            .enumerate()
            .filter(|(_, mark)| mark.group.split_whitespace().any(|g| g == name))
            .map(|(i, _)| MarkTypeId(i as u16))
            .collect();
        if members.is_empty() {
            return Err(SchemaError::UnknownName {
                kind: "mark",
                name: name.to_string(),
                context: context.to_string(),
            });
        }
        for id in members {
            if !out.contains(&id) {
                out.push(id);
            }
        }
    }
    Ok(out)
}

fn default_attrs_of(specs: &[AttrSpec]) -> (Attrs, bool) {
    let pairs: Vec<(String, AttrValue)> = specs
        .iter()
        .filter_map(|s| s.default.clone().map(|v| (s.name.clone(), v)))
        .collect();
    let required = specs.iter().any(|s| s.default.is_none());
    (Attrs::from_pairs(pairs), required)
}

fn compile_marks(
    spec: &SchemaSpec,
    mark_by_name: &BTreeMap<String, MarkTypeId>,
) -> Result<Vec<MarkType>, SchemaError> {
    let mut out = Vec::with_capacity(spec.marks.len());
    for (i, mark) in spec.marks.iter().enumerate() {
        let id = MarkTypeId(i as u16);
        let excludes = match mark.excludes.as_deref() {
            None => vec![id],
            Some("_") => (0..spec.marks.len())
                .map(|i| MarkTypeId(i as u16))
                .collect(),
            Some("") => Vec::new(),
            Some(list) => resolve_mark_names(
                list,
                spec,
                mark_by_name,
                &format!("excludes of mark `{}`", mark.name),
            )?,
        };
        let (default_attrs, has_required_attrs) = default_attrs_of(&mark.attrs);
        out.push(MarkType {
            id,
            name: mark.name.clone(),
            groups: mark.group.split_whitespace().map(String::from).collect(),
            attrs: mark.attrs.clone(),
            excludes,
            inclusive: mark.inclusive,
            rank: mark.rank,
            default_attrs,
            has_required_attrs,
        });
    }
    Ok(out)
}

fn compile_nodes(
    spec: &SchemaSpec,
    mark_by_name: &BTreeMap<String, MarkTypeId>,
) -> Result<Vec<NodeType>, SchemaError> {
    let mut out = Vec::with_capacity(spec.nodes.len());
    for (i, node) in spec.nodes.iter().enumerate() {
        let id = NodeTypeId(i as u16);
        let allowed_marks = match node.marks.as_deref() {
            Some("_") => None,
            Some("") => Some(Vec::new()),
            Some(list) => Some(resolve_mark_names(
                list,
                spec,
                mark_by_name,
                &format!("marks of node `{}`", node.name),
            )?),
            // Resolved once the content expressions are compiled.
            None => Some(Vec::new()),
        };
        let (default_attrs, has_required_attrs) = default_attrs_of(&node.attrs);
        let mut names = Vec::new();
        for attr in &node.attrs {
            if names.contains(&attr.name) {
                return Err(SchemaError::InvalidSpec {
                    node: node.name.clone(),
                    message: format!("duplicate attribute `{}`", attr.name),
                });
            }
            names.push(attr.name.clone());
        }
        out.push(NodeType {
            id,
            name: node.name.clone(),
            groups: node.group.split_whitespace().map(String::from).collect(),
            inline: node.inline,
            is_text: node.text,
            attrs: node.attrs.clone(),
            allowed_marks,
            atom: node.atom,
            selectable: node.selectable,
            draggable: node.draggable,
            defining: node.defining,
            isolating: node.isolating,
            code: node.code,
            inline_content: false,
            is_leaf: true,
            default_attrs,
            has_required_attrs,
        });
    }
    Ok(out)
}
