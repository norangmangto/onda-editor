use onda_core::document::DocumentId;

/// A single entry in the jump list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JumpPos {
    pub doc_id: DocumentId,
    /// Character-index within the document.
    pub char_pos: usize,
}

impl JumpPos {
    pub fn new(doc_id: DocumentId, char_pos: usize) -> Self {
        Self { doc_id, char_pos }
    }
}

/// A Vim-style jump list (`<C-o>` / `<C-i>`).
///
/// - `push` adds an entry, deduplicates consecutive identical positions, and
///   truncates any "future" entries (those ahead of the current pointer).
/// - `older` moves the pointer back one step and returns the entry there.
/// - `newer` moves the pointer forward one step and returns the entry there.
#[derive(Debug, Default)]
pub struct JumpList {
    entries: Vec<JumpPos>,
    /// Index of the entry that was most recently jumped to (or the tail when
    /// not navigating).
    current: usize,
}

impl JumpList {
    pub fn new() -> Self {
        Self::default()
    }

    /// Push a new position.
    ///
    /// - Truncates entries after `current` (forward history is discarded).
    /// - Deduplicates: if the new position is identical to the last entry,
    ///   it is not added again.
    pub fn push(&mut self, pos: JumpPos) {
        // Truncate future entries.
        if !self.entries.is_empty() {
            self.entries.truncate(self.current + 1);
        }

        // Deduplicate consecutive identical positions.
        if self.entries.last() == Some(&pos) {
            return;
        }

        self.entries.push(pos);
        self.current = self.entries.len() - 1;
    }

    /// Move one step toward older entries.
    ///
    /// `current_pos` is the caller's present location; it is pushed before
    /// the jump so that `newer` can return to it.
    ///
    /// Returns `None` if already at the oldest entry.
    pub fn older(&mut self, current_pos: JumpPos) -> Option<JumpPos> {
        if self.entries.is_empty() {
            return None;
        }

        // If we are sitting at the very end (not currently navigating), save
        // the current position before jumping away so that `newer` can return
        // to it later. This must run *before* the "already at the oldest
        // entry" check below: with exactly one recorded entry, `current == 0`
        // already, but that entry hasn't been visited yet — it's still the
        // single jump target of the very first `<C-o>`, not "no more history".
        if self.current + 1 == self.entries.len() && self.entries.last() != Some(&current_pos) {
            self.entries.push(current_pos);
            self.current = self.entries.len() - 1;
        }

        // Already at the oldest entry — nowhere further back to go.
        if self.current == 0 {
            return None;
        }

        self.current -= 1;
        Some(self.entries[self.current])
    }

    /// Move one step toward newer entries.
    ///
    /// Returns `None` if already at the newest entry.
    pub fn newer(&mut self) -> Option<JumpPos> {
        if self.entries.is_empty() {
            return None;
        }
        if self.current + 1 >= self.entries.len() {
            return None;
        }
        self.current += 1;
        Some(self.entries[self.current])
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id() -> DocumentId {
        DocumentId::new()
    }

    fn jp(char_pos: usize) -> JumpPos {
        JumpPos::new(id(), char_pos)
    }

    #[test]
    fn push_deduplicates() {
        let mut jl = JumpList::new();
        let p = jp(10);
        jl.push(p);
        jl.push(p);
        assert_eq!(jl.entries.len(), 1);
    }

    #[test]
    fn push_truncates_future() {
        let mut jl = JumpList::new();
        let p1 = jp(1);
        let p2 = jp(2);
        let p3 = jp(3);
        jl.push(p1);
        jl.push(p2);
        jl.push(p3);
        // Go back one.
        jl.older(p3);
        // Now push a new entry — p3 (future) should be gone.
        let p4 = jp(4);
        jl.push(p4);
        // entries should be: p1, p2, p3(saved by older), p4
        assert!(jl.newer().is_none());
    }

    #[test]
    fn older_newer_roundtrip() {
        let mut jl = JumpList::new();
        let p1 = jp(1);
        let p2 = jp(2);
        let p3 = jp(3);
        jl.push(p1);
        jl.push(p2);
        jl.push(p3);

        let back1 = jl.older(p3);
        assert!(back1.is_some());
        let fwd = jl.newer();
        assert!(fwd.is_some());
    }

    #[test]
    fn older_undoes_the_very_first_jump() {
        // A single recorded entry must still be reachable: this is the ordinary
        // "one big jump, then <C-o> to go back" case (e.g. `G` then `<C-o>`).
        let mut jl = JumpList::new();
        let p = jp(5);
        jl.push(p);
        let live = jp(99); // where the cursor ended up after the jump
        assert_eq!(jl.older(live), Some(p));
        // `newer` returns to the live position, which `older` auto-saved.
        assert_eq!(jl.newer(), Some(live));
    }

    #[test]
    fn older_returns_none_once_truly_exhausted() {
        let mut jl = JumpList::new();
        let p1 = jp(1);
        jl.push(p1);
        // First call walks back to the one recorded entry (auto-saving the live
        // position first, so there's now history *and* a saved "future").
        assert_eq!(jl.older(jp(99)), Some(p1));
        // Now at the oldest entry — nowhere further back to go.
        assert!(jl.older(jp(99)).is_none());
    }

    #[test]
    fn is_empty_on_new() {
        let jl = JumpList::new();
        assert!(jl.is_empty());
    }
}
