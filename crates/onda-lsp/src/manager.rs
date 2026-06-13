/// LSP server manager — one server per workspace root, shared across splits.
///
/// Lives in the tokio background pool.
/// The main loop receives `LspEvent` via `BgMessage::Lsp`.
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::client::{LspClient, LspError, RequestId};
use crate::types::LspEvent;

// ── Server configuration ──────────────────────────────────────────────────────

/// Configuration for a language server.
#[derive(Debug, Clone)]
pub struct LspServerConfig {
    /// The command to run (e.g. "rust-analyzer" or "gopls").
    pub command: String,
    /// Arguments to pass to the server.
    pub args: Vec<String>,
    /// Language IDs this server handles.
    pub language_ids: Vec<String>,
    /// File extensions this server handles.
    pub extensions: Vec<String>,
}

impl LspServerConfig {
    /// Built-in config for rust-analyzer.
    pub fn rust_analyzer() -> Self {
        Self {
            command: "rust-analyzer".into(),
            args: vec![],
            language_ids: vec!["rust".into()],
            extensions: vec!["rs".into()],
        }
    }

    /// Built-in config for gopls.
    pub fn gopls() -> Self {
        Self {
            command: "gopls".into(),
            args: vec![],
            language_ids: vec!["go".into()],
            extensions: vec!["go".into()],
        }
    }
}

// ── LspManager ────────────────────────────────────────────────────────────────

/// Manages multiple LSP servers across workspace roots.
///
/// Call [`LspManager::get_or_spawn`] to lazily start a server for a root.
/// All events from all servers are forwarded to the single `event_tx` channel.
pub struct LspManager {
    /// Active clients keyed by workspace root.
    clients: HashMap<PathBuf, LspClient>,
    /// Configurations for language servers.
    configs: Vec<LspServerConfig>,
    /// Event channel to the main loop.
    event_tx: mpsc::Sender<LspEvent>,
    /// Next request ID shared across all servers.
    next_request_id: u64,
}

impl LspManager {
    /// Create a new manager with default language server configurations.
    pub fn new(event_tx: mpsc::Sender<LspEvent>) -> Self {
        Self {
            clients: HashMap::new(),
            configs: vec![LspServerConfig::rust_analyzer(), LspServerConfig::gopls()],
            event_tx,
            next_request_id: 1,
        }
    }

    /// Allocate a fresh request ID.
    pub fn alloc_request_id(&mut self) -> RequestId {
        let id = self.next_request_id;
        self.next_request_id += 1;
        id
    }

    /// Find the best server config for the given file extension.
    pub fn config_for_extension(&self, ext: &str) -> Option<&LspServerConfig> {
        self.configs
            .iter()
            .find(|c| c.extensions.iter().any(|e| e == ext))
    }

    /// Return the language ID for a file extension.
    pub fn language_id_for_extension(&self, ext: &str) -> Option<&str> {
        self.config_for_extension(ext)
            .and_then(|c| c.language_ids.first())
            .map(|s| s.as_str())
    }

    /// Lazily spawn a server for the given root directory and extension.
    ///
    /// If a server is already running for this root, returns immediately.
    /// Actual server spawn happens asynchronously; the caller receives
    /// `LspEvent::Ready` on success.
    pub async fn ensure_server(&mut self, root: PathBuf, extension: &str) -> Result<(), LspError> {
        if self.clients.contains_key(&root) {
            return Ok(());
        }

        let config = match self.config_for_extension(extension) {
            Some(c) => c.clone(),
            None => return Ok(()),
        };

        info!("Spawning LSP server '{}' for {:?}", config.command, root);

        let args: Vec<&str> = config.args.iter().map(|s| s.as_str()).collect();
        let (client, event_rx) = LspClient::spawn(&config.command, &args, root.clone()).await?;

        self.clients.insert(root.clone(), client);

        // Forward all events from this server to the main channel
        let tx = self.event_tx.clone();
        tokio::spawn(async move {
            let mut rx = event_rx;
            while let Some(ev) = rx.recv().await {
                if tx.send(ev).await.is_err() {
                    break;
                }
            }
        });

        Ok(())
    }

    /// Get the client for a root, if any.
    pub fn client_for(&self, root: &Path) -> Option<&LspClient> {
        self.clients.get(root)
    }

    /// Get the mutable client for a root, if any.
    pub fn client_for_mut(&mut self, root: &Path) -> Option<&mut LspClient> {
        self.clients.get_mut(root)
    }

    /// Notify all relevant servers of a file open.
    pub async fn did_open(&mut self, path: &Path, text: &str, root: &Path) {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let lang_id = self
            .language_id_for_extension(ext)
            .unwrap_or("plaintext")
            .to_string();

        if let Some(client) = self.clients.get_mut(root) {
            if let Err(e) = client.did_open(path, text, &lang_id).await {
                warn!("LSP didOpen error: {e}");
            }
        }
    }

    /// Notify the server of a file change.
    pub async fn did_change(&mut self, path: &Path, text: &str, version: i64, root: &Path) {
        if let Some(client) = self.clients.get_mut(root) {
            if let Err(e) = client.did_change(path, text, version).await {
                warn!("LSP didChange error: {e}");
            }
        }
    }

    /// Shut down all servers gracefully.
    pub async fn shutdown_all(&mut self) {
        for (root, client) in &mut self.clients {
            info!("Shutting down LSP server for {:?}", root);
            if let Err(e) = client.shutdown().await {
                warn!("LSP shutdown error: {e}");
            }
        }
        self.clients.clear();
    }
}
