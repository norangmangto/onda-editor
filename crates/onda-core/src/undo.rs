use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{document::Document, selection::Selection, transaction::Transaction};

#[derive(Debug, Error)]
pub enum UndoHistoryError {
    #[error("nothing to undo")]
    NothingToUndo,
    #[error("nothing to redo")]
    NothingToRedo,
    #[error("document error: {0}")]
    Document(#[from] crate::document::DocumentError),
}

/// A node in the undo tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct UndoNode {
    /// Index of the parent node, or `None` for a root node.
    parent: Option<usize>,
    /// Indices of child nodes.
    children: Vec<usize>,
    /// Monotonically increasing sequence number; lower == older.
    seq: u64,
    forward: Transaction,
    inverse: Transaction,
    selection_before: Selection,
    selection_after: Selection,
    /// Group ID: nodes with the same non-zero group_id are undone/redone together.
    group_id: u32,
}

/// A tree-based undo/redo history.
///
/// Branching happens whenever a new edit is pushed while `current` is not
/// the latest node on a branch (i.e. after one or more undos).  The new node
/// becomes a new child of `current`, preserving the old branch.
///
/// `undo()` walks toward the root (parent); `redo()` walks toward the
/// most-recently-used child (highest `seq` among direct children).
/// `undo_older` / `undo_newer` traverse the flat sequence order across ALL
/// branches, enabling Vim-style `g-` / `g+` navigation.
#[derive(Serialize, Deserialize)]
pub struct UndoTree {
    nodes: Vec<UndoNode>,
    /// Index of the node we are currently "at" (i.e. the last applied node).
    /// `None` means we are at the root (no changes applied).
    current: Option<usize>,
    /// Current active group id (0 = no group).
    current_group: u32,
    next_group_id: u32,
    /// Global sequence counter.
    next_seq: u64,
}

/// Public alias kept for API compatibility.
pub type UndoHistory = UndoTree;

impl Default for UndoTree {
    fn default() -> Self {
        Self::new()
    }
}

impl UndoTree {
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            current: None,
            current_group: 0,
            next_group_id: 1,
            next_seq: 1,
        }
    }

    // ── Internal helpers ───────────────────────────────────────────────────

    fn alloc_seq(&mut self) -> u64 {
        let s = self.next_seq;
        self.next_seq += 1;
        s
    }

    /// Index of the node with the smallest `seq` that is strictly greater than
    /// `seq_threshold`.  Returns `None` if no such node exists.
    fn find_next_seq_after(&self, seq_threshold: u64) -> Option<usize> {
        self.nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.seq > seq_threshold)
            .min_by_key(|(_, n)| n.seq)
            .map(|(i, _)| i)
    }

    /// Index of the node with the largest `seq` that is strictly less than
    /// `seq_threshold`.  Returns `None` if no such node exists.
    fn find_prev_seq_before(&self, seq_threshold: u64) -> Option<usize> {
        self.nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.seq < seq_threshold)
            .max_by_key(|(_, n)| n.seq)
            .map(|(i, _)| i)
    }

    // ── Public API ─────────────────────────────────────────────────────────

    /// Record a transaction that has just been applied to `doc`.
    pub fn push(
        &mut self,
        forward: Transaction,
        inverse: Transaction,
        selection_before: Selection,
        selection_after: Selection,
    ) {
        let seq = self.alloc_seq();
        let parent = self.current;
        let idx = self.nodes.len();
        self.nodes.push(UndoNode {
            parent,
            children: Vec::new(),
            seq,
            forward,
            inverse,
            selection_before,
            selection_after,
            group_id: self.current_group,
        });
        // Register as child of the current node (if any).
        if let Some(p) = parent {
            self.nodes[p].children.push(idx);
        }
        self.current = Some(idx);
    }

    /// Begin an insert-mode grouping run.  Entries pushed while a group is
    /// active share the same `group_id` and are undone/redone atomically.
    pub fn begin_group(&mut self) {
        if self.current_group == 0 {
            self.current_group = self.next_group_id;
            self.next_group_id += 1;
        }
    }

    /// End the current grouping run.
    pub fn end_group(&mut self) {
        self.current_group = 0;
    }

    /// Undo the last transaction (or group) by walking toward the tree root.
    ///
    /// Returns the selection to restore, or an error if there is nothing to undo.
    pub fn undo(&mut self, doc: &mut Document) -> Result<Selection, UndoHistoryError> {
        let idx = self.current.ok_or(UndoHistoryError::NothingToUndo)?;
        let gid = self.nodes[idx].group_id;

        doc.apply(&self.nodes[idx].inverse.clone())?;
        let mut sel = self.nodes[idx].selection_before.clone();
        self.current = self.nodes[idx].parent;

        // Undo all contiguous nodes in the same group (walking toward root).
        if gid != 0 {
            while let Some(cur) = self.current {
                if self.nodes[cur].group_id != gid {
                    break;
                }
                doc.apply(&self.nodes[cur].inverse.clone())?;
                sel = self.nodes[cur].selection_before.clone();
                self.current = self.nodes[cur].parent;
            }
        }

        Ok(sel)
    }

    /// Redo by walking to the most-recently-used child (highest `seq`).
    ///
    /// Returns the selection to restore, or an error if there is nothing to redo.
    pub fn redo(&mut self, doc: &mut Document) -> Result<Selection, UndoHistoryError> {
        // Determine candidate children.
        let children: Vec<usize> = match self.current {
            None => {
                // We are at the virtual root; look for top-level nodes (parent == None).
                self.nodes
                    .iter()
                    .enumerate()
                    .filter(|(_, n)| n.parent.is_none())
                    .map(|(i, _)| i)
                    .collect()
            }
            Some(cur) => self.nodes[cur].children.clone(),
        };

        if children.is_empty() {
            return Err(UndoHistoryError::NothingToRedo);
        }

        // Pick the child with the highest seq (most recently created / visited).
        // INVARIANT: children is non-empty, so unwrap is safe.
        let next_idx = *children.iter().max_by_key(|&&i| self.nodes[i].seq).unwrap();
        let gid = self.nodes[next_idx].group_id;

        doc.apply(&self.nodes[next_idx].forward.clone())?;
        let mut sel = self.nodes[next_idx].selection_after.clone();
        self.current = Some(next_idx);

        // Redo all contiguous nodes in the same group (walking toward leaves).
        if gid != 0 {
            loop {
                let cur = self.current.unwrap(); // set just above
                let next_children: Vec<usize> = self.nodes[cur].children.clone();
                let Some(&child_idx) = next_children.iter().max_by_key(|&&i| self.nodes[i].seq)
                else {
                    break;
                };
                if self.nodes[child_idx].group_id != gid {
                    break;
                }
                doc.apply(&self.nodes[child_idx].forward.clone())?;
                sel = self.nodes[child_idx].selection_after.clone();
                self.current = Some(child_idx);
            }
        }

        Ok(sel)
    }

    /// Walk to the chronologically **previous** node (by `seq`) across any branch,
    /// applying its inverse to `doc`.  This is the Vim `g-` analogue.
    pub fn undo_older(&mut self, doc: &mut Document) -> Result<Selection, UndoHistoryError> {
        let current_seq = match self.current {
            None => return Err(UndoHistoryError::NothingToUndo),
            Some(idx) => self.nodes[idx].seq,
        };

        // Find the node with the largest seq < current_seq.
        let target = self
            .find_prev_seq_before(current_seq)
            .ok_or(UndoHistoryError::NothingToUndo)?;

        // Walk from `current` up to the common ancestor, then down to `target`.
        // Collect the path from `current` to root and from `target` to root.
        let path_from = |start: Option<usize>, nodes: &Vec<UndoNode>| -> Vec<usize> {
            let mut path = Vec::new();
            let mut cur = start;
            while let Some(idx) = cur {
                path.push(idx);
                cur = nodes[idx].parent;
            }
            path
        };

        let from_path = path_from(self.current, &self.nodes);
        let to_path = path_from(Some(target), &self.nodes);

        // Find LCA (lowest common ancestor) — it is the first element that appears
        // in both paths (from_path starts at current, to_path starts at target).
        let to_path_set: std::collections::HashSet<usize> = to_path.iter().copied().collect();
        let lca: Option<usize> = from_path.iter().copied().find(|n| to_path_set.contains(n));

        // Undo from `current` up to (but not including) LCA.
        let undo_until = lca;
        let mut sel = {
            let cur = self.current.unwrap();
            self.nodes[cur].selection_before.clone()
        };
        loop {
            if self.current == undo_until {
                break;
            }
            let Some(cur) = self.current else { break };
            doc.apply(&self.nodes[cur].inverse.clone())?;
            sel = self.nodes[cur].selection_before.clone();
            self.current = self.nodes[cur].parent;
        }

        // Redo from LCA down to `target`.
        // Build the path from LCA to `target`.
        let mut redo_path: Vec<usize> = Vec::new();
        {
            // Walk to_path from target toward root; stop when we hit LCA.
            let mut cur = Some(target);
            while cur != lca {
                let idx = cur.unwrap();
                redo_path.push(idx);
                cur = self.nodes[idx].parent;
            }
        }
        redo_path.reverse(); // now it goes from just-below-LCA down to target

        for &idx in &redo_path {
            doc.apply(&self.nodes[idx].forward.clone())?;
            sel = self.nodes[idx].selection_after.clone();
            self.current = Some(idx);
        }

        Ok(sel)
    }

    /// Walk to the chronologically **next** node (by `seq`) across any branch,
    /// applying its forward transaction to `doc`.  This is the Vim `g+` analogue.
    pub fn undo_newer(&mut self, doc: &mut Document) -> Result<Selection, UndoHistoryError> {
        let current_seq = match self.current {
            None => 0,
            Some(idx) => self.nodes[idx].seq,
        };

        let target = self
            .find_next_seq_after(current_seq)
            .ok_or(UndoHistoryError::NothingToRedo)?;

        let path_from = |start: Option<usize>, nodes: &Vec<UndoNode>| -> Vec<usize> {
            let mut path = Vec::new();
            let mut cur = start;
            while let Some(idx) = cur {
                path.push(idx);
                cur = nodes[idx].parent;
            }
            path
        };

        let from_path = path_from(self.current, &self.nodes);
        let to_path = path_from(Some(target), &self.nodes);

        let to_path_set: std::collections::HashSet<usize> = to_path.iter().copied().collect();
        let lca: Option<usize> = from_path.iter().copied().find(|n| to_path_set.contains(n));

        // Undo from `current` up to (but not including) LCA.
        let undo_until = lca;
        let mut sel = match self.current {
            None => {
                // Already at virtual root; nothing to undo upward.
                // sel will be overwritten by the redo phase.
                Selection::point(0)
            }
            Some(cur) => self.nodes[cur].selection_before.clone(),
        };
        loop {
            if self.current == undo_until {
                break;
            }
            let Some(cur) = self.current else { break };
            doc.apply(&self.nodes[cur].inverse.clone())?;
            sel = self.nodes[cur].selection_before.clone();
            self.current = self.nodes[cur].parent;
        }

        // Redo from LCA down to `target`.
        let mut redo_path: Vec<usize> = Vec::new();
        {
            let mut cur = Some(target);
            while cur != lca {
                let idx = cur.unwrap();
                redo_path.push(idx);
                cur = self.nodes[idx].parent;
            }
        }
        redo_path.reverse();

        for &idx in &redo_path {
            doc.apply(&self.nodes[idx].forward.clone())?;
            sel = self.nodes[idx].selection_after.clone();
            self.current = Some(idx);
        }

        Ok(sel)
    }

    pub fn can_undo(&self) -> bool {
        self.current.is_some()
    }

    pub fn can_redo(&self) -> bool {
        match self.current {
            None => self.nodes.iter().any(|n| n.parent.is_none()),
            Some(idx) => !self.nodes[idx].children.is_empty(),
        }
    }

    /// Total number of nodes in the tree.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{document::Document, selection::Selection, transaction::Transaction};

    fn apply_insert(doc: &mut Document, history: &mut UndoTree, text: &str) {
        let sel_before = Selection::point(doc.len_chars());
        let cs = crate::transaction::ChangeSetBuilder::new(doc.len_chars())
            .retain(doc.len_chars())
            .insert(text)
            .build();
        let tx = Transaction::new(cs);
        let inverse = doc.apply(&tx).unwrap();
        let sel_after = Selection::point(doc.len_chars());
        history.push(tx, inverse, sel_before, sel_after);
    }

    #[test]
    fn undo_redo_basic() {
        let mut doc = Document::new_empty();
        let mut history = UndoTree::new();

        apply_insert(&mut doc, &mut history, "hello");
        assert_eq!(doc.rope().to_string(), "hello");

        apply_insert(&mut doc, &mut history, " world");
        assert_eq!(doc.rope().to_string(), "hello world");

        history.undo(&mut doc).unwrap();
        assert_eq!(doc.rope().to_string(), "hello");

        history.undo(&mut doc).unwrap();
        assert_eq!(doc.rope().to_string(), "");

        assert!(history.undo(&mut doc).is_err());

        history.redo(&mut doc).unwrap();
        assert_eq!(doc.rope().to_string(), "hello");

        history.redo(&mut doc).unwrap();
        assert_eq!(doc.rope().to_string(), "hello world");

        assert!(history.redo(&mut doc).is_err());
    }

    #[test]
    fn redo_picks_most_recent_branch() {
        // After undo, a new edit creates a branch.  redo() should follow the
        // *new* (higher-seq) branch, not the old one — matching the "most recently
        // used child" policy.
        let mut doc = Document::new_empty();
        let mut history = UndoTree::new();

        apply_insert(&mut doc, &mut history, "a"); // node 0, seq 1
        history.undo(&mut doc).unwrap(); // back to root

        apply_insert(&mut doc, &mut history, "b"); // node 1, seq 2  (new branch)
        history.undo(&mut doc).unwrap(); // back to root

        // redo() should pick node 1 (seq 2) over node 0 (seq 1).
        history.redo(&mut doc).unwrap();
        assert_eq!(doc.rope().to_string(), "b");
    }

    #[test]
    fn redo_cleared_on_new_edit() {
        let mut doc = Document::new_empty();
        let mut history = UndoTree::new();

        apply_insert(&mut doc, &mut history, "a");
        apply_insert(&mut doc, &mut history, "b");
        history.undo(&mut doc).unwrap();

        // New edit creates a new branch; the old "b" branch still exists in the
        // tree but redo() will pick the new (higher-seq) branch.
        apply_insert(&mut doc, &mut history, "c");
        // The new branch IS the redo target, so can_redo is false from this position
        // (we are at the leaf of the new branch).
        assert!(!history.can_redo());
        assert_eq!(doc.rope().to_string(), "ac");
    }

    #[test]
    fn group_undone_together() {
        let mut doc = Document::new_empty();
        let mut history = UndoTree::new();

        apply_insert(&mut doc, &mut history, "a");
        history.begin_group();
        apply_insert(&mut doc, &mut history, "b");
        history.begin_group(); // idempotent while group is open
        apply_insert(&mut doc, &mut history, "c");
        assert_eq!(doc.rope().to_string(), "abc");

        // Undo should take us back to "a" (b and c were grouped)
        history.undo(&mut doc).unwrap();
        assert_eq!(doc.rope().to_string(), "a");
    }

    #[test]
    fn undo_older_crosses_branch() {
        // edit_A → undo → edit_B  then  undo_older should reach the A state.
        let mut doc = Document::new_empty();
        let mut history = UndoTree::new();

        // Apply edit A.
        apply_insert(&mut doc, &mut history, "A"); // node 0, seq 1
        let state_after_a = doc.rope().to_string(); // "A"

        // Undo edit A (back to root).
        history.undo(&mut doc).unwrap();
        assert_eq!(doc.rope().to_string(), "");

        // Apply edit B on the new branch.
        apply_insert(&mut doc, &mut history, "B"); // node 1, seq 2
        assert_eq!(doc.rope().to_string(), "B");

        // undo_older should traverse back to the state after edit A.
        history.undo_older(&mut doc).unwrap();
        assert_eq!(doc.rope().to_string(), state_after_a);
    }

    #[test]
    fn undo_newer_crosses_branch() {
        let mut doc = Document::new_empty();
        let mut history = UndoTree::new();

        apply_insert(&mut doc, &mut history, "A"); // seq 1
        history.undo(&mut doc).unwrap();
        apply_insert(&mut doc, &mut history, "B"); // seq 2
        history.undo(&mut doc).unwrap(); // back to root

        // undo_newer from root should go to seq 1 (A) — the oldest not-yet-applied.
        // Wait — from root (current = None, current_seq = 0) the next seq is 1 (A).
        history.undo_newer(&mut doc).unwrap();
        assert_eq!(doc.rope().to_string(), "A");

        // Another undo_newer should go to seq 2 (B branch).
        history.undo_newer(&mut doc).unwrap();
        assert_eq!(doc.rope().to_string(), "B");
    }
}
