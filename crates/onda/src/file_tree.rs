//! Lazy, gitignore-aware file tree for the Explorer sidebar (Phase 6 W34).
//!
//! The tree keeps a flat list of *visible* entries in display order; expanding a
//! directory reads its immediate children (one `readdir`) and splices them in.
//! Reads are one level deep so a single expand is cheap; the model itself is pure
//! and unit-testable (the rendering/keys live in the binary).

use std::path::{Path, PathBuf};

/// One visible row in the tree.
#[derive(Debug, Clone)]
pub struct TreeEntry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    /// Indent level (root children are depth 0).
    pub depth: usize,
    /// Whether a directory is currently expanded.
    pub expanded: bool,
}

/// A flat, navigable file tree rooted at `root`.
#[derive(Debug)]
pub struct FileTree {
    pub root: PathBuf,
    entries: Vec<TreeEntry>,
    /// Index of the highlighted row.
    pub selected: usize,
    /// First visible row (for scrolling within the sidebar height).
    pub scroll: usize,
}

impl FileTree {
    /// Build a tree showing `root`'s immediate children (collapsed).
    pub fn new(root: PathBuf) -> Self {
        let entries = read_children(&root, 0);
        Self {
            root,
            entries,
            selected: 0,
            scroll: 0,
        }
    }

    pub fn entries(&self) -> &[TreeEntry] {
        &self.entries
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    #[allow(dead_code)] // API completeness; used in tests
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    #[allow(dead_code)] // used in tests; handy for future tree actions
    pub fn selected_entry(&self) -> Option<&TreeEntry> {
        self.entries.get(self.selected)
    }

    /// Move the selection by `delta`, keeping it on screen for `rows` visible rows.
    pub fn move_selection(&mut self, delta: isize, rows: usize) {
        if self.entries.is_empty() {
            return;
        }
        let max = (self.entries.len() - 1) as isize;
        self.selected = (self.selected as isize + delta).clamp(0, max) as usize;
        self.ensure_visible(rows);
    }

    /// Adjust `scroll` so the selected row is within `[scroll, scroll + rows)`.
    pub fn ensure_visible(&mut self, rows: usize) {
        if rows == 0 {
            return;
        }
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + rows {
            self.scroll = self.selected + 1 - rows;
        }
    }

    /// Activate the selected entry: toggle a directory (returns `None`), or return
    /// the path of a file to open.
    pub fn activate(&mut self) -> Option<PathBuf> {
        let e = self.entries.get(self.selected)?;
        if e.is_dir {
            if e.expanded {
                self.collapse_at(self.selected);
            } else {
                self.expand_at(self.selected);
            }
            None
        } else {
            Some(e.path.clone())
        }
    }

    /// `h`: collapse the selected directory if expanded, else jump to its parent.
    pub fn collapse_or_parent(&mut self) {
        let Some(e) = self.entries.get(self.selected) else {
            return;
        };
        if e.is_dir && e.expanded {
            self.collapse_at(self.selected);
            return;
        }
        let depth = e.depth;
        if depth > 0 {
            for i in (0..self.selected).rev() {
                if self.entries[i].depth < depth {
                    self.selected = i;
                    break;
                }
            }
        }
    }

    fn expand_at(&mut self, idx: usize) {
        let (path, depth) = {
            let e = &self.entries[idx];
            (e.path.clone(), e.depth)
        };
        let children = read_children(&path, depth + 1);
        self.entries[idx].expanded = true;
        let tail = self.entries.split_off(idx + 1);
        self.entries.extend(children);
        self.entries.extend(tail);
    }

    fn collapse_at(&mut self, idx: usize) {
        let depth = self.entries[idx].depth;
        self.entries[idx].expanded = false;
        let mut end = idx + 1;
        while end < self.entries.len() && self.entries[end].depth > depth {
            end += 1;
        }
        self.entries.drain(idx + 1..end);
    }

    /// Rebuild from the root (collapsing everything). Picks up created/deleted files.
    pub fn refresh(&mut self) {
        self.entries = read_children(&self.root, 0);
        self.selected = self.selected.min(self.entries.len().saturating_sub(1));
        self.scroll = 0;
    }
}

/// Read the immediate children of `dir` (gitignore-aware), sorted directories-first
/// then alphabetically. `.git` is always hidden.
fn read_children(dir: &Path, depth: usize) -> Vec<TreeEntry> {
    use ignore::WalkBuilder;
    let mut out: Vec<TreeEntry> = Vec::new();
    let walker = WalkBuilder::new(dir)
        .max_depth(Some(1))
        .hidden(false) // show dotfiles; .gitignore still applies
        .build();
    for dent in walker.flatten() {
        let p = dent.path();
        if p == dir {
            continue; // the directory itself
        }
        let name = match p.file_name().and_then(|n| n.to_str()) {
            Some(n) if n != ".git" => n.to_string(),
            _ => continue,
        };
        let is_dir = dent.file_type().map(|t| t.is_dir()).unwrap_or(false);
        out.push(TreeEntry {
            path: p.to_path_buf(),
            name,
            is_dir,
            depth,
            expanded: false,
        });
    }
    out.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn fixture() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir(d.path().join("src")).unwrap();
        fs::write(d.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(d.path().join("src/lib.rs"), "\n").unwrap();
        fs::write(d.path().join("README.md"), "# x\n").unwrap();
        fs::create_dir(d.path().join(".git")).unwrap();
        fs::write(d.path().join(".git/HEAD"), "ref\n").unwrap();
        d
    }

    #[test]
    fn lists_root_children_dirs_first_skipping_git() {
        let d = fixture();
        let t = FileTree::new(d.path().to_path_buf());
        let names: Vec<&str> = t.entries().iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["src", "README.md"]); // dir first, .git hidden
        assert!(t.entries()[0].is_dir);
    }

    #[test]
    fn expand_and_collapse_directory() {
        let d = fixture();
        let mut t = FileTree::new(d.path().to_path_buf());
        // selected = "src"; activate expands it (returns None for a dir).
        assert!(t.activate().is_none());
        let names: Vec<&str> = t.entries().iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["src", "lib.rs", "main.rs", "README.md"]);
        assert!(t.entries()[0].expanded);
        // children are indented one level deeper.
        assert_eq!(t.entries()[1].depth, 1);
        // collapse again.
        assert!(t.activate().is_none());
        assert_eq!(t.len(), 2);
        assert!(!t.entries()[0].expanded);
    }

    #[test]
    fn activate_file_returns_path() {
        let d = fixture();
        let mut t = FileTree::new(d.path().to_path_buf());
        t.move_selection(1, 10); // → "README.md"
        let p = t.activate().expect("file path");
        assert!(p.ends_with("README.md"));
    }

    #[test]
    fn collapse_or_parent_jumps_to_parent() {
        let d = fixture();
        let mut t = FileTree::new(d.path().to_path_buf());
        t.activate(); // expand src
        t.move_selection(1, 10); // on a child (lib.rs, depth 1)
        assert_eq!(t.selected_entry().unwrap().depth, 1);
        t.collapse_or_parent(); // → jump to parent "src"
        assert_eq!(t.selected_entry().unwrap().name, "src");
    }

    #[test]
    fn move_selection_clamps_and_scrolls() {
        let d = fixture();
        let mut t = FileTree::new(d.path().to_path_buf());
        t.move_selection(-5, 1); // clamp at 0
        assert_eq!(t.selected, 0);
        t.move_selection(10, 1); // clamp at last; scroll follows
        assert_eq!(t.selected, t.len() - 1);
        assert_eq!(t.scroll, t.len() - 1); // rows=1 → scroll to selected
    }
}
