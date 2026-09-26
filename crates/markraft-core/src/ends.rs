//! How two versions of a list line up after an edit.
//!
//! An edit rebuilds one run of a document's blocks, or of a projection's
//! lines, and hands the ones before and after it over unchanged. Whatever was
//! built for the old list — a layout, a count, a reading — can then be kept
//! for its two ends and built again only for the run between them.
//! [`KeptEnds`] finds those ends once and says where each item of the new list
//! came from, so the callers that keep things across edits do not each count
//! from both ends and translate indices on their own.

use std::ops::Range;

use crate::Node;

/// The items at the start and at the end of a new list that stand, in the same
/// order, at the same end of an old one. The two ends never overlap in either
/// list, so what lies between them is what the edit changed: the old list's
/// [`KeptEnds::old_middle`] became the new list's [`KeptEnds::new_middle`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeptEnds {
    prefix: usize,
    suffix: usize,
    old_len: usize,
    new_len: usize,
}

impl KeptEnds {
    /// Matches `old` and `new` from the front and then from the back, an item
    /// kept where `same` says it is.
    pub fn of<T>(old: &[T], new: &[T], mut same: impl FnMut(&T, &T) -> bool) -> Self {
        let prefix = old.iter().zip(new).take_while(|(a, b)| same(a, b)).count();
        let suffix = old[prefix..]
            .iter()
            .rev()
            .zip(new[prefix..].iter().rev())
            .take_while(|(a, b)| same(a, b))
            .count();
        Self {
            prefix,
            suffix,
            old_len: old.len(),
            new_len: new.len(),
        }
    }

    /// Matches nodes by identity ([`Node::ptr_eq`]): an edit hands the
    /// subtrees it did not touch over as the same nodes.
    pub fn by_identity(old: &[Node], new: &[Node]) -> Self {
        Self::of(old, new, Node::ptr_eq)
    }

    /// How many items both lists share at their start.
    pub fn prefix(&self) -> usize {
        self.prefix
    }

    /// How many items both lists share at their end.
    pub fn suffix(&self) -> usize {
        self.suffix
    }

    /// The old list's items between the kept ends.
    pub fn old_middle(&self) -> Range<usize> {
        self.prefix..self.old_len - self.suffix
    }

    /// The new list's items between the kept ends.
    pub fn new_middle(&self) -> Range<usize> {
        self.prefix..self.new_len - self.suffix
    }

    /// Whether the two lists are the same throughout.
    pub fn is_unchanged(&self) -> bool {
        self.old_middle().is_empty() && self.new_middle().is_empty()
    }

    /// The index in the old list of item `index` of the new one, when that
    /// item is one of the kept ends.
    pub fn old_index(&self, index: usize) -> Option<usize> {
        if index < self.prefix {
            Some(index)
        } else if index < self.new_len && self.new_len - index <= self.suffix {
            Some(self.old_len - (self.new_len - index))
        } else {
            None
        }
    }

    /// Whether item `index` of the new list is one of the kept ends.
    pub fn is_kept(&self, index: usize) -> bool {
        self.old_index(index).is_some()
    }

    /// `held`, one value for each item of the old list, carried over to the
    /// new one: the kept ends take their values along and the middle has none.
    pub fn carry<V>(&self, held: Vec<V>) -> Vec<Option<V>> {
        debug_assert_eq!(held.len(), self.old_len, "one value per old item");
        let mut held: Vec<Option<V>> = held.into_iter().map(Some).collect();
        (0..self.new_len)
            .map(|index| self.old_index(index).and_then(|at| held[at].take()))
            .collect()
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::KeptEnds;

    fn ends(old: &str, new: &str) -> KeptEnds {
        let (old, new): (Vec<char>, Vec<char>) = (old.chars().collect(), new.chars().collect());
        KeptEnds::of(&old, &new, |a, b| a == b)
    }

    #[test]
    fn the_middle_is_what_changed() {
        let kept = ends("abcde", "abXYde");
        assert_eq!((kept.prefix(), kept.suffix()), (2, 2));
        assert_eq!(kept.old_middle(), 2..3);
        assert_eq!(kept.new_middle(), 2..4);
        assert_eq!(
            (0..6)
                .map(|index| kept.old_index(index))
                .collect::<Vec<_>>(),
            [Some(0), Some(1), None, None, Some(3), Some(4)]
        );
    }

    #[test]
    fn the_ends_never_overlap() {
        // A repeated item could be read as both ends; the suffix only counts
        // what the prefix left.
        let kept = ends("aa", "aaa");
        assert_eq!((kept.prefix(), kept.suffix()), (2, 0));
        assert_eq!(kept.new_middle(), 2..3);
        assert!(ends("abc", "abc").is_unchanged());
        assert!(!ends("", "a").is_unchanged());
    }

    #[test]
    fn carried_values_follow_their_items() {
        let kept = ends("abcd", "aXd");
        assert_eq!(kept.carry(vec![1, 2, 3, 4]), [Some(1), None, Some(4)]);
    }
}
