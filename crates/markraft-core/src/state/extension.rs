//! The extension tree: what a configuration is built from.
//!
//! An [`Extension`] is a single state field, a single facet input, a list of
//! extensions, or one of those wrapped in a precedence
//! ([`Prec`]) or a [`Compartment`]. Resolving a configuration flattens the tree
//! and sorts it: explicit precedence first, then position in the flattened
//! tree.
//!
//! Extensions have identity. The same field or facet input included twice takes
//! part once, at the highest precedence it was given.

use std::sync::Arc;

use super::EditorState;
use super::effect::StateEffect;
use super::facet::{FacetProvider, next_id};
use super::field::AnyField;
use super::protocol::compartment_reconfigure;

/// How early an extension is placed in the configuration order.
///
/// `Highest` comes first, `Lowest` last. An extension without a precedence
/// inherits the nearest enclosing one, defaulting to [`Prec::Default`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Prec {
    /// Before everything else.
    Highest,
    /// Before the default.
    High,
    /// The default.
    Default,
    /// After the default.
    Low,
    /// After everything else.
    Lowest,
}

impl Prec {
    /// The five levels, in configuration order.
    pub(crate) const LEVELS: [Prec; 5] = [
        Prec::Highest,
        Prec::High,
        Prec::Default,
        Prec::Low,
        Prec::Lowest,
    ];

    pub(crate) fn rank(self) -> usize {
        match self {
            Prec::Highest => 0,
            Prec::High => 1,
            Prec::Default => 2,
            Prec::Low => 3,
            Prec::Lowest => 4,
        }
    }

    /// Wrap an extension at this precedence.
    pub fn of(self, extension: Extension) -> Extension {
        Extension::new(ExtKind::Prec(self, extension))
    }

    /// Wrap an extension so it comes before everything else.
    pub fn highest(extension: Extension) -> Extension {
        Prec::Highest.of(extension)
    }

    /// Wrap an extension so it comes before the default ones.
    pub fn high(extension: Extension) -> Extension {
        Prec::High.of(extension)
    }

    /// Wrap an extension at the default precedence, overriding an enclosing one.
    pub fn default(extension: Extension) -> Extension {
        Prec::Default.of(extension)
    }

    /// Wrap an extension so it comes after the default ones.
    pub fn low(extension: Extension) -> Extension {
        Prec::Low.of(extension)
    }

    /// Wrap an extension so it comes after everything else.
    pub fn lowest(extension: Extension) -> Extension {
        Prec::Lowest.of(extension)
    }
}

pub(crate) enum ExtKind {
    Field(Arc<dyn AnyField>),
    Provider(Arc<FacetProvider>),
    List(Vec<Extension>),
    Prec(Prec, Extension),
    Compartment(Compartment, Extension),
}

pub(crate) struct ExtensionNode {
    pub(crate) id: u64,
    pub(crate) kind: ExtKind,
}

/// A piece of editor configuration.
///
/// Cloning is a reference-count bump, and a clone keeps the original's
/// identity, so including the same `Extension` value twice is idempotent.
#[derive(Clone)]
pub struct Extension(pub(crate) Arc<ExtensionNode>);

impl Extension {
    fn new(kind: ExtKind) -> Extension {
        Extension(Arc::new(ExtensionNode {
            id: next_id(),
            kind,
        }))
    }

    /// An extension that configures nothing.
    pub fn none() -> Extension {
        Extension::new(ExtKind::List(Vec::new()))
    }

    /// Group several extensions into one.
    pub fn all(items: impl IntoIterator<Item = Extension>) -> Extension {
        Extension::new(ExtKind::List(items.into_iter().collect()))
    }

    /// The extension's unique id, which is what makes it deduplicate.
    pub fn id(&self) -> u64 {
        self.0.id
    }

    pub(crate) fn field(field: Arc<dyn AnyField>) -> Extension {
        // Reuse the field's id so that two `field.extension()` calls produce
        // extensions that deduplicate against each other.
        let id = field.id();
        Extension(Arc::new(ExtensionNode {
            id,
            kind: ExtKind::Field(field),
        }))
    }

    pub(crate) fn provider(provider: Arc<FacetProvider>) -> Extension {
        let id = provider.id;
        Extension(Arc::new(ExtensionNode {
            id,
            kind: ExtKind::Provider(provider),
        }))
    }
}

impl std::fmt::Debug for Extension {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match &self.0.kind {
            ExtKind::Field(_) => "field",
            ExtKind::Provider(_) => "facet input",
            ExtKind::List(_) => "list",
            ExtKind::Prec(..) => "precedence",
            ExtKind::Compartment(..) => "compartment",
        };
        f.debug_struct("Extension")
            .field("id", &self.0.id)
            .field("kind", &kind)
            .finish()
    }
}

impl FromIterator<Extension> for Extension {
    fn from_iter<T: IntoIterator<Item = Extension>>(iter: T) -> Extension {
        Extension::all(iter)
    }
}

struct CompartmentData {
    id: u64,
}

/// A replaceable slot in the configuration.
///
/// Wrapping part of a configuration in a compartment lets a transaction swap
/// exactly that part with [`Compartment::reconfigure`], leaving the rest — and
/// the state fields it configures — untouched.
#[derive(Clone)]
pub struct Compartment(Arc<CompartmentData>);

impl Default for Compartment {
    fn default() -> Compartment {
        Compartment::new()
    }
}

impl Compartment {
    /// A fresh compartment.
    pub fn new() -> Compartment {
        Compartment(Arc::new(CompartmentData { id: next_id() }))
    }

    /// The compartment's unique id.
    pub fn id(&self) -> u64 {
        self.0.id
    }

    /// Put `extension` in this compartment.
    ///
    /// # Panics
    ///
    /// Resolving a configuration that uses one compartment twice panics.
    pub fn of(&self, extension: Extension) -> Extension {
        Extension::new(ExtKind::Compartment(self.clone(), extension))
    }

    /// An effect that replaces this compartment's content.
    pub fn reconfigure(&self, extension: Extension) -> StateEffect {
        compartment_reconfigure().of((self.clone(), extension))
    }

    /// This compartment's current content in `state`, if it is configured.
    pub fn get(&self, state: &EditorState) -> Option<Extension> {
        state.config().compartment(self.id())
    }
}

impl PartialEq for Compartment {
    fn eq(&self, other: &Compartment) -> bool {
        self.0.id == other.0.id
    }
}

impl Eq for Compartment {}

impl std::fmt::Debug for Compartment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Compartment")
            .field("id", &self.0.id)
            .finish()
    }
}
