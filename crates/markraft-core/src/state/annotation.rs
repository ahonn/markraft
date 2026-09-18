//! Annotations: typed metadata about a transaction as a whole.
//!
//! Use an annotation for something that describes the transaction — when it
//! happened, what caused it, whether the history should record it. Use a
//! [`StateEffect`](super::StateEffect) for something that happens *alongside*
//! the transaction's changes and therefore has to be mapped through them.

use std::marker::PhantomData;
use std::sync::{Arc, LazyLock};

use super::facet::{AnyValue, next_id};

/// The identity and value type of a kind of annotation.
pub struct AnnotationType<T> {
    id: u64,
    marker: PhantomData<fn() -> T>,
}

impl<T> Clone for AnnotationType<T> {
    fn clone(&self) -> AnnotationType<T> {
        *self
    }
}

impl<T> Copy for AnnotationType<T> {}

impl<T> std::fmt::Debug for AnnotationType<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnnotationType")
            .field("id", &self.id)
            .finish()
    }
}

impl<T: Send + Sync + 'static> AnnotationType<T> {
    /// Define a new kind of annotation.
    pub fn define() -> AnnotationType<T> {
        AnnotationType {
            id: next_id(),
            marker: PhantomData,
        }
    }

    /// The type's unique id.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// An annotation of this type carrying `value`.
    pub fn of(&self, value: T) -> Annotation {
        Annotation {
            ty: self.id,
            value: Arc::new(value),
        }
    }
}

/// A tagged value attached to a transaction.
#[derive(Clone)]
pub struct Annotation {
    ty: u64,
    value: AnyValue,
}

impl Annotation {
    /// Whether this annotation has the given type.
    pub fn is<T: Send + Sync + 'static>(&self, ty: &AnnotationType<T>) -> bool {
        self.ty == ty.id
    }

    /// The value, when this annotation has the given type.
    pub fn value<T: Send + Sync + 'static>(&self, ty: &AnnotationType<T>) -> Option<&T> {
        if self.ty == ty.id {
            self.value.downcast_ref::<T>()
        } else {
            None
        }
    }
}

impl std::fmt::Debug for Annotation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Annotation")
            .field("type", &self.ty)
            .finish()
    }
}

static USER_EVENT: LazyLock<AnnotationType<String>> = LazyLock::new(AnnotationType::define);
static ADD_TO_HISTORY: LazyLock<AnnotationType<bool>> = LazyLock::new(AnnotationType::define);
static TIME: LazyLock<AnnotationType<u64>> = LazyLock::new(AnnotationType::define);
static REMOTE: LazyLock<AnnotationType<bool>> = LazyLock::new(AnnotationType::define);
static ORIGIN: LazyLock<AnnotationType<String>> = LazyLock::new(AnnotationType::define);

/// What the user did, as a dotted hierarchy.
///
/// The vocabulary the rest of this crate uses: `input`, `input.type`,
/// `input.type.compose`, `input.paste`, `input.drop`, `delete`,
/// `delete.selection`, `delete.forward`, `delete.backward`, `delete.cut`,
/// `move`, `move.drop`, `select`, `select.pointer`, `select.all`, `undo`,
/// `redo`, `insert`, `mark`, `mark.add`, `mark.remove`, `split`, `wrap`,
/// `unwrap`, `settype`.
///
/// [`Transaction::is_user_event`](super::Transaction::is_user_event) matches on
/// dotted prefixes, so `select` matches `select.pointer`.
pub fn user_event() -> &'static AnnotationType<String> {
    &USER_EVENT
}

/// Whether the undo history should record this transaction.
///
/// Absent means "yes". With `false`, the history maps its entries through the
/// change instead of recording it, which is how a remote or programmatic edit
/// keeps the local history valid.
pub fn add_to_history() -> &'static AnnotationType<bool> {
    &ADD_TO_HISTORY
}

/// When the transaction was created, in milliseconds.
///
/// Added automatically by [`EditorState::update`](super::EditorState::update)
/// when a spec does not supply it. Tests that care about grouping supply it.
pub fn time() -> &'static AnnotationType<u64> {
    &TIME
}

/// Whether the transaction represents another actor's edit.
///
/// Corrections and other transaction extenders that add changes skip remote
/// transactions, because acting on them makes collaborating peers correct the
/// same thing over and over.
pub fn remote() -> &'static AnnotationType<bool> {
    &REMOTE
}

/// A free-form provenance tag, for a host that needs to tell its own edits
/// apart beyond what `user_event` expresses.
pub fn origin() -> &'static AnnotationType<String> {
    &ORIGIN
}

/// Whether `event` is `prefix` or a dotted refinement of it.
pub(crate) fn matches_user_event(event: &str, prefix: &str) -> bool {
    event == prefix
        || (event.len() > prefix.len()
            && event.starts_with(prefix)
            && event.as_bytes()[prefix.len()] == b'.')
}
