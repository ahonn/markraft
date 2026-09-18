//! Sorted sets of ranges and points that survive document changes.
//!
//! Both sets are values: mapping returns a new set, so a decoration set kept in
//! a state field is updated the same way everything else in this crate is.
//!
//! # Mapping
//!
//! A range's ends are mapped with the association its flags describe. An
//! *inclusive* end absorbs content inserted exactly at it — the start sticks
//! before the insertion, the end after it — while an exclusive end pushes it
//! out. A range whose content is deleted entirely collapses and is dropped; a
//! range that was already empty stays, because an empty range marks a position
//! rather than content.
//!
//! A point is mapped with its own side as the association, so a widget on the
//! left of a position stays left of whatever is inserted there.

use crate::change::{ChangeDesc, TrackMode};

/// One range in a [`RangeSet`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeItem<T> {
    /// Start of the range.
    pub from: usize,
    /// End of the range.
    pub to: usize,
    /// Whether content inserted at [`RangeItem::from`] joins the range.
    pub inclusive_start: bool,
    /// Whether content inserted at [`RangeItem::to`] joins the range.
    pub inclusive_end: bool,
    /// The value the range carries.
    pub value: T,
}

impl<T> RangeItem<T> {
    /// A range whose ends both push inserted content out.
    pub fn new(from: usize, to: usize, value: T) -> RangeItem<T> {
        RangeItem {
            from: from.min(to),
            to: from.max(to),
            inclusive_start: false,
            inclusive_end: false,
            value,
        }
    }

    /// Set both inclusivity flags.
    pub fn inclusive(mut self, start: bool, end: bool) -> RangeItem<T> {
        self.inclusive_start = start;
        self.inclusive_end = end;
        self
    }

    /// Whether the range covers nothing.
    pub fn is_empty(&self) -> bool {
        self.from == self.to
    }

    /// Whether the range overlaps or touches `from..to`.
    pub fn touches(&self, from: usize, to: usize) -> bool {
        self.to >= from && self.from <= to
    }
}

/// A set of ranges, sorted by start and then end.
///
/// Ranges may overlap: layering is the caller's business, and a view that needs
/// non-overlapping runs asks for
/// [`Decoration::inline_pieces`](super::Decoration::inline_pieces).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeSet<T> {
    items: Vec<RangeItem<T>>,
}

impl<T> Default for RangeSet<T> {
    fn default() -> RangeSet<T> {
        RangeSet { items: Vec::new() }
    }
}

impl<T: Clone> RangeSet<T> {
    /// The empty set.
    pub fn new() -> RangeSet<T> {
        RangeSet::default()
    }

    /// A set holding `items`.
    pub fn from_items(items: impl IntoIterator<Item = RangeItem<T>>) -> RangeSet<T> {
        let mut items: Vec<RangeItem<T>> = items.into_iter().collect();
        items.sort_by_key(|item| (item.from, item.to));
        RangeSet { items }
    }

    /// A copy of this set with `items` added.
    pub fn add(&self, items: impl IntoIterator<Item = RangeItem<T>>) -> RangeSet<T> {
        let mut all = self.items.clone();
        all.extend(items);
        RangeSet::from_items(all)
    }

    /// A copy of this set without the ranges `drop` accepts.
    pub fn remove(&self, drop: impl Fn(&RangeItem<T>) -> bool) -> RangeSet<T> {
        RangeSet {
            items: self
                .items
                .iter()
                .filter(|item| !drop(item))
                .cloned()
                .collect(),
        }
    }

    /// The ranges that overlap or touch `from..to`.
    pub fn find(&self, from: usize, to: usize) -> Vec<&RangeItem<T>> {
        self.items
            .iter()
            .filter(|item| item.touches(from, to))
            .collect()
    }

    /// The ranges that lie entirely inside `from..to`.
    pub fn between(&self, from: usize, to: usize) -> Vec<&RangeItem<T>> {
        self.items
            .iter()
            .filter(|item| item.from >= from && item.to <= to)
            .collect()
    }

    /// Iterate over the ranges, in order.
    pub fn iter(&self) -> std::slice::Iter<'_, RangeItem<T>> {
        self.items.iter()
    }

    /// The ranges as a slice.
    pub fn as_slice(&self) -> &[RangeItem<T>] {
        &self.items
    }

    /// The number of ranges.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether the set holds no ranges.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Move every range through `changes`, dropping the ones whose content is
    /// gone. See the module documentation for the exact rules.
    pub fn map(&self, changes: &ChangeDesc) -> RangeSet<T> {
        let end = changes.length_after();
        let mut out = Vec::with_capacity(self.items.len());
        for item in &self.items {
            let start_assoc = if item.inclusive_start { -1 } else { 1 };
            let end_assoc = if item.inclusive_end { 1 } else { -1 };
            let from = changes
                .map_pos(item.from, start_assoc, TrackMode::Simple)
                .unwrap_or(end);
            let to = changes
                .map_pos(item.to, end_assoc, TrackMode::Simple)
                .unwrap_or(end);
            if to < from || (to == from && !item.is_empty()) {
                continue;
            }
            out.push(RangeItem {
                from,
                to,
                inclusive_start: item.inclusive_start,
                inclusive_end: item.inclusive_end,
                value: item.value.clone(),
            });
        }
        RangeSet::from_items(out)
    }
}

/// One point in a [`PointSet`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PointItem<T> {
    /// The position.
    pub pos: usize,
    /// Which side of the position the point sits on: `-1` before content
    /// inserted there, `1` after it.
    pub side: i32,
    /// The value the point carries.
    pub value: T,
}

impl<T> PointItem<T> {
    /// A point.
    pub fn new(pos: usize, side: i32, value: T) -> PointItem<T> {
        PointItem {
            pos,
            side: if side < 0 { -1 } else { 1 },
            value,
        }
    }
}

/// A set of positions, sorted by position and then side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PointSet<T> {
    items: Vec<PointItem<T>>,
}

impl<T> Default for PointSet<T> {
    fn default() -> PointSet<T> {
        PointSet { items: Vec::new() }
    }
}

impl<T: Clone> PointSet<T> {
    /// The empty set.
    pub fn new() -> PointSet<T> {
        PointSet::default()
    }

    /// A set holding `items`.
    pub fn from_items(items: impl IntoIterator<Item = PointItem<T>>) -> PointSet<T> {
        let mut items: Vec<PointItem<T>> = items.into_iter().collect();
        items.sort_by_key(|item| (item.pos, item.side));
        PointSet { items }
    }

    /// A copy of this set with `items` added.
    pub fn add(&self, items: impl IntoIterator<Item = PointItem<T>>) -> PointSet<T> {
        let mut all = self.items.clone();
        all.extend(items);
        PointSet::from_items(all)
    }

    /// A copy of this set without the points `drop` accepts.
    pub fn remove(&self, drop: impl Fn(&PointItem<T>) -> bool) -> PointSet<T> {
        PointSet {
            items: self
                .items
                .iter()
                .filter(|item| !drop(item))
                .cloned()
                .collect(),
        }
    }

    /// The points inside `from..=to`.
    pub fn find(&self, from: usize, to: usize) -> Vec<&PointItem<T>> {
        self.items
            .iter()
            .filter(|item| item.pos >= from && item.pos <= to)
            .collect()
    }

    /// Iterate over the points, in order.
    pub fn iter(&self) -> std::slice::Iter<'_, PointItem<T>> {
        self.items.iter()
    }

    /// The points as a slice.
    pub fn as_slice(&self) -> &[PointItem<T>] {
        &self.items
    }

    /// The number of points.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether the set holds no points.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Move every point through `changes`, using its side as the association.
    pub fn map(&self, changes: &ChangeDesc) -> PointSet<T> {
        let end = changes.length_after();
        PointSet::from_items(self.items.iter().map(|item| {
            PointItem {
                pos: changes
                    .map_pos(item.pos, item.side, TrackMode::Simple)
                    .unwrap_or(end),
                side: item.side,
                value: item.value.clone(),
            }
        }))
    }
}
