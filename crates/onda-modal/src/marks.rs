use std::collections::HashMap;

use onda_core::document::DocumentId;

/// Per-document character-position marks (`ma` … `mz` / backtick-a … backtick-z).
///
/// Only lowercase letters `a`–`z` are stored; uppercase and special marks are
/// not tracked here (they belong to the global mark system, which is out of
/// scope for Phase 1).
#[derive(Debug, Default)]
pub struct MarkStore {
    /// Maps `(document, mark_char)` → char-index within the document.
    local: HashMap<(DocumentId, char), usize>,
}

impl MarkStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set mark `mark` in `doc_id` to `char_pos`.
    ///
    /// Only lowercase `a`–`z` are accepted; any other character is silently
    /// ignored.
    pub fn set(&mut self, doc_id: DocumentId, mark: char, char_pos: usize) {
        if mark.is_ascii_lowercase() {
            self.local.insert((doc_id, mark), char_pos);
        }
    }

    /// Return the stored char-position for `mark` in `doc_id`, if any.
    pub fn get(&self, doc_id: DocumentId, mark: char) -> Option<usize> {
        self.local.get(&(doc_id, mark)).copied()
    }

    /// Remove all marks that belong to `doc_id` (called when a document is closed).
    pub fn remove_doc(&mut self, doc_id: DocumentId) {
        self.local.retain(|(id, _), _| *id != doc_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_id() -> DocumentId {
        DocumentId::new()
    }

    #[test]
    fn set_and_get() {
        let mut store = MarkStore::new();
        let id = make_id();
        store.set(id, 'a', 42);
        assert_eq!(store.get(id, 'a'), Some(42));
    }

    #[test]
    fn unknown_mark_returns_none() {
        let store = MarkStore::new();
        let id = make_id();
        assert!(store.get(id, 'z').is_none());
    }

    #[test]
    fn uppercase_mark_ignored() {
        let mut store = MarkStore::new();
        let id = make_id();
        store.set(id, 'A', 10); // should be ignored
        assert!(store.get(id, 'A').is_none());
    }

    #[test]
    fn remove_doc_clears_its_marks() {
        let mut store = MarkStore::new();
        let id1 = make_id();
        let id2 = make_id();
        store.set(id1, 'a', 1);
        store.set(id2, 'b', 2);
        store.remove_doc(id1);
        assert!(store.get(id1, 'a').is_none());
        assert_eq!(store.get(id2, 'b'), Some(2));
    }
}
