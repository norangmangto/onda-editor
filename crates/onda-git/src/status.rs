//! Working-tree status and index staging (T16.1, with hunk staging deferred to T16.4).

use std::path::Path;

use git2::{Repository, Status, StatusOptions};

use crate::GitError;

/// A single kind of change to a file, on either the index or worktree side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileState {
    New,
    Modified,
    Deleted,
    Renamed,
    TypeChange,
    Conflicted,
}

/// Combined status of one path in the working tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStatus {
    /// Path relative to the repo working directory.
    pub path: String,
    /// Staged change (index vs HEAD), if any.
    pub index: Option<FileState>,
    /// Unstaged change (worktree vs index), if any.
    pub worktree: Option<FileState>,
}

impl FileStatus {
    /// True when the file has staged changes.
    pub fn is_staged(&self) -> bool {
        self.index.is_some()
    }

    /// A single-character badge for file-tree / picker display (`M`/`A`/`D`/`?`/`R`).
    pub fn badge(&self) -> char {
        // Prefer the worktree state; fall back to the staged state.
        match self.worktree.or(self.index) {
            Some(FileState::New) => '?',
            Some(FileState::Modified) | Some(FileState::TypeChange) => 'M',
            Some(FileState::Deleted) => 'D',
            Some(FileState::Renamed) => 'R',
            Some(FileState::Conflicted) => 'U',
            None => ' ',
        }
    }
}

/// Translate the index-side bits of a libgit2 `Status` into a `FileState`.
fn index_state(s: Status) -> Option<FileState> {
    if s.is_conflicted() {
        return Some(FileState::Conflicted);
    }
    if s.is_index_new() {
        Some(FileState::New)
    } else if s.is_index_modified() {
        Some(FileState::Modified)
    } else if s.is_index_deleted() {
        Some(FileState::Deleted)
    } else if s.is_index_renamed() {
        Some(FileState::Renamed)
    } else if s.is_index_typechange() {
        Some(FileState::TypeChange)
    } else {
        None
    }
}

/// Translate the worktree-side bits of a libgit2 `Status` into a `FileState`.
fn worktree_state(s: Status) -> Option<FileState> {
    if s.is_wt_new() {
        Some(FileState::New)
    } else if s.is_wt_modified() {
        Some(FileState::Modified)
    } else if s.is_wt_deleted() {
        Some(FileState::Deleted)
    } else if s.is_wt_renamed() {
        Some(FileState::Renamed)
    } else if s.is_wt_typechange() {
        Some(FileState::TypeChange)
    } else {
        None
    }
}

/// List the status of every changed/untracked path in the repo.
pub fn file_statuses(repo: &Repository) -> Result<Vec<FileStatus>, GitError> {
    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .renames_head_to_index(true)
        .renames_index_to_workdir(true);

    let statuses = repo.statuses(Some(&mut opts))?;
    let mut out = Vec::with_capacity(statuses.len());
    for entry in statuses.iter() {
        let s = entry.status();
        if s.is_ignored() {
            continue;
        }
        let path = entry.path().unwrap_or("").to_string();
        if path.is_empty() {
            continue;
        }
        out.push(FileStatus {
            path,
            index: index_state(s),
            worktree: worktree_state(s),
        });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// Stage all changes to `rel_path` (modified/new → add; deleted → remove).
pub fn stage_file(repo: &Repository, rel_path: &Path) -> Result<(), GitError> {
    let mut index = repo.index()?;
    let exists = repo
        .workdir()
        .map(|w| w.join(rel_path).exists())
        .unwrap_or(false);
    if exists {
        index.add_path(rel_path)?;
    } else {
        index.remove_path(rel_path)?;
    }
    index.write()?;
    Ok(())
}

/// Unstage `rel_path`, resetting its index entry to the HEAD version.
pub fn unstage_file(repo: &Repository, rel_path: &Path) -> Result<(), GitError> {
    match repo.head() {
        Ok(head) => {
            let obj = head.peel(git2::ObjectType::Commit)?;
            repo.reset_default(Some(&obj), [rel_path])?;
        }
        Err(_) => {
            // Unborn branch: simply drop the entry from the index.
            let mut index = repo.index()?;
            let _ = index.remove_path(rel_path);
            index.write()?;
        }
    }
    Ok(())
}

/// Discard unstaged working-tree changes to `rel_path`, restoring the index/HEAD copy.
pub fn discard_file(repo: &Repository, rel_path: &Path) -> Result<(), GitError> {
    let mut cb = git2::build::CheckoutBuilder::new();
    cb.force();
    cb.update_index(false);
    cb.path(rel_path);
    repo.checkout_head(Some(&mut cb))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn init_committed(files: &[(&str, &str)]) -> (Repository, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        for (n, c) in files {
            fs::write(dir.path().join(n), c).unwrap();
        }
        {
            let mut index = repo.index().unwrap();
            for (n, _) in files {
                index.add_path(Path::new(n)).unwrap();
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

    fn find<'a>(v: &'a [FileStatus], p: &str) -> Option<&'a FileStatus> {
        v.iter().find(|f| f.path == p)
    }

    #[test]
    fn modified_file_shows_in_status() {
        let (repo, dir) = init_committed(&[("a.txt", "hi\n")]);
        fs::write(dir.path().join("a.txt"), "bye\n").unwrap();
        let st = file_statuses(&repo).unwrap();
        let f = find(&st, "a.txt").unwrap();
        assert_eq!(f.worktree, Some(FileState::Modified));
        assert!(!f.is_staged());
        assert_eq!(f.badge(), 'M');
    }

    #[test]
    fn untracked_file_shows_as_new() {
        let (repo, dir) = init_committed(&[("a.txt", "hi\n")]);
        fs::write(dir.path().join("new.txt"), "x\n").unwrap();
        let st = file_statuses(&repo).unwrap();
        let f = find(&st, "new.txt").unwrap();
        assert_eq!(f.worktree, Some(FileState::New));
        assert_eq!(f.badge(), '?');
    }

    #[test]
    fn stage_moves_change_to_index() {
        let (repo, dir) = init_committed(&[("a.txt", "hi\n")]);
        fs::write(dir.path().join("a.txt"), "bye\n").unwrap();
        stage_file(&repo, Path::new("a.txt")).unwrap();
        let st = file_statuses(&repo).unwrap();
        let f = find(&st, "a.txt").unwrap();
        assert_eq!(f.index, Some(FileState::Modified));
        assert!(f.is_staged());
    }

    #[test]
    fn unstage_returns_change_to_worktree() {
        let (repo, dir) = init_committed(&[("a.txt", "hi\n")]);
        fs::write(dir.path().join("a.txt"), "bye\n").unwrap();
        stage_file(&repo, Path::new("a.txt")).unwrap();
        unstage_file(&repo, Path::new("a.txt")).unwrap();
        let st = file_statuses(&repo).unwrap();
        let f = find(&st, "a.txt").unwrap();
        assert_eq!(f.index, None);
        assert_eq!(f.worktree, Some(FileState::Modified));
    }

    #[test]
    fn discard_restores_head_content() {
        let (repo, dir) = init_committed(&[("a.txt", "hi\n")]);
        fs::write(dir.path().join("a.txt"), "bye\n").unwrap();
        discard_file(&repo, Path::new("a.txt")).unwrap();
        let content = fs::read_to_string(dir.path().join("a.txt")).unwrap();
        assert_eq!(content, "hi\n");
        let st = file_statuses(&repo).unwrap();
        assert!(find(&st, "a.txt").is_none());
    }

    #[test]
    fn clean_repo_is_empty() {
        let (repo, _d) = init_committed(&[("a.txt", "hi\n")]);
        assert!(file_statuses(&repo).unwrap().is_empty());
    }
}
