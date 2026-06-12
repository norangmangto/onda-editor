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

/// An entry in the undo stack.
#[derive(Debug, Clone)]
struct UndoEntry {
    forward: Transaction,
    inverse: Transaction,
    selection_before: Selection,
    selection_after: Selection,
    /// Group ID: entries with the same non-zero group_id are undone/redone together.
    group_id: u32,
}

/// A linear undo/redo history backed by a stack.
pub struct UndoHistory {
    entries: Vec<UndoEntry>,
    cursor: usize,
    /// Current active group id (0 = no group).
    current_group: u32,
    next_group_id: u32,
}

impl Default for UndoHistory {
    fn default() -> Self {
        Self::new()
    }
}

impl UndoHistory {
    pub fn new() -> Self {
        Self { entries: Vec::new(), cursor: 0, current_group: 0, next_group_id: 1 }
    }

    /// Record a transaction that has just been applied to `doc`.
    pub fn push(
        &mut self,
        forward: Transaction,
        inverse: Transaction,
        selection_before: Selection,
        selection_after: Selection,
    ) {
        self.entries.truncate(self.cursor);
        self.entries.push(UndoEntry {
            forward,
            inverse,
            selection_before,
            selection_after,
            group_id: self.current_group,
        });
        self.cursor = self.entries.len();
    }

    /// Begin an insert-mode grouping run. Entries pushed while a group is active
    /// share the same group_id and will be undone/redone together.
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

    /// Undo the last transaction (or group), applying the inverse to `doc`.
    ///
    /// Returns the selection to restore, or an error if there's nothing to undo.
    pub fn undo(&mut self, doc: &mut Document) -> Result<Selection, UndoHistoryError> {
        if self.cursor == 0 {
            return Err(UndoHistoryError::NothingToUndo);
        }

        self.cursor -= 1;
        let entry = &self.entries[self.cursor];
        let gid = entry.group_id;
        doc.apply(&entry.inverse)?;
        let mut sel = entry.selection_before.clone();

        // Undo all entries in the same group
        if gid != 0 {
            while self.cursor > 0 && self.entries[self.cursor - 1].group_id == gid {
                self.cursor -= 1;
                let entry = &self.entries[self.cursor];
                doc.apply(&entry.inverse)?;
                sel = entry.selection_before.clone();
            }
        }

        Ok(sel)
    }

    /// Redo the next transaction, applying it to `doc`.
    ///
    /// Returns the selection to restore, or an error if there's nothing to redo.
    pub fn redo(&mut self, doc: &mut Document) -> Result<Selection, UndoHistoryError> {
        if self.cursor >= self.entries.len() {
            return Err(UndoHistoryError::NothingToRedo);
        }

        let entry = &self.entries[self.cursor];
        let gid = entry.group_id;
        doc.apply(&entry.forward)?;
        let mut sel = entry.selection_after.clone();
        self.cursor += 1;

        // Redo all entries in the same group
        if gid != 0 {
            while self.cursor < self.entries.len() && self.entries[self.cursor].group_id == gid {
                let entry = &self.entries[self.cursor];
                doc.apply(&entry.forward)?;
                sel = entry.selection_after.clone();
                self.cursor += 1;
            }
        }

        Ok(sel)
    }

    pub fn can_undo(&self) -> bool {
        self.cursor > 0
    }

    pub fn can_redo(&self) -> bool {
        self.cursor < self.entries.len()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{document::Document, selection::Selection, transaction::Transaction};

    fn apply_insert(doc: &mut Document, history: &mut UndoHistory, text: &str) {
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
        let mut history = UndoHistory::new();

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
    fn redo_cleared_on_new_edit() {
        let mut doc = Document::new_empty();
        let mut history = UndoHistory::new();

        apply_insert(&mut doc, &mut history, "a");
        apply_insert(&mut doc, &mut history, "b");
        history.undo(&mut doc).unwrap();

        // New edit clears redo stack
        apply_insert(&mut doc, &mut history, "c");
        assert!(!history.can_redo());
        assert_eq!(doc.rope().to_string(), "ac");
    }

    #[test]
    fn group_undone_together() {
        let mut doc = Document::new_empty();
        let mut history = UndoHistory::new();

        apply_insert(&mut doc, &mut history, "a");
        history.begin_group();
        apply_insert(&mut doc, &mut history, "b");
        history.begin_group();
        apply_insert(&mut doc, &mut history, "c");
        assert_eq!(doc.rope().to_string(), "abc");

        // Undo should take us back to "a" (b and c were grouped with the undo)
        history.undo(&mut doc).unwrap();
        // "c" undone; "b" is grouped too
        assert_eq!(doc.rope().to_string(), "a");
    }
}
