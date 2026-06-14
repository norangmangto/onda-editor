//! Persistent undo store (Session L2, T29.1).
//!
//! Serializes a file's [`UndoTree`] to a blob keyed by a hash of the file *content*.
//! On load, the key inherently validates: a blob is only found when the current
//! content hashes to the same key, so a changed file silently falls back to an empty
//! history (the DESIGN §5.8 invalidation rule). The store is capped with LRU eviction
//! so it can't grow without bound. Opt-in (`undo.persistent`), default off for v0.1.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

use onda_core::UndoTree;
use tracing::warn;

/// Maximum number of undo blobs retained (oldest evicted first).
const MAX_BLOBS: usize = 128;

/// A directory-backed persistent undo store.
#[derive(Debug, Clone)]
pub struct UndoStore {
    dir: PathBuf,
}

impl UndoStore {
    /// Create a store rooted at `dir` (created on first save).
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// The default store under `~/.local/share/onda/undo`, if `HOME` is set.
    pub fn default_path() -> Option<PathBuf> {
        std::env::var("HOME")
            .ok()
            .map(|h| PathBuf::from(h).join(".local/share/onda/undo"))
    }

    fn blob_path(&self, content: &str) -> PathBuf {
        self.dir
            .join(format!("{:016x}.json", content_hash(content)))
    }

    /// Persist `tree` keyed by `content`'s hash. Best-effort; errors are logged.
    pub fn save(&self, content: &str, tree: &UndoTree) {
        if let Err(e) = self.try_save(content, tree) {
            warn!("undo store save failed: {e}");
        }
    }

    fn try_save(&self, content: &str, tree: &UndoTree) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let path = self.blob_path(content);
        let json = serde_json::to_vec(tree)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(&path, json)?;
        self.evict_if_needed();
        Ok(())
    }

    /// Load the undo tree for `content`, or `None` if absent/corrupt (discarded).
    pub fn load(&self, content: &str) -> Option<UndoTree> {
        let path = self.blob_path(content);
        let bytes = std::fs::read(&path).ok()?;
        match serde_json::from_slice::<UndoTree>(&bytes) {
            Ok(tree) => Some(tree),
            Err(e) => {
                // Corrupt blob: discard it and fall back to an empty history.
                warn!("undo blob {} corrupt ({e}); discarding", path.display());
                let _ = std::fs::remove_file(&path);
                None
            }
        }
    }

    /// LRU eviction: keep at most `MAX_BLOBS`, removing oldest by mtime first.
    fn evict_if_needed(&self) {
        let mut blobs: Vec<(std::time::SystemTime, PathBuf)> = match std::fs::read_dir(&self.dir) {
            Ok(rd) => rd
                .flatten()
                .filter(|e| e.path().extension().map(|x| x == "json").unwrap_or(false))
                .filter_map(|e| {
                    let mtime = e.metadata().ok()?.modified().ok()?;
                    Some((mtime, e.path()))
                })
                .collect(),
            Err(_) => return,
        };
        if blobs.len() <= MAX_BLOBS {
            return;
        }
        blobs.sort_by_key(|(t, _)| *t); // oldest first
        let excess = blobs.len() - MAX_BLOBS;
        for (_, path) in blobs.into_iter().take(excess) {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Stable-within-a-build content hash for keying undo blobs.
pub fn content_hash(content: &str) -> u64 {
    let mut h = DefaultHasher::new();
    content.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use onda_core::{transaction::ChangeSetBuilder, Document, Selection, Transaction};

    fn tree_with_one_edit() -> (UndoTree, Document) {
        let mut doc = Document::new_empty();
        let mut tree = UndoTree::new();
        let cs = ChangeSetBuilder::new(0).insert("hello").build();
        let tx = Transaction::new(cs);
        let inv = doc.apply(&tx).unwrap();
        tree.push(tx, inv, Selection::point(0), Selection::point(5));
        (tree, doc)
    }

    #[test]
    fn save_then_load_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let store = UndoStore::new(dir.path().to_path_buf());
        let (tree, _doc) = tree_with_one_edit();
        // Key the blob by the saved content ("hello").
        store.save("hello", &tree);
        let loaded = store.load("hello").expect("blob should load");
        assert!(loaded.can_undo());
    }

    #[test]
    fn content_mismatch_discards() {
        let dir = tempfile::tempdir().unwrap();
        let store = UndoStore::new(dir.path().to_path_buf());
        let (tree, _doc) = tree_with_one_edit();
        store.save("hello", &tree);
        // Different content → different key → nothing found.
        assert!(store.load("world").is_none());
    }

    #[test]
    fn corrupt_blob_is_discarded() {
        let dir = tempfile::tempdir().unwrap();
        let store = UndoStore::new(dir.path().to_path_buf());
        std::fs::create_dir_all(dir.path()).unwrap();
        let path = dir.path().join(format!("{:016x}.json", content_hash("x")));
        std::fs::write(&path, b"{ not valid json").unwrap();
        assert!(store.load("x").is_none());
        assert!(!path.exists(), "corrupt blob should be removed");
    }

    #[test]
    fn missing_store_loads_none() {
        let store = UndoStore::new(PathBuf::from("/no/such/onda/undo/dir"));
        assert!(store.load("anything").is_none());
    }

    #[test]
    fn lru_evicts_oldest() {
        let dir = tempfile::tempdir().unwrap();
        let store = UndoStore::new(dir.path().to_path_buf());
        let (tree, _doc) = tree_with_one_edit();
        // Write more than the cap; the dir should never exceed MAX_BLOBS.
        for i in 0..(MAX_BLOBS + 10) {
            store.save(&format!("content-{i}"), &tree);
        }
        let count = std::fs::read_dir(dir.path()).unwrap().count();
        assert!(count <= MAX_BLOBS, "store exceeded cap: {count}");
    }
}
