use smallvec::SmallVec;

use crate::transaction::ChangeSet;

/// Which side of an insert a cursor should land on when position-mapping through a ChangeSet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Assoc {
    /// Land before the inserted text (cursor stays at same visual position).
    Before,
    /// Land after the inserted text (cursor follows the insert).
    After,
}

/// A selection range with an anchor and a head.
///
/// `anchor` is where the selection started; `head` is where the cursor is now.
/// When `anchor == head`, the range represents a pure cursor with no selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Range {
    /// The fixed end of the selection (where it started).
    pub anchor: usize,
    /// The moving end (where the cursor is).
    pub head: usize,
}

impl Range {
    /// Create a cursor (zero-width range) at the given position.
    #[inline]
    pub fn point(pos: usize) -> Self {
        Self {
            anchor: pos,
            head: pos,
        }
    }

    /// Create a range from `anchor` to `head`.
    #[inline]
    pub fn new(anchor: usize, head: usize) -> Self {
        Self { anchor, head }
    }

    /// The lesser of anchor/head.
    #[inline]
    pub fn from(&self) -> usize {
        self.anchor.min(self.head)
    }

    /// The greater of anchor/head.
    #[inline]
    pub fn to(&self) -> usize {
        self.anchor.max(self.head)
    }

    /// `true` when head < anchor (selection extends leftwards).
    #[inline]
    pub fn is_backwards(&self) -> bool {
        self.head < self.anchor
    }

    /// `true` when anchor == head (pure cursor, no selected text).
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    /// Number of characters selected (0 for a pure cursor).
    #[inline]
    pub fn len(&self) -> usize {
        self.to() - self.from()
    }

    /// Flip anchor and head.
    #[inline]
    pub fn flip(&self) -> Range {
        Range {
            anchor: self.head,
            head: self.anchor,
        }
    }

    /// Collapse the range to a cursor at the head position.
    #[inline]
    pub fn collapse_to_head(&self) -> Range {
        Range::point(self.head)
    }

    /// Map this range through a changeset.
    pub fn map(&self, cs: &ChangeSet, assoc: Assoc) -> Range {
        Range {
            anchor: cs.map_pos(self.anchor, assoc),
            head: cs.map_pos(self.head, assoc),
        }
    }

    /// `true` if this range overlaps with `other`.
    pub fn overlaps(&self, other: &Range) -> bool {
        self.from() < other.to() && other.from() < self.to()
    }

    /// `true` if this range contains `pos` (exclusive of `to`).
    pub fn contains(&self, pos: usize) -> bool {
        pos >= self.from() && pos < self.to()
    }

    /// `true` if this range contains `pos` (inclusive of `to`).
    pub fn contains_inclusive(&self, pos: usize) -> bool {
        pos >= self.from() && pos <= self.to()
    }
}

/// A collection of 1..N selection ranges. Multicursor is always first-class (ADR-006).
///
/// Invariants (maintained by [`Selection::normalize`]):
/// - Ranges are sorted by their `from()` position.
/// - Overlapping or adjacent ranges are merged.
/// - There is always at least one range.
#[derive(Debug, Clone, PartialEq)]
pub struct Selection {
    ranges: SmallVec<[Range; 1]>,
    /// Index of the primary (last-active) cursor.
    primary: usize,
}

impl Selection {
    /// Create a selection with a single cursor.
    pub fn point(pos: usize) -> Self {
        Self {
            ranges: SmallVec::from_buf([Range::point(pos)]),
            primary: 0,
        }
    }

    /// Create a selection from a non-empty list of ranges.
    ///
    /// Panics in debug if `ranges` is empty.
    pub fn new(ranges: impl Into<SmallVec<[Range; 1]>>, primary: usize) -> Self {
        let ranges = ranges.into();
        debug_assert!(!ranges.is_empty());
        let primary = primary.min(ranges.len().saturating_sub(1));
        let mut sel = Self { ranges, primary };
        sel.normalize();
        sel
    }

    /// The primary range (where the "main" cursor is).
    #[inline]
    pub fn primary(&self) -> Range {
        self.ranges[self.primary]
    }

    /// Index of the primary range.
    #[inline]
    pub fn primary_index(&self) -> usize {
        self.primary
    }

    /// All ranges.
    #[inline]
    pub fn ranges(&self) -> &[Range] {
        &self.ranges
    }

    /// Number of ranges.
    #[inline]
    pub fn len(&self) -> usize {
        self.ranges.len()
    }

    /// `true` if there are no ranges (should not occur in valid state).
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    /// `true` if there is only one range.
    #[inline]
    pub fn is_single(&self) -> bool {
        self.ranges.len() == 1
    }

    /// Map all ranges through a changeset.
    pub fn map(&self, cs: &ChangeSet) -> Selection {
        let ranges: SmallVec<[Range; 1]> = self
            .ranges
            .iter()
            .map(|r| r.map(cs, Assoc::After))
            .collect();
        let primary = self.primary;
        // After mapping, ranges may overlap/reorder — normalize
        let mut sel = Selection { ranges, primary };
        sel.normalize();
        sel
    }

    /// Collapse all cursors to their head positions (exit visual mode).
    pub fn collapse_to_head(&self) -> Selection {
        let ranges: SmallVec<[Range; 1]> =
            self.ranges.iter().map(Range::collapse_to_head).collect();
        let mut sel = Selection {
            ranges,
            primary: self.primary,
        };
        sel.normalize();
        sel
    }

    /// Replace all ranges with the result of applying `f` to each.
    pub fn transform<F>(&self, f: F) -> Selection
    where
        F: Fn(Range) -> Range,
    {
        let ranges: SmallVec<[Range; 1]> = self.ranges.iter().copied().map(f).collect();
        let mut sel = Selection {
            ranges,
            primary: self.primary,
        };
        sel.normalize();
        sel
    }

    /// Normalize: sort ranges by `from()`, merge overlapping/adjacent ones.
    fn normalize(&mut self) {
        if self.ranges.len() <= 1 {
            return;
        }

        let primary_range = self.ranges[self.primary];

        // Sort by from()
        self.ranges.sort_unstable_by_key(|r| (r.from(), r.to()));

        // Merge overlapping ranges
        let mut merged: SmallVec<[Range; 1]> = SmallVec::new();
        for &r in &self.ranges {
            if let Some(last) = merged.last_mut() {
                if r.from() <= last.to() {
                    // Merge: extend to() to the larger end
                    let new_to = r.to().max(last.to());
                    let new_from = last.from();
                    // Preserve direction of the last range
                    *last = if last.is_backwards() {
                        Range::new(new_to, new_from)
                    } else {
                        Range::new(new_from, new_to)
                    };
                    continue;
                }
            }
            merged.push(r);
        }

        self.ranges = merged;

        // Re-find primary (nearest to original primary_range)
        self.primary = self
            .ranges
            .iter()
            .enumerate()
            .min_by_key(|(_, r)| (r.head as isize - primary_range.head as isize).unsigned_abs())
            .map(|(i, _)| i)
            .unwrap_or(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_from_to() {
        let r = Range::new(5, 3);
        assert_eq!(r.from(), 3);
        assert_eq!(r.to(), 5);
        assert!(r.is_backwards());
    }

    #[test]
    fn range_map_insert_before() {
        let cs = crate::transaction::ChangeSetBuilder::new(10)
            .retain(5)
            .insert("XX")
            .retain(5)
            .build();
        let r = Range::point(5);
        let mapped = r.map(&cs, Assoc::After);
        assert_eq!(mapped.head, 7);
    }

    #[test]
    fn selection_normalize_merge_overlapping() {
        let ranges = vec![Range::new(0, 5), Range::new(3, 8), Range::new(10, 12)];
        let sel = Selection::new(ranges, 0);
        assert_eq!(sel.len(), 2);
        assert_eq!(sel.ranges()[0].from(), 0);
        assert_eq!(sel.ranges()[0].to(), 8);
    }

    #[test]
    fn selection_normalize_sort() {
        let ranges = vec![Range::point(10), Range::point(3), Range::point(7)];
        let sel = Selection::new(ranges, 0);
        assert_eq!(sel.ranges()[0].head, 3);
        assert_eq!(sel.ranges()[1].head, 7);
        assert_eq!(sel.ranges()[2].head, 10);
    }

    #[test]
    fn selection_map_through_insert() {
        let cs = crate::transaction::ChangeSetBuilder::new(5)
            .retain(2)
            .insert("XX")
            .retain(3)
            .build();
        let sel = Selection::point(4);
        let mapped = sel.map(&cs);
        assert_eq!(mapped.primary().head, 6);
    }

    #[test]
    fn single_cursor_invariant() {
        let sel = Selection::point(0);
        assert_eq!(sel.len(), 1);
        assert!(sel.is_single());
    }
}
