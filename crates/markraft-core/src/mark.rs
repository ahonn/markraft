//! Marks and canonically ordered mark sets.
//!
//! Each inline node carries a canonical set of marks. Schemas may also use
//! inline containers to preserve nested semantic scopes, including the same
//! mark on both an ancestor and its child. A [`MarkSet`] is sorted by
//! `(rank, type id, attrs)` within each individual scope.

use std::borrow::Borrow;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use crate::attr::Attrs;
use crate::schema::{MarkTypeId, Schema};

/// A mark: a mark type plus its attributes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Mark {
    /// The mark type.
    pub ty: MarkTypeId,
    /// The mark's attributes.
    pub attrs: Attrs,
}

impl Mark {
    /// A mark with no attributes.
    pub fn new(ty: MarkTypeId) -> Mark {
        Mark {
            ty,
            attrs: Attrs::empty(),
        }
    }

    /// A mark with attributes.
    pub fn with_attrs(ty: MarkTypeId, attrs: Attrs) -> Mark {
        Mark { ty, attrs }
    }

    fn sort_key(&self, schema: &Schema) -> (u8, u16) {
        let ty = schema.mark_type(self.ty);
        (ty.rank(), self.ty.0)
    }
}

/// A canonically ordered set holding at most one mark per mark type.
///
/// Cloning is a reference-count bump; the empty set does not allocate.
#[derive(Debug, Clone, Default)]
pub struct MarkSet(Option<Arc<Vec<Mark>>>);

impl MarkSet {
    /// The set holding exactly `marks`, which are already canonically ordered.
    pub(crate) fn from_sorted(marks: Vec<Mark>) -> MarkSet {
        if marks.is_empty() {
            MarkSet(None)
        } else {
            MarkSet(Some(Arc::new(marks)))
        }
    }

    /// The empty mark set.
    pub fn empty() -> MarkSet {
        MarkSet(None)
    }

    /// Build a set by adding each mark in turn, applying exclusion rules.
    pub fn from_marks(schema: &Schema, marks: impl IntoIterator<Item = Mark>) -> MarkSet {
        let mut set = MarkSet::empty();
        for mark in marks {
            set = set.add(schema, mark);
        }
        set
    }

    /// Whether the set holds no marks.
    pub fn is_empty(&self) -> bool {
        self.0.is_none()
    }

    /// The number of marks in the set.
    pub fn len(&self) -> usize {
        self.0.as_ref().map_or(0, |v| v.len())
    }

    /// Iterate over the marks in canonical order.
    pub fn iter(&self) -> impl Iterator<Item = &Mark> {
        self.0.iter().flat_map(|v| v.iter())
    }

    /// The marks as a slice, in canonical order.
    pub fn as_slice(&self) -> &[Mark] {
        self.0.as_ref().map_or(&[], |v| v.as_slice())
    }

    /// Whether an equal mark (same type *and* attributes) is in the set.
    pub fn contains(&self, mark: &Mark) -> bool {
        self.iter().any(|m| m == mark)
    }

    /// Whether some mark of the given type is in the set.
    pub fn contains_type(&self, ty: MarkTypeId) -> bool {
        self.iter().any(|m| m.ty == ty)
    }

    /// The mark of the given type, if present.
    pub fn get(&self, ty: MarkTypeId) -> Option<&Mark> {
        self.iter().find(|m| m.ty == ty)
    }

    /// Add `mark`, honouring `excludes`.
    ///
    /// A mark of the same type is replaced. When an existing mark excludes the
    /// new one, the set is returned unchanged; otherwise every mark the new one
    /// excludes is dropped.
    pub fn add(&self, schema: &Schema, mark: Mark) -> MarkSet {
        let new_type = schema.mark_type(mark.ty);
        let new_key = mark.sort_key(schema);
        let mut out: Vec<Mark> = Vec::with_capacity(self.len() + 1);
        let mut placed = false;
        for other in self.iter() {
            if *other == mark {
                return self.clone();
            }
            if new_type.excludes(other.ty) {
                continue;
            }
            if schema.mark_type(other.ty).excludes(mark.ty) {
                return self.clone();
            }
            if !placed && other.sort_key(schema) > new_key {
                out.push(mark.clone());
                placed = true;
            }
            out.push(other.clone());
        }
        if !placed {
            out.push(mark);
        }
        schema.shared_set(out)
    }

    /// Remove the mark equal to `mark`, if present.
    pub fn remove(&self, mark: &Mark) -> MarkSet {
        if !self.contains(mark) {
            return self.clone();
        }
        let rest: Vec<Mark> = self.iter().filter(|m| *m != mark).cloned().collect();
        if rest.is_empty() {
            MarkSet::empty()
        } else {
            MarkSet(Some(Arc::new(rest)))
        }
    }

    /// Remove any mark of the given type.
    pub fn remove_type(&self, ty: MarkTypeId) -> MarkSet {
        if !self.contains_type(ty) {
            return self.clone();
        }
        let rest: Vec<Mark> = self.iter().filter(|m| m.ty != ty).cloned().collect();
        if rest.is_empty() {
            MarkSet::empty()
        } else {
            MarkSet(Some(Arc::new(rest)))
        }
    }

    /// Keep only the marks that `keep` accepts.
    pub fn filter(&self, keep: impl Fn(&Mark) -> bool) -> MarkSet {
        if self.iter().all(&keep) {
            return self.clone();
        }
        let rest: Vec<Mark> = self.iter().filter(|m| keep(m)).cloned().collect();
        if rest.is_empty() {
            MarkSet::empty()
        } else {
            MarkSet(Some(Arc::new(rest)))
        }
    }

    /// Whether both sets are the very same allocation, not only equal.
    #[cfg(test)]
    pub(crate) fn shares(&self, other: &MarkSet) -> bool {
        match (&self.0, &other.0) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }

    /// Whether two sets hold the same marks. Equivalent to `==`, spelled out to
    /// match the guide's vocabulary.
    pub fn same_set(&self, other: &MarkSet) -> bool {
        self == other
    }
}

impl PartialEq for MarkSet {
    fn eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b) || a == b,
            (None, None) => true,
            _ => false,
        }
    }
}

impl Eq for MarkSet {}

/// Hashes as its marks do, so that a set can be looked up by a slice of marks
/// before one is allocated for it.
impl Hash for MarkSet {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_slice().hash(state);
    }
}

impl Borrow<[Mark]> for MarkSet {
    fn borrow(&self) -> &[Mark] {
        self.as_slice()
    }
}

impl PartialOrd for MarkSet {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for MarkSet {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.as_slice().cmp(other.as_slice())
    }
}
