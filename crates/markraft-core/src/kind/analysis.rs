//! Read-only document semantics, independent of windows and layout.
//!
//! An analysis belongs to one projection and one set of semantic options.
//! Callers refresh it only after accepting a complete transaction chain; the
//! projection can then be read without recursively evaluating state fields.

use std::sync::Arc;

use crate::projection::Projection;

use super::{DocTypes, equations::EquationIndex};

/// Options that affect document meaning rather than screen geometry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AnalysisOptions {
    /// Assign consecutive numbers to standalone display equations.
    pub auto_number_equations: bool,
}

/// A consistent snapshot of projected content and its derived references.
#[derive(Clone)]
pub struct DocumentAnalysis {
    projection: Arc<Projection>,
    equations: Arc<EquationIndex>,
    options: AnalysisOptions,
}

impl DocumentAnalysis {
    /// Analyze one immutable projection without creating an editor view.
    pub fn new(projection: Arc<Projection>, types: &DocTypes, options: AnalysisOptions) -> Self {
        let equations = Arc::new(EquationIndex::build(
            &projection,
            types,
            options.auto_number_equations,
        ));
        Self {
            projection,
            equations,
            options,
        }
    }

    /// The projection whose coordinates all results use.
    pub fn projection(&self) -> &Arc<Projection> {
        &self.projection
    }

    /// Resolved equation numbers, references and diagnostics.
    pub fn equations(&self) -> &Arc<EquationIndex> {
        &self.equations
    }

    /// Options used to construct these results.
    pub fn options(&self) -> AnalysisOptions {
        self.options
    }

    /// Refresh after an accepted editing state or semantic option change. Selection,
    /// font and viewport changes reuse the exact same analysis allocations.
    /// `types` must describe the same schema throughout this analysis's life.
    pub fn sync(
        &mut self,
        projection: Arc<Projection>,
        types: &DocTypes,
        options: AnalysisOptions,
    ) -> bool {
        if Arc::ptr_eq(&self.projection, &projection) && self.options == options {
            return false;
        }
        *self = Self::new(projection, types, options);
        true
    }
}
