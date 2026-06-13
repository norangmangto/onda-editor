//! Background git worker (T16.1).
//!
//! All libgit2 work runs here, on a dedicated OS thread, so the main event loop never
//! blocks on disk or libgit2 (AGENTS.md rule 2). The worker owns no editor state; it
//! receives [`GitCommand`]s and emits [`GitEvent`]s back over a channel the binary
//! bridges into its own background-message queue.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use crate::{diff, status, LineSign};

/// A request to the git worker.
#[derive(Debug)]
pub enum GitCommand {
    /// Recompute gutter signs for a document's live buffer.
    ComputeSigns {
        doc_id: u64,
        path: PathBuf,
        buffer: Vec<u8>,
    },
    /// List the repo status for the repo containing `path`.
    Status { path: PathBuf },
    /// Stage all changes to the file at `path`, then re-emit status.
    Stage { path: PathBuf },
    /// Unstage the file at `path`, then re-emit status.
    Unstage { path: PathBuf },
    /// Discard worktree changes to the file at `path`, then re-emit status.
    Discard { path: PathBuf },
    /// Stop the worker thread.
    Shutdown,
}

/// A result emitted by the git worker.
#[derive(Debug)]
pub enum GitEvent {
    /// Recomputed gutter signs for a document.
    Signs {
        doc_id: u64,
        signs: Vec<(usize, LineSign)>,
    },
    /// A repo status listing, keyed by the repo's working directory.
    Status {
        root: PathBuf,
        entries: Vec<status::FileStatus>,
    },
    /// A non-fatal error (logged + shown on the message line).
    Error(String),
}

/// Handle to the background git worker. Dropping it shuts the worker down.
pub struct GitWorker {
    tx: Sender<GitCommand>,
}

impl GitWorker {
    /// Spawn the worker thread, delivering events on `events`.
    pub fn spawn(events: Sender<GitEvent>) -> Self {
        let (tx, rx) = mpsc::channel::<GitCommand>();
        thread::Builder::new()
            .name("onda-git".into())
            .spawn(move || worker_loop(rx, events))
            .expect("spawn onda-git worker thread");
        Self { tx }
    }

    /// Request a gutter-sign recompute. Silently dropped if the worker is gone.
    pub fn compute_signs(&self, doc_id: u64, path: PathBuf, buffer: Vec<u8>) {
        let _ = self.tx.send(GitCommand::ComputeSigns {
            doc_id,
            path,
            buffer,
        });
    }

    pub fn status(&self, path: PathBuf) {
        let _ = self.tx.send(GitCommand::Status { path });
    }

    pub fn stage(&self, path: PathBuf) {
        let _ = self.tx.send(GitCommand::Stage { path });
    }

    pub fn unstage(&self, path: PathBuf) {
        let _ = self.tx.send(GitCommand::Unstage { path });
    }

    pub fn discard(&self, path: PathBuf) {
        let _ = self.tx.send(GitCommand::Discard { path });
    }
}

impl Drop for GitWorker {
    fn drop(&mut self) {
        let _ = self.tx.send(GitCommand::Shutdown);
    }
}

fn worker_loop(rx: Receiver<GitCommand>, events: Sender<GitEvent>) {
    while let Ok(cmd) = rx.recv() {
        let result = handle(cmd, &events);
        match result {
            Ok(true) => continue,
            Ok(false) => break, // Shutdown
            Err(msg) => {
                if events.send(GitEvent::Error(msg)).is_err() {
                    break;
                }
            }
        }
    }
}

/// Handle one command. Returns `Ok(false)` on shutdown, `Ok(true)` to keep going,
/// or `Err(message)` for a non-fatal error.
fn handle(cmd: GitCommand, events: &Sender<GitEvent>) -> Result<bool, String> {
    match cmd {
        GitCommand::Shutdown => return Ok(false),

        GitCommand::ComputeSigns {
            doc_id,
            path,
            buffer,
        } => {
            let (repo, rel) = crate::open_for(&path).map_err(|e| e.to_string())?;
            let signs = diff::gutter_signs(&repo, &rel, &buffer).map_err(|e| e.to_string())?;
            let _ = events.send(GitEvent::Signs { doc_id, signs });
        }

        GitCommand::Status { path } => {
            emit_status(&path, events)?;
        }

        GitCommand::Stage { path } => {
            let (repo, rel) = crate::open_for(&path).map_err(|e| e.to_string())?;
            status::stage_file(&repo, &rel).map_err(|e| e.to_string())?;
            emit_status(&path, events)?;
        }

        GitCommand::Unstage { path } => {
            let (repo, rel) = crate::open_for(&path).map_err(|e| e.to_string())?;
            status::unstage_file(&repo, &rel).map_err(|e| e.to_string())?;
            emit_status(&path, events)?;
        }

        GitCommand::Discard { path } => {
            let (repo, rel) = crate::open_for(&path).map_err(|e| e.to_string())?;
            status::discard_file(&repo, &rel).map_err(|e| e.to_string())?;
            emit_status(&path, events)?;
        }
    }
    Ok(true)
}

fn emit_status(path: &std::path::Path, events: &Sender<GitEvent>) -> Result<(), String> {
    let repo = crate::discover(path).map_err(|e| e.to_string())?;
    let entries = status::file_statuses(&repo).map_err(|e| e.to_string())?;
    let root = repo
        .workdir()
        .map(|w| w.to_path_buf())
        .unwrap_or_else(|| path.to_path_buf());
    let _ = events.send(GitEvent::Status { root, entries });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use std::time::Duration;

    fn committed_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        fs::write(dir.path().join("a.txt"), "one\ntwo\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("a.txt")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("t", "t@t").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();
        dir
    }

    #[test]
    fn worker_computes_signs() {
        let dir = committed_repo();
        let file = dir.path().join("a.txt");
        fs::write(&file, "one\nTWO\n").unwrap();

        let (etx, erx) = mpsc::channel();
        let worker = GitWorker::spawn(etx);
        worker.compute_signs(7, file.clone(), b"one\nTWO\n".to_vec());

        let ev = erx.recv_timeout(Duration::from_secs(5)).unwrap();
        match ev {
            GitEvent::Signs { doc_id, signs } => {
                assert_eq!(doc_id, 7);
                assert_eq!(signs, vec![(1, LineSign::Modified)]);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn worker_stage_then_status() {
        let dir = committed_repo();
        let file = dir.path().join("a.txt");
        fs::write(&file, "one\nTWO\n").unwrap();

        let (etx, erx) = mpsc::channel();
        let worker = GitWorker::spawn(etx);
        worker.stage(file.clone());

        let ev = erx.recv_timeout(Duration::from_secs(5)).unwrap();
        match ev {
            GitEvent::Status { entries, .. } => {
                let f = entries.iter().find(|f| f.path == "a.txt").unwrap();
                assert!(f.is_staged());
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }
}
