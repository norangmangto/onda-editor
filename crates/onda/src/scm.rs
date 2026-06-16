//! Source-control surface backed by the `git` CLI (Phase 6 W38).
//!
//! onda keeps libgit2 out of the editor core (ADR/​plan): the SCM panel shells out
//! to `git` on a background thread (never the main loop, rule 2) and parses its
//! porcelain output here. Parsing is pure and unit-tested; the editor renders the
//! result in the Source Control sidebar view and issues stage/unstage/commit.

use std::path::Path;
use std::process::Command;

/// One changed file from `git status --porcelain`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStatus {
    /// Index (staged) status char: `M`/`A`/`D`/`R`/`C`/`?`/' '.
    pub staged: char,
    /// Worktree (unstaged) status char.
    pub unstaged: char,
    /// Path relative to the repo root (the new path for renames).
    pub path: String,
}

impl FileStatus {
    /// True if the file has staged changes.
    pub fn is_staged(&self) -> bool {
        self.staged != ' ' && self.staged != '?'
    }
    /// True if the file has unstaged/worktree changes (including untracked).
    #[allow(dead_code)] // API completeness; used in tests
    pub fn is_unstaged(&self) -> bool {
        self.unstaged != ' ' || self.staged == '?'
    }
    /// A short two-char badge for display, e.g. `M ` or `??`.
    pub fn badge(&self) -> String {
        format!("{}{}", self.staged, self.unstaged)
    }
}

/// Parse `git status --porcelain=v1` output into a list of changed files.
pub fn parse_status(output: &str) -> Vec<FileStatus> {
    let mut out = Vec::new();
    for line in output.lines() {
        if line.len() < 3 {
            continue;
        }
        let bytes = line.as_bytes();
        let staged = bytes[0] as char;
        let unstaged = bytes[1] as char;
        // Path starts at column 3 (after "XY ").
        let rest = &line[3..];
        // Renames/copies show "old -> new"; keep the new path.
        let path = rest.rsplit(" -> ").next().unwrap_or(rest).to_string();
        out.push(FileStatus {
            staged,
            unstaged,
            path,
        });
    }
    out
}

/// Run a `git` subcommand in `root`, returning stdout on success.
/// Blocking — call only from a worker thread, never the main loop (rule 2).
pub fn run_git(root: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Fetch the working-tree status (changed files) for `root`.
pub fn status(root: &Path) -> Result<Vec<FileStatus>, String> {
    run_git(root, &["status", "--porcelain=v1"]).map(|o| parse_status(&o))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_staged_unstaged_untracked() {
        let out = "\
M  staged.rs
 M dirty.rs
MM both.rs
?? new.rs
A  added.rs
";
        let s = parse_status(out);
        assert_eq!(s.len(), 5);
        assert_eq!(s[0].path, "staged.rs");
        assert!(s[0].is_staged() && !s[0].is_unstaged());
        assert!(!s[1].is_staged() && s[1].is_unstaged()); // " M"
        assert!(s[2].is_staged() && s[2].is_unstaged()); // "MM"
        assert_eq!(s[3].badge(), "??");
        assert!(s[3].is_unstaged() && !s[3].is_staged()); // untracked
        assert!(s[4].is_staged());
    }

    #[test]
    fn rename_keeps_new_path() {
        let s = parse_status("R  old.rs -> new.rs\n");
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].path, "new.rs");
        assert_eq!(s[0].staged, 'R');
    }

    #[test]
    fn empty_and_short_lines_ignored() {
        assert!(parse_status("").is_empty());
        assert!(parse_status("\n\nx\n").is_empty());
    }
}
