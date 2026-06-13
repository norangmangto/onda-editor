//! Git blame (T16.3) and unified-diff hunks (T16.2) / hunk staging (T16.4).

use std::path::Path;

use git2::{ApplyLocation, ApplyOptions, DiffOptions, Patch, Repository};

use crate::diff::head_blob_bytes;
use crate::GitError;

/// Per-line blame annotation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlameLine {
    /// 0-based line number in the working file.
    pub line: usize,
    /// Abbreviated commit hash (or "00000000" for not-yet-committed lines).
    pub commit: String,
    pub author: String,
    /// Commit time as a unix timestamp (seconds).
    pub time: i64,
    /// First line of the commit summary.
    pub summary: String,
}

/// Blame `rel_path` at HEAD, returning one annotation per line.
pub fn blame_file(repo: &Repository, rel_path: &Path) -> Result<Vec<BlameLine>, GitError> {
    let blame = repo.blame_file(rel_path, None)?;
    let mut out = Vec::new();
    for hunk in blame.iter() {
        let sig = hunk.final_signature();
        let author = sig.name().unwrap_or("?").to_string();
        let time = sig.when().seconds();
        let commit_id = hunk.final_commit_id();
        let short = commit_id.to_string().chars().take(8).collect::<String>();
        let summary = repo
            .find_commit(commit_id)
            .ok()
            .and_then(|c| c.summary().map(|s| s.to_string()))
            .unwrap_or_default();
        let start = hunk.final_start_line(); // 1-based
        for i in 0..hunk.lines_in_hunk() {
            out.push(BlameLine {
                line: start + i - 1,
                commit: short.clone(),
                author: author.clone(),
                time,
                summary: summary.clone(),
            });
        }
    }
    out.sort_by_key(|b| b.line);
    Ok(out)
}

/// A unified-diff hunk of the working buffer vs HEAD.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffHunk {
    pub old_start: u32,
    pub old_lines: u32,
    pub new_start: u32,
    pub new_lines: u32,
    /// The hunk body lines, each prefixed by its origin (` `, `+`, `-`).
    pub lines: Vec<String>,
}

/// Compute unified-diff hunks of `buffer` vs the HEAD version of `rel_path`.
pub fn file_hunks(
    repo: &Repository,
    rel_path: &Path,
    buffer: &[u8],
) -> Result<Vec<DiffHunk>, GitError> {
    let head = head_blob_bytes(repo, rel_path)?;
    let old = head.as_deref().unwrap_or(&[]);
    let patch = Patch::from_buffers(old, Some(rel_path), buffer, Some(rel_path), None)?;

    let mut hunks = Vec::new();
    for i in 0..patch.num_hunks() {
        let (h, line_count) = patch.hunk(i)?;
        let mut lines = Vec::new();
        for l in 0..line_count {
            let dl = patch.line_in_hunk(i, l)?;
            let origin = dl.origin();
            let content = String::from_utf8_lossy(dl.content());
            lines.push(format!("{origin}{}", content.trim_end_matches('\n')));
        }
        hunks.push(DiffHunk {
            old_start: h.old_start(),
            old_lines: h.old_lines(),
            new_start: h.new_start(),
            new_lines: h.new_lines(),
            lines,
        });
    }
    Ok(hunks)
}

/// Stage the single hunk of `rel_path` (working tree vs index) that contains
/// new-file line `target_new_line` (0-based). Uses libgit2's apply-to-index with a
/// hunk filter, equivalent to `git add --patch` for one hunk.
pub fn stage_hunk(
    repo: &Repository,
    rel_path: &Path,
    target_new_line: usize,
) -> Result<bool, GitError> {
    apply_hunk(repo, rel_path, target_new_line, false)
}

/// Reset (unstage→discard from worktree) the hunk containing `target_new_line` by
/// reverse-applying it to the working tree.
pub fn reset_hunk(
    repo: &Repository,
    rel_path: &Path,
    target_new_line: usize,
) -> Result<bool, GitError> {
    apply_hunk(repo, rel_path, target_new_line, true)
}

fn apply_hunk(
    repo: &Repository,
    rel_path: &Path,
    target_new_line: usize,
    reverse: bool,
) -> Result<bool, GitError> {
    let mut diff_opts = DiffOptions::new();
    diff_opts.pathspec(rel_path);
    if reverse {
        diff_opts.reverse(true);
    }
    let diff = repo.diff_index_to_workdir(None, Some(&mut diff_opts))?;

    let target = (target_new_line + 1) as u32; // git lines are 1-based
    let applied = std::cell::Cell::new(false);
    let mut opts = ApplyOptions::new();
    opts.hunk_callback(|hunk| {
        let matches = match hunk {
            Some(h) => {
                let start = h.new_start();
                let end = start + h.new_lines();
                target >= start && target < end.max(start + 1)
            }
            None => false,
        };
        if matches {
            applied.set(true);
        }
        matches
    });

    let location = if reverse {
        ApplyLocation::WorkDir
    } else {
        ApplyLocation::Index
    };
    repo.apply(&diff, location, Some(&mut opts))?;
    Ok(applied.get())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn committed(files: &[(&str, &str)]) -> (Repository, tempfile::TempDir) {
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

    #[test]
    fn blame_reports_author_per_line() {
        let (repo, _d) = committed(&[("a.txt", "one\ntwo\n")]);
        let blame = blame_file(&repo, Path::new("a.txt")).unwrap();
        assert_eq!(blame.len(), 2);
        assert_eq!(blame[0].author, "t");
        assert_eq!(blame[0].line, 0);
        assert!(!blame[0].commit.is_empty());
    }

    #[test]
    fn hunks_describe_changes() {
        let (repo, _d) = committed(&[("a.txt", "one\ntwo\nthree\n")]);
        let hunks = file_hunks(&repo, Path::new("a.txt"), b"one\nTWO\nthree\n").unwrap();
        assert_eq!(hunks.len(), 1);
        let h = &hunks[0];
        assert!(h
            .lines
            .iter()
            .any(|l| l.starts_with('-') && l.contains("two")));
        assert!(h
            .lines
            .iter()
            .any(|l| l.starts_with('+') && l.contains("TWO")));
    }

    #[test]
    fn no_hunks_when_unchanged() {
        let (repo, _d) = committed(&[("a.txt", "x\n")]);
        let hunks = file_hunks(&repo, Path::new("a.txt"), b"x\n").unwrap();
        assert!(hunks.is_empty());
    }

    #[test]
    fn stage_hunk_stages_only_that_hunk() {
        // 20 lines so two edits (line 2 and line 19) are >2× the diff context apart
        // and therefore form distinct hunks.
        let base: String = (1..=20).map(|i| format!("{i}\n")).collect();
        let (repo, dir) = committed(&[("a.txt", base.as_str())]);
        let mut edited: Vec<String> = (1..=20).map(|i| i.to_string()).collect();
        edited[1] = "TWO".into();
        edited[18] = "NINETEEN".into();
        fs::write(dir.path().join("a.txt"), edited.join("\n") + "\n").unwrap();

        // Stage the hunk around line 2 (0-based line 1).
        let staged = stage_hunk(&repo, Path::new("a.txt"), 1).unwrap();
        assert!(staged, "a hunk should have matched and staged");

        // The index now differs from HEAD (something got staged), and the worktree
        // still has unstaged changes (the other hunk).
        let st = crate::status::file_statuses(&repo).unwrap();
        let f = st.iter().find(|f| f.path == "a.txt").unwrap();
        assert!(f.is_staged(), "file should be partially staged");
        assert!(f.worktree.is_some(), "the other hunk remains unstaged");
    }
}
