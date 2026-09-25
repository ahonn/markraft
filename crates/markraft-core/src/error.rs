//! Error types for schema construction, document validation and change handling.

use thiserror::Error;

/// Failure while compiling a [`SchemaSpec`](crate::SchemaSpec) into a
/// [`Schema`](crate::Schema).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SchemaError {
    /// Two node or mark types share a name.
    #[error("duplicate {kind} type name `{name}`")]
    DuplicateName {
        /// Either `"node"` or `"mark"`.
        kind: &'static str,
        /// The offending name.
        name: String,
    },
    /// A name referenced by a content expression, group list or `excludes`
    /// clause does not resolve to anything in the schema.
    #[error("unknown {kind} `{name}` referenced by `{context}`")]
    UnknownName {
        /// Either `"node"`, `"mark"` or `"group"`.
        kind: &'static str,
        /// The unresolved name.
        name: String,
        /// The spec that referenced it.
        context: String,
    },
    /// A content expression could not be parsed.
    #[error("invalid content expression for `{node}`: {message} (in `{expr}`)")]
    ContentExpr {
        /// Node type whose content expression failed to parse.
        node: String,
        /// The expression text.
        expr: String,
        /// Human readable reason.
        message: String,
    },
    /// The schema declares no top node, or more than one text node.
    #[error("{0}")]
    Structure(String),
    /// A node type mixes properties that cannot be combined.
    #[error("invalid spec for `{node}`: {message}")]
    InvalidSpec {
        /// Node or mark type name.
        node: String,
        /// Human readable reason.
        message: String,
    },
}

/// Failure while building or validating a document node.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum NodeError {
    /// The content of a node does not satisfy its content expression.
    #[error("invalid content for node `{node}`: {message}")]
    InvalidContent {
        /// The parent node type.
        node: String,
        /// Human readable reason.
        message: String,
    },
    /// A mark is not allowed on the node it sits on.
    #[error("mark `{mark}` is not allowed on node `{node}`")]
    MarkNotAllowed {
        /// The mark type name.
        mark: String,
        /// The node type name.
        node: String,
    },
    /// An attribute is missing, unknown, or of the wrong kind.
    #[error("invalid attribute `{attr}` on `{owner}`: {message}")]
    InvalidAttr {
        /// The attribute name.
        attr: String,
        /// Node or mark type name.
        owner: String,
        /// Human readable reason.
        message: String,
    },
    /// Text content is malformed: a text node is empty, is not of the schema's
    /// text type, or a node of that type carries no text; or the text cannot
    /// be inserted where it was asked to go.
    #[error("invalid text node: {0}")]
    InvalidText(String),
    /// A position was outside the range an operation accepts — for example
    /// `0..=doc.content_size()` when resolving, or the content or text length
    /// when cutting — or fell inside a non-text leaf. A reversed range reports
    /// its start.
    #[error("position {pos} is out of range (size {size})")]
    PosOutOfRange {
        /// The requested position.
        pos: usize,
        /// The size of the content or text the position was checked against.
        size: usize,
    },
    /// JSON input did not describe a node in this schema.
    #[error("invalid JSON document: {0}")]
    Json(String),
}

/// Failure while creating or applying a [`ChangeSet`](crate::ChangeSet).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ChangeError {
    /// A change range was reversed or reached past the end of the document.
    #[error("change range {from}..{to} is invalid for a document of size {size}")]
    BadRange {
        /// Range start.
        from: usize,
        /// Range end.
        to: usize,
        /// Size of the starting document.
        size: usize,
    },
    /// Two changes in the same set touch overlapping ranges.
    #[error("changes {a_from}..{a_to} and {b_from}..{b_to} overlap")]
    Overlapping {
        /// Start of the earlier change.
        a_from: usize,
        /// End of the earlier change.
        a_to: usize,
        /// Start of the later change.
        b_from: usize,
        /// End of the later change.
        b_to: usize,
    },
    /// The change set is applied to a document whose size does not match the
    /// size it was created against.
    #[error("change set expects a document of size {expected}, got {actual}")]
    LengthMismatch {
        /// Size recorded in the change set.
        expected: usize,
        /// Size of the document passed to `apply`.
        actual: usize,
    },
    /// The resulting token stream opens or closes more containers than the
    /// surrounding document allows. Fitting can repair this.
    #[error("unbalanced token stream: {0}")]
    Unbalanced(String),
    /// The result of the change is not a valid document under its schema.
    #[error(transparent)]
    Invalid(#[from] NodeError),
    /// Fitting could not turn the replacement into valid content.
    #[error("cannot fit content: {0}")]
    Unfittable(String),
    /// Repairing one change widened it over another change in the same set.
    /// Put the two changes in separate sets and compose them.
    #[error(
        "repairing the change at {from}..{to} widened it to {fitted_from}..{fitted_to}, \
         which covers another change in the same set"
    )]
    FitConflict {
        /// Start of the change as the caller wrote it.
        from: usize,
        /// End of the change as the caller wrote it.
        to: usize,
        /// Start of the repaired replacement.
        fitted_from: usize,
        /// End of the repaired replacement.
        fitted_to: usize,
    },
    /// Two change sets that have to work together were built against different
    /// schemas.
    #[error("the two change sets were created against different schemas")]
    SchemaMismatch,
}
