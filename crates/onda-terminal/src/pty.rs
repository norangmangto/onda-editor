/// PTY backend for the integrated terminal.
///
/// Spawns a shell via `portable-pty`, reads output asynchronously and
/// pushes `PtyEvent` frames to the main loop.
/// All PTY I/O runs in dedicated tokio tasks; the main thread only enqueues
/// writes via `write_tx` and drains `PtyEvent`s once per frame.
use std::io::Read;
use std::thread;

use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use thiserror::Error;
use tokio::sync::mpsc;
use tracing::warn;

#[derive(Debug, Error)]
pub enum PtyError {
    #[error("PTY system error: {0}")]
    Pty(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

// ── PtyEvent ──────────────────────────────────────────────────────────────────

/// Event emitted from the PTY reader task to the main loop.
#[derive(Debug, Clone)]
pub enum PtyEvent {
    /// Raw bytes from the PTY (to be fed to VT100 parser).
    Data(Vec<u8>),
    /// The child process exited.
    Exited(u32),
}

// ── PtyProcess ────────────────────────────────────────────────────────────────

/// Owns the master side of a PTY and the spawned shell process.
///
/// Cloning is not supported; use `Arc<Mutex<PtyProcess>>` if sharing is needed.
pub struct PtyProcess {
    master: Box<dyn MasterPty + Send>,
    /// Sender for bytes to write to the PTY (keyboard input).
    write_tx: mpsc::UnboundedSender<Vec<u8>>,
    /// Terminal dimensions.
    cols: u16,
    rows: u16,
}

impl PtyProcess {
    /// Spawn a shell process under a new PTY.
    ///
    /// Returns `(PtyProcess, event_receiver)`.  Feed the `event_receiver` into
    /// the main `BgMessage` channel via a bridge task.
    pub fn spawn(cols: u16, rows: u16) -> Result<(Self, mpsc::Receiver<PtyEvent>), PtyError> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| PtyError::Pty(e.to_string()))?;

        // Determine shell
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
        let mut cmd = CommandBuilder::new(&shell);
        cmd.env("TERM", "xterm-256color");

        let _child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| PtyError::Pty(e.to_string()))?;

        // Reader thread: reads raw bytes from master and pushes to the event channel.
        // We use a std thread because portable-pty uses synchronous I/O.
        let (event_tx, event_rx) = mpsc::channel(1024);
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| PtyError::Pty(e.to_string()))?;
        let event_tx_clone = event_tx.clone();
        thread::spawn(move || {
            let mut buf = vec![0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => {
                        let _ = event_tx_clone.blocking_send(PtyEvent::Exited(0));
                        break;
                    }
                    Ok(n) => {
                        let _ = event_tx_clone.blocking_send(PtyEvent::Data(buf[..n].to_vec()));
                    }
                    Err(e) => {
                        warn!("PTY read error: {e}");
                        break;
                    }
                }
            }
        });

        // Writer task: receives bytes from write_tx and forwards to master.
        let (write_tx, mut write_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let mut writer = pair
            .master
            .take_writer()
            .map_err(|e| PtyError::Pty(e.to_string()))?;
        tokio::spawn(async move {
            while let Some(bytes) = write_rx.recv().await {
                if writer.write_all(&bytes).is_err() {
                    break;
                }
            }
        });

        Ok((
            Self {
                master: pair.master,
                write_tx,
                cols,
                rows,
            },
            event_rx,
        ))
    }

    /// Write bytes to the PTY (forwarded to the shell).
    pub fn write(&self, bytes: &[u8]) {
        let _ = self.write_tx.send(bytes.to_vec());
    }

    /// Resize the PTY.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.cols = cols;
        self.rows = rows;
        if let Err(e) = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        }) {
            warn!("PTY resize error: {e}");
        }
    }

    pub fn cols(&self) -> u16 {
        self.cols
    }

    pub fn rows(&self) -> u16 {
        self.rows
    }
}
