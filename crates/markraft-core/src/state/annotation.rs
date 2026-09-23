//! Annotations: typed metadata about a transaction as a whole.
//!
//! Use an annotation for something that describes the transaction — when it
//! happened, what caused it, whether the history should record it. Use a
//! [`StateEffect`](super::StateEffect) for something that happens *alongside*
//! the transaction's changes and therefore has to be mapped through them.

use std::marker::PhantomData;
use std::sync::Arc;

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
    /// The id of this annotation's type.
    ///
    /// Two annotations with the same id annotate the same thing, which is what
    /// lets one replace the other when specs are merged.
    pub fn type_id(&self) -> u64 {
        self.ty
    }

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
