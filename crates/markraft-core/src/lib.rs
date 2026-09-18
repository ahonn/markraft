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
//! use markraft_core::{MarkTypeSpec, NodeTypeSpec, Schema, SchemaSpec};
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
//! # Known limitations of the change system
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
//! # Editor state
//!
//! [`EditorState`] adds the editing layer on top of the model: a selection, a
//! configuration built from [`Extension`]s, [`Facet`]s and [`StateField`]s, and
//! [`Transaction`]s that produce the next state. [`history`] records the
//! inverted change sets, [`composition`] tracks IME marked text, and
//! [`corrections`] repair shapes the schema alone cannot forbid — running until
//! they have nothing left to ask for. A
//! [`transaction_appender`] reacts to a finished transaction with another one;
//! [`EditorState::update_with_appended`] is what returns the whole chain.
//!
//! # Editing, presentation and layout
//!
//! Three namespaced modules sit on top of that:
//!
//! * [`commands`] — the catalogue of editing operations, as pure functions
//!   from a state to a [`TransactionSpec`], plus input rules.
//! * [`decorations`] — presentation attached to ranges, points and node types
//!   without changing the document.
//! * [`projection`] — a flat, line-oriented view of a document for renderers
//!   and for the platform text APIs that think in lines and UTF-16.
//!
//! # A document kind, as an editing surface sees it
//!
//! [`DocTypeNames`] and [`Codecs`] are the two things a view needs of a
//! concrete document kind: the names its schema gives the roles the view knows
//! about, and how a [`Slice`] becomes text, markup or HTML and reads back.
//! Neither names a document kind or a platform, so a view is written against
//! them and a host supplies the implementations.
//!
//! # Known limitations
//!
//! * Repairing a change can widen it ([`Fit`]). When the widened range reaches
//!   over another change in the same set, [`ChangeSet::create`] reports
//!   [`ChangeError::FitConflict`] rather than guessing how to merge the two;
//!   put such changes in separate sets and [`ChangeSet::compose`] them.

mod attr;
mod build;
mod change;
pub mod commands;
mod composition;
mod corrections;
pub mod decorations;
mod error;
mod fit;
mod fragment;
mod history;
mod json;
mod kind;
mod mark;
mod node;
mod pos;
pub mod projection;
mod schema;
mod selection;
mod slice;
mod state;

#[cfg(test)]
mod tests;

pub use attr::{AttrKind, AttrSpec, AttrValue, Attrs};
pub use change::{
    Change, ChangeDesc, ChangeKind, ChangeRange, ChangeSet, MappedRange, MarkChange, TrackMode,
};
pub use error::{ChangeError, NodeError, SchemaError};
pub use fit::Fit;
pub use fragment::Fragment;
pub use kind::{Codecs, DocTypeNames};
pub use mark::{Mark, MarkSet};
pub use node::{Markup, Node, NodeVisitor};
pub use pos::{NodeRange, ResolvedPos};
pub use schema::{
    ContentExpr, ContentMatch, MarkType, MarkTypeId, MarkTypeSpec, NodeType, NodeTypeId,
    NodeTypeSpec, Schema, SchemaSpec,
};
pub use slice::{Slice, Token, min_prefix_delta, node_tokens, tokens_cut, tokens_size};

pub use composition::{
    COMPOSE_USER_EVENT, CompositionRange, composition, composition_field, composition_range,
    end_composition, finish_composition, is_composing, set_composition_range, start_composition,
    update_composition,
};
pub use corrections::{
    Correction, CorrectionContext, CorrectionTrigger, MAX_CORRECTION_ROUNDS, collect_corrections,
    correction, corrections, corrections_diverged, fill_required_content,
};
pub use history::{
    HistoryConfig, HistoryState, InvertedEffectsFn, IsolateHistory, begin_undo_group,
    end_undo_group, history, history_config, history_field, inverted_effects, isolate,
    isolate_history, redo, redo_depth, redo_selection, undo, undo_depth, undo_selection,
};
pub use selection::{Selection, SelectionKind, SelectionRange};
pub use state::{
    Annotation, AnnotationType, Appended, ChangeFilterFn, ChangeFilterResult, Compartment,
    Configuration, Dep, EditorState, EditorStateConfig, Extension, Facet, FacetConfig,
    MAX_APPENDED_TRANSACTIONS, Prec, StateEffect, StateEffectType, StateError, StateField,
    StateFieldConfig, StateJsonFields, Transaction, TransactionAppenderFn, TransactionExtenderFn,
    TransactionFilterFn, TransactionSpec, add_to_history, append_config, appended,
    appenders_diverged, change_filter, compartment_reconfigure, origin, reconfigure, remote, time,
    transaction_appender, transaction_extender, transaction_filter, user_event,
};
