/// High-level LSP client built on top of `Transport`.
///
/// Handles:
/// - `initialize` / `initialized` handshake
/// - `textDocument/didOpen`, `didChange`, `didClose` notifications
/// - Outgoing requests (hover, definition, completion, rename, format)
/// - Routing incoming notifications/responses to the main loop via `LspEvent` channel
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use lsp_types::*;
use serde_json::{json, Value};
use tokio::sync::{mpsc, Mutex};
use tokio::time::timeout;
use tracing::{debug, info};

use crate::transport::{IncomingMessage, Transport, TransportError};
use crate::types::{LspDiagnostic, LspEvent};

pub type RequestId = u64;

#[derive(Debug, thiserror::Error)]
pub enum LspError {
    #[error("transport error: {0}")]
    Transport(#[from] TransportError),
    #[error("server not initialized")]
    NotInitialized,
    #[error("request timed out")]
    Timeout,
    #[error("JSON-RPC error {code}: {message}")]
    RpcError { code: i64, message: String },
}

// ── LspClient ─────────────────────────────────────────────────────────────────

/// A single LSP client session for one workspace root.
///
/// Lives entirely in background tokio tasks; communicates with the main loop
/// via an `mpsc::Sender<LspEvent>`.
pub struct LspClient {
    root: PathBuf,
    transport: Arc<Mutex<Transport>>,
    /// Pending response channels keyed by request id.
    pending: Arc<Mutex<HashMap<u64, tokio::sync::oneshot::Sender<Value>>>>,
    /// Channel to the main loop.
    event_tx: mpsc::Sender<LspEvent>,
    /// Whether the server has completed initialization.
    initialized: bool,
}

impl LspClient {
    /// Spawn a language server and start the client session.
    ///
    /// Returns `(client, LspEvent receiver)`.
    pub async fn spawn(
        command: &str,
        args: &[&str],
        root: PathBuf,
    ) -> Result<(Self, mpsc::Receiver<LspEvent>), LspError> {
        let (transport, incoming_rx) = Transport::spawn(command, args, &root).await?;
        let transport = Arc::new(Mutex::new(transport));
        let pending: Arc<Mutex<HashMap<u64, tokio::sync::oneshot::Sender<Value>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let (event_tx, event_rx) = mpsc::channel(256);

        // Start the dispatcher task
        tokio::spawn(dispatcher_task(
            incoming_rx,
            pending.clone(),
            event_tx.clone(),
            root.clone(),
        ));

        let mut client = Self {
            root: root.clone(),
            transport: transport.clone(),
            pending,
            event_tx: event_tx.clone(),
            initialized: false,
        };

        // Perform the initialize handshake
        client.initialize().await?;

        Ok((client, event_rx))
    }

    async fn initialize(&mut self) -> Result<(), LspError> {
        // Percent-encode like file URIs elsewhere (spaces / non-ASCII roots).
        let root_uri = format!("{}/", path_to_uri(&self.root));

        let params = json!({
            "processId": std::process::id(),
            "rootUri": root_uri,
            "capabilities": {
                "textDocument": {
                    "synchronization": { "dynamicRegistration": false, "didSave": true },
                    "publishDiagnostics": { "relatedInformation": true },
                    "hover": { "contentFormat": ["plaintext", "markdown"] },
                    "definition": { "linkSupport": false },
                    "references": {},
                    "completion": {
                        "completionItem": {
                            "snippetSupport": true,
                            "documentationFormat": ["plaintext"]
                        }
                    },
                    "rename": { "prepareSupport": false },
                    "formatting": {}
                },
                "workspace": {
                    "applyEdit": true,
                    "workspaceEdit": { "documentChanges": true }
                }
            },
            "initializationOptions": null
        });

        let id = {
            let mut t = self.transport.lock().await;
            t.send_request("initialize", params).await?
        };

        let resp = self.await_response(id, Duration::from_secs(30)).await?;
        debug!("LSP initialize response: {:?}", resp);

        // Send initialized notification
        {
            let mut t = self.transport.lock().await;
            t.send_notification("initialized", json!({})).await?;
        }

        self.initialized = true;
        info!("LSP server initialized at {:?}", self.root);

        let _ = self
            .event_tx
            .send(LspEvent::Ready {
                root: self.root.clone(),
            })
            .await;
        Ok(())
    }

    // ── Document sync ─────────────────────────────────────────────────────────

    /// Notify the server that a file was opened.
    pub async fn did_open(
        &mut self,
        path: &Path,
        text: &str,
        language_id: &str,
    ) -> Result<(), LspError> {
        self.require_init()?;
        let uri = path_to_uri(path);
        let params = json!({
            "textDocument": {
                "uri": uri,
                "languageId": language_id,
                "version": 1,
                "text": text
            }
        });
        let mut t = self.transport.lock().await;
        t.send_notification("textDocument/didOpen", params).await?;
        Ok(())
    }

    /// Notify the server that a file changed (full-sync).
    pub async fn did_change(
        &mut self,
        path: &Path,
        text: &str,
        version: i64,
    ) -> Result<(), LspError> {
        self.require_init()?;
        let uri = path_to_uri(path);
        let params = json!({
            "textDocument": { "uri": uri, "version": version },
            "contentChanges": [{ "text": text }]
        });
        let mut t = self.transport.lock().await;
        t.send_notification("textDocument/didChange", params)
            .await?;
        Ok(())
    }

    /// Notify the server that a file was closed.
    pub async fn did_close(&mut self, path: &Path) -> Result<(), LspError> {
        self.require_init()?;
        let uri = path_to_uri(path);
        let params = json!({ "textDocument": { "uri": uri } });
        let mut t = self.transport.lock().await;
        t.send_notification("textDocument/didClose", params).await?;
        Ok(())
    }

    // ── Requests ──────────────────────────────────────────────────────────────

    /// Request hover information. Result delivered via `LspEvent::HoverResult`.
    pub async fn hover(
        &mut self,
        path: &Path,
        position: Position,
        request_id: u64,
    ) -> Result<(), LspError> {
        self.require_init()?;
        let uri = path_to_uri(path);
        let params = json!({
            "textDocument": { "uri": uri },
            "position": position
        });
        let rpc_id = {
            let mut t = self.transport.lock().await;
            t.send_request("textDocument/hover", params).await?
        };
        let tx = self.event_tx.clone();
        let pending = self.pending.clone();
        tokio::spawn(async move {
            if let Ok(resp) = collect_response(pending, rpc_id, Duration::from_secs(5)).await {
                let content = parse_hover_content(&resp);
                let _ = tx
                    .send(LspEvent::HoverResult {
                        request_id,
                        content,
                    })
                    .await;
            }
        });
        Ok(())
    }

    /// Request go-to-definition. Result delivered via `LspEvent::DefinitionResult`.
    pub async fn definition(
        &mut self,
        path: &Path,
        position: Position,
        request_id: u64,
    ) -> Result<(), LspError> {
        self.require_init()?;
        let uri = path_to_uri(path);
        let params = json!({ "textDocument": { "uri": uri }, "position": position });
        let rpc_id = {
            let mut t = self.transport.lock().await;
            t.send_request("textDocument/definition", params).await?
        };
        let tx = self.event_tx.clone();
        let pending = self.pending.clone();
        tokio::spawn(async move {
            if let Ok(resp) = collect_response(pending, rpc_id, Duration::from_secs(5)).await {
                let locations = parse_locations(&resp);
                let _ = tx
                    .send(LspEvent::DefinitionResult {
                        request_id,
                        locations,
                    })
                    .await;
            }
        });
        Ok(())
    }

    /// Request references. Result delivered via `LspEvent::ReferencesResult`.
    pub async fn references(
        &mut self,
        path: &Path,
        position: Position,
        request_id: u64,
    ) -> Result<(), LspError> {
        self.require_init()?;
        let uri = path_to_uri(path);
        let params = json!({
            "textDocument": { "uri": uri },
            "position": position,
            "context": { "includeDeclaration": true }
        });
        let rpc_id = {
            let mut t = self.transport.lock().await;
            t.send_request("textDocument/references", params).await?
        };
        let tx = self.event_tx.clone();
        let pending = self.pending.clone();
        tokio::spawn(async move {
            if let Ok(resp) = collect_response(pending, rpc_id, Duration::from_secs(5)).await {
                let locations = parse_location_list(&resp);
                let _ = tx
                    .send(LspEvent::ReferencesResult {
                        request_id,
                        locations,
                    })
                    .await;
            }
        });
        Ok(())
    }

    /// Request type definition. Result delivered via `LspEvent::DefinitionResult`.
    pub async fn type_definition(
        &mut self,
        path: &Path,
        position: Position,
        request_id: u64,
    ) -> Result<(), LspError> {
        self.require_init()?;
        let uri = path_to_uri(path);
        let params = json!({ "textDocument": { "uri": uri }, "position": position });
        let rpc_id = {
            let mut t = self.transport.lock().await;
            t.send_request("textDocument/typeDefinition", params)
                .await?
        };
        let tx = self.event_tx.clone();
        let pending = self.pending.clone();
        tokio::spawn(async move {
            if let Ok(resp) = collect_response(pending, rpc_id, Duration::from_secs(5)).await {
                let locations = parse_locations(&resp);
                let _ = tx
                    .send(LspEvent::DefinitionResult {
                        request_id,
                        locations,
                    })
                    .await;
            }
        });
        Ok(())
    }

    /// Request completions. Result delivered via `LspEvent::CompletionResult`.
    pub async fn completion(
        &mut self,
        path: &Path,
        position: Position,
        request_id: u64,
    ) -> Result<(), LspError> {
        self.require_init()?;
        let uri = path_to_uri(path);
        let params = json!({
            "textDocument": { "uri": uri },
            "position": position,
            "context": { "triggerKind": 1 }
        });
        let rpc_id = {
            let mut t = self.transport.lock().await;
            t.send_request("textDocument/completion", params).await?
        };
        let tx = self.event_tx.clone();
        let pending = self.pending.clone();
        tokio::spawn(async move {
            if let Ok(resp) = collect_response(pending, rpc_id, Duration::from_secs(3)).await {
                let items = parse_completion_items(&resp);
                let _ = tx
                    .send(LspEvent::CompletionResult { request_id, items })
                    .await;
            }
        });
        Ok(())
    }

    /// Request rename. Result delivered via `LspEvent::RenameResult`.
    pub async fn rename(
        &mut self,
        path: &Path,
        position: Position,
        new_name: String,
        request_id: u64,
    ) -> Result<(), LspError> {
        self.require_init()?;
        let uri = path_to_uri(path);
        let params = json!({
            "textDocument": { "uri": uri },
            "position": position,
            "newName": new_name
        });
        let rpc_id = {
            let mut t = self.transport.lock().await;
            t.send_request("textDocument/rename", params).await?
        };
        let tx = self.event_tx.clone();
        let pending = self.pending.clone();
        tokio::spawn(async move {
            if let Ok(resp) = collect_response(pending, rpc_id, Duration::from_secs(10)).await {
                let edit = serde_json::from_value::<WorkspaceEdit>(resp).ok();
                let _ = tx.send(LspEvent::RenameResult { request_id, edit }).await;
            }
        });
        Ok(())
    }

    /// Request document formatting. Result delivered via `LspEvent::FormattingResult`.
    pub async fn formatting(&mut self, path: &Path, request_id: u64) -> Result<(), LspError> {
        self.require_init()?;
        let uri = path_to_uri(path);
        let params = json!({
            "textDocument": { "uri": uri },
            "options": { "tabSize": 4, "insertSpaces": true }
        });
        let rpc_id = {
            let mut t = self.transport.lock().await;
            t.send_request("textDocument/formatting", params).await?
        };
        let tx = self.event_tx.clone();
        let pending = self.pending.clone();
        tokio::spawn(async move {
            if let Ok(resp) = collect_response(pending, rpc_id, Duration::from_secs(10)).await {
                let edits = serde_json::from_value::<Vec<TextEdit>>(resp).unwrap_or_default();
                let _ = tx
                    .send(LspEvent::FormattingResult { request_id, edits })
                    .await;
            }
        });
        Ok(())
    }

    /// Gracefully shut down the server.
    pub async fn shutdown(&mut self) -> Result<(), LspError> {
        if self.initialized {
            let id = {
                let mut t = self.transport.lock().await;
                t.send_request("shutdown", json!(null)).await?
            };
            let _ = self.await_response(id, Duration::from_secs(5)).await;
            let mut t = self.transport.lock().await;
            let _ = t.send_notification("exit", json!(null)).await;
        }
        let mut t = self.transport.lock().await;
        t.shutdown().await;
        Ok(())
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn require_init(&self) -> Result<(), LspError> {
        if self.initialized {
            Ok(())
        } else {
            Err(LspError::NotInitialized)
        }
    }

    async fn await_response(&self, id: u64, dur: Duration) -> Result<Value, LspError> {
        collect_response(self.pending.clone(), id, dur).await
    }
}

// ── Dispatcher task ───────────────────────────────────────────────────────────

/// Routes incoming JSON-RPC messages from the transport to pending waiters
/// and the event channel.
async fn dispatcher_task(
    mut rx: tokio::sync::mpsc::Receiver<IncomingMessage>,
    pending: Arc<Mutex<HashMap<u64, tokio::sync::oneshot::Sender<Value>>>>,
    event_tx: mpsc::Sender<LspEvent>,
    root: PathBuf,
) {
    while let Some(msg) = rx.recv().await {
        match msg {
            IncomingMessage::Response(resp) => {
                if let Some(id_val) = resp.id {
                    if let Some(id) = id_val.as_u64() {
                        let mut map = pending.lock().await;
                        if let Some(waiter) = map.remove(&id) {
                            let value = resp.result.unwrap_or(Value::Null);
                            let _ = waiter.send(value);
                        }
                    }
                }
            }
            IncomingMessage::Notification(notif) => {
                handle_notification(notif, &event_tx, &root).await;
            }
        }
    }
}

async fn handle_notification(
    notif: crate::transport::JsonRpcNotification,
    event_tx: &mpsc::Sender<LspEvent>,
    _root: &Path,
) {
    match notif.method.as_str() {
        "textDocument/publishDiagnostics" => {
            if let Some(params) = notif.params {
                if let Ok(p) = serde_json::from_value::<lsp_types::PublishDiagnosticsParams>(params)
                {
                    let path = uri_to_path(&p.uri);
                    let diagnostics: Vec<LspDiagnostic> = p
                        .diagnostics
                        .iter()
                        .map(|d| LspDiagnostic::from_lsp(path.clone(), d))
                        .collect();
                    let _ = event_tx
                        .send(LspEvent::Diagnostics { path, diagnostics })
                        .await;
                }
            }
        }
        "window/logMessage" | "window/showMessage" => {
            if let Some(params) = notif.params {
                debug!("LSP message: {:?}", params);
            }
        }
        _ => {
            debug!("LSP unhandled notification: {}", notif.method);
        }
    }
}

// ── Response collection ────────────────────────────────────────────────────────

async fn collect_response(
    pending: Arc<Mutex<HashMap<u64, tokio::sync::oneshot::Sender<Value>>>>,
    id: u64,
    dur: Duration,
) -> Result<Value, LspError> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    {
        let mut map = pending.lock().await;
        map.insert(id, tx);
    }
    match timeout(dur, rx).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(_)) => Err(LspError::Transport(
            crate::transport::TransportError::ProcessExited,
        )),
        Err(_) => Err(LspError::Timeout),
    }
}

// ── Response parsers ──────────────────────────────────────────────────────────

fn parse_hover_content(v: &Value) -> Option<String> {
    if v.is_null() {
        return None;
    }
    let hover: lsp_types::Hover = serde_json::from_value(v.clone()).ok()?;
    Some(match hover.contents {
        HoverContents::Scalar(MarkedString::String(s)) => s,
        HoverContents::Scalar(MarkedString::LanguageString(ls)) => ls.value,
        HoverContents::Array(arr) => arr
            .into_iter()
            .map(|ms| match ms {
                MarkedString::String(s) => s,
                MarkedString::LanguageString(ls) => ls.value,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        HoverContents::Markup(mc) => mc.value,
    })
}

fn parse_locations(v: &Value) -> Vec<Location> {
    if v.is_null() {
        return vec![];
    }
    if let Ok(loc) = serde_json::from_value::<Location>(v.clone()) {
        return vec![loc];
    }
    if let Ok(locs) = serde_json::from_value::<Vec<Location>>(v.clone()) {
        return locs;
    }
    // GotoDefinitionResponse
    if let Ok(gdr) = serde_json::from_value::<GotoDefinitionResponse>(v.clone()) {
        return match gdr {
            GotoDefinitionResponse::Scalar(l) => vec![l],
            GotoDefinitionResponse::Array(ls) => ls,
            GotoDefinitionResponse::Link(links) => links
                .into_iter()
                .map(|l| Location {
                    uri: l.target_uri,
                    range: l.target_selection_range,
                })
                .collect(),
        };
    }
    vec![]
}

fn parse_location_list(v: &Value) -> Vec<Location> {
    if v.is_null() {
        return vec![];
    }
    serde_json::from_value::<Vec<Location>>(v.clone()).unwrap_or_default()
}

fn parse_completion_items(v: &Value) -> Vec<lsp_types::CompletionItem> {
    if v.is_null() {
        return vec![];
    }
    if let Ok(list) = serde_json::from_value::<lsp_types::CompletionList>(v.clone()) {
        return list.items;
    }
    serde_json::from_value::<Vec<lsp_types::CompletionItem>>(v.clone()).unwrap_or_default()
}

// ── Utilities ─────────────────────────────────────────────────────────────────

fn path_to_uri(path: &Path) -> String {
    // Construct a file:// URI from the path without the url crate.
    let path_str = path.to_string_lossy();
    if path_str.starts_with('/') {
        format!("file://{}", percent_encode_path(&path_str))
    } else {
        format!("file:///{}", percent_encode_path(&path_str))
    }
}

/// Percent-encode a file path for use in a URI (spaces and special chars).
fn percent_encode_path(s: &str) -> String {
    s.chars()
        .flat_map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':') {
                vec![c]
            } else {
                let bytes = c.to_string();
                bytes
                    .as_bytes()
                    .iter()
                    .flat_map(|b| format!("%{:02X}", b).chars().collect::<Vec<_>>())
                    .collect()
            }
        })
        .collect()
}

/// Convert a file:// URI string to a file system path.
fn uri_to_path(uri: &lsp_types::Uri) -> PathBuf {
    let s = uri.as_str();
    if let Some(rest) = s.strip_prefix("file://") {
        // Decode percent-encoding
        let decoded = percent_decode(rest);
        PathBuf::from(decoded)
    } else {
        PathBuf::from(s)
    }
}

fn percent_decode(s: &str) -> String {
    // Decode into raw bytes first, then interpret as UTF-8. Pushing each decoded
    // byte as a `char` would corrupt multibyte sequences (e.g. a percent-encoded
    // Hangul path), so collect bytes and decode once.
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 3 <= bytes.len() {
            if let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3]) {
                if let Ok(byte) = u8::from_str_radix(hex, 16) {
                    out.push(byte);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod uri_tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn percent_roundtrip_spaces() {
        let s = "/home/user/my project/file.rs";
        let enc = percent_encode_path(s);
        assert!(!enc.contains(' '));
        assert!(enc.contains("%20"));
        assert_eq!(percent_decode(&enc), s);
    }

    #[test]
    fn percent_roundtrip_multibyte() {
        // Regression: percent-decoding must reassemble multibyte UTF-8, not push
        // each byte as a char (which corrupted non-ASCII paths).
        let s = "/home/사용자/파일.rs";
        let enc = percent_encode_path(s);
        assert!(enc.is_ascii(), "encoded form should be ASCII: {enc}");
        assert_eq!(percent_decode(&enc), s);
    }

    #[test]
    fn path_to_uri_unix_absolute_encodes_space() {
        let uri = path_to_uri(Path::new("/tmp/a b.rs"));
        assert_eq!(uri, "file:///tmp/a%20b.rs");
    }

    #[test]
    fn uri_to_path_roundtrips_through_path_to_uri() {
        for p in ["/home/user/space dir/x.rs", "/tmp/한글/파일.rs"] {
            let pb = PathBuf::from(p);
            let uri = path_to_uri(&pb);
            let parsed = lsp_types::Uri::from_str(&uri).expect("valid uri");
            assert_eq!(uri_to_path(&parsed), pb, "roundtrip failed for {p}");
        }
    }
}
