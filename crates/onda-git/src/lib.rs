//! Git integration for onda (T16.1+).
//!
//! Wraps `git2` (libgit2) for status, gutter-sign diffs, and index staging. All
//! libgit2 calls are blocking, so the editor drives them through [`worker::GitWorker`],
//! which runs on a dedicated OS thread and never touches the main event loop
//! (AGENTS.md rule 2). The pure query functions here are also unit-testable directly.

use std::path::{Path, PathBuf};

use git2::Repository;
use thiserror::Error;

pub mod blame;
pub mod diff;
pub mod status;
pub mod worker;

pub use blame::{blame_file, file_hunks, reset_hunk, stage_hunk, BlameLine, DiffHunk};
pub use diff::{gutter_signs, LineSign};
pub use status::{discard_file, file_statuses, stage_file, unstage_file, FileState, FileStatus};
pub use worker::{GitCommand, GitEvent, GitWorker};

/// Errors surfaced by the git integration.
#[derive(Debug, Error)]
pub enum GitError {
    #[error("git error: {0}")]
    Git(#[from] git2::Error),
    #[error("path {0} is not inside a git working tree")]
    NotInWorkdir(PathBuf),
    #[error("object for {0} is not a blob")]
    NotABlob(PathBuf),
}

/// Discover the repository containing `path` (a file or directory).
pub fn discover(path: &Path) -> Result<Repository, GitError> {
    let start = if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(path)
    };
    Ok(Repository::discover(start)?)
}

/// Discover the repository containing `path` and return it alongside `path`
/// expressed relative to the working directory (the form libgit2 APIs expect).
pub fn open_for(path: &Path) -> Result<(Repository, PathBuf), GitError> {
    let repo = discover(path)?;
    let rel = workdir_relative(&repo, path)?;
    Ok((repo, rel))
}

/// Express `path` relative to the repo working directory.
fn workdir_relative(repo: &Repository, path: &Path) -> Result<PathBuf, GitError> {
    let workdir = repo
        .workdir()
        .ok_or_else(|| GitError::NotInWorkdir(path.to_path_buf()))?;
    // Canonicalize both sides so symlinked temp dirs (common on macOS) compare equal.
    let abs = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let workdir = workdir
        .canonicalize()
        .unwrap_or_else(|_| workdir.to_path_buf());
    abs.strip_prefix(&workdir)
        .map(|p| p.to_path_buf())
        .map_err(|_| GitError::NotInWorkdir(path.to_path_buf()))
}
