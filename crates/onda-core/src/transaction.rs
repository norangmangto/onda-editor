use ropey::Rope;
use thiserror::Error;

use crate::selection::{Assoc, Selection};

#[derive(Debug, Error)]
pub enum ChangeSetError {
    #[error("changeset length mismatch: expected doc length {expected}, got {actual}")]
    LengthMismatch { expected: usize, actual: usize },
    #[error("changeset ops would exceed document length")]
    OutOfBounds,
}

/// A single operation in a changeset.
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    /// Keep N characters unchanged.
    Retain(usize),
    /// Insert text at the current position.
    Insert(String),
    /// Delete N characters.
    Delete(usize),
}

/// An ordered sequence of ops that transforms one document into another.
///
/// All positions are in Unicode scalar values (char indices), matching ropey's API.
#[derive(Debug, Clone, Default)]
pub struct ChangeSet {
    ops: Vec<Op>,
    /// Length of the document before the changeset is applied.
    len_before: usize,
    /// Length of the document after the changeset is applied.
    len_after: usize,
}

impl ChangeSet {
    /// Create an identity changeset for a document of the given length.
    pub fn new(len: usize) -> Self {
        Self { ops: Vec::new(), len_before: len, len_after: len }
    }

    pub fn len_before(&self) -> usize {
        self.len_before
    }

    pub fn len_after(&self) -> usize {
        self.len_after
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    pub fn ops(&self) -> &[Op] {
        &self.ops
    }

    /// Append a retain operation, merging with the previous retain if possible.
    fn push_retain(&mut self, n: usize) {
        if n == 0 {
            return;
        }
        match self.ops.last_mut() {
            Some(Op::Retain(last)) => *last += n,
            _ => self.ops.push(Op::Retain(n)),
        }
    }

    /// Append an insert operation, merging with the previous insert if possible.
    fn push_insert(&mut self, s: String) {
        if s.is_empty() {
            return;
        }
        let char_count = s.chars().count();
        match self.ops.last_mut() {
            Some(Op::Insert(last)) => last.push_str(&s),
            _ => self.ops.push(Op::Insert(s)),
        }
        self.len_after += char_count;
    }

    /// Append a delete operation, merging with the previous delete if possible.
    fn push_delete(&mut self, n: usize) {
        if n == 0 {
            return;
        }
        match self.ops.last_mut() {
            Some(Op::Delete(last)) => *last += n,
            _ => self.ops.push(Op::Delete(n)),
        }
        self.len_after = self.len_after.saturating_sub(n);
    }

    /// Apply this changeset to a rope in-place.
    ///
    /// `orig_pos` tracks position in the original document.
    /// `new_pos` tracks position in the accumulating modified document.
    pub fn apply(&self, rope: &mut Rope) -> Result<(), ChangeSetError> {
        if rope.len_chars() != self.len_before {
            return Err(ChangeSetError::LengthMismatch {
                expected: self.len_before,
                actual: rope.len_chars(),
            });
        }

        let mut new_pos = 0usize;
        let mut orig_pos = 0usize;

        for op in &self.ops {
            match op {
                Op::Retain(n) => {
                    orig_pos += n;
                    new_pos += n;
                }
                Op::Insert(s) => {
                    rope.insert(new_pos, s);
                    new_pos += s.chars().count();
                }
                Op::Delete(n) => {
                    if new_pos + n > rope.len_chars() {
                        return Err(ChangeSetError::OutOfBounds);
                    }
                    rope.remove(new_pos..new_pos + n);
                    orig_pos += n;
                }
            }
        }

        Ok(())
    }

    /// Invert this changeset given the original document text, producing the undo changeset.
    pub fn invert(&self, original: &Rope) -> Self {
        let mut inv = ChangeSet::new(self.len_after);
        let mut pos = 0usize;

        for op in &self.ops {
            match op {
                Op::Retain(n) => {
                    inv.push_retain(*n);
                    pos += n;
                }
                Op::Insert(s) => {
                    let n = s.chars().count();
                    inv.push_delete(n);
                }
                Op::Delete(n) => {
                    let text: String = original.chars_at(pos).take(*n).collect();
                    inv.push_insert(text);
                    pos += n;
                }
            }
        }

        inv.len_before = self.len_after;
        inv.len_after = self.len_before;
        inv
    }

    /// Compose two changesets: apply `self` first, then `other`.
    ///
    /// Panics in debug if `self.len_after != other.len_before`.
    pub fn compose(&self, other: &ChangeSet) -> ChangeSet {
        debug_assert_eq!(self.len_after, other.len_before, "changeset lengths don't match");

        let mut result = ChangeSet::new(self.len_before);
        result.len_before = self.len_before;
        result.len_after = other.len_after;
        result.ops = compose_ops(&self.ops, self.len_before, &other.ops, other.len_before);
        result
    }

    /// Map a position in the pre-change document to its position in the post-change document.
    pub fn map_pos(&self, pos: usize, assoc: Assoc) -> usize {
        let mut orig = 0usize;
        let mut new = 0usize;

        for op in &self.ops {
            match op {
                Op::Retain(n) => {
                    if orig + n > pos || (assoc == Assoc::Before && orig + n == pos) {
                        return new + (pos - orig);
                    }
                    orig += n;
                    new += n;
                }
                Op::Insert(s) => {
                    let n = s.chars().count();
                    if assoc == Assoc::Before && orig == pos {
                        return new;
                    }
                    new += n;
                }
                Op::Delete(n) => {
                    if orig + n > pos {
                        return new;
                    }
                    orig += n;
                }
            }
        }
        new + (pos.saturating_sub(orig))
    }
}

/// Compose two op sequences. This is the O(m+n) algorithm.
fn compose_ops(
    a_ops: &[Op],
    a_len_before: usize,
    b_ops: &[Op],
    _b_len_before: usize,
) -> Vec<Op> {
    let mut result: Vec<Op> = Vec::new();

    // After applying A, we have a document of length A.len_after.
    // B operates on that document.
    // We need to merge A and B into a single op list on the original document.

    // Strategy: expand A's output character by character and track what B does to each.
    // In practice we work in runs:

    let mut ai = 0usize;
    let mut bi = 0usize;
    let mut a_offset = 0usize; // consumed in current a op
    let mut b_offset = 0usize; // consumed in current b op

    let a_len = a_ops.len();
    let b_len = b_ops.len();

    let push_retain = |v: &mut Vec<Op>, n: usize| {
        if n == 0 {
            return;
        }
        match v.last_mut() {
            Some(Op::Retain(last)) => *last += n,
            _ => v.push(Op::Retain(n)),
        }
    };
    let push_insert = |v: &mut Vec<Op>, s: String| {
        if s.is_empty() {
            return;
        }
        match v.last_mut() {
            Some(Op::Insert(last)) => last.push_str(&s),
            _ => v.push(Op::Insert(s)),
        }
    };
    let push_delete = |v: &mut Vec<Op>, n: usize| {
        if n == 0 {
            return;
        }
        match v.last_mut() {
            Some(Op::Delete(last)) => *last += n,
            _ => v.push(Op::Delete(n)),
        }
    };

    while ai < a_len || bi < b_len {
        // Handle A inserts first (they don't consume original doc)
        if ai < a_len {
            if let Op::Insert(s) = &a_ops[ai] {
                let remaining = s.chars().count() - a_offset;
                // B might retain, delete, or have exhausted this insert
                if bi < b_len {
                    match &b_ops[bi] {
                        Op::Retain(bn) => {
                            let take = remaining.min(*bn - b_offset);
                            let chunk: String = s.chars().skip(a_offset).take(take).collect();
                            push_insert(&mut result, chunk);
                            a_offset += take;
                            b_offset += take;
                            if a_offset >= s.chars().count() {
                                ai += 1;
                                a_offset = 0;
                            }
                            if b_offset >= *bn {
                                bi += 1;
                                b_offset = 0;
                            }
                        }
                        Op::Delete(bn) => {
                            let take = remaining.min(*bn - b_offset);
                            a_offset += take;
                            b_offset += take;
                            if a_offset >= s.chars().count() {
                                ai += 1;
                                a_offset = 0;
                            }
                            if b_offset >= *bn {
                                bi += 1;
                                b_offset = 0;
                            }
                        }
                        Op::Insert(_) => {
                            // B inserts before consuming A's insert; handle below
                        }
                    }
                } else {
                    // B is exhausted; keep A's remaining insert
                    let chunk: String = s.chars().skip(a_offset).collect();
                    push_insert(&mut result, chunk);
                    ai += 1;
                    a_offset = 0;
                }
                continue;
            }
        }

        // Handle B inserts (don't consume intermediate doc)
        if bi < b_len {
            if let Op::Insert(s) = &b_ops[bi] {
                push_insert(&mut result, s.clone());
                bi += 1;
                b_offset = 0;
                continue;
            }
        }

        if ai >= a_len || bi >= b_len {
            // Drain the non-empty side
            if ai < a_len {
                match &a_ops[ai] {
                    Op::Retain(n) => {
                        push_retain(&mut result, n - a_offset);
                        ai += 1;
                        a_offset = 0;
                    }
                    Op::Delete(n) => {
                        push_delete(&mut result, n - a_offset);
                        ai += 1;
                        a_offset = 0;
                    }
                    Op::Insert(_) => unreachable!("handled above"),
                }
            }
            if bi < b_len {
                match &b_ops[bi] {
                    Op::Retain(n) => {
                        push_retain(&mut result, n - b_offset);
                        bi += 1;
                        b_offset = 0;
                    }
                    Op::Delete(n) => {
                        push_delete(&mut result, n - b_offset);
                        bi += 1;
                        b_offset = 0;
                    }
                    Op::Insert(_) => unreachable!("handled above"),
                }
            }
            continue;
        }

        // Both A and B have non-insert ops
        match (&a_ops[ai], &b_ops[bi]) {
            (Op::Retain(an), Op::Retain(bn)) => {
                let a_rem = an - a_offset;
                let b_rem = bn - b_offset;
                let take = a_rem.min(b_rem);
                push_retain(&mut result, take);
                a_offset += take;
                b_offset += take;
                if a_offset >= *an {
                    ai += 1;
                    a_offset = 0;
                }
                if b_offset >= *bn {
                    bi += 1;
                    b_offset = 0;
                }
            }
            (Op::Retain(an), Op::Delete(bn)) => {
                let a_rem = an - a_offset;
                let b_rem = bn - b_offset;
                let take = a_rem.min(b_rem);
                push_delete(&mut result, take);
                a_offset += take;
                b_offset += take;
                if a_offset >= *an {
                    ai += 1;
                    a_offset = 0;
                }
                if b_offset >= *bn {
                    bi += 1;
                    b_offset = 0;
                }
            }
            (Op::Delete(an), Op::Retain(_)) => {
                let a_rem = an - a_offset;
                push_delete(&mut result, a_rem);
                ai += 1;
                a_offset = 0;
            }
            (Op::Delete(an), Op::Delete(_)) => {
                let a_rem = an - a_offset;
                push_delete(&mut result, a_rem);
                ai += 1;
                a_offset = 0;
            }
            (Op::Insert(_), _) | (_, Op::Insert(_)) => unreachable!("handled above"),
            (Op::Retain(_), Op::Retain(_)) => unreachable!(),
        }
    }

    result
}

/// Builder for constructing a [`ChangeSet`] incrementally.
pub struct ChangeSetBuilder {
    cs: ChangeSet,
}

impl ChangeSetBuilder {
    pub fn new(len: usize) -> Self {
        Self { cs: ChangeSet::new(len) }
    }

    pub fn retain(mut self, n: usize) -> Self {
        self.cs.push_retain(n);
        self
    }

    pub fn insert(mut self, s: impl Into<String>) -> Self {
        self.cs.push_insert(s.into());
        self
    }

    pub fn delete(mut self, n: usize) -> Self {
        self.cs.push_delete(n);
        self
    }

    pub fn build(mut self) -> ChangeSet {
        // Ensure trailing retains are normalised away
        while matches!(self.cs.ops.last(), Some(Op::Retain(_))) {
            self.cs.ops.pop();
        }
        self.cs
    }
}

/// A transaction pairs a [`ChangeSet`] with the resulting [`Selection`].
#[derive(Debug, Clone)]
pub struct Transaction {
    pub changes: ChangeSet,
    /// Selection after the transaction is applied. `None` means: map the current selection
    /// through the changeset automatically.
    pub selection: Option<Selection>,
}

impl Transaction {
    pub fn new(changes: ChangeSet) -> Self {
        Self { changes, selection: None }
    }

    pub fn with_selection(mut self, sel: Selection) -> Self {
        self.selection = Some(sel);
        self
    }

    /// Invert this transaction to produce its undo counterpart.
    pub fn invert(&self, original: &Rope) -> Transaction {
        let inv_changes = self.changes.invert(original);
        Transaction { changes: inv_changes, selection: None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ropey::Rope;

    #[test]
    fn apply_insert_at_start() {
        let mut rope = Rope::from_str("world");
        let cs = ChangeSetBuilder::new(5).insert("hello ").build();
        cs.apply(&mut rope).unwrap();
        assert_eq!(rope.to_string(), "hello world");
    }

    #[test]
    fn apply_delete_range() {
        let mut rope = Rope::from_str("hello world");
        let cs = ChangeSetBuilder::new(11).retain(5).delete(6).build();
        cs.apply(&mut rope).unwrap();
        assert_eq!(rope.to_string(), "hello");
    }

    #[test]
    fn apply_replace() {
        let mut rope = Rope::from_str("foo bar");
        let cs = ChangeSetBuilder::new(7).delete(3).insert("baz").retain(4).build();
        cs.apply(&mut rope).unwrap();
        assert_eq!(rope.to_string(), "baz bar");
    }

    #[test]
    fn invert_roundtrip() {
        let original = Rope::from_str("hello world");
        let cs = ChangeSetBuilder::new(11).retain(6).delete(5).insert("earth").build();
        let mut rope = original.clone();
        cs.apply(&mut rope).unwrap();
        assert_eq!(rope.to_string(), "hello earth");

        let inv = cs.invert(&original);
        inv.apply(&mut rope).unwrap();
        assert_eq!(rope.to_string(), "hello world");
    }

    #[test]
    fn map_pos_retain() {
        let cs = ChangeSetBuilder::new(10).retain(5).insert("XX").retain(5).build();
        // pos 0..5 unchanged
        assert_eq!(cs.map_pos(3, Assoc::After), 3);
        // pos 5: before the insert
        assert_eq!(cs.map_pos(5, Assoc::Before), 5);
        // pos 5: after the insert
        assert_eq!(cs.map_pos(5, Assoc::After), 7);
        // pos 6 → 8
        assert_eq!(cs.map_pos(6, Assoc::After), 8);
    }

    #[test]
    fn map_pos_delete() {
        let cs = ChangeSetBuilder::new(10).retain(3).delete(4).retain(3).build();
        assert_eq!(cs.map_pos(2, Assoc::After), 2);
        assert_eq!(cs.map_pos(4, Assoc::After), 3); // deleted → land at deletion
        assert_eq!(cs.map_pos(7, Assoc::After), 3);
        assert_eq!(cs.map_pos(8, Assoc::After), 4);
    }

    #[test]
    fn length_mismatch_error() {
        let mut rope = Rope::from_str("hello");
        let cs = ChangeSetBuilder::new(10).retain(10).build();
        assert!(cs.apply(&mut rope).is_err());
    }
}
