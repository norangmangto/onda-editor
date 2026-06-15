//! Per-line gutter-sign diffs of buffer content against HEAD (T16.1).

use std::path::Path;

use git2::{Patch, Repository};

use crate::GitError;

/// A gutter sign for a single (0-based) buffer line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineSign {
    /// Line is new relative to HEAD.
    Added,
    /// Line existed but changed.
    Modified,
    /// Lines were deleted *above* this line (shown on the line below the deletion).
    Deleted,
}

impl LineSign {
    /// The single-character gutter glyph for this sign.
    pub fn glyph(self) -> char {
        match self {
            LineSign::Added => '+',
            LineSign::Modified => '~',
            LineSign::Deleted => '_',
        }
    }
}

/// Compute gutter signs for `buffer` (the live, possibly-unsaved content) versus the
/// HEAD version of `rel_path`. Returns `(line, sign)` pairs sorted by line.
///
/// A file absent from HEAD is treated as wholly added. A buffer identical to HEAD
/// yields an empty vec.
pub fn gutter_signs(
    repo: &Repository,
    rel_path: &Path,
    buffer: &[u8],
) -> Result<Vec<(usize, LineSign)>, GitError> {
    let head_bytes = head_blob_bytes(repo, rel_path)?;
    let old = head_bytes.as_deref().unwrap_or(&[]);

    // `from_buffers` diffs two in-memory byte buffers — exactly what we need to compare
    // HEAD against the unsaved editor buffer without writing to disk.
    let patch = Patch::from_buffers(old, Some(rel_path), buffer, Some(rel_path), None)?;

    let mut signs = Vec::new();
    for i in 0..patch.num_hunks() {
        let (_hunk, line_count) = patch.hunk(i)?;

        // Inspect per-line origins: a hunk carries unchanged context lines too, so we
        // can only sign the lines the diff actually added/removed.
        let mut added: Vec<usize> = Vec::new(); // 0-based new-file line numbers
        let mut deletions = 0u32;
        let mut last_context_new: Option<usize> = None; // line above a pure deletion

        for l in 0..line_count {
            let dl = patch.line_in_hunk(i, l)?;
            match dl.origin() {
                '+' => {
                    if let Some(n) = dl.new_lineno() {
                        added.push(n.saturating_sub(1) as usize);
                    }
                }
                '-' => deletions += 1,
                // Context (' ') and EOFNL markers — track position for delete anchoring.
                _ => {
                    if let Some(n) = dl.new_lineno() {
                        last_context_new = Some(n.saturating_sub(1) as usize);
                    }
                }
            }
        }

        if added.is_empty() {
            if deletions > 0 {
                // Pure deletion: flag the line above the removed block.
                signs.push((last_context_new.unwrap_or(0), LineSign::Deleted));
            }
        } else {
            // Added lines that replace removed lines read as modifications.
            let sign = if deletions > 0 {
                LineSign::Modified
            } else {
                LineSign::Added
            };
            for n in added {
                signs.push((n, sign));
            }
        }
    }

    signs.sort_by_key(|(l, _)| *l);
    signs.dedup();
    Ok(signs)
}

/// Fetch the raw bytes of `rel_path` as stored in HEAD, or `None` when the file is
/// not tracked in HEAD (new file, or an unborn branch with no commits yet).
pub(crate) fn head_blob_bytes(
    repo: &Repository,
    rel_path: &Path,
) -> Result<Option<Vec<u8>>, GitError> {
    let head = match repo.head() {
        Ok(h) => h,
        // Unborn branch (no commits): everything is new.
        Err(_) => return Ok(None),
    };
    let commit = head.peel_to_commit()?;
    let tree = commit.tree()?;
    match tree.get_path(rel_path) {
        Ok(entry) => {
            let obj = entry.to_object(repo)?;
            let blob = obj
                .as_blob()
                .ok_or_else(|| GitError::NotABlob(rel_path.to_path_buf()))?;
            Ok(Some(blob.content().to_vec()))
        }
        // Not present in HEAD → new file.
        Err(_) => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    /// Build a temp repo with one commit containing `files`, returning the repo and dir.
    fn repo_with(files: &[(&str, &str)]) -> (Repository, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        for (name, content) in files {
            fs::write(dir.path().join(name), content).unwrap();
        }
        {
            let mut index = repo.index().unwrap();
            for (name, _) in files {
                index.add_path(Path::new(name)).unwrap();
            }
            index.write().unwrap();
            let tree_id = index.write_tree().unwrap();
            let tree = repo.find_tree(tree_id).unwrap();
            let sig = git2::Signature::now("t", "t@t").unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
                .unwrap();
        }
        (repo, dir)
    }

    fn signs_for(repo: &Repository, name: &str, buffer: &str) -> Vec<(usize, LineSign)> {
        gutter_signs(repo, Path::new(name), buffer.as_bytes()).unwrap()
    }

    #[test]
    fn unchanged_buffer_has_no_signs() {
        let (repo, _d) = repo_with(&[("a.txt", "one\ntwo\nthree\n")]);
        assert!(signs_for(&repo, "a.txt", "one\ntwo\nthree\n").is_empty());
    }

    #[test]
    fn modified_line_marked() {
        let (repo, _d) = repo_with(&[("a.txt", "one\ntwo\nthree\n")]);
        let signs = signs_for(&repo, "a.txt", "one\nTWO\nthree\n");
        assert_eq!(signs, vec![(1, LineSign::Modified)]);
    }

    #[test]
    fn added_lines_marked() {
        let (repo, _d) = repo_with(&[("a.txt", "one\ntwo\n")]);
        let signs = signs_for(&repo, "a.txt", "one\ntwo\nthree\nfour\n");
        assert_eq!(signs, vec![(2, LineSign::Added), (3, LineSign::Added)]);
    }

    #[test]
    fn deleted_lines_marked() {
        let (repo, _d) = repo_with(&[("a.txt", "one\ntwo\nthree\nfour\n")]);
        let signs = signs_for(&repo, "a.txt", "one\nfour\n");
        // Two interior lines removed after line "one".
        assert!(signs.iter().any(|(_, s)| *s == LineSign::Deleted));
    }

    #[test]
    fn new_file_all_added() {
        let (repo, dir) = repo_with(&[("a.txt", "x\n")]);
        fs::write(dir.path().join("b.txt"), "p\nq\n").unwrap();
        let signs = signs_for(&repo, "b.txt", "p\nq\n");
        assert_eq!(signs, vec![(0, LineSign::Added), (1, LineSign::Added)]);
    }

    #[test]
    fn unborn_branch_all_added() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let _ = PathBuf::new();
        let signs = signs_for(&repo, "fresh.txt", "hello\nworld\n");
        assert_eq!(signs, vec![(0, LineSign::Added), (1, LineSign::Added)]);
    }

    #[test]
    fn glyphs() {
        assert_eq!(LineSign::Added.glyph(), '+');
        assert_eq!(LineSign::Modified.glyph(), '~');
        assert_eq!(LineSign::Deleted.glyph(), '_');
    }
}
