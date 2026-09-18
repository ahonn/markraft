//! A general-purpose rich-text document model: an immutable tree, a
//! data-driven schema, and a change system in original-document coordinates.
//!
//! # The model
//!
//! A document is a [`Node`] tree with value semantics. Nodes have no identity
//! and no parent pointer; an edit produces a new tree that shares every
//! untouched subtree with the old one, so cloning and comparing are cheap.
//!
//! # Positions
//!
//! Positions are integer token offsets over the tree:
//!
//! * a container contributes an open token and a close token,
//! * a text leaf contributes one token per Unicode scalar value (`char`),
//! * any other leaf contributes one token.
//!
//! The start of the document is 0 and the end is `doc.content_size()`: the top
//! node's own open and close tokens sit outside the coordinate frame.
//! [`Node::resolve`] turns an offset into a [`ResolvedPos`] carrying the
//! ancestor chain.
//!
//! # Changes
//!
//! A [`ChangeSet`] holds a set of [`Change`]s whose positions all refer to the
//! *starting* document — changes in one set never compensate for each other.
//! Every structural edit is a token-level edit: splitting a block inserts a
//! close and an open token, joining deletes them, wrapping inserts an open
//! token before and a close token after a range, and lifting deletes the
//! wrapper's two tokens. [`Fit`] repairs a replacement whose token run would
//! not produce a valid document.
//!
//! Change sets support [`ChangeSet::apply`], [`ChangeSet::map_pos`],
//! [`ChangeSet::compose`], [`ChangeSet::invert`] and [`ChangeSet::transform`].
//!
//! # Marks
//!
//! Marks belong to inline content, and which marks may appear is declared by
//! the node that *holds* the content
//! ([`NodeType::allows_mark_in_content`]) — so a schema forbids emphasis inside
//! code by giving `code_block` an empty mark list, not by restricting the text
//! type that every textblock shares. [`ChangeSet::create`] resolves a mark
//! change against the document, recording it only where the parent allows it,
//! and [`Fit`] strips marks from content that lands somewhere they are not
//! allowed.
//!
//! # Schema
//!
//! Nothing here knows about Markdown, HTML or any other concrete document kind.
//! A [`SchemaSpec`] declares node types, mark types, groups, attributes and
//! content expressions at runtime, and [`Schema::new`] compiles it.
//!
//! ```
//! use markraft_doc::{MarkTypeSpec, NodeTypeSpec, Schema, SchemaSpec};
//!
//! let schema = Schema::new(
//!     SchemaSpec::new()
//!         .node(NodeTypeSpec::new("doc", "block+"))
//!         .node(NodeTypeSpec::new("paragraph", "inline*").group("block"))
//!         .node(NodeTypeSpec::text("text").group("inline"))
//!         .mark(MarkTypeSpec::new("strong")),
//! )
//! .unwrap();
//!
//! let doc = schema.doc([schema.node("paragraph", [schema.text("hi")]).unwrap()]).unwrap();
//! assert_eq!(doc.content_size(), 4);
//! doc.check(&schema).unwrap();
//! ```
//!
//! # Known limitations
//!
//! * [`ChangeSet::transform`] guarantees that both rebased sets apply and
//!   leave a valid document, and that changes touching disjoint stretches of
//!   the document converge on the same result. It does **not** guarantee
//!   convergence when the two changes touch overlapping ranges and at least one
//!   of them has to be repaired: each side repairs against a document the other
//!   never saw, and there is no document that honours both intents. Neither
//!   order loses either side's inserted content. See
//!   `transform_diverges_only_on_overlapping_conflicts` for the pinned case.
//! * Repairing a rebase costs one application and one validation of the result,
//!   so [`ChangeSet::transform`] is priced for rebasing, not for every edit.
//! * [`ChangeSet::compose`] applies a later mark change to content the earlier
//!   change inserted by marking the inserted nodes. When the mark type is one a
//!   *container* type allows and that container straddles the insertion, the
//!   composed result marks the inner nodes where sequential application would
//!   have marked the container.
//! * Repairing a change can widen it ([`Fit`]). When the widened range reaches
//!   over another change in the same set, [`ChangeSet::create`] reports
//!   [`ChangeError::FitConflict`] rather than guessing how to merge the two;
//!   put such changes in separate sets and [`ChangeSet::compose`] them.

mod attr;
mod build;
mod change;
mod error;
mod fit;
mod fragment;
mod json;
mod mark;
mod node;
mod pos;
mod schema;
mod slice;

#[cfg(test)]
mod tests;

pub use attr::{AttrKind, AttrSpec, AttrValue, Attrs};
pub use change::{
    Change, ChangeDesc, ChangeKind, ChangeRange, ChangeSet, MappedRange, MarkChange, TrackMode,
};
pub use error::{ChangeError, NodeError, SchemaError};
pub use fit::Fit;
pub use fragment::Fragment;
pub use mark::{Mark, MarkSet};
pub use node::{Markup, Node, NodeVisitor};
pub use pos::{NodeRange, ResolvedPos};
pub use schema::{
    ContentExpr, ContentMatch, MarkType, MarkTypeId, MarkTypeSpec, NodeType, NodeTypeId,
    NodeTypeSpec, Schema, SchemaSpec,
};
pub use slice::{Slice, Token, min_prefix_delta, node_tokens, tokens_cut, tokens_size};
