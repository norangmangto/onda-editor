use std::sync::Arc;

use ropey::Rope;
use tokio::sync::{mpsc, watch};
use tracing::debug;

use crate::highlight::{parse_highlights, Highlights};

/// Commands sent from the editor to the background syntax worker.
#[derive(Debug)]
pub enum WorkerCmd {
    /// Request a (re-)parse of the buffer.
    Parse {
        rope: Rope,
        language: String,
        version: u64,
    },
    /// Gracefully shut down the worker task.
    Shutdown,
}

/// A background task that runs tree-sitter parsing off the main thread.
///
/// The worker owns a `tokio::sync::watch` channel so the latest highlights are
/// always accessible without blocking (rule 2 of AGENTS.md).
pub struct SyntaxWorker {
    tx: mpsc::Sender<WorkerCmd>,
    rx: watch::Receiver<Option<Arc<Highlights>>>,
}

impl SyntaxWorker {
    /// Spawn the background worker task and return a `SyntaxWorker` handle.
    pub fn spawn() -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel::<WorkerCmd>(64);
        let (result_tx, result_rx) = watch::channel::<Option<Arc<Highlights>>>(None);

        tokio::spawn(worker_task(cmd_rx, result_tx));

        Self {
            tx: cmd_tx,
            rx: result_rx,
        }
    }

    /// Send a parse request.  Non-blocking: if the channel is full the oldest
    /// pending command will be superseded by the worker draining strategy.
    pub fn request_parse(&self, rope: Rope, language: String, version: u64) {
        // Best-effort send; the worker drains the channel before each parse so
        // only the latest version matters.
        let _ = self.tx.try_send(WorkerCmd::Parse {
            rope,
            language,
            version,
        });
    }

    /// Read the most recently computed highlights without blocking.
    pub fn current_highlights(&self) -> Option<Arc<Highlights>> {
        self.rx.borrow().clone()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Worker task
// ─────────────────────────────────────────────────────────────────────────────

async fn worker_task(
    mut rx: mpsc::Receiver<WorkerCmd>,
    tx: watch::Sender<Option<Arc<Highlights>>>,
) {
    debug!("syntax worker started");

    while let Some(cmd) = rx.recv().await {
        match cmd {
            WorkerCmd::Shutdown => {
                debug!("syntax worker shutting down");
                break;
            }
            WorkerCmd::Parse {
                mut rope,
                mut language,
                mut version,
            } => {
                // Drain the channel: only process the *latest* parse request.
                loop {
                    match rx.try_recv() {
                        Ok(WorkerCmd::Parse {
                            rope: r,
                            language: l,
                            version: v,
                        }) => {
                            rope = r;
                            language = l;
                            version = v;
                        }
                        Ok(WorkerCmd::Shutdown) => {
                            debug!("syntax worker shutting down");
                            return;
                        }
                        Err(_) => break,
                    }
                }

                // Run the (potentially expensive) parse on the blocking thread pool
                // so we never stall the async executor.
                let highlights = tokio::task::spawn_blocking(move || {
                    parse_highlights(&rope, &language, version)
                })
                .await
                .unwrap_or(None);

                let _ = tx.send(highlights.map(Arc::new));
            }
        }
    }

    debug!("syntax worker exited");
}

// ─────────────────────────────────────────────────────────────────────────────
// Legacy handle kept for existing callers in other crates
// ─────────────────────────────────────────────────────────────────────────────

use crate::highlight::HighlightEvent;

/// Requests sent from the editor to the syntax worker (legacy API).
#[derive(Debug)]
pub enum WorkerRequest {
    /// Parse or re-parse the buffer with the given language name and rope snapshot.
    Parse {
        buffer_id: u64,
        language: String,
        text: String,
    },
    /// Shut down the worker gracefully.
    Shutdown,
}

/// Responses sent from the syntax worker back to the editor (legacy API).
#[derive(Debug)]
pub enum WorkerResponse {
    /// Highlight events for a buffer after a successful parse.
    Highlights {
        buffer_id: u64,
        events: Vec<HighlightEvent>,
    },
    /// The worker encountered a non-fatal error.
    Error { buffer_id: u64, message: String },
}

/// A handle to the background syntax worker (legacy API).
pub struct SyntaxWorkerHandle {
    pub tx: mpsc::Sender<WorkerRequest>,
    pub rx: mpsc::Receiver<WorkerResponse>,
}
