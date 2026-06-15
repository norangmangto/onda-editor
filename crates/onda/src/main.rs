use std::{collections::HashMap, path::PathBuf, sync::mpsc, time::Duration};

#[cfg(feature = "bench")]
use std::time::Instant;

mod doctor;
mod plugin_host;

use anyhow::{Context, Result};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind, MouseButton, MouseEvent,
    MouseEventKind,
};
use onda_config::Config;
use onda_core::{Document, DocumentId, Selection, Transaction, UndoHistory};
use onda_lsp::{
    types::{LspDiagnostic, LspEvent},
    LspManager,
};
use onda_modal::{
    build_buffer_picker, build_file_picker, find_all, find_next, find_prev, Action, CommandLine,
    ExCommand, JumpList, Key, KeyMod, Keymap, KeymapState, MacroRecorder, MarkStore, Mode, Motion,
    Operator, PendingResult, Picker, Register, RegisterBank, SearchState,
};
use onda_plugin::PluginApiCall;
use onda_render::{
    draw_borders, render_completion_menu, render_float, render_picker, Backend, Compositor,
    DiagnosticSpan, DocumentView, Layout, Message, MessageLine, ModeIndicator, NullBackend, Rect,
    RenderError, Statusline, TerminalBackend, Viewport, WindowId,
};
use onda_session::{Session, SessionManager};
use onda_syntax::{LanguageRegistry, SyntaxWorker};
use onda_terminal::{PtyEvent, PtyProcess, TerminalScreen};
use plugin_host::{PluginEvent, PluginHost};
use tokio::sync::mpsc as tokio_mpsc;
use tracing::debug;

// ── Background message channel ─────────────────────────────────────────────────

#[allow(dead_code)]
enum BgMessage {
    FileLoaded {
        doc: Document,
    },
    FileError {
        path: PathBuf,
        error: String,
    },
    /// LSP event from a language server.
    Lsp(LspEvent),
    /// Raw PTY output bytes for the terminal pane.
    PtyData {
        pane_id: usize,
        data: Vec<u8>,
    },
    /// PTY process exited.
    PtyExited {
        pane_id: usize,
    },
    /// Event from the ACP agent.
    Agent(onda_agent::AgentEvent),
    /// The agent finished connecting; carries the client handle.
    AgentClientReady(onda_agent::AgentClient),
    /// The active theme file changed on disk (live reload, T18.1).
    ThemeReload,
}

// ── Latency tracer ────────────────────────────────────────────────────────────

#[cfg(feature = "bench")]
#[derive(Default)]
struct LatencyTracer {
    samples: Vec<u64>,
    key_time: Option<Instant>,
}

#[cfg(feature = "bench")]
impl LatencyTracer {
    fn mark_key(&mut self) {
        self.key_time = Some(Instant::now());
    }

    fn mark_frame(&mut self) {
        if let Some(t) = self.key_time.take() {
            let us = t.elapsed().as_micros() as u64;
            self.samples.push(us);
        }
    }

    fn report(&self) {
        if self.samples.is_empty() {
            println!("No latency samples recorded.");
            return;
        }
        let mut s = self.samples.clone();
        s.sort_unstable();
        let p50 = s[s.len() * 50 / 100];
        let p95 = s[s.len() * 95 / 100];
        let p99 = s[s.len() * 99 / 100];
        println!(
            "Latency p50={p50}µs p95={p95}µs p99={p99}µs (n={})",
            s.len()
        );
    }
}

// ── WindowState ───────────────────────────────────────────────────────────────

/// Per-window state: which document is shown and the cursor/viewport within it.
struct WindowState {
    /// Index into `App::docs`.
    doc_idx: usize,
    selection: Selection,
    viewport: Viewport,
    /// Undo history is per-window (each window tracks its own edit history).
    undo: UndoHistory,
}

impl WindowState {
    fn new(doc_idx: usize) -> Self {
        Self {
            doc_idx,
            selection: Selection::point(0),
            viewport: Viewport::new(),
            undo: UndoHistory::new(),
        }
    }
}

// ── PickerKind ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PickerKind {
    File,
    Buffer,
}

/// Pre-formatted agent-panel render data: (styled lines, input, busy, title).
type AgentPanelData = (Vec<(onda_render::Style, String)>, String, bool, String);

/// Active agent diff-review session (T24.2): one file's proposed change, per-hunk.
#[derive(Debug, Clone)]
struct ReviewState {
    path: PathBuf,
    /// The content the review diffs against (current buffer/disk at review start).
    base: String,
    hunks: Vec<onda_agent::Hunk>,
    /// Per-hunk accept decision (default accept).
    accept: Vec<bool>,
    /// Currently focused hunk.
    cursor: usize,
    /// Other staged files queued for review after this one.
    remaining: Vec<PathBuf>,
}

/// One item in the agent conversation thread (W23).
#[derive(Debug, Clone)]
enum AgentItem {
    User(String),
    /// Streaming assistant text; chunks append to the last `Assistant` item.
    Assistant(String),
    Thought(String),
    Tool {
        title: String,
        status: String,
    },
    Plan(Vec<String>),
    Notice(String),
}

/// Active command-line completion (T18.3): a cycling candidate list with the
/// fixed prefix (`base`) that precedes the token being completed.
#[derive(Debug, Clone)]
struct CmdCompletion {
    base: String,
    candidates: Vec<String>,
    selected: usize,
}

// ── SearchMode ────────────────────────────────────────────────────────────────

/// Whether we are entering a forward or backward search pattern in command mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchInputDir {
    Forward,
    Backward,
}

// ── TerminalPane ──────────────────────────────────────────────────────────────

/// An integrated terminal pane.
struct TerminalPane {
    process: PtyProcess,
    screen: TerminalScreen,
    pane_id: usize,
}

// ── HoverFloat ─────────────────────────────────────────────────────────────────

/// An active hover float window.
struct HoverFloat {
    lines: Vec<String>,
    /// Screen position where the float should appear.
    col: u16,
    row: u16,
}

// ── CompletionState ────────────────────────────────────────────────────────────

/// Active completion menu state.
#[allow(dead_code)]
struct CompletionState {
    items: Vec<(String, String)>, // (label, kind_icon)
    selected: usize,
    /// Request ID used to cancel stale completions.
    request_id: u64,
}

// ── App ────────────────────────────────────────────────────────────────────────

struct App<B: Backend> {
    // ── Documents ──────────────────────────────────────────────────────────────
    docs: Vec<Document>,

    // ── Windows ────────────────────────────────────────────────────────────────
    windows: Vec<WindowState>,
    focused_window: usize,
    layout: Layout,
    next_window_id: usize,

    // ── Mode ───────────────────────────────────────────────────────────────────
    mode: Mode,

    // ── Keymap ─────────────────────────────────────────────────────────────────
    keymap: Keymap,
    keymap_state: KeymapState,

    // ── Registers ──────────────────────────────────────────────────────────────
    registers: RegisterBank,
    /// Pending register for the next operator (set by `"`{char}).
    pending_register: Option<char>,

    // ── Macro / dot-repeat ─────────────────────────────────────────────────────
    macros: MacroRecorder,
    last_macro_reg: Option<char>,

    // ── Search ─────────────────────────────────────────────────────────────────
    search: SearchState,
    search_matches: Vec<onda_core::Range>,
    /// Set when entering command mode in search mode; controls whether Enter
    /// triggers a search instead of an ex-command.
    search_input_dir: Option<SearchInputDir>,

    // ── Marks / jump list ──────────────────────────────────────────────────────
    marks: MarkStore,
    jumps: JumpList,

    // ── Picker ────────────────────────────────────────────────────────────────
    picker: Option<Picker>,
    picker_kind: PickerKind,

    // ── Syntax workers ────────────────────────────────────────────────────────
    /// One optional SyntaxWorker per document slot.
    syntax_workers: Vec<Option<SyntaxWorker>>,
    syntax_versions: Vec<u64>,
    lang_registry: LanguageRegistry,

    // ── UI ─────────────────────────────────────────────────────────────────────
    message: Message,
    /// Message history (shown by `:messages`).
    message_history: Vec<String>,
    goal_col: Option<usize>,
    compositor: Compositor,
    backend: B,
    running: bool,
    command_line: CommandLine,
    /// Active command-line completion popup, if any (T18.3).
    cmd_completion: Option<CmdCompletion>,
    bg_tx: mpsc::SyncSender<BgMessage>,
    bg_rx: mpsc::Receiver<BgMessage>,

    // ── Config ────────────────────────────────────────────────────────────────
    #[allow(dead_code)]
    config: Config,

    // ── Theme ─────────────────────────────────────────────────────────────────
    /// Active theme (T18.1).
    theme: onda_render::Theme,
    /// Filesystem watcher on the active theme file (live reload), kept alive.
    theme_watcher: Option<notify::RecommendedWatcher>,
    /// Path of the on-disk theme file currently watched, if any.
    theme_path: Option<PathBuf>,
    /// Highlight overrides registered by plugins (`decorations.set-group`),
    /// re-applied on top of the theme after every switch/reload (the ThemeChanged effect).
    plugin_highlights: Vec<(String, onda_plugin::Style)>,

    // ── Data views (CSV table) ─────────────────────────────────────────────────
    /// Docs currently shown as a CSV/TSV table, with their sniffed dialect.
    table_docs: HashMap<usize, onda_data::Dialect>,
    /// Cached per-column layout for table docs (computed on `:table` enable).
    table_layout: HashMap<usize, onda_data::ColumnLayout>,

    // ── Agent panel (W23) ───────────────────────────────────────────────────────
    /// Configured agents (agents.toml).
    agent_registry: onda_agent::AgentRegistry,
    /// Active agent client (None when not connected).
    agent_client: Option<onda_agent::AgentClient>,
    /// Name of the connected agent (for the panel title + permission scoping).
    agent_name: Option<String>,
    /// Whether the right-side agent panel is shown.
    agent_panel_open: bool,
    /// Whether keystrokes are routed to the panel input box.
    agent_input_focused: bool,
    /// The panel input buffer.
    agent_input: String,
    /// The conversation thread.
    agent_thread: Vec<AgentItem>,
    /// True between sending a prompt and `TurnEnded`.
    agent_busy: bool,
    /// Persisted permission rules.
    agent_perms: onda_agent::PermissionStore,
    /// A permission request awaiting a single-key decision (id + params).
    agent_pending_perm: Option<(serde_json::Value, onda_agent::RequestPermissionParams)>,
    /// Agent-proposed file edits awaiting review (T24.1 staging).
    agent_staging: onda_agent::StagingArea,
    /// Active diff-review session (T24.2), if any.
    review: Option<ReviewState>,

    // ── Persistent undo (T29.1) ─────────────────────────────────────────────────
    /// Persistent undo store (Some only when `editor.persistent_undo` is on).
    undo_store: Option<onda_session::UndoStore>,
    /// Doc indices whose persisted undo tree we've already tried to load (lazy, once).
    undo_loaded: std::collections::HashSet<usize>,

    // ── LSP ───────────────────────────────────────────────────────────────────
    /// LSP manager (None when tokio runtime unavailable, e.g. bench).
    #[allow(dead_code)]
    lsp_manager: Option<LspManager>,
    /// LSP event sender — used to bridge tokio → std channel.
    #[allow(dead_code)]
    lsp_event_tx: Option<tokio_mpsc::Sender<LspEvent>>,
    /// Diagnostics per document (keyed by path string).
    diagnostics: HashMap<PathBuf, Vec<LspDiagnostic>>,
    /// Diagnostic spans per document index (resolved to char offsets).
    diagnostic_spans: HashMap<usize, Vec<DiagnosticSpan>>,
    /// Next LSP request ID.
    #[allow(dead_code)]
    lsp_request_id: u64,
    /// Active hover float, if any.
    hover_float: Option<HoverFloat>,
    /// Active completion state, if any.
    completion: Option<CompletionState>,

    // ── Terminal ──────────────────────────────────────────────────────────────
    /// Active terminal panes (one per terminal window).
    terminal_panes: Vec<TerminalPane>,
    /// Next pane ID.
    next_pane_id: usize,
    /// Which window is a terminal pane (window_id → pane_id).
    window_to_pane: HashMap<usize, usize>,

    // ── Session ───────────────────────────────────────────────────────────────
    session_manager: SessionManager,

    // ── WASM plugins ───────────────────────────────────────────────────────────
    /// WASM plugin host (None in bench / when the engine fails to start).
    plugin_host: Option<PluginHost>,
    /// True after an idle tick has fired plugin events; reset on input.
    plugin_idle_fired: bool,
    /// Plugin decorations to paint: doc index → namespace → batch.
    plugin_decorations: HashMap<usize, HashMap<String, onda_plugin::DecorationBatch>>,

    /// Last buffer char-length we reparsed syntax for, per doc (change detector).
    doc_last_len: HashMap<usize, usize>,

    // ── Soft wrap ─────────────────────────────────────────────────────────────
    #[allow(dead_code)]
    soft_wrap: bool,

    #[cfg(feature = "bench")]
    tracer: LatencyTracer,
}

// ── Convenience accessors ─────────────────────────────────────────────────────

impl<B: Backend> App<B> {
    fn focused_win(&self) -> &WindowState {
        &self.windows[self.focused_window]
    }

    fn focused_win_mut(&mut self) -> &mut WindowState {
        &mut self.windows[self.focused_window]
    }

    fn doc(&self) -> &Document {
        let idx = self.focused_win().doc_idx;
        &self.docs[idx]
    }

    fn doc_mut(&mut self) -> &mut Document {
        let idx = self.focused_win().doc_idx;
        &mut self.docs[idx]
    }

    fn selection(&self) -> &Selection {
        &self.focused_win().selection
    }

    fn selection_mut(&mut self) -> &mut Selection {
        &mut self.focused_win_mut().selection
    }

    fn undo(&mut self) -> &mut UndoHistory {
        &mut self.focused_win_mut().undo
    }

    /// Lazily load the persisted undo tree for `doc_idx` on first undo of a fresh
    /// session (T29.1). No-op unless `editor.persistent_undo` is on; protects startup
    /// by never touching disk until the user actually undoes.
    fn maybe_load_persistent_undo(&mut self, doc_idx: usize) {
        if self.undo_store.is_none() || self.undo_loaded.contains(&doc_idx) {
            return;
        }
        self.undo_loaded.insert(doc_idx);
        // Only restore when the in-memory history is empty (nothing this session).
        if self.windows[self.focused_window].undo.can_undo() {
            return;
        }
        if self.docs[doc_idx].path().is_none() {
            return;
        }
        let content = self.docs[doc_idx].rope().to_string();
        if let Some(store) = self.undo_store.as_ref() {
            if let Some(tree) = store.load(&content) {
                self.windows[self.focused_window].undo = tree;
                self.message =
                    Message::Info("restored undo history from a previous session".into());
            }
        }
    }

    /// Persist the focused window's undo tree, keyed by the just-saved content (T29.1).
    fn persist_undo_on_save(&self, doc_idx: usize) {
        if let Some(store) = self.undo_store.as_ref() {
            if self.docs[doc_idx].path().is_some() {
                let content = self.docs[doc_idx].rope().to_string();
                store.save(&content, &self.windows[self.focused_window].undo);
            }
        }
    }

    fn viewport_mut(&mut self) -> &mut Viewport {
        &mut self.focused_win_mut().viewport
    }

    fn mode_indicator(&self) -> ModeIndicator {
        match self.mode {
            Mode::Normal => ModeIndicator::Normal,
            Mode::Insert => ModeIndicator::Insert,
            Mode::Visual => ModeIndicator::Visual,
            Mode::VisualLine => ModeIndicator::VisualLine,
            Mode::VisualBlock => ModeIndicator::Visual,
            Mode::Command => ModeIndicator::Command,
            Mode::Terminal => ModeIndicator::Terminal,
            Mode::TerminalScroll => ModeIndicator::TerminalScroll,
        }
    }

    #[allow(dead_code)]
    fn alloc_lsp_request_id(&mut self) -> u64 {
        let id = self.lsp_request_id;
        self.lsp_request_id += 1;
        id
    }

    /// Return the workspace root for the focused document.
    #[allow(dead_code)]
    fn workspace_root(&self) -> PathBuf {
        self.doc()
            .path()
            .and_then(|p| p.parent())
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
    }

    /// LSP: ensure a server is running for the current document.
    #[allow(dead_code)]
    fn ensure_lsp_for_current_doc(&self) {
        // Actual spawn is done in drain_bg_channel via tokio
    }

    fn current_doc_id(&self) -> DocumentId {
        self.doc().id()
    }

    /// Return the syntax worker for the focused window's document (if any).
    #[allow(dead_code)]
    fn current_syntax_worker(&self) -> Option<&SyntaxWorker> {
        let doc_idx = self.focused_win().doc_idx;
        self.syntax_workers.get(doc_idx).and_then(|w| w.as_ref())
    }

    // ── Search helpers ────────────────────────────────────────────────────────

    fn update_search_matches(&mut self) {
        if let Some(regex) = self.search.regex.as_ref() {
            let rope = self.doc().rope().clone();
            self.search_matches = find_all(&rope, regex);
        } else {
            self.search_matches.clear();
        }
    }

    // ── Syntax helpers ────────────────────────────────────────────────────────

    fn request_syntax_parse_for_doc(&mut self, doc_idx: usize) {
        let doc = &self.docs[doc_idx];
        let path_str = doc
            .path()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let first_line: Option<String> = if doc.len_lines() > 0 {
            Some(doc.rope().line(0).to_string())
        } else {
            None
        };
        let lang_name = self
            .lang_registry
            .detect(&path_str, first_line.as_deref())
            .map(|c| c.name.clone());

        if let Some(lang) = lang_name {
            // Ensure we have a worker slot for this doc index.
            while self.syntax_workers.len() <= doc_idx {
                self.syntax_workers.push(None);
                self.syntax_versions.push(0);
            }
            if self.syntax_workers[doc_idx].is_none() {
                // SyntaxWorker::spawn() requires a Tokio runtime context.
                // During bench/null-backend runs no runtime may be available,
                // so we skip worker creation if the try-spawn fails.
                // TODO T6.4: always spawn worker, handle no-runtime gracefully.
            }
            if let Some(worker) = self.syntax_workers[doc_idx].as_ref() {
                // Bump the version so the worker treats this as a fresh parse.
                self.syntax_versions[doc_idx] = self.syntax_versions[doc_idx].wrapping_add(1);
                let version = self.syntax_versions[doc_idx];
                worker.request_parse(doc.rope().clone(), lang, version);
            }
        }
    }

    fn try_spawn_syntax_worker_for_doc(&mut self, doc_idx: usize) {
        while self.syntax_workers.len() <= doc_idx {
            self.syntax_workers.push(None);
            self.syntax_versions.push(0);
        }
        if self.syntax_workers[doc_idx].is_none() {
            self.syntax_workers[doc_idx] = Some(SyntaxWorker::spawn());
        }
        self.request_syntax_parse_for_doc(doc_idx);
    }

    /// Resolve the window's syntax highlights into theme-styled char spans for the
    /// visible region only (byte→char converted, last-writer-wins on overlap → a
    /// sorted, non-overlapping list the renderer can stream).
    fn build_highlights(&self, win_idx: usize, height: u16) -> Vec<onda_render::HlSpan> {
        let doc_idx = self.windows[win_idx].doc_idx;
        let worker = match self.syntax_workers.get(doc_idx).and_then(|w| w.as_ref()) {
            Some(w) => w,
            None => return Vec::new(),
        };
        let hls = match worker.current_highlights() {
            Some(h) => h,
            None => return Vec::new(),
        };
        let doc = &self.docs[doc_idx];
        let rope = doc.rope();
        let total_lines = doc.len_lines();

        // Visible char window.
        let vp = &self.windows[win_idx].viewport;
        let first_line = vp.offset_line.min(total_lines.saturating_sub(1));
        let last_line = (vp.offset_line + height as usize).min(total_lines);
        let first_char = doc.line_to_char(first_line);
        let last_char = if last_line >= total_lines {
            rope.len_chars()
        } else {
            doc.line_to_char(last_line)
        };
        if last_char <= first_char {
            return Vec::new();
        }
        let len = last_char - first_char;
        let mut painted: Vec<Option<onda_render::Style>> = vec![None; len];

        let start_byte = rope.char_to_byte(first_char);
        let end_byte = rope.char_to_byte(last_char);
        for span in hls.spans_in_range(start_byte, end_byte) {
            let style = self.theme.syntax(scope_name(span.scope));
            let cs = rope
                .byte_to_char(span.start.min(rope.len_bytes()))
                .max(first_char);
            let ce = rope
                .byte_to_char(span.end.min(rope.len_bytes()))
                .min(last_char);
            for slot in painted
                .iter_mut()
                .take(ce - first_char)
                .skip(cs - first_char)
            {
                *slot = Some(style);
            }
        }

        // Emit contiguous runs of equal style as HlSpans (absolute char coords).
        let mut out = Vec::new();
        let mut run_start = 0usize;
        while run_start < len {
            match painted[run_start] {
                None => run_start += 1,
                Some(style) => {
                    let mut run_end = run_start + 1;
                    while run_end < len && painted[run_end] == Some(style) {
                        run_end += 1;
                    }
                    out.push(onda_render::HlSpan {
                        start: first_char + run_start,
                        end: first_char + run_end,
                        style,
                    });
                    run_start = run_end;
                }
            }
        }
        out
    }

    // ── Data views (CSV table / JSONL fields) ──────────────────────────────────

    /// Toggle CSV/TSV table view for the focused doc. View-only: the rope is
    /// untouched; on enable we sniff the dialect and cache column widths from a
    /// sample of the file.
    fn toggle_table_view(&mut self) {
        let doc_idx = self.focused_win().doc_idx;
        if self.table_docs.remove(&doc_idx).is_some() {
            self.table_layout.remove(&doc_idx);
            self.compositor.buf.invalidate();
            self.message = Message::Info("table view off".into());
            return;
        }
        let doc = &self.docs[doc_idx];
        // Sample the first chunk for sniffing + widths (bounded for big files).
        let sample_lines: Vec<String> = (0..doc.len_lines().min(500))
            .map(|l| {
                let s = doc.line_to_char(l);
                let len = doc.line_len_no_eol(l);
                onda_data::csv::clean_line(&doc.rope().slice(s..s + len).to_string()).to_string()
            })
            .collect();
        let dialect = onda_data::sniff(&sample_lines.join("\n"));
        let rows: Vec<Vec<String>> = sample_lines
            .iter()
            .map(|l| onda_data::parse_fields(l, dialect.delimiter, dialect.quote))
            .collect();
        let layout = onda_data::column_layout(&rows);
        self.table_docs.insert(doc_idx, dialect);
        self.table_layout.insert(doc_idx, layout);
        self.compositor.buf.invalidate();
        self.message = Message::Info(format!(
            "table view on ({} cols, delim {:?})",
            self.table_layout[&doc_idx].column_count(),
            dialect.delimiter
        ));
    }

    /// Show the JSONL field schema for the focused doc in a float overlay.
    fn show_jsonl_fields(&mut self) {
        let doc = self.doc();
        let n = doc.len_lines().min(1000);
        let lines: Vec<String> = (0..n)
            .map(|l| {
                let s = doc.line_to_char(l);
                let len = doc.line_len_no_eol(l);
                doc.rope().slice(s..s + len).to_string()
            })
            .collect();
        let schema = onda_data::field_schema(lines.iter().map(|s| s.as_str()), n);
        if schema.is_empty() {
            self.message = Message::Info("no JSON records found".into());
            return;
        }
        let mut out = vec![format!("fields (sampled {n} records):")];
        for f in &schema {
            let types: Vec<String> = f
                .types
                .iter()
                .map(|(t, c)| format!("{}×{}", t.label(), c))
                .collect();
            out.push(format!(
                "  {}  [{}]  ({})",
                f.key,
                f.count,
                types.join(", ")
            ));
        }
        self.hover_float = Some(HoverFloat {
            lines: out,
            col: 4,
            row: 2,
        });
    }

    // ── Agent panel (W23) ───────────────────────────────────────────────────────

    /// `:agent [name]` — with a name, connect + open + focus; without, toggle panel.
    fn agent_command(&mut self, name: Option<String>) {
        match name {
            Some(n) => self.agent_connect(&n),
            None => {
                self.agent_panel_open = !self.agent_panel_open;
                self.agent_input_focused = self.agent_panel_open;
                self.compositor.buf.invalidate();
            }
        }
    }

    fn agent_connect(&mut self, name: &str) {
        let cfg = match self.agent_registry.get(name) {
            Some(c) => c.clone(),
            None => {
                let avail: Vec<&str> = self.agent_registry.names().collect();
                self.message = Message::Error(format!(
                    "unknown agent '{name}' (have: {})",
                    avail.join(", ")
                ));
                return;
            }
        };
        self.agent_panel_open = true;
        self.agent_input_focused = true;
        self.agent_name = Some(name.to_string());
        self.agent_thread.clear();
        self.agent_thread
            .push(AgentItem::Notice(format!("connecting to {name}…")));
        self.compositor.buf.invalidate();

        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let bg_tx = self.bg_tx.clone();
        let (etx, mut erx) = tokio::sync::mpsc::channel::<onda_agent::AgentEvent>(256);
        let bridge_tx = bg_tx.clone();
        tokio::spawn(async move {
            while let Some(ev) = erx.recv().await {
                if bridge_tx.try_send(BgMessage::Agent(ev)).is_err() {
                    break;
                }
            }
        });
        tokio::spawn(async move {
            match onda_agent::AgentClient::connect(&cfg, cwd, etx).await {
                Ok(client) => {
                    let _ = bg_tx.try_send(BgMessage::AgentClientReady(client));
                }
                Err(e) => {
                    let _ = bg_tx.try_send(BgMessage::Agent(onda_agent::AgentEvent::Error {
                        message: format!("connect failed: {e}"),
                    }));
                }
            }
        });
    }

    /// Send the input box as a prompt (resolving `@mentions`).
    fn agent_send(&mut self) {
        let text = self.agent_input.trim().to_string();
        if text.is_empty() {
            return;
        }
        let Some(client) = self.agent_client.as_ref() else {
            self.agent_thread
                .push(AgentItem::Notice("not connected (:agent <name>)".into()));
            self.agent_input.clear();
            return;
        };
        let blocks = self.agent_resolve_mentions(&text);
        client.dispatch(onda_agent::AgentCommand::Prompt(blocks));
        self.agent_thread.push(AgentItem::User(text));
        self.agent_thread.push(AgentItem::Assistant(String::new()));
        self.agent_busy = true;
        self.agent_input.clear();
    }

    /// Resolve `@file`/`@selection`/`@buffer`/`@diagnostics` mentions into ACP content
    /// blocks (text first, then attached resources). Open buffers are read live.
    fn agent_resolve_mentions(&self, text: &str) -> Vec<onda_agent::ContentBlock> {
        use onda_agent::mentions::{build_context, MentionKind, DEFAULT_MAX_LINES};
        let mut blocks = vec![onda_agent::ContentBlock::text(text)];
        for m in onda_agent::parse_mentions(text) {
            match m.kind {
                MentionKind::File | MentionKind::Buffer => {
                    if let Some(arg) = &m.arg {
                        // Prefer an open buffer (dirty content); else read disk.
                        let content = self
                            .docs
                            .iter()
                            .find(|d| {
                                d.path().map(|p| p.ends_with(arg)).unwrap_or(false)
                                    || d.name() == arg
                            })
                            .map(|d| d.rope().to_string())
                            .or_else(|| std::fs::read_to_string(arg).ok());
                        if let Some(c) = content {
                            blocks.push(
                                build_context(format!("file://{arg}"), None, &c, DEFAULT_MAX_LINES)
                                    .block,
                            );
                        }
                    }
                }
                MentionKind::Selection => {
                    let sel = self.selection().primary();
                    let (from, to) = (sel.from(), sel.to());
                    if to > from {
                        let content = self.doc().rope().slice(from..to).to_string();
                        blocks.push(
                            build_context("onda-selection://", None, &content, DEFAULT_MAX_LINES)
                                .block,
                        );
                    }
                }
                MentionKind::Diagnostics => {
                    let doc_idx = self.focused_win().doc_idx;
                    if let Some(spans) = self.diagnostic_spans.get(&doc_idx) {
                        let items: Vec<onda_agent::DiagnosticItem> = spans
                            .iter()
                            .map(|s| {
                                let line = self.doc().char_to_line(s.from);
                                onda_agent::DiagnosticItem {
                                    line,
                                    col: 0,
                                    severity: match s.severity {
                                        0 => onda_agent::Severity::Error,
                                        1 => onda_agent::Severity::Warning,
                                        _ => onda_agent::Severity::Info,
                                    },
                                    message: String::new(),
                                }
                            })
                            .collect();
                        let text =
                            onda_agent::format_diagnostics(&items, onda_agent::Severity::Hint);
                        if !text.is_empty() {
                            blocks.push(
                                build_context(
                                    "onda-diagnostics://",
                                    None,
                                    &text,
                                    DEFAULT_MAX_LINES,
                                )
                                .block,
                            );
                        }
                    }
                }
                MentionKind::Terminal | MentionKind::Unknown => {}
            }
        }
        blocks
    }

    /// Append/route an agent event into the thread.
    fn handle_agent_event(&mut self, ev: onda_agent::AgentEvent) {
        use onda_agent::AgentEvent as E;
        match ev {
            E::Initialized { .. } => {}
            E::SessionCreated { .. } => {
                self.agent_thread
                    .push(AgentItem::Notice("session ready".into()));
            }
            E::MessageChunk { text } => {
                if let Some(AgentItem::Assistant(s)) = self.agent_thread.last_mut() {
                    s.push_str(&text);
                } else {
                    self.agent_thread.push(AgentItem::Assistant(text));
                }
            }
            E::ThoughtChunk { text } => {
                if let Some(AgentItem::Thought(s)) = self.agent_thread.last_mut() {
                    s.push_str(&text);
                } else {
                    self.agent_thread.push(AgentItem::Thought(text));
                }
            }
            E::ToolCallStarted(tc) => self.agent_thread.push(AgentItem::Tool {
                title: tc.title,
                status: format!("{:?}", tc.status).to_lowercase(),
            }),
            E::ToolCallUpdated(u) => {
                if let Some(st) = u.status {
                    if let Some(AgentItem::Tool { status, .. }) = self
                        .agent_thread
                        .iter_mut()
                        .rev()
                        .find(|i| matches!(i, AgentItem::Tool { .. }))
                    {
                        *status = format!("{st:?}").to_lowercase();
                    }
                }
            }
            E::Plan(entries) => self.agent_thread.push(AgentItem::Plan(
                entries.into_iter().map(|e| e.content).collect(),
            )),
            E::PermissionRequest { request_id, params } => {
                self.agent_handle_permission(request_id, params)
            }
            E::FileReadRequest { request_id, params } => {
                self.agent_serve_file_read(request_id, params)
            }
            E::FileWriteRequest { request_id, params } => {
                // Stage the proposed edit (T24.1) for review; do not touch the buffer.
                let base = self
                    .docs
                    .iter()
                    .find(|d| {
                        d.path()
                            .map(|p| p.to_string_lossy() == params.path)
                            .unwrap_or(false)
                    })
                    .map(|d| d.rope().to_string())
                    .or_else(|| std::fs::read_to_string(&params.path).ok())
                    .unwrap_or_default();
                self.agent_staging
                    .stage(PathBuf::from(&params.path), base, params.content);
                if let Some(client) = self.agent_client.as_ref() {
                    client.dispatch(onda_agent::AgentCommand::RespondFileWrite {
                        id: request_id,
                        result: Ok(()),
                    });
                }
                self.agent_thread.push(AgentItem::Notice(format!(
                    "proposed edit to {} — :agent-review",
                    params.path
                )));
            }
            E::UnknownRequest { request_id, .. } => {
                if let Some(client) = self.agent_client.as_ref() {
                    client.dispatch(onda_agent::AgentCommand::RespondUnknown { id: request_id });
                }
            }
            E::TurnEnded { .. } => {
                self.agent_busy = false;
                self.agent_thread
                    .push(AgentItem::Notice("— turn complete —".into()));
            }
            E::Error { message } => self
                .agent_thread
                .push(AgentItem::Notice(format!("error: {message}"))),
            E::Malformed(_) => {}
        }
    }

    /// Serve an `fs/read_text_file` from a live buffer (dirty content) or disk.
    fn agent_serve_file_read(
        &mut self,
        request_id: serde_json::Value,
        params: onda_agent::ReadTextFileParams,
    ) {
        let path = &params.path;
        let content = self
            .docs
            .iter()
            .find(|d| {
                d.path()
                    .map(|p| p.to_string_lossy() == *path)
                    .unwrap_or(false)
            })
            .map(|d| d.rope().to_string())
            .or_else(|| std::fs::read_to_string(path).ok());
        if let Some(client) = self.agent_client.as_ref() {
            client.dispatch(onda_agent::AgentCommand::RespondFileRead {
                id: request_id,
                content: content.ok_or_else(|| format!("cannot read {path}")),
            });
        }
    }

    /// Handle a permission request: auto-decide from the store, else prompt in-panel.
    fn agent_handle_permission(
        &mut self,
        request_id: serde_json::Value,
        params: onda_agent::RequestPermissionParams,
    ) {
        let agent = self.agent_name.clone().unwrap_or_else(|| "agent".into());
        let target = onda_agent::Target::Command(params.tool_call.title.clone());
        let tool = params.tool_call.kind;
        if let Some(decision) = self.agent_perms.check(&agent, tool, &target) {
            let allow = decision == onda_agent::Decision::Allow;
            self.agent_respond_permission(request_id, &params, allow);
            self.agent_thread.push(AgentItem::Notice(format!(
                "permission {} (rule): {}",
                if allow { "allowed" } else { "denied" },
                params.tool_call.title
            )));
            return;
        }
        self.agent_thread.push(AgentItem::Notice(format!(
            "[permission] {} — (a)llow once / (A)lways / (d)eny",
            params.tool_call.title
        )));
        self.agent_pending_perm = Some((request_id, params));
    }

    /// Resolve the pending permission with a single-key choice.
    fn agent_resolve_pending_perm(&mut self, key: char) {
        let Some((id, params)) = self.agent_pending_perm.take() else {
            return;
        };
        let agent = self.agent_name.clone().unwrap_or_else(|| "agent".into());
        let target = onda_agent::Target::Command(params.tool_call.title.clone());
        let tool = params.tool_call.kind;
        let (allow, persist) = match key {
            'a' => (true, false),
            'A' => (true, true),
            'd' => (false, false),
            'D' => (false, true),
            _ => {
                self.agent_pending_perm = Some((id, params));
                return;
            }
        };
        if persist {
            let kind = if allow {
                onda_agent::PermissionOptionKind::AllowAlways
            } else {
                onda_agent::PermissionOptionKind::RejectAlways
            };
            self.agent_perms.apply_choice(&agent, tool, &target, kind);
            if let Some(p) = agent_perms_path() {
                let _ = self.agent_perms.save(&p);
            }
        }
        self.agent_respond_permission(id, &params, allow);
        self.agent_thread.push(AgentItem::Notice(format!(
            "permission {}",
            if allow { "allowed" } else { "denied" }
        )));
    }

    fn agent_respond_permission(
        &self,
        id: serde_json::Value,
        params: &onda_agent::RequestPermissionParams,
        allow: bool,
    ) {
        let want = if allow {
            [
                onda_agent::PermissionOptionKind::AllowOnce,
                onda_agent::PermissionOptionKind::AllowAlways,
            ]
        } else {
            [
                onda_agent::PermissionOptionKind::RejectOnce,
                onda_agent::PermissionOptionKind::RejectAlways,
            ]
        };
        let option_id = params
            .options
            .iter()
            .find(|o| want.contains(&o.kind))
            .or_else(|| params.options.first())
            .map(|o| o.option_id.clone());
        let outcome = match option_id {
            Some(id) => onda_agent::PermissionOutcome::Selected { option_id: id },
            None => onda_agent::PermissionOutcome::Cancelled,
        };
        if let Some(client) = self.agent_client.as_ref() {
            client.dispatch(onda_agent::AgentCommand::RespondPermission { id, outcome });
        }
    }

    /// Export the conversation transcript to a scratch buffer (`:agent-export`).
    fn agent_export(&mut self) {
        if self.agent_thread.is_empty() {
            self.message = Message::Info("agent: no transcript".into());
            return;
        }
        let mut text = String::from("# Agent transcript\n\n");
        for item in &self.agent_thread {
            match item {
                AgentItem::User(s) => text.push_str(&format!("## you\n{s}\n\n")),
                AgentItem::Assistant(s) => text.push_str(&format!("## agent\n{s}\n\n")),
                AgentItem::Thought(s) => text.push_str(&format!("> (thinking) {s}\n\n")),
                AgentItem::Tool { title, status } => {
                    text.push_str(&format!("- tool: {title} [{status}]\n"))
                }
                AgentItem::Plan(entries) => {
                    text.push_str("plan:\n");
                    for e in entries {
                        text.push_str(&format!("  - {e}\n"));
                    }
                    text.push('\n');
                }
                AgentItem::Notice(s) => text.push_str(&format!("_{s}_\n\n")),
            }
        }
        let mut doc = Document::new_empty();
        let cs = onda_core::transaction::ChangeSetBuilder::new(0)
            .insert(&text)
            .build();
        let _ = doc.apply(&Transaction::new(cs));
        let idx = self.docs.len();
        self.docs.push(doc);
        self.focused_win_mut().doc_idx = idx;
        *self.selection_mut() = Selection::point(0);
        self.message = Message::Info("agent: transcript exported to a new buffer".into());
    }

    /// Keystrokes while the panel input is focused. Returns true if consumed.
    fn handle_agent_input_key(&mut self, key: &Key) -> bool {
        // A pending permission steals single-key a/A/d/D.
        if self.agent_pending_perm.is_some() {
            if let Key::Char(c, _) = key {
                if matches!(c, 'a' | 'A' | 'd' | 'D') {
                    self.agent_resolve_pending_perm(*c);
                    return true;
                }
            }
        }
        match key {
            Key::Esc => {
                self.agent_input_focused = false;
                true
            }
            Key::Enter => {
                self.agent_send();
                true
            }
            Key::Backspace => {
                self.agent_input.pop();
                true
            }
            Key::Char(c, _) => {
                self.agent_input.push(*c);
                true
            }
            _ => false,
        }
    }

    /// Width reserved for the agent panel (0 when closed), clamped to sane bounds.
    fn agent_panel_width(&self, total: u16) -> u16 {
        if !self.agent_panel_open {
            return 0;
        }
        (total / 3).clamp(30, 64).min(total.saturating_sub(20))
    }

    /// Format the thread into styled panel lines (newest at the bottom).
    fn agent_panel_lines(&self) -> Vec<(onda_render::Style, String)> {
        use onda_render::{Color, Style};
        let mut out = Vec::new();
        let wrap = 36usize;
        let push_wrapped =
            |prefix: &str, text: &str, style: Style, out: &mut Vec<(Style, String)>| {
                let full = format!("{prefix}{text}");
                for chunk in wrap_text(&full, wrap) {
                    out.push((style, chunk));
                }
            };
        for item in &self.agent_thread {
            match item {
                AgentItem::User(s) => {
                    push_wrapped("you: ", s, Style::default().fg(Color::LightCyan), &mut out)
                }
                AgentItem::Assistant(s) => push_wrapped("", s, self.theme.text(), &mut out),
                AgentItem::Thought(s) => {
                    push_wrapped("· ", s, Style::default().fg(Color::DarkGray), &mut out)
                }
                AgentItem::Tool { title, status } => out.push((
                    Style::default().fg(Color::Yellow),
                    format!("⚙ {title} [{status}]"),
                )),
                AgentItem::Plan(entries) => {
                    out.push((Style::default().fg(Color::Magenta), "plan:".into()));
                    for e in entries {
                        out.push((Style::default().fg(Color::Magenta), format!("  • {e}")));
                    }
                }
                AgentItem::Notice(s) => {
                    out.push((Style::default().fg(Color::DarkGray), format!("— {s}")))
                }
            }
        }
        out
    }

    // ── Agent diff review (T24.2) ───────────────────────────────────────────────

    /// `:agent-review` — start reviewing staged agent edits, one file at a time.
    fn agent_review_start(&mut self) {
        let mut paths: Vec<PathBuf> = self.agent_staging.files().map(|(p, _)| p.clone()).collect();
        paths.sort();
        if paths.is_empty() {
            self.message = Message::Info("no proposed edits to review".into());
            return;
        }
        let first = paths.remove(0);
        match self.build_review(&first, paths) {
            Some(rs) => {
                self.review = Some(rs);
                self.agent_input_focused = false;
                self.compositor.buf.invalidate();
            }
            None => self.message = Message::Info("proposed edit has no changes".into()),
        }
    }

    /// Build a `ReviewState` for `path`, diffing the live buffer/disk against the
    /// staged proposal. Returns None when there's nothing to review.
    fn build_review(&self, path: &std::path::Path, remaining: Vec<PathBuf>) -> Option<ReviewState> {
        let proposed = self.agent_staging.get(path)?.proposed.clone();
        let base = self
            .docs
            .iter()
            .find(|d| d.path().map(|p| p == path).unwrap_or(false))
            .map(|d| d.rope().to_string())
            .or_else(|| std::fs::read_to_string(path).ok())
            .unwrap_or_default();
        let hunks = onda_agent::file_hunks(&base, &proposed);
        if hunks.is_empty() {
            return None;
        }
        let accept = vec![true; hunks.len()];
        Some(ReviewState {
            path: path.to_path_buf(),
            base,
            hunks,
            accept,
            cursor: 0,
            remaining,
        })
    }

    /// Keystrokes while a review is active. Returns true if consumed.
    fn handle_review_key(&mut self, key: &Key) -> bool {
        let Some(rs) = self.review.as_mut() else {
            return false;
        };
        match key {
            Key::Esc | Key::Char('q', _) => {
                self.review = None;
                self.message = Message::Info("review cancelled".into());
            }
            Key::Char('j', _) | Key::Down => {
                if rs.cursor + 1 < rs.hunks.len() {
                    rs.cursor += 1;
                }
            }
            Key::Char('k', _) | Key::Up => {
                rs.cursor = rs.cursor.saturating_sub(1);
            }
            Key::Char('a', _) => rs.accept[rs.cursor] = true,
            Key::Char('r', _) => rs.accept[rs.cursor] = false,
            Key::Char('A', _) => rs.accept.iter_mut().for_each(|a| *a = true),
            Key::Char('R', _) => rs.accept.iter_mut().for_each(|a| *a = false),
            Key::Enter | Key::Char('y', _) => {
                self.review_apply();
            }
            _ => {}
        }
        self.compositor.buf.invalidate();
        true
    }

    /// Apply the accepted hunks of the active review to the buffer as one undo step,
    /// then advance to the next staged file.
    fn review_apply(&mut self) {
        let Some(rs) = self.review.take() else { return };
        let accepted: Vec<onda_agent::Hunk> = rs
            .hunks
            .iter()
            .zip(rs.accept.iter())
            .filter(|(_, a)| **a)
            .map(|(h, _)| h.clone())
            .collect();
        let n_accepted = accepted.len();
        let new_content = onda_agent::apply_selected(&rs.base, &accepted);

        // Open the file into a buffer if it isn't already, then focus it.
        let doc_idx = match self
            .docs
            .iter()
            .position(|d| d.path().map(|p| p == rs.path).unwrap_or(false))
        {
            Some(i) => i,
            None => match Document::open(&rs.path) {
                Ok(doc) => {
                    let i = self.docs.len();
                    self.docs.push(doc);
                    self.try_spawn_syntax_worker_for_doc(i);
                    i
                }
                Err(e) => {
                    self.message = Message::Error(format!("review apply: {e}"));
                    self.agent_staging.remove(&rs.path);
                    self.review_advance(rs.remaining);
                    return;
                }
            },
        };
        self.focused_win_mut().doc_idx = doc_idx;

        // Full-buffer replace as a single undo step (one per buffer, per T24.2/T11.5).
        if new_content != rs.base {
            let len = self.doc().len_chars();
            let cs = onda_core::transaction::ChangeSetBuilder::new(len)
                .delete(len)
                .insert(&new_content)
                .build();
            let tx = Transaction::new(cs);
            let sel_before = self.selection().clone();
            if let Ok(inv) = self.doc_mut().apply(&tx) {
                *self.selection_mut() = Selection::point(0);
                let sel_after = self.selection().clone();
                self.undo().push(tx, inv, sel_before, sel_after);
            }
        }
        self.agent_staging.remove(&rs.path);
        let total = rs.hunks.len();
        self.message = Message::Info(format!(
            "applied {n_accepted}/{total} hunks to {}",
            rs.path.display()
        ));
        // Tell the agent which hunks were rejected (context for the next turn).
        if n_accepted < total {
            if let Some(client) = self.agent_client.as_ref() {
                let _ = client.dispatch(onda_agent::AgentCommand::Prompt(vec![
                    onda_agent::ContentBlock::text(format!(
                        "[review] applied {n_accepted}/{total} hunks to {}; the rest were rejected.",
                        rs.path.display()
                    )),
                ]));
            }
        }
        self.review_advance(rs.remaining);
    }

    /// Move to the next staged file, or close review when done.
    fn review_advance(&mut self, remaining: Vec<PathBuf>) {
        let mut rest = remaining;
        while let Some(next) = rest.first().cloned() {
            rest.remove(0);
            if let Some(rs) = self.build_review(&next, rest.clone()) {
                self.review = Some(rs);
                self.compositor.buf.invalidate();
                return;
            }
            self.agent_staging.remove(&next);
        }
        self.review = None;
        self.compositor.buf.invalidate();
    }

    /// Format the active review into styled overlay lines (header + hunks).
    fn review_lines(&self) -> Vec<(onda_render::Style, String)> {
        use onda_render::{Color, Style};
        let mut out = Vec::new();
        let Some(rs) = self.review.as_ref() else {
            return out;
        };
        let accepted = rs.accept.iter().filter(|a| **a).count();
        out.push((
            Style::default().bold(),
            format!(
                "Review {}  ({} hunk(s), {accepted} accepted)  a/r toggle · A/R all · ⏎ apply · q cancel",
                rs.path.display(),
                rs.hunks.len()
            ),
        ));
        out.push((self.theme.line_nr(), String::new()));
        for (i, h) in rs.hunks.iter().enumerate() {
            let mark = if rs.accept[i] { "✓" } else { "✗" };
            let focus = if i == rs.cursor { "▶" } else { " " };
            out.push((
                if i == rs.cursor {
                    Style::default().fg(Color::LightCyan).bold()
                } else {
                    self.theme.text()
                },
                format!("{focus} {mark} hunk {} @ line {}", i + 1, h.base_start + 1),
            ));
            for removed in onda_agent::hunk_removed(h, &rs.base) {
                out.push((Style::default().fg(Color::Red), format!("  - {removed}")));
            }
            for added in &h.replacement {
                out.push((Style::default().fg(Color::Green), format!("  + {added}")));
            }
        }
        out
    }

    // ── Decoration refresh ──────────────────────────────────────────────────────

    /// Reparse syntax for the focused doc when its length changed since the last
    /// request (cheap per-key change detector). The syntax worker debounces and rope
    /// clones are O(1)-ish, so this stays off the hot path.
    fn maybe_refresh_decorations(&mut self) {
        let doc_idx = self.focused_win().doc_idx;
        let len = match self.docs.get(doc_idx) {
            Some(d) => d.len_chars(),
            None => return,
        };
        if self.doc_last_len.get(&doc_idx) == Some(&len) {
            return;
        }
        self.doc_last_len.insert(doc_idx, len);
        self.request_syntax_parse_for_doc(doc_idx);
    }

    // ── Theme helpers (T18.1) ─────────────────────────────────────────────────

    /// Switch to the named theme: load it, re-watch its file, re-apply Lua overrides,
    /// and force a full damage-tracked re-render.
    fn apply_theme(&mut self, name: &str) {
        let (theme, path) = load_theme(name);
        self.theme = theme;
        self.theme_watcher = path
            .as_ref()
            .and_then(|p| spawn_theme_watcher(p, self.bg_tx.clone()));
        self.theme_path = path;
        self.reapply_plugin_highlights();
        // Full re-render: theme changes touch every cell (rule 3 allows redraw on
        // theme change). The compositor still diffs, so only changed cells flush.
        self.compositor.buf.invalidate();
    }

    /// Re-read the active theme file from disk (live reload).
    fn reload_theme_file(&mut self) {
        let Some(path) = self.theme_path.clone() else {
            return;
        };
        let name = self.theme.name().to_string();
        match std::fs::read_to_string(&path) {
            Ok(text) => match onda_render::Theme::from_toml(&name, &text) {
                Ok(theme) => {
                    self.theme = theme;
                    self.reapply_plugin_highlights();
                    self.compositor.buf.invalidate();
                }
                Err(e) => {
                    self.message = Message::Error(format!("theme error: {e}"));
                }
            },
            Err(e) => {
                self.message = Message::Error(format!("theme read error: {e}"));
            }
        }
    }

    /// Re-apply all plugin-registered highlight overrides on top of the current theme.
    fn reapply_plugin_highlights(&mut self) {
        for (group, style) in &self.plugin_highlights {
            let _ = self.theme.set_parsed(
                group,
                style.fg.as_deref(),
                style.bg.as_deref(),
                style.bold,
                style.italic,
                style.underline,
            );
        }
    }
}

// ── Render ────────────────────────────────────────────────────────────────────

impl<B: Backend> App<B> {
    fn render_frame(&mut self) -> Result<(), RenderError> {
        let (width, height) = self.backend.size();
        if width == 0 || height == 0 {
            return Ok(());
        }

        let content_height = height.saturating_sub(2);
        let status_row = height.saturating_sub(2);
        let msg_row = height.saturating_sub(1);
        let mode_ind = self.mode_indicator();

        // Carve a right strip for the agent panel, if open.
        let panel_width = self.agent_panel_width(width);
        let editor_width = width - panel_width;

        // ── Phase 1: update viewports (no grid access yet) ────────────────────
        let content_area = Rect::new(0, 0, editor_width, content_height);
        let rects = self.layout.rects(content_area);
        let focused_win_id = WindowId(self.focused_window);

        for (win_id, rect) in &rects {
            let win_idx = win_id.0;
            if win_idx >= self.windows.len() {
                continue;
            }
            let doc_idx = self.windows[win_idx].doc_idx;
            if doc_idx >= self.docs.len() {
                continue;
            }
            let cursor_line = {
                let doc = &self.docs[doc_idx];
                let sel = &self.windows[win_idx].selection;
                doc.char_to_line(sel.primary().head)
            };
            self.windows[win_idx]
                .viewport
                .scroll_to(cursor_line, rect.height as usize);
        }

        // ── Phase 2: collect all render data ──────────────────────────────────

        // Build message string
        let msg = if self.mode == Mode::Command {
            if let Some(dir) = self.search_input_dir {
                let prefix = if dir == SearchInputDir::Forward {
                    "/"
                } else {
                    "?"
                };
                Message::Command(format!("{}{}", prefix, self.command_line.as_str()))
            } else {
                Message::Command(self.command_line.as_str().to_string())
            }
        } else {
            self.message.clone()
        };

        // Build picker data
        #[allow(clippy::type_complexity)]
        let picker_data: Option<(String, String, Vec<(String, bool)>, u16, u16)> =
            self.picker.as_ref().and_then(|p| {
                if p.is_visible() {
                    let items: Vec<(String, bool)> = p
                        .filtered_items()
                        .enumerate()
                        .map(|(i, item)| (item.display.clone(), i == p.selected_index()))
                        .collect();
                    let pw = (width * 2 / 3).max(40).min(width);
                    let ph = 20u16.min(height.saturating_sub(4));
                    Some((p.title().to_string(), p.query().to_string(), items, pw, ph))
                } else {
                    None
                }
            });

        let macro_recording = self.macros.is_recording();
        let search_matches = self.search_matches.clone();

        // Cursor position (computed before getting grid)
        let (cursor_col, cursor_row) = self.cursor_screen_pos(&rects);

        // Syntax highlights per window, resolved to styled char spans *before* the
        // grid borrow (so the worker output + theme can be read off `self`).
        let mut highlights: HashMap<usize, Vec<onda_render::HlSpan>> = HashMap::new();
        for (win_id, rect) in &rects {
            let win_idx = win_id.0;
            if win_idx >= self.windows.len() || self.window_to_pane.contains_key(&win_idx) {
                continue;
            }
            let spans = self.build_highlights(win_idx, rect.height);
            if !spans.is_empty() {
                highlights.insert(win_idx, spans);
            }
        }

        // Agent panel content, formatted before the grid borrow.
        let agent_panel: Option<AgentPanelData> = if panel_width > 0 {
            Some((
                self.agent_panel_lines(),
                self.agent_input.clone(),
                self.agent_busy,
                self.agent_name.clone().unwrap_or_else(|| "Agent".into()),
            ))
        } else {
            None
        };

        // Diff-review overlay lines, formatted before the grid borrow.
        let review_lines: Vec<(onda_render::Style, String)> = if self.review.is_some() {
            self.review_lines()
        } else {
            Vec::new()
        };

        // ── Phase 3: render into grid ─────────────────────────────────────────
        {
            let theme = &self.theme;
            let grid = self.compositor.buf.current_mut();

            // Draw borders
            if rects.len() > 1 {
                draw_borders(grid, &rects, onda_render::SplitDir::Horizontal);
                draw_borders(grid, &rects, onda_render::SplitDir::Vertical);
            }

            // Render each window
            for (win_id, rect) in &rects {
                let win_idx = win_id.0;
                if win_idx >= self.windows.len() {
                    continue;
                }

                // Check if this is a terminal pane
                if let Some(&pane_id) = self.window_to_pane.get(&win_idx) {
                    if let Some(pane) = self.terminal_panes.iter().find(|p| p.pane_id == pane_id) {
                        render_terminal_pane(grid, &pane.screen, rect);
                        continue;
                    }
                }

                let doc_idx = self.windows[win_idx].doc_idx;
                if doc_idx >= self.docs.len() {
                    continue;
                }
                let doc = &self.docs[doc_idx];
                let sel = &self.windows[win_idx].selection;
                let viewport = &self.windows[win_idx].viewport;
                let is_focused = *win_id == focused_win_id;
                let matches: &[onda_core::Range] = if is_focused { &search_matches } else { &[] };
                let diag_spans = self
                    .diagnostic_spans
                    .get(&doc_idx)
                    .map(|v| v.as_slice())
                    .unwrap_or(&[]);
                let hl_spans: &[onda_render::HlSpan] = highlights
                    .get(&win_idx)
                    .map(|v| v.as_slice())
                    .unwrap_or(&[]);

                // CSV/TSV table view takes over rendering for this window.
                if let (Some(dialect), Some(layout)) = (
                    self.table_docs.get(&doc_idx),
                    self.table_layout.get(&doc_idx),
                ) {
                    render_table(grid, doc, sel, viewport, rect, *dialect, layout, theme);
                    continue;
                }

                if diag_spans.is_empty() {
                    DocumentView::render_with_highlights(
                        grid,
                        doc,
                        sel,
                        viewport,
                        mode_ind,
                        rect.y,
                        rect.height,
                        hl_spans,
                        matches,
                        theme,
                    );
                } else {
                    DocumentView::render_with_diagnostics(
                        grid,
                        doc,
                        sel,
                        viewport,
                        mode_ind,
                        rect.y,
                        rect.height,
                        hl_spans,
                        matches,
                        diag_spans,
                        theme,
                    );
                }

                // Plugin decorations: highlights (cell overlay), gutter signs,
                // and end-of-line virtual text, per namespace.
                if let Some(ns_map) = self.plugin_decorations.get(&doc_idx) {
                    for batch in ns_map.values() {
                        draw_plugin_highlights(grid, rect, viewport, doc, &batch.highlights);
                        draw_plugin_signs(grid, rect, viewport, &batch.signs);
                        draw_plugin_virt_text(grid, rect, viewport, doc, &batch.virt_texts);
                    }
                }
            }

            // Statusline
            {
                let focused_doc_idx = self.windows[self.focused_window].doc_idx;
                let doc = &self.docs[focused_doc_idx];
                let sel = &self.windows[self.focused_window].selection;
                Statusline::render(grid, status_row, mode_ind, doc, sel, macro_recording, theme);
            }

            // Message line
            MessageLine::render(grid, msg_row, &msg, theme);

            // Agent panel (right strip).
            if let Some((lines, input, busy, title)) = &agent_panel {
                onda_render::render_agent_panel(
                    grid,
                    editor_width,
                    0,
                    panel_width,
                    content_height,
                    title,
                    lines,
                    input,
                    *busy,
                    theme,
                );
            }

            // Diff-review overlay (centered, over everything else).
            if !review_lines.is_empty() {
                draw_review_overlay(grid, width, content_height, &review_lines, theme);
            }

            // Command-line completion popup (above the command line).
            if self.mode == Mode::Command {
                if let Some(comp) = self.cmd_completion.as_ref() {
                    draw_cmd_completion(grid, comp, msg_row);
                }
            }

            // Picker overlay
            if let Some((title, query, items, pw, ph)) = picker_data {
                let items_ref: Vec<(&str, bool)> =
                    items.iter().map(|(s, b)| (s.as_str(), *b)).collect();
                render_picker(grid, &title, &query, &items_ref, pw, ph, theme);
            }

            // Hover float overlay
            if let Some(ref hover) = self.hover_float {
                let lines_ref: Vec<&str> = hover.lines.iter().map(|s| s.as_str()).collect();
                let float_width = hover.lines.iter().map(|l| l.len()).max().unwrap_or(0) as u16 + 4;
                render_float(
                    grid,
                    "Hover",
                    &lines_ref,
                    hover.col,
                    hover.row,
                    float_width.min(width - 4),
                    theme,
                );
            }

            // Completion menu
            if let Some(ref comp) = self.completion {
                let items_ref: Vec<(&str, &str)> = comp
                    .items
                    .iter()
                    .map(|(l, k)| (l.as_str(), k.as_str()))
                    .collect();
                render_completion_menu(
                    grid,
                    &items_ref,
                    comp.selected,
                    cursor_col,
                    cursor_row,
                    10,
                    theme,
                );
            }
        }

        self.compositor.cursor_col = cursor_col;
        self.compositor.cursor_row = cursor_row;

        #[cfg(feature = "debug-overlay")]
        self.compositor.render_debug_overlay();

        self.compositor.flush(&mut self.backend, mode_ind)?;

        #[cfg(feature = "bench")]
        self.tracer.mark_frame();

        Ok(())
    }

    fn cursor_screen_pos(&self, rects: &[(WindowId, Rect)]) -> (u16, u16) {
        let (width, height) = self.backend.size();

        // When the agent input is focused, the cursor sits in the panel input line.
        if self.agent_input_focused {
            let pw = self.agent_panel_width(width);
            let editor_width = width - pw;
            let col = (editor_width as usize + 3 + self.agent_input.chars().count())
                .min(width.saturating_sub(1) as usize) as u16;
            let row = height.saturating_sub(3); // content_height - 1
            return (col, row);
        }

        if self.mode == Mode::Command {
            let prefix_len = 1usize; // ':' or '/'
            let cmd_col = (self.command_line.as_str().len() + prefix_len) as u16;
            return (
                cmd_col.min(width.saturating_sub(1)),
                height.saturating_sub(1),
            );
        }

        let focused_win_id = WindowId(self.focused_window);
        let rect = rects
            .iter()
            .find(|(id, _)| *id == focused_win_id)
            .map(|(_, r)| *r)
            .unwrap_or(Rect::new(0, 0, width, height.saturating_sub(2)));

        let doc = self.doc();
        let win = self.focused_win();
        let head = win.selection.primary().head;
        let (line, col) = doc.char_to_visual_pos(head);

        let screen_row = rect.y + line.saturating_sub(win.viewport.offset_line) as u16;
        let screen_col = rect.x
            + (col.saturating_sub(win.viewport.offset_col) as u16)
                .saturating_add(win.viewport.line_nr_width);
        (
            screen_col.min(width.saturating_sub(1)),
            screen_row.min(height.saturating_sub(3)),
        )
    }
}

// ── Event handling ────────────────────────────────────────────────────────────

impl<B: Backend> App<B> {
    fn handle_event(&mut self, event: Event) -> Result<()> {
        match event {
            Event::Key(ev) if ev.kind == KeyEventKind::Press => {
                #[cfg(feature = "bench")]
                self.tracer.mark_key();

                let key = Key::from_event(&ev);

                // In terminal mode, forward keys to PTY
                if self.mode == Mode::Terminal {
                    self.handle_terminal_key(&key);
                    return Ok(());
                }

                self.handle_key(key)?;

                // Reparse syntax if the buffer changed this keypress.
                self.maybe_refresh_decorations();

                // Clear info messages on any keypress in normal mode
                if self.mode == Mode::Normal && matches!(self.message, Message::Info(_)) {
                    self.message = Message::None;
                }
            }
            Event::Mouse(mouse_ev) => {
                self.handle_mouse_event(mouse_ev)?;
            }
            Event::Resize(w, h) => {
                self.compositor.resize(w, h);
                // Resize terminal panes
                for pane in &mut self.terminal_panes {
                    let pane_rows = h.saturating_sub(3);
                    let pane_cols = w;
                    pane.process.resize(pane_cols, pane_rows);
                    pane.screen.resize(pane_rows, pane_cols);
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_terminal_key(&mut self, key: &Key) {
        // Ctrl-\ Ctrl-n: escape terminal mode
        if *key == Key::Char('n', KeyMod::CTRL) {
            self.mode = Mode::Normal;
            return;
        }
        // Forward key as raw bytes to the PTY for the focused pane
        let focused_win = self.focused_window;
        let pane_id = self.window_to_pane.get(&focused_win).copied();
        if let Some(pane_id) = pane_id {
            if let Some(pane) = self.terminal_panes.iter().find(|p| p.pane_id == pane_id) {
                let bytes = key_to_pty_bytes(key);
                pane.process.write(&bytes);
            }
        }
    }

    fn handle_mouse_event(&mut self, ev: MouseEvent) -> Result<()> {
        let (width, height) = self.backend.size();
        let content_height = height.saturating_sub(2);
        let content_area = Rect::new(0, 0, width, content_height);
        let rects = self.layout.rects(content_area);

        match ev.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                // Click to move cursor
                let click_col = ev.column;
                let click_row = ev.row;

                // If picker is open, handle click in picker
                if self
                    .picker
                    .as_ref()
                    .map(|p| p.is_visible())
                    .unwrap_or(false)
                {
                    // Just close on any click outside (TODO: proper hit testing)
                    return Ok(());
                }

                // Find which window was clicked
                for (win_id, rect) in &rects {
                    if click_col >= rect.x
                        && click_col < rect.x + rect.width
                        && click_row >= rect.y
                        && click_row < rect.y + rect.height
                    {
                        let win_idx = win_id.0;
                        if win_idx >= self.windows.len() {
                            continue;
                        }
                        self.focused_window = win_idx;

                        // Convert click to document position
                        let doc_idx = self.windows[win_idx].doc_idx;
                        if doc_idx >= self.docs.len() {
                            continue;
                        }
                        let viewport = &self.windows[win_idx].viewport;
                        let text_col =
                            click_col.saturating_sub(rect.x + viewport.line_nr_width) as usize;
                        let doc_line =
                            viewport.offset_line + click_row.saturating_sub(rect.y) as usize;

                        let doc = &self.docs[doc_idx];
                        if doc_line < doc.len_lines() {
                            let line_start = doc.line_to_char(doc_line);
                            let line_len = doc.line_len_no_eol(doc_line);
                            let col = (text_col + viewport.offset_col).min(line_len);
                            let char_pos =
                                (line_start + col).min(doc.len_chars().saturating_sub(1));
                            self.windows[win_idx].selection = Selection::point(char_pos);
                        }
                        break;
                    }
                }
            }
            MouseEventKind::ScrollDown => {
                let vp = self.viewport_mut();
                vp.offset_line = vp.offset_line.saturating_add(3);
            }
            MouseEventKind::ScrollUp => {
                let vp = self.viewport_mut();
                vp.offset_line = vp.offset_line.saturating_sub(3);
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_key(&mut self, key: Key) -> Result<()> {
        // Record key for macros (before routing to mode-specific handler)
        self.macros.record_key(&key);

        // Route to picker if visible
        if self
            .picker
            .as_ref()
            .map(|p| p.is_visible())
            .unwrap_or(false)
        {
            return self.handle_picker_key(key);
        }

        // Route to the diff-review overlay when active.
        if self.review.is_some() && self.handle_review_key(&key) {
            return Ok(());
        }

        // Route to the agent panel input box when focused.
        if self.agent_input_focused && self.handle_agent_input_key(&key) {
            return Ok(());
        }

        match self.mode {
            Mode::Insert => self.handle_insert_key(key),
            Mode::Command => self.handle_command_key(key),
            Mode::Normal | Mode::Visual | Mode::VisualLine | Mode::VisualBlock => {
                self.handle_normal_key(key)
            }
            Mode::Terminal | Mode::TerminalScroll => {
                // Already handled before handle_key is called
                Ok(())
            }
        }
    }

    fn handle_picker_key(&mut self, key: Key) -> Result<()> {
        match &key {
            Key::Esc => {
                if let Some(ref mut picker) = self.picker {
                    picker.close();
                }
            }
            Key::Enter => {
                let selected_value = self
                    .picker
                    .as_ref()
                    .and_then(|p| p.selected_item())
                    .map(|item| item.value.clone());

                if let Some(value) = selected_value {
                    let kind = self.picker_kind;
                    if let Some(ref mut picker) = self.picker {
                        picker.close();
                    }
                    match kind {
                        PickerKind::File => {
                            match Document::open(&value) {
                                Ok(doc) => {
                                    let doc_idx = self.docs.len();
                                    self.docs.push(doc);
                                    // Reuse current window for the opened file
                                    self.focused_win_mut().doc_idx = doc_idx;
                                    *self.selection_mut() = Selection::point(0);
                                    self.message = Message::Info(format!("Opened: {value}"));
                                    self.try_spawn_syntax_worker_for_doc(doc_idx);
                                }
                                Err(e) => {
                                    self.message = Message::Error(format!("E: {e}"));
                                }
                            }
                        }
                        PickerKind::Buffer => {
                            // Find the doc with this name and switch to it
                            if let Some(idx) = self.docs.iter().position(|d| d.name() == value) {
                                self.focused_win_mut().doc_idx = idx;
                                *self.selection_mut() = Selection::point(0);
                            }
                        }
                    }
                }
            }
            Key::Down => {
                if let Some(ref mut picker) = self.picker {
                    picker.move_down();
                }
            }
            Key::Char('n', m) if m.contains(KeyMod::CTRL) => {
                if let Some(ref mut picker) = self.picker {
                    picker.move_down();
                }
            }
            Key::Char('j', _) => {
                if let Some(ref mut picker) = self.picker {
                    picker.move_down();
                }
            }
            Key::Up => {
                if let Some(ref mut picker) = self.picker {
                    picker.move_up();
                }
            }
            Key::Char('p', m) if m.contains(KeyMod::CTRL) => {
                if let Some(ref mut picker) = self.picker {
                    picker.move_up();
                }
            }
            Key::Char('k', _) => {
                if let Some(ref mut picker) = self.picker {
                    picker.move_up();
                }
            }
            Key::Backspace => {
                if let Some(ref mut picker) = self.picker {
                    picker.pop_char();
                }
            }
            Key::Char(c, _) => {
                let ch = *c;
                if let Some(ref mut picker) = self.picker {
                    picker.push_char(ch);
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_insert_key(&mut self, key: Key) -> Result<()> {
        match &key {
            Key::Esc => {
                // Move cursor left by one when leaving insert mode (vim behaviour)
                let doc = self.doc();
                let pos = self.selection().primary().head;
                let line = doc.char_to_line(pos);
                let line_start = doc.line_to_char(line);
                if pos > line_start {
                    *self.selection_mut() = Selection::point(pos - 1);
                }
                self.undo().end_group();
                self.mode = Mode::Normal;
                self.keymap_state.reset();
                self.macros.end_change();
            }
            Key::Enter => {
                self.apply_insert(|doc, sel| onda_modal::operator::insert_char(doc, sel, '\n'))?;
                // Move cursor to new line
                let new_pos = self.selection().primary().head + 1;
                let len = self.doc().len_chars();
                *self.selection_mut() = Selection::point(new_pos.min(len));
            }
            Key::Backspace => {
                let tx = onda_modal::operator::delete_before_cursor(self.doc(), self.selection());
                if !tx.changes.is_empty() {
                    let sel_before = self.selection().clone();
                    let inv = self.doc_mut().apply(&tx)?;
                    let new_pos = self.selection().primary().head.saturating_sub(1);
                    *self.selection_mut() = Selection::point(new_pos);
                    let sel_after = self.selection().clone();
                    self.undo().push(tx, inv, sel_before, sel_after);
                    self.undo().begin_group();
                }
                self.update_search_matches();
            }
            Key::Delete => {
                let tx = onda_modal::operator::delete_char_at_cursor(self.doc(), self.selection());
                if !tx.changes.is_empty() {
                    let sel_before = self.selection().clone();
                    let inv = self.doc_mut().apply(&tx)?;
                    let sel_after = self.selection().clone();
                    self.undo().push(tx, inv, sel_before, sel_after);
                    self.undo().begin_group();
                }
                self.update_search_matches();
            }
            Key::Char(c, _) => {
                let ch = *c;
                let sel_before = self.selection().clone();
                let tx = onda_modal::operator::insert_char(self.doc(), self.selection(), ch);
                let inv = self.doc_mut().apply(&tx)?;
                let new_pos = self.selection().primary().head + 1;
                let len = self.doc().len_chars();
                *self.selection_mut() = Selection::point(new_pos.min(len));
                let sel_after = self.selection().clone();
                self.undo().push(tx, inv, sel_before, sel_after);
                self.undo().begin_group();
                self.update_search_matches();
            }
            _ => {}
        }
        Ok(())
    }

    fn apply_insert<F>(&mut self, build: F) -> Result<()>
    where
        F: FnOnce(&Document, &Selection) -> Transaction,
    {
        let tx = build(self.doc(), self.selection());
        if !tx.changes.is_empty() {
            let sel_before = self.selection().clone();
            let inv = self.doc_mut().apply(&tx)?;
            let new_sel = self.selection().map(&tx.changes);
            *self.selection_mut() = new_sel.clone();
            self.undo().push(tx, inv, sel_before, new_sel);
        }
        self.update_search_matches();
        Ok(())
    }

    fn handle_command_key(&mut self, key: Key) -> Result<()> {
        // Completion: <Tab>/<S-Tab> cycle candidates; never leaves command mode.
        // File/command completion only applies to `:` ex-commands, not `/` search.
        match &key {
            Key::Tab if self.search_input_dir.is_none() => {
                self.cmd_complete_advance(1);
                return Ok(());
            }
            Key::BackTab if self.search_input_dir.is_none() => {
                self.cmd_complete_advance(-1);
                return Ok(());
            }
            _ => {}
        }

        match &key {
            Key::Esc => {
                // A first <Esc> while a completion popup is open just dismisses it.
                if self.cmd_completion.take().is_some() {
                    return Ok(());
                }
                self.command_line.clear();
                self.search_input_dir = None;
                self.mode = Mode::Normal;
                self.message = Message::None;
            }
            Key::Enter => {
                // <CR> accepts an open completion (keeps the line) without submitting.
                if self.cmd_completion.take().is_some() {
                    return Ok(());
                }
                if let Some(dir) = self.search_input_dir.take() {
                    // Search mode: compile pattern and jump
                    let pattern = self.command_line.as_str().to_string();
                    self.command_line.clear();
                    self.mode = Mode::Normal;
                    let smartcase = false; // TODO: read from config
                    self.search.set_pattern(pattern, smartcase);
                    self.search.direction = match dir {
                        SearchInputDir::Forward => onda_modal::SearchDir::Forward,
                        SearchInputDir::Backward => onda_modal::SearchDir::Backward,
                    };
                    self.update_search_matches();
                    // Jump to first match from current position
                    if let Some(regex) = self.search.regex.as_ref() {
                        let rope = self.doc().rope().clone();
                        let from = self.selection().primary().head;
                        let found = match dir {
                            SearchInputDir::Forward => find_next(&rope, regex, from),
                            SearchInputDir::Backward => find_prev(&rope, regex, from),
                        };
                        if let Some(m) = found {
                            *self.selection_mut() = Selection::point(m.from());
                        } else {
                            self.message = Message::Info("Pattern not found".to_string());
                        }
                    }
                } else {
                    match self.command_line.submit() {
                        Ok(cmd) => self.execute_ex_command(cmd)?,
                        // An unknown `:name …` may be a plugin-registered command.
                        Err(onda_modal::CommandError::Unknown(line))
                            if self.try_plugin_command(&line) =>
                        {
                            self.mode = Mode::Normal;
                        }
                        Err(e) => {
                            self.message = Message::Error(format!("E: {e}"));
                            self.mode = Mode::Normal;
                        }
                    }
                }
            }
            Key::Backspace => {
                self.cmd_completion = None;
                if self.command_line.as_str().is_empty() {
                    self.search_input_dir = None;
                    self.mode = Mode::Normal;
                    self.message = Message::None;
                } else {
                    self.command_line.backspace();
                }
            }
            Key::Char(c, _) => {
                self.cmd_completion = None;
                self.command_line.push_char(*c);
            }
            _ => {}
        }
        Ok(())
    }

    /// Advance command-line completion by `dir` (+1 forward, -1 backward), computing
    /// candidates on first use, then writing the selected candidate into the line.
    fn cmd_complete_advance(&mut self, dir: i32) {
        if self.cmd_completion.is_none() {
            let extra: Vec<String> = self
                .plugin_host
                .as_ref()
                .map(|h| h.command_names())
                .unwrap_or_default();
            let line = self.command_line.as_str().to_string();
            let (base, candidates) = match onda_modal::analyze(&line, &extra) {
                onda_modal::Completion::Commands {
                    base, candidates, ..
                } => (base, candidates),
                onda_modal::Completion::Paths {
                    base, candidates, ..
                } => (base, candidates),
                onda_modal::Completion::None => return,
            };
            self.cmd_completion = Some(CmdCompletion {
                base,
                candidates,
                selected: 0,
            });
        } else if let Some(c) = self.cmd_completion.as_mut() {
            let n = c.candidates.len();
            if n > 0 {
                c.selected = ((c.selected as i32 + dir).rem_euclid(n as i32)) as usize;
            }
        }
        // Write the selected candidate into the command line.
        if let Some(c) = self.cmd_completion.as_ref() {
            if let Some(cand) = c.candidates.get(c.selected) {
                self.command_line.buffer = format!("{}{}", c.base, cand);
            }
        }
    }

    fn execute_ex_command(&mut self, cmd: ExCommand) -> Result<()> {
        self.mode = Mode::Normal;
        match cmd {
            // ── Phase 2 commands ──────────────────────────────────────────────
            ExCommand::Terminal => {
                self.open_terminal_pane()?;
            }
            ExCommand::Format => {
                if self.lsp_manager.is_some() {
                    self.message = Message::Info("LSP: format requested (async)".into());
                } else {
                    self.message = Message::Info("LSP Format: not yet connected".into());
                }
            }
            ExCommand::LspNext => {
                let doc_idx = self.focused_win().doc_idx;
                let cur = self.selection().primary().head;
                if let Some(spans) = self.diagnostic_spans.get(&doc_idx) {
                    if let Some(span) = spans.iter().find(|s| s.from > cur) {
                        let pos = span.from;
                        *self.selection_mut() = Selection::point(pos);
                        let line = self.doc().char_to_line(pos);
                        let h = self.compositor.buf.current().height().saturating_sub(2) as usize;
                        self.viewport_mut().scroll_to(line, h);
                    } else {
                        self.message = Message::Info("No more diagnostics".into());
                    }
                }
            }
            ExCommand::LspPrev => {
                let doc_idx = self.focused_win().doc_idx;
                let cur = self.selection().primary().head;
                if let Some(spans) = self.diagnostic_spans.get(&doc_idx) {
                    if let Some(span) = spans.iter().rfind(|s| s.from < cur) {
                        let pos = span.from;
                        *self.selection_mut() = Selection::point(pos);
                        let line = self.doc().char_to_line(pos);
                        let h = self.compositor.buf.current().height().saturating_sub(2) as usize;
                        self.viewport_mut().scroll_to(line, h);
                    } else {
                        self.message = Message::Info("No previous diagnostics".into());
                    }
                }
            }
            ExCommand::Messages => {
                let history = self.message_history.join("\n");
                if history.is_empty() {
                    self.message = Message::Info("No messages.".into());
                } else {
                    self.message = Message::Info(history);
                }
            }
            ExCommand::GrammarFetch => {
                self.message = Message::Info(
                    "Grammar fetch: use :GrammarFetch (auto-triggered on file open)".into(),
                );
            }
            ExCommand::ListBuffers => {
                let list: Vec<String> = self
                    .docs
                    .iter()
                    .enumerate()
                    .map(|(i, d)| {
                        format!(
                            "{}: {}{}",
                            i + 1,
                            d.name(),
                            if d.is_modified() { " [+]" } else { "" }
                        )
                    })
                    .collect();
                self.message = Message::Info(list.join(" | "));
            }
            ExCommand::Theme(name) => match name {
                Some(n) => {
                    self.apply_theme(&n);
                    self.message = Message::Info(format!("theme: {}", self.theme.name()));
                }
                None => {
                    let avail = onda_render::BUILTIN_THEMES.join(", ");
                    self.message =
                        Message::Info(format!("theme: {} (available: {avail})", self.theme.name()));
                }
            },
            ExCommand::Table => self.toggle_table_view(),
            ExCommand::Fields => self.show_jsonl_fields(),
            ExCommand::Agent(name) => self.agent_command(name),
            ExCommand::AgentExport => self.agent_export(),
            ExCommand::AgentReview => self.agent_review_start(),
            ExCommand::SessionSave(name) => {
                let session = self.build_session();
                let name = name.as_deref().unwrap_or("default");
                match self.session_manager.save(name, &session) {
                    Ok(_) => self.message = Message::Info(format!("Session saved: {name}")),
                    Err(e) => self.message = Message::Error(format!("Session save error: {e}")),
                }
            }
            ExCommand::SessionRestore(name) => {
                let name_owned = name.unwrap_or_else(|| "default".into());
                match self.session_manager.load(&name_owned) {
                    Ok(session) => {
                        self.apply_session(session)?;
                        self.message = Message::Info(format!("Session restored: {name_owned}"));
                    }
                    Err(e) => {
                        self.message = Message::Error(format!("Session restore error: {e}"));
                    }
                }
            }
            ExCommand::WriteQuitAll => {
                // Write all modified docs, then auto-save session and quit
                for doc in &mut self.docs {
                    if doc.is_modified() {
                        if let Err(e) = doc.save() {
                            self.message = Message::Error(format!("E: {e}"));
                            return Ok(());
                        }
                        doc.mark_saved();
                    }
                }
                let session = self.build_session();
                let _ = self.session_manager.auto_save(&session);
                self.running = false;
            }
            ExCommand::Write(path) => {
                let result = if let Some(p) = path {
                    self.doc_mut().set_path(p.into());
                    self.doc().save()
                } else {
                    self.doc().save()
                };
                match result {
                    Ok(()) => {
                        self.doc_mut().mark_saved();
                        self.message = Message::Info(format!("\"{}\" written", self.doc().name()));
                        let doc_idx = self.focused_win().doc_idx;
                        self.persist_undo_on_save(doc_idx);
                    }
                    Err(e) => {
                        self.message = Message::Error(format!("E: {e}"));
                    }
                }
            }
            ExCommand::Quit { force } => {
                if !force && self.doc().is_modified() {
                    self.message = Message::Error(
                        "E37: No write since last change (add ! to override)".to_string(),
                    );
                    return Ok(());
                }
                self.running = false;
            }
            ExCommand::WriteQuit => match self.doc().save() {
                Ok(()) => {
                    self.doc_mut().mark_saved();
                    let doc_idx = self.focused_win().doc_idx;
                    self.persist_undo_on_save(doc_idx);
                    self.running = false;
                }
                Err(e) => {
                    self.message = Message::Error(format!("E: {e}"));
                }
            },
            ExCommand::Edit(path) => match Document::open(&path) {
                Ok(doc) => {
                    let doc_idx = self.docs.len();
                    self.docs.push(doc);
                    self.focused_win_mut().doc_idx = doc_idx;
                    *self.selection_mut() = Selection::point(0);
                    *self.viewport_mut() = Viewport::new();
                    self.try_spawn_syntax_worker_for_doc(doc_idx);
                }
                Err(e) => {
                    self.message = Message::Error(format!("E: {e}"));
                }
            },
            ExCommand::NextBuffer => {
                if self.docs.len() > 1 {
                    let cur = self.focused_win().doc_idx;
                    self.focused_win_mut().doc_idx = (cur + 1) % self.docs.len();
                    *self.selection_mut() = Selection::point(0);
                }
            }
            ExCommand::PrevBuffer => {
                if self.docs.len() > 1 {
                    let cur = self.focused_win().doc_idx;
                    let n = self.docs.len();
                    self.focused_win_mut().doc_idx = (cur + n - 1) % n;
                    *self.selection_mut() = Selection::point(0);
                }
            }
            ExCommand::Split(path) => {
                // Horizontal split — same as SplitHorizontal action
                let new_id = self.next_window_id;
                self.next_window_id += 1;
                let doc_idx = if let Some(p) = path {
                    match Document::open(&p) {
                        Ok(doc) => {
                            let idx = self.docs.len();
                            self.docs.push(doc);
                            idx
                        }
                        Err(e) => {
                            self.message = Message::Error(format!("E: {e}"));
                            self.focused_win().doc_idx
                        }
                    }
                } else {
                    self.focused_win().doc_idx
                };
                let new_win = WindowState::new(doc_idx);
                self.windows.push(new_win);
                let old_layout = std::mem::replace(&mut self.layout, Layout::single(WindowId(0)));
                self.layout = old_layout.split_h(WindowId(new_id));
            }
            ExCommand::VSplit(path) => {
                let new_id = self.next_window_id;
                self.next_window_id += 1;
                let doc_idx = if let Some(p) = path {
                    match Document::open(&p) {
                        Ok(doc) => {
                            let idx = self.docs.len();
                            self.docs.push(doc);
                            idx
                        }
                        Err(e) => {
                            self.message = Message::Error(format!("E: {e}"));
                            self.focused_win().doc_idx
                        }
                    }
                } else {
                    self.focused_win().doc_idx
                };
                let new_win = WindowState::new(doc_idx);
                self.windows.push(new_win);
                let old_layout = std::mem::replace(&mut self.layout, Layout::single(WindowId(0)));
                self.layout = old_layout.split_v(WindowId(new_id));
            }
            ExCommand::NoHighlight => {
                self.search_matches.clear();
                self.search.regex = None;
            }
            ExCommand::Set(key, value) => {
                // Apply config settings at runtime (T5.1 partial: scrolloff)
                if key == "scrolloff" {
                    if let Some(v) = value.as_deref().and_then(|s| s.parse::<usize>().ok()) {
                        self.viewport_mut().scrolloff = v;
                    }
                } else if self.config.editor.scrolloff != 5 {
                    // Apply scrolloff from loaded config to current viewport
                    let so = self.config.editor.scrolloff;
                    self.viewport_mut().scrolloff = so;
                }
            }
            ExCommand::Substitute {
                range_all,
                pattern,
                replacement,
                flags_global,
                flags_case_insensitive: _,
            } => {
                // TODO T5.2: full substitution support
                let smartcase = false;
                let mut search = SearchState::new();
                search.set_pattern(pattern, smartcase);
                if let Some(regex) = search.regex.as_ref() {
                    let rope = self.doc().rope().clone();
                    let line_start = if range_all {
                        0
                    } else {
                        let cur = self.selection().primary().head;
                        self.doc().char_to_line(cur)
                    };
                    let line_end = if range_all {
                        self.doc().len_chars()
                    } else {
                        let cur = self.selection().primary().head;
                        let line = self.doc().char_to_line(cur);
                        self.doc().line_to_char(line) + self.doc().line_len_no_eol(line)
                    };
                    let result = onda_modal::substitute(
                        &rope,
                        regex,
                        &replacement,
                        flags_global,
                        line_start,
                        line_end,
                    );
                    if let Some((new_text, _count)) = result {
                        let len = self.doc().len_chars();
                        let cs = onda_core::transaction::ChangeSetBuilder::new(len)
                            .delete(len)
                            .insert(&new_text)
                            .build();
                        let tx = Transaction::new(cs);
                        let sel_before = self.selection().clone();
                        if let Ok(inv) = self.doc_mut().apply(&tx) {
                            let sel_after = self.selection().clone();
                            self.undo().push(tx, inv, sel_before, sel_after);
                        }
                        self.update_search_matches();
                    }
                }
            }
        }
        Ok(())
    }

    fn handle_normal_key(&mut self, key: Key) -> Result<()> {
        let viewport_height = {
            let (_, h) = self.backend.size();
            h.saturating_sub(2) as usize
        };

        let result = self.keymap_state.process(&key, self.mode, &self.keymap);
        match result {
            PendingResult::Action(action, count) => {
                self.execute_action(action, count, viewport_height)?;
            }
            PendingResult::NeedMore => {}
            PendingResult::NoMatch => {}
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn execute_action(
        &mut self,
        action: Action,
        count: usize,
        viewport_height: usize,
    ) -> Result<()> {
        match action {
            // ── Mode transitions ──────────────────────────────────────────────
            Action::EnterInsert => {
                self.mode = Mode::Insert;
                self.undo().begin_group();
                self.macros.begin_change();
            }
            Action::EnterInsertLineStart => {
                let doc = self.doc();
                let line = doc.char_to_line(self.selection().primary().head);
                let line_start = doc.line_to_char(line);
                *self.selection_mut() = Selection::point(line_start);
                self.mode = Mode::Insert;
                self.undo().begin_group();
                self.macros.begin_change();
            }
            Action::EnterInsertAfter => {
                let pos = self.selection().primary().head;
                let doc = self.doc();
                let len = doc.len_chars();
                let line = doc.char_to_line(pos);
                let line_end = {
                    let line_len = doc.line_len_no_eol(line);
                    doc.line_to_char(line) + line_len
                };
                *self.selection_mut() = Selection::point((pos + 1).min(line_end).min(len));
                self.mode = Mode::Insert;
                self.undo().begin_group();
                self.macros.begin_change();
            }
            Action::EnterInsertLineEnd => {
                let doc = self.doc();
                let line = doc.char_to_line(self.selection().primary().head);
                let line_end = doc.line_to_char(line) + doc.line_len_no_eol(line);
                *self.selection_mut() = Selection::point(line_end.min(doc.len_chars()));
                self.mode = Mode::Insert;
                self.undo().begin_group();
                self.macros.begin_change();
            }
            Action::EnterInsertNewLineBelow => {
                let (tx, new_sel) =
                    onda_modal::operator::open_line(self.doc(), self.selection(), false);
                let sel_before = self.selection().clone();
                let inv = self.doc_mut().apply(&tx)?;
                *self.selection_mut() = new_sel.clone();
                self.undo().push(tx, inv, sel_before, new_sel);
                self.mode = Mode::Insert;
                self.undo().begin_group();
                self.macros.begin_change();
            }
            Action::EnterInsertNewLineAbove => {
                let (tx, new_sel) =
                    onda_modal::operator::open_line(self.doc(), self.selection(), true);
                let sel_before = self.selection().clone();
                let inv = self.doc_mut().apply(&tx)?;
                *self.selection_mut() = new_sel.clone();
                self.undo().push(tx, inv, sel_before, new_sel);
                self.mode = Mode::Insert;
                self.undo().begin_group();
                self.macros.begin_change();
            }
            Action::EnterNormal => {
                if self.mode == Mode::Insert {
                    self.undo().end_group();
                    self.macros.end_change();
                }
                self.mode = Mode::Normal;
                let collapsed = self.selection().collapse_to_head();
                *self.selection_mut() = collapsed;
            }
            Action::EnterVisual => {
                self.mode = Mode::Visual;
            }
            Action::EnterVisualLine => {
                self.mode = Mode::VisualLine;
            }
            Action::EnterVisualBlock => {
                self.mode = Mode::VisualBlock;
            }
            Action::EnterCommand => {
                self.mode = Mode::Command;
                self.command_line.clear();
                self.search_input_dir = None;
            }

            // ── Motion ───────────────────────────────────────────────────────
            Action::Move(motion) => {
                let rope = self.doc().rope().clone();
                let (new_sel, new_goal) = motion.apply_to_selection(
                    &rope,
                    self.selection(),
                    count,
                    self.goal_col,
                    viewport_height,
                );
                if self.mode.is_visual() {
                    let primary = self.selection().primary();
                    let new_head = new_sel.primary().head;
                    *self.selection_mut() =
                        Selection::new(vec![onda_core::Range::new(primary.anchor, new_head)], 0);
                } else {
                    *self.selection_mut() = new_sel;
                }
                self.goal_col = new_goal;
            }

            // ── Operator + motion ─────────────────────────────────────────────
            Action::ApplyOperatorMotion(op, motion) => {
                let rope = self.doc().rope().clone();
                let (motion_sel, _) = motion.apply_to_selection(
                    &rope,
                    self.selection(),
                    count,
                    self.goal_col,
                    viewport_height,
                );
                let primary = self.selection().primary();
                let motion_head = motion_sel.primary().head;
                let op_range = onda_core::Range::new(
                    primary.head.min(motion_head),
                    primary.head.max(motion_head),
                );
                let op_sel = Selection::new(vec![op_range], 0);
                self.apply_operator(op, &op_sel, false)?;
            }

            // ── Operator + text object ─────────────────────────────────────────
            Action::ApplyOperatorTextObj(op, textobj) => {
                let pos = self.selection().primary().head;
                let rope = self.doc().rope().clone();
                let range = self.resolve_textobj(&rope, pos, textobj);
                if let Some(r) = range {
                    let op_sel = Selection::new(vec![r], 0);
                    self.apply_operator(op, &op_sel, false)?;
                } else if Self::is_ts_textobj(textobj) {
                    self.message = Message::Info("no tree-sitter text object here".to_string());
                }
            }

            // ── Visual-mode text object (viw, vaf, via) ─────────────────────────
            Action::SelectTextObj(textobj) => {
                let pos = self.selection().primary().head;
                let rope = self.doc().rope().clone();
                match self.resolve_textobj(&rope, pos, textobj) {
                    Some(r) => {
                        // Extend the selection to cover the text object.
                        *self.selection_mut() = Selection::new(vec![r], 0);
                    }
                    None if Self::is_ts_textobj(textobj) => {
                        self.message = Message::Info("no tree-sitter text object here".to_string());
                    }
                    None => {}
                }
            }

            // ── Line operator ─────────────────────────────────────────────────
            Action::OperatorLine(op) => {
                let sel = self.selection().clone();
                self.apply_operator(op, &sel, true)?;
            }

            // ── Selection operator ────────────────────────────────────────────
            Action::OperatorSelection(op) => {
                let sel = self.selection().clone();
                self.apply_operator(op, &sel, false)?;
                self.mode = Mode::Normal;
            }

            // ── Immediate edits ───────────────────────────────────────────────
            Action::DeleteChar => {
                let tx = onda_modal::operator::delete_char_at_cursor(self.doc(), self.selection());
                if !tx.changes.is_empty() {
                    let sel_before = self.selection().clone();
                    let inv = self.doc_mut().apply(&tx)?;
                    let new_sel = self.selection().map(&tx.changes);
                    let len = self.doc().len_chars();
                    let head = new_sel.primary().head.min(len.saturating_sub(1));
                    *self.selection_mut() = Selection::point(head);
                    let sel_after = self.selection().clone();
                    self.undo().push(tx, inv, sel_before, sel_after);
                }
                self.update_search_matches();
            }
            Action::ReplaceChar(c) => {
                let tx = onda_modal::operator::replace_char(self.doc(), self.selection(), c);
                if !tx.changes.is_empty() {
                    let sel_before = self.selection().clone();
                    let inv = self.doc_mut().apply(&tx)?;
                    let sel_after = self.selection().clone();
                    self.undo().push(tx, inv, sel_before, sel_after);
                }
                self.update_search_matches();
            }
            Action::ChangeToEnd => {
                let rope = self.doc().rope().clone();
                let (end_sel, _) = Motion::LineEnd.apply_to_selection(
                    &rope,
                    self.selection(),
                    1,
                    None,
                    viewport_height,
                );
                let primary = self.selection().primary();
                let end = end_sel.primary().head;
                let range = onda_core::Range::new(primary.head, end);
                let del_sel = Selection::new(vec![range], 0);
                self.apply_operator(Operator::Change, &del_sel, false)?;
            }
            Action::DeleteToEnd => {
                let rope = self.doc().rope().clone();
                let (end_sel, _) = Motion::LineEnd.apply_to_selection(
                    &rope,
                    self.selection(),
                    1,
                    None,
                    viewport_height,
                );
                let primary = self.selection().primary();
                let end = end_sel.primary().head;
                let range = onda_core::Range::new(primary.head, end);
                let del_sel = Selection::new(vec![range], 0);
                self.apply_operator(Operator::Delete, &del_sel, false)?;
            }
            Action::PasteAfter => {
                let reg = self.active_register();
                if let Some(reg) = reg {
                    let tx = onda_modal::operator::paste_after(self.doc(), self.selection(), &reg);
                    if !tx.changes.is_empty() {
                        let sel_before = self.selection().clone();
                        let inv = self.doc_mut().apply(&tx)?;
                        let new_sel = self.selection().map(&tx.changes);
                        *self.selection_mut() = new_sel.clone();
                        self.undo().push(tx, inv, sel_before, new_sel);
                    }
                    self.update_search_matches();
                }
            }
            Action::PasteBefore => {
                let reg = self.active_register();
                if let Some(reg) = reg {
                    let tx = onda_modal::operator::paste_before(self.doc(), self.selection(), &reg);
                    if !tx.changes.is_empty() {
                        let sel_before = self.selection().clone();
                        let inv = self.doc_mut().apply(&tx)?;
                        let new_sel = self.selection().map(&tx.changes);
                        *self.selection_mut() = new_sel.clone();
                        self.undo().push(tx, inv, sel_before, new_sel);
                    }
                    self.update_search_matches();
                }
            }
            Action::JoinLine => {
                let tx = onda_modal::operator::join_line(self.doc(), self.selection());
                if !tx.changes.is_empty() {
                    let sel_before = self.selection().clone();
                    let inv = self.doc_mut().apply(&tx)?;
                    let sel_after = self.selection().clone();
                    self.undo().push(tx, inv, sel_before, sel_after);
                }
                self.update_search_matches();
            }

            // ── Undo/Redo ─────────────────────────────────────────────────────
            Action::Undo => {
                let doc_idx = self.focused_win().doc_idx;
                self.maybe_load_persistent_undo(doc_idx);
                for _ in 0..count {
                    let doc = &mut self.docs[doc_idx];
                    match self.windows[self.focused_window].undo.undo(doc) {
                        Ok(sel) => {
                            self.windows[self.focused_window].selection = sel;
                        }
                        Err(_) => {
                            self.message = Message::Info("Already at oldest change".to_string());
                            break;
                        }
                    }
                }
                self.update_search_matches();
            }
            Action::Redo => {
                let doc_idx = self.focused_win().doc_idx;
                for _ in 0..count {
                    let doc = &mut self.docs[doc_idx];
                    match self.windows[self.focused_window].undo.redo(doc) {
                        Ok(sel) => {
                            self.windows[self.focused_window].selection = sel;
                        }
                        Err(_) => {
                            self.message = Message::Info("Already at newest change".to_string());
                            break;
                        }
                    }
                }
                self.update_search_matches();
            }
            Action::UndoOlder => {
                let doc_idx = self.focused_win().doc_idx;
                let doc = &mut self.docs[doc_idx];
                match self.windows[self.focused_window].undo.undo_older(doc) {
                    Ok(sel) => {
                        self.windows[self.focused_window].selection = sel;
                    }
                    Err(_) => {
                        self.message = Message::Info("Already at oldest change".to_string());
                    }
                }
                self.update_search_matches();
            }
            Action::UndoNewer => {
                let doc_idx = self.focused_win().doc_idx;
                let doc = &mut self.docs[doc_idx];
                match self.windows[self.focused_window].undo.undo_newer(doc) {
                    Ok(sel) => {
                        self.windows[self.focused_window].selection = sel;
                    }
                    Err(_) => {
                        self.message = Message::Info("Already at newest change".to_string());
                    }
                }
                self.update_search_matches();
            }

            // ── Visual mode ───────────────────────────────────────────────────
            Action::SwapAnchorHead => {
                let new_sel = self.selection().transform(|r| r.flip());
                *self.selection_mut() = new_sel;
            }

            // ── Marks ─────────────────────────────────────────────────────────
            Action::SetMark(c) => {
                let doc_id = self.current_doc_id();
                let pos = self.selection().primary().head;
                self.marks.set(doc_id, c, pos);
            }
            Action::JumpToMark(c) => {
                let doc_id = self.current_doc_id();
                if let Some(pos) = self.marks.get(doc_id, c) {
                    let len = self.doc().len_chars();
                    *self.selection_mut() = Selection::point(pos.min(len.saturating_sub(1)));
                }
            }
            Action::JumpToMarkLine(c) => {
                let doc_id = self.current_doc_id();
                if let Some(pos) = self.marks.get(doc_id, c) {
                    let doc = self.doc();
                    let line = doc.char_to_line(pos);
                    let line_start = doc.line_to_char(line);
                    *self.selection_mut() = Selection::point(line_start);
                }
            }

            // ── Jump list ─────────────────────────────────────────────────────
            Action::JumpOlder => {
                let doc_id = self.current_doc_id();
                let pos = self.selection().primary().head;
                let current_jump = onda_modal::jumplist::JumpPos::new(doc_id, pos);
                if let Some(jump) = self.jumps.older(current_jump) {
                    // TODO T6.5: support cross-document jumps; for now only same-doc
                    if jump.doc_id == doc_id {
                        let len = self.doc().len_chars();
                        *self.selection_mut() =
                            Selection::point(jump.char_pos.min(len.saturating_sub(1)));
                    }
                }
            }
            Action::JumpNewer => {
                if let Some(jump) = self.jumps.newer() {
                    let doc_id = self.current_doc_id();
                    if jump.doc_id == doc_id {
                        let len = self.doc().len_chars();
                        *self.selection_mut() =
                            Selection::point(jump.char_pos.min(len.saturating_sub(1)));
                    }
                }
            }

            // ── Search ────────────────────────────────────────────────────────
            Action::SearchForward => {
                self.mode = Mode::Command;
                self.command_line.clear();
                self.search_input_dir = Some(SearchInputDir::Forward);
            }
            Action::SearchBackward => {
                self.mode = Mode::Command;
                self.command_line.clear();
                self.search_input_dir = Some(SearchInputDir::Backward);
            }
            Action::SearchNext => {
                if let Some(regex) = self.search.regex.clone() {
                    let rope = self.doc().rope().clone();
                    let from = self.selection().primary().head;
                    let found = match self.search.direction {
                        onda_modal::SearchDir::Forward => find_next(&rope, &regex, from + 1),
                        onda_modal::SearchDir::Backward => find_prev(&rope, &regex, from),
                    };
                    if let Some(m) = found {
                        *self.selection_mut() = Selection::point(m.from());
                    } else {
                        self.message = Message::Info("Pattern not found".to_string());
                    }
                }
            }
            Action::SearchPrev => {
                if let Some(regex) = self.search.regex.clone() {
                    let rope = self.doc().rope().clone();
                    let from = self.selection().primary().head;
                    let found = match self.search.direction {
                        onda_modal::SearchDir::Forward => find_prev(&rope, &regex, from),
                        onda_modal::SearchDir::Backward => find_next(&rope, &regex, from + 1),
                    };
                    if let Some(m) = found {
                        *self.selection_mut() = Selection::point(m.from());
                    } else {
                        self.message = Message::Info("Pattern not found".to_string());
                    }
                }
            }
            Action::SearchWordUnder => {
                let pos = self.selection().primary().head;
                let rope = self.doc().rope().clone();
                if let Some(range) = onda_modal::textobj::inner_word(&rope, pos) {
                    let word: String = rope.slice(range.from()..range.to()).chars().collect();
                    if !word.is_empty() {
                        self.search.set_pattern(format!("\\b{}\\b", word), false);
                        self.search.direction = onda_modal::SearchDir::Forward;
                        self.update_search_matches();
                        // Jump to next occurrence
                        if let Some(regex) = self.search.regex.clone() {
                            if let Some(m) = find_next(&rope, &regex, pos + 1) {
                                *self.selection_mut() = Selection::point(m.from());
                            }
                        }
                    }
                }
            }
            Action::SearchWordUnderBack => {
                let pos = self.selection().primary().head;
                let rope = self.doc().rope().clone();
                if let Some(range) = onda_modal::textobj::inner_word(&rope, pos) {
                    let word: String = rope.slice(range.from()..range.to()).chars().collect();
                    if !word.is_empty() {
                        self.search.set_pattern(format!("\\b{}\\b", word), false);
                        self.search.direction = onda_modal::SearchDir::Backward;
                        self.update_search_matches();
                        if let Some(regex) = self.search.regex.clone() {
                            if let Some(m) = find_prev(&rope, &regex, pos) {
                                *self.selection_mut() = Selection::point(m.from());
                            }
                        }
                    }
                }
            }
            Action::ClearSearch => {
                self.search_matches.clear();
                self.search.regex = None;
                self.search.pattern.clear();
            }

            // ── Macros / dot-repeat ───────────────────────────────────────────
            Action::StartRecordMacro(c) => {
                self.macros.start_recording(c);
                let reg = self.macros.is_recording();
                if let Some(r) = reg {
                    self.message = Message::Info(format!("recording @{r}"));
                }
            }
            Action::StopRecordMacro => {
                if let Some(reg) = self.macros.stop_recording() {
                    self.message = Message::Info(format!("Recorded @{reg}"));
                }
            }
            Action::PlayMacro(c) => {
                if let Some(keys) = self.macros.get_macro(c).map(|s| s.to_vec()) {
                    self.last_macro_reg = Some(c);
                    for key in keys {
                        self.handle_key(key)?;
                    }
                } else {
                    self.message = Message::Info(format!("No macro in register @{c}"));
                }
            }
            Action::PlayLastMacro => {
                if let Some(reg) = self.last_macro_reg {
                    if let Some(keys) = self.macros.get_macro(reg).map(|s| s.to_vec()) {
                        for key in keys {
                            self.handle_key(key)?;
                        }
                    } else {
                        self.message = Message::Info(format!("No macro in register @{reg}"));
                    }
                } else {
                    self.message = Message::Info("No macro register set".into());
                }
            }
            Action::DotRepeat => {
                if let Some(keys) = self.macros.dot_repeat().map(|s| s.to_vec()) {
                    for key in keys {
                        self.handle_key(key)?;
                    }
                }
            }

            // ── Register selection ────────────────────────────────────────────
            Action::SetRegister(c) => {
                self.pending_register = Some(c);
            }

            // ── Window management ─────────────────────────────────────────────
            Action::SplitHorizontal => {
                let new_id = self.next_window_id;
                self.next_window_id += 1;
                let doc_idx = self.focused_win().doc_idx;
                let new_win = WindowState::new(doc_idx);
                self.windows.push(new_win);
                // Replace the layout with a horizontal split
                let old_layout = std::mem::replace(&mut self.layout, Layout::single(WindowId(0)));
                self.layout = old_layout.split_h(WindowId(new_id));
            }
            Action::SplitVertical => {
                let new_id = self.next_window_id;
                self.next_window_id += 1;
                let doc_idx = self.focused_win().doc_idx;
                let new_win = WindowState::new(doc_idx);
                self.windows.push(new_win);
                let old_layout = std::mem::replace(&mut self.layout, Layout::single(WindowId(0)));
                self.layout = old_layout.split_v(WindowId(new_id));
            }
            Action::FocusWindowNext => {
                let current_id = WindowId(self.focused_window);
                if let Some(next_id) = self.layout.cycle_next(current_id) {
                    self.focused_window = next_id.0;
                }
            }
            Action::FocusWindowPrev => {
                let current_id = WindowId(self.focused_window);
                if let Some(prev_id) = self.layout.cycle_prev(current_id) {
                    self.focused_window = prev_id.0;
                }
            }
            Action::CloseWindow => {
                if self.windows.len() <= 1 {
                    // Last window: quit
                    self.running = false;
                } else {
                    let target_id = WindowId(self.focused_window);
                    let old_layout =
                        std::mem::replace(&mut self.layout, Layout::single(WindowId(0)));
                    if let Some(new_layout) = old_layout.remove(target_id) {
                        self.layout = new_layout;
                    } else {
                        // Layout removal returned None (last window case handled above)
                        self.running = false;
                        return Ok(());
                    }
                    self.windows.remove(self.focused_window);
                    if self.focused_window >= self.windows.len() {
                        self.focused_window = self.windows.len() - 1;
                    }
                }
            }
            Action::OnlyWindow => {
                let kept = self.windows.remove(self.focused_window);
                self.windows.clear();
                self.windows.push(kept);
                self.focused_window = 0;
                self.layout = Layout::single(WindowId(0));
            }

            // ── Picker ────────────────────────────────────────────────────────
            Action::OpenFilePicker => {
                let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                let picker = build_file_picker(&cwd);
                self.picker = Some(picker);
                self.picker_kind = PickerKind::File;
            }
            Action::OpenBufferPicker => {
                let names: Vec<String> = self.docs.iter().map(|d| d.name().to_string()).collect();
                let picker = build_buffer_picker(&names);
                self.picker = Some(picker);
                self.picker_kind = PickerKind::Buffer;
            }

            // ── Ex commands (dispatched via command mode) ─────────────────────
            Action::WriteFile
            | Action::Quit
            | Action::QuitForce
            | Action::WriteQuit
            | Action::EditFile(_)
            | Action::NextBuffer
            | Action::PrevBuffer => {}

            Action::PendingOperator(_) => {} // handled by KeymapState
        }

        // Reset goal_col unless a vertical motion just set it
        match &action {
            Action::Move(Motion::Up)
            | Action::Move(Motion::Down)
            | Action::Move(Motion::HalfPageDown)
            | Action::Move(Motion::HalfPageUp) => {}
            _ => self.goal_col = None,
        }

        Ok(())
    }

    // ── Text-object helper ────────────────────────────────────────────────────

    fn resolve_textobj(
        &self,
        rope: &ropey::Rope,
        pos: usize,
        textobj: onda_modal::TextObj,
    ) -> Option<onda_core::Range> {
        use onda_modal::textobj as to;
        use onda_modal::TextObj;
        match textobj {
            TextObj::InnerWord => to::inner_word(rope, pos),
            TextObj::OuterWord => to::outer_word(rope, pos),
            TextObj::InnerBigWord => to::inner_big_word(rope, pos),
            TextObj::OuterBigWord => to::outer_big_word(rope, pos),
            TextObj::InnerParens => to::inner_parens(rope, pos),
            TextObj::OuterParens => to::outer_parens(rope, pos),
            TextObj::InnerBrackets => to::inner_brackets(rope, pos),
            TextObj::OuterBrackets => to::outer_brackets(rope, pos),
            TextObj::InnerBraces => to::inner_braces(rope, pos),
            TextObj::OuterBraces => to::outer_braces(rope, pos),
            TextObj::InnerDoubleQuote => to::inner_double_quote(rope, pos),
            TextObj::OuterDoubleQuote => to::outer_double_quote(rope, pos),
            TextObj::InnerSingleQuote => to::inner_single_quote(rope, pos),
            TextObj::OuterSingleQuote => to::outer_single_quote(rope, pos),
            TextObj::InnerBacktick => to::inner_backtick(rope, pos),
            TextObj::OuterBacktick => to::outer_backtick(rope, pos),
            TextObj::InnerParagraph => to::inner_paragraph(rope, pos),
            TextObj::OuterParagraph => to::outer_paragraph(rope, pos),
            // Tree-sitter text objects (T18.2) resolved via onda-syntax.
            TextObj::InnerFunction => self.resolve_ts_textobj(
                rope,
                pos,
                onda_syntax::TextObjectKind::Function,
                onda_syntax::TextObjectScope::Inner,
            ),
            TextObj::OuterFunction => self.resolve_ts_textobj(
                rope,
                pos,
                onda_syntax::TextObjectKind::Function,
                onda_syntax::TextObjectScope::Outer,
            ),
            TextObj::InnerClass => self.resolve_ts_textobj(
                rope,
                pos,
                onda_syntax::TextObjectKind::Class,
                onda_syntax::TextObjectScope::Inner,
            ),
            TextObj::OuterClass => self.resolve_ts_textobj(
                rope,
                pos,
                onda_syntax::TextObjectKind::Class,
                onda_syntax::TextObjectScope::Outer,
            ),
            TextObj::InnerArgument => self.resolve_ts_textobj(
                rope,
                pos,
                onda_syntax::TextObjectKind::Parameter,
                onda_syntax::TextObjectScope::Inner,
            ),
            TextObj::OuterArgument => self.resolve_ts_textobj(
                rope,
                pos,
                onda_syntax::TextObjectKind::Parameter,
                onda_syntax::TextObjectScope::Outer,
            ),
        }
    }

    /// Resolve a tree-sitter text object for the current document's language.
    /// Returns `None` (graceful fallback) when no grammar is available.
    fn resolve_ts_textobj(
        &self,
        rope: &ropey::Rope,
        pos: usize,
        kind: onda_syntax::TextObjectKind,
        scope: onda_syntax::TextObjectScope,
    ) -> Option<onda_core::Range> {
        let lang = self.current_language_name()?;
        onda_syntax::text_object(rope, pos, &lang, kind, scope)
    }

    /// Detect the current document's language name via the registry.
    fn current_language_name(&self) -> Option<String> {
        let doc = self.doc();
        let path_str = doc
            .path()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let first_line = if doc.len_lines() > 0 {
            Some(doc.rope().line(0).to_string())
        } else {
            None
        };
        self.lang_registry
            .detect(&path_str, first_line.as_deref())
            .map(|c| c.name.clone())
    }

    /// True when `textobj` is a tree-sitter (grammar-backed) text object.
    fn is_ts_textobj(textobj: onda_modal::TextObj) -> bool {
        use onda_modal::TextObj::*;
        matches!(
            textobj,
            InnerFunction | OuterFunction | InnerClass | OuterClass | InnerArgument | OuterArgument
        )
    }

    // ── Register helper ───────────────────────────────────────────────────────

    /// Return a clone of the active register (pending if set, otherwise unnamed).
    fn active_register(&mut self) -> Option<Register> {
        let name = self.pending_register.take().unwrap_or('"');
        self.registers.get(name).cloned()
    }

    // ── Operator apply ────────────────────────────────────────────────────────

    fn apply_operator(&mut self, op: Operator, sel: &Selection, linewise: bool) -> Result<()> {
        let (tx, reg) = if linewise {
            onda_modal::operator::delete_lines(self.doc(), sel)
        } else {
            onda_modal::operator::delete(self.doc(), sel)
        };

        match op {
            Operator::Yank => {
                let reg_name = self.pending_register.take().unwrap_or('"');
                self.registers.set(reg_name, reg.clone());
                self.registers.set('"', reg);
                return Ok(());
            }
            Operator::Delete | Operator::Change => {
                let reg_name = self.pending_register.take().unwrap_or('"');
                self.registers.set(reg_name, reg.clone());
                self.registers.set('"', reg);
                if !tx.changes.is_empty() {
                    let sel_before = self.selection().clone();
                    let inv = self.doc_mut().apply(&tx)?;
                    let new_pos = tx
                        .changes
                        .map_pos(sel.primary().from(), onda_core::Assoc::After);
                    let new_pos = new_pos.min(self.doc().len_chars().saturating_sub(1));
                    *self.selection_mut() = Selection::point(new_pos);
                    let sel_after = self.selection().clone();
                    self.undo().push(tx, inv, sel_before, sel_after);
                }
                if op == Operator::Change {
                    self.mode = Mode::Insert;
                    self.undo().begin_group();
                }
                self.update_search_matches();
            }
        }
        Ok(())
    }

    // ── Terminal pane ─────────────────────────────────────────────────────────

    fn open_terminal_pane(&mut self) -> Result<()> {
        let (_, height) = self.backend.size();
        let (width, _) = self.backend.size();
        let rows = height.saturating_sub(3);
        let cols = width;

        let pane_id = self.next_pane_id;
        self.next_pane_id += 1;

        let bg_tx = self.get_bg_tx();
        match PtyProcess::spawn(cols, rows) {
            Ok((process, mut event_rx)) => {
                let pane_id_copy = pane_id;
                // Bridge PTY events to bg_rx via std thread
                std::thread::spawn(move || {
                    while let Some(ev) = event_rx.blocking_recv() {
                        match ev {
                            PtyEvent::Data(bytes) => {
                                let _ = bg_tx.send(BgMessage::PtyData {
                                    pane_id: pane_id_copy,
                                    data: bytes,
                                });
                            }
                            PtyEvent::Exited(_code) => {
                                let _ = bg_tx.send(BgMessage::PtyExited {
                                    pane_id: pane_id_copy,
                                });
                                break;
                            }
                        }
                    }
                });

                let screen = TerminalScreen::new(rows, cols);
                let pane = TerminalPane {
                    process,
                    screen,
                    pane_id,
                };
                self.terminal_panes.push(pane);

                // Open a new split for the terminal
                let new_id = self.next_window_id;
                self.next_window_id += 1;
                // Create a scratch doc for the terminal window
                let doc_idx = self.docs.len();
                self.docs.push(Document::new_empty());
                let new_win = WindowState::new(doc_idx);
                self.windows.push(new_win);
                let old_layout = std::mem::replace(&mut self.layout, Layout::single(WindowId(0)));
                self.layout = old_layout.split_h(WindowId(new_id));

                // Associate window with pane
                self.window_to_pane.insert(new_id, pane_id);
                self.focused_window = new_id;
                self.mode = Mode::Terminal;
                self.message = Message::Info("Terminal opened. Ctrl-\\ Ctrl-n to exit.".into());
            }
            Err(e) => {
                self.message = Message::Error(format!("Terminal error: {e}"));
            }
        }
        Ok(())
    }

    fn get_bg_tx(&self) -> mpsc::SyncSender<BgMessage> {
        self.bg_tx.clone()
    }

    // ── Session ────────────────────────────────────────────────────────────────

    fn build_session(&self) -> Session {
        use onda_session::{BufferEntry, CursorPos, SplitEntry, WindowEntry};

        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

        let buffers: Vec<BufferEntry> = self
            .docs
            .iter()
            .enumerate()
            .map(|(i, d)| {
                let unsaved = if d.is_modified() && d.path().is_none() {
                    Some(d.rope().to_string())
                } else {
                    None
                };
                BufferEntry {
                    id: i,
                    path: d.path().map(|p| p.to_path_buf()),
                    name: d.name().to_string(),
                    unsaved_content: unsaved,
                }
            })
            .collect();

        let windows: Vec<WindowEntry> = self
            .windows
            .iter()
            .enumerate()
            .map(|(i, w)| WindowEntry {
                id: i,
                buffer_id: w.doc_idx,
                cursor: CursorPos {
                    char_offset: w.selection.primary().head,
                    viewport_line: w.viewport.offset_line,
                },
            })
            .collect();

        Session {
            version: Session::CURRENT_VERSION,
            cwd,
            buffers,
            windows,
            layout: SplitEntry::Window { window_id: 0 },
            focused_window: self.focused_window,
        }
    }

    fn apply_session(&mut self, session: Session) -> Result<()> {
        self.docs.clear();
        self.windows.clear();
        self.focused_window = session.focused_window;

        for buf in &session.buffers {
            let doc = if let Some(path) = &buf.path {
                match Document::open(path) {
                    Ok(d) => d,
                    Err(_) => Document::new_empty(),
                }
            } else if let Some(content) = &buf.unsaved_content {
                let mut d = Document::new_empty();
                let cs = onda_core::transaction::ChangeSetBuilder::new(0)
                    .insert(content)
                    .build();
                let _ = d.apply(&Transaction::new(cs));
                d
            } else {
                Document::new_empty()
            };
            self.docs.push(doc);
        }

        for win in &session.windows {
            let doc_idx = win.buffer_id.min(self.docs.len().saturating_sub(1));
            let mut ws = WindowState::new(doc_idx);
            let max_pos = self.docs[doc_idx].len_chars().saturating_sub(1);
            ws.selection = Selection::point(win.cursor.char_offset.min(max_pos));
            ws.viewport.offset_line = win.cursor.viewport_line;
            self.windows.push(ws);
        }

        if self.windows.is_empty() {
            self.windows.push(WindowState::new(0));
        }
        if self.focused_window >= self.windows.len() {
            self.focused_window = 0;
        }

        // Reset layout to single window for simplicity
        // (full layout serialization deferred to Phase 3)
        self.layout = Layout::single(WindowId(0));

        Ok(())
    }

    // ── WASM plugin integration ───────────────────────────────────────────────

    /// A read-only snapshot of the focused buffer for plugin reads: `(buf id,
    /// path, snapshot)`. The buffer id is the doc index.
    fn focused_plugin_snapshot(&self) -> (u64, String, onda_plugin::BufferSnapshot) {
        let idx = self.focused_win().doc_idx;
        let doc = &self.docs[idx];
        (
            idx as u64,
            doc.name().to_string(),
            onda_plugin::BufferSnapshot::new(doc.rope().to_string()),
        )
    }

    /// Fire `buffer-open` to plugins (startup, file load, `:e`).
    fn fire_plugin_open(&mut self, doc_idx: usize) {
        if self.plugin_host.is_none() || doc_idx >= self.docs.len() {
            return;
        }
        let doc = &self.docs[doc_idx];
        let snap = onda_plugin::BufferSnapshot::new(doc.rope().to_string());
        let path = doc.name().to_string();
        let calls = self.plugin_host.as_mut().unwrap().fire(
            PluginEvent::BufferOpen {
                buf: doc_idx as u64,
                path,
            },
            snap,
        );
        self.apply_plugin_calls(calls);
    }

    /// Idle tick: fire `cursor-hold` + `buffer-change` to plugins, apply results.
    fn tick_plugins(&mut self) {
        if self.plugin_host.is_none() {
            return;
        }
        let (buf, path, snap) = self.focused_plugin_snapshot();
        let pos = self.selection().primary().head as u32;
        let mut calls = Vec::new();
        {
            let host = self.plugin_host.as_mut().unwrap();
            calls.extend(host.fire(PluginEvent::CursorHold { buf, pos }, snap.clone()));
            calls.extend(host.fire(PluginEvent::BufferChange { buf, path }, snap));
        }
        self.apply_plugin_calls(calls);
    }

    /// Dispatch an unknown `:name …` to a plugin command. Returns true if handled.
    fn try_plugin_command(&mut self, line: &str) -> bool {
        if self.plugin_host.is_none() {
            return false;
        }
        let mut parts = line.split_whitespace();
        let Some(name) = parts.next().map(|s| s.to_string()) else {
            return false;
        };
        let args: Vec<String> = parts.map(|s| s.to_string()).collect();
        let (buf, _path, snap) = self.focused_plugin_snapshot();
        let calls = self
            .plugin_host
            .as_mut()
            .unwrap()
            .run_command(&name, args, buf, snap);
        match calls {
            Some(calls) => {
                self.apply_plugin_calls(calls);
                true
            }
            None => false,
        }
    }

    /// Apply the effectful calls a plugin made (rule 2: between frames).
    fn apply_plugin_calls(&mut self, calls: Vec<PluginApiCall>) {
        for call in calls {
            match call {
                PluginApiCall::Notify { msg, level } => {
                    self.message_history.push(msg.clone());
                    self.message = match level {
                        onda_plugin::NotifyLevel::Error => Message::Error(msg),
                        _ => Message::Info(msg),
                    };
                }
                PluginApiCall::BufferApply { buf_id, mut edits } => {
                    let idx = buf_id as usize;
                    if idx < self.docs.len() {
                        let len = self.docs[idx].len_chars();
                        edits.sort_by_key(|e| e.start);
                        let mut b = onda_core::transaction::ChangeSetBuilder::new(len);
                        let mut pos = 0usize;
                        for e in edits {
                            // Skip overlapping / out-of-bounds edits (snapshot was stale).
                            if e.start < pos || e.end > len || e.start > e.end {
                                continue;
                            }
                            b = b
                                .retain(e.start - pos)
                                .delete(e.end - e.start)
                                .insert(&e.text);
                            pos = e.end;
                        }
                        let tx = Transaction::new(b.build());
                        let _ = self.docs[idx].apply(&tx);
                    }
                }
                PluginApiCall::SetCursor { win_id, pos } => {
                    let win = win_id as usize;
                    if win < self.windows.len() {
                        let doc_idx = self.windows[win].doc_idx;
                        if doc_idx < self.docs.len() {
                            let clamped = pos.min(self.docs[doc_idx].len_chars());
                            self.windows[win].selection = Selection::point(clamped);
                        }
                    }
                }
                PluginApiCall::SetSelection { ranges, .. } => {
                    if let Some((_anchor, head)) = ranges.first().copied() {
                        let clamped = head.min(self.doc().len_chars());
                        *self.selection_mut() = Selection::point(clamped);
                    }
                }
                PluginApiCall::UiFloat { lines, .. } => {
                    self.hover_float = Some(HoverFloat {
                        lines,
                        col: 4,
                        row: 4,
                    });
                }
                PluginApiCall::HighlightGroup { group, style } => {
                    // Apply now and remember it so it survives theme switches.
                    let _ = self.theme.set_parsed(
                        &group,
                        style.fg.as_deref(),
                        style.bg.as_deref(),
                        style.bold,
                        style.italic,
                        style.underline,
                    );
                    self.plugin_highlights.retain(|(g, _)| g != &group);
                    self.plugin_highlights.push((group, style));
                    self.compositor.buf.invalidate();
                }
                PluginApiCall::SetDecorations { buf_id, batch } => {
                    self.plugin_decorations
                        .entry(buf_id as usize)
                        .or_default()
                        .insert(batch.namespace.clone(), batch);
                    self.compositor.buf.invalidate();
                }
                PluginApiCall::ClearDecorations { buf_id, namespace } => {
                    if let Some(ns) = self.plugin_decorations.get_mut(&(buf_id as usize)) {
                        ns.remove(&namespace);
                    }
                    self.compositor.buf.invalidate();
                }
                // Picker contributions, statusline segments, and plugin keymaps
                // are follow-ups — see docs/BACKLOG.md. CmdCreate is owned by
                // PluginHost (the command registry).
                PluginApiCall::UiPick { .. }
                | PluginApiCall::StatuslineSegment { .. }
                | PluginApiCall::CmdCreate { .. }
                | PluginApiCall::KeymapSet { .. } => {}
            }
        }
    }

    // ── Background channel drain ──────────────────────────────────────────────

    fn drain_bg_channel(&mut self) {
        while let Ok(msg) = self.bg_rx.try_recv() {
            match msg {
                BgMessage::FileLoaded { doc } => {
                    let name = doc.name().to_string();
                    let doc_idx = self.docs.len();
                    self.docs.push(doc);
                    self.focused_win_mut().doc_idx = doc_idx;
                    *self.selection_mut() = Selection::point(0);
                    *self.viewport_mut() = Viewport::new();
                    self.message = Message::Info(format!("Loaded: {name}"));
                    self.try_spawn_syntax_worker_for_doc(doc_idx);
                }
                BgMessage::FileError { path, error } => {
                    self.message = Message::Error(format!("{}: {error}", path.display()));
                }
                BgMessage::Lsp(lsp_ev) => {
                    self.handle_lsp_event(lsp_ev);
                }
                BgMessage::PtyData { pane_id, data } => {
                    if let Some(pane) = self
                        .terminal_panes
                        .iter_mut()
                        .find(|p| p.pane_id == pane_id)
                    {
                        pane.screen.process(&data);
                    }
                }
                BgMessage::PtyExited { pane_id } => {
                    self.message = Message::Info(format!("Terminal [{}] exited", pane_id));
                    self.terminal_panes.retain(|p| p.pane_id != pane_id);
                    // Return to normal mode if focused window was this terminal
                    if self.mode == Mode::Terminal {
                        self.mode = Mode::Normal;
                    }
                }
                BgMessage::ThemeReload => {
                    self.reload_theme_file();
                }
                BgMessage::Agent(ev) => self.handle_agent_event(ev),
                BgMessage::AgentClientReady(client) => {
                    self.agent_client = Some(client);
                }
            }
        }
    }

    fn handle_lsp_event(&mut self, ev: LspEvent) {
        match ev {
            LspEvent::Ready { root } => {
                self.message = Message::Info(format!(
                    "LSP ready for {:?}",
                    root.file_name().unwrap_or_default()
                ));
            }
            LspEvent::Diagnostics { path, diagnostics } => {
                // Find the document index for this path
                let doc_idx = self
                    .docs
                    .iter()
                    .position(|d| d.path().map(|p| p == path).unwrap_or(false));

                // Store diagnostics
                self.diagnostics.insert(path.clone(), diagnostics.clone());

                // Resolve to char offsets for rendering
                if let Some(idx) = doc_idx {
                    let doc = &self.docs[idx];
                    let spans: Vec<DiagnosticSpan> = diagnostics
                        .iter()
                        .map(|d| {
                            let start_line = d.range.start.line as usize;
                            let start_col = d.range.start.character as usize;
                            let end_line = d.range.end.line as usize;
                            let end_col = d.range.end.character as usize;

                            let from = if start_line < doc.len_lines() {
                                doc.line_to_char(start_line) + start_col
                            } else {
                                doc.len_chars()
                            };
                            let to = if end_line < doc.len_lines() {
                                doc.line_to_char(end_line) + end_col
                            } else {
                                doc.len_chars()
                            };
                            let severity = match d.severity {
                                onda_lsp::types::DiagnosticSeverity::Error => 0,
                                onda_lsp::types::DiagnosticSeverity::Warning => 1,
                                _ => 2,
                            };
                            DiagnosticSpan { from, to, severity }
                        })
                        .collect();
                    self.diagnostic_spans.insert(idx, spans);
                }

                // Update statusline diagnostic count
                let error_count = diagnostics
                    .iter()
                    .filter(|d| matches!(d.severity, onda_lsp::types::DiagnosticSeverity::Error))
                    .count();
                let warn_count = diagnostics
                    .iter()
                    .filter(|d| matches!(d.severity, onda_lsp::types::DiagnosticSeverity::Warning))
                    .count();
                if error_count > 0 || warn_count > 0 {
                    debug!("Diagnostics: E:{} W:{}", error_count, warn_count);
                }
            }
            LspEvent::HoverResult {
                request_id: _,
                content,
            } => {
                if let Some(text) = content {
                    let lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();
                    let (_, h) = self.backend.size();
                    let row = h.saturating_sub(10).max(2);
                    self.hover_float = Some(HoverFloat { lines, col: 4, row });
                } else {
                    self.message = Message::Info("No hover information available".into());
                }
            }
            LspEvent::DefinitionResult {
                request_id: _,
                locations,
            } => {
                if locations.is_empty() {
                    self.message = Message::Info("No definition found".into());
                } else if locations.len() == 1 {
                    let loc = &locations[0];
                    let uri_str = loc.uri.as_str();
                    let path = if let Some(rest) = uri_str.strip_prefix("file://") {
                        PathBuf::from(rest)
                    } else {
                        PathBuf::from(uri_str)
                    };
                    self.jump_to_location(
                        path,
                        loc.range.start.line as usize,
                        loc.range.start.character as usize,
                    );
                } else {
                    // Multiple definitions: show in picker
                    self.message = Message::Info(format!(
                        "{} definitions found (use :gr for list)",
                        locations.len()
                    ));
                }
            }
            LspEvent::ReferencesResult {
                request_id: _,
                locations,
            } => {
                self.message = Message::Info(format!("{} references found", locations.len()));
            }
            LspEvent::CompletionResult { request_id, items } => {
                if !items.is_empty() {
                    let menu_items: Vec<(String, String)> = items
                        .iter()
                        .map(|item| {
                            let label = item.label.clone();
                            let kind_icon = completion_kind_icon(item.kind);
                            (label, kind_icon.to_string())
                        })
                        .collect();
                    self.completion = Some(CompletionState {
                        items: menu_items,
                        selected: 0,
                        request_id,
                    });
                }
            }
            LspEvent::RenameResult {
                request_id: _,
                edit,
            } => {
                if let Some(_edit) = edit {
                    self.message = Message::Info("Rename: workspace edit applied (stub)".into());
                }
            }
            LspEvent::FormattingResult {
                request_id: _,
                edits,
            } => {
                if !edits.is_empty() {
                    self.message = Message::Info(format!("Format: {} edits applied", edits.len()));
                }
            }
            LspEvent::ServerError {
                root: _root,
                message,
            } => {
                self.message = Message::Error(format!("LSP error: {message}"));
            }
        }
    }

    fn jump_to_location(&mut self, path: PathBuf, line: usize, col: usize) {
        // Find or open the document
        let doc_idx = if let Some(idx) = self
            .docs
            .iter()
            .position(|d| d.path().map(|p| p == path).unwrap_or(false))
        {
            idx
        } else {
            match Document::open(&path) {
                Ok(doc) => {
                    let idx = self.docs.len();
                    self.docs.push(doc);
                    idx
                }
                Err(e) => {
                    self.message = Message::Error(format!("Cannot open {:?}: {}", path, e));
                    return;
                }
            }
        };

        self.focused_win_mut().doc_idx = doc_idx;
        let doc = &self.docs[doc_idx];
        let char_pos = if line < doc.len_lines() {
            let ls = doc.line_to_char(line);
            ls + col.min(doc.line_len_no_eol(line))
        } else {
            0
        };
        *self.selection_mut() = Selection::point(char_pos);
    }

    // ── Main loop ─────────────────────────────────────────────────────────────

    fn run(&mut self) -> Result<()> {
        self.compositor.buf.invalidate();
        self.render_frame().context("initial render")?;

        while self.running {
            let had_event = event::poll(Duration::from_millis(8))?;
            if had_event {
                let ev = event::read()?;
                self.handle_event(ev)?;
                self.plugin_idle_fired = false;
            }

            self.drain_bg_channel();
            // On idle (no input this tick), fire plugin cursor-hold/buffer-change
            // once, so plugins react without being polled every frame.
            if !had_event && !self.plugin_idle_fired {
                self.tick_plugins();
                self.plugin_idle_fired = true;
            }
            self.render_frame().context("render frame")?;
        }

        // Auto-save session on quit
        let session = self.build_session();
        let _ = self.session_manager.auto_save(&session);

        #[cfg(feature = "bench")]
        self.tracer.report();

        Ok(())
    }
}

// ── Agent config ─────────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
struct AgentsFile {
    #[serde(default)]
    agent: Vec<AgentToml>,
}

#[derive(serde::Deserialize)]
struct AgentToml {
    name: String,
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: Vec<(String, String)>,
}

/// Load agents from `~/.config/onda/agents.toml`, with a built-in `claude` default.
fn load_agent_registry() -> onda_agent::AgentRegistry {
    let mut reg = onda_agent::AgentRegistry::new();
    reg.add(onda_agent::AgentConfig {
        name: "claude".into(),
        command: "claude-code".into(),
        args: vec!["acp".into()],
        env: vec![],
    });
    if let Ok(home) = std::env::var("HOME") {
        let path = PathBuf::from(home).join(".config/onda/agents.toml");
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(file) = toml::from_str::<AgentsFile>(&text) {
                for a in file.agent {
                    reg.add(onda_agent::AgentConfig {
                        name: a.name,
                        command: a.command,
                        args: a.args,
                        env: a.env,
                    });
                }
            }
        }
    }
    reg
}

/// Path to the persisted agent permission rules.
fn agent_perms_path() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .map(|h| PathBuf::from(h).join(".config/onda/agent-perms.json"))
}

fn load_agent_perms() -> onda_agent::PermissionStore {
    match agent_perms_path() {
        Some(p) => onda_agent::PermissionStore::load(&p),
        None => onda_agent::PermissionStore::new(),
    }
}

// ── Theme loading ──────────────────────────────────────────────────────────────

/// Candidate on-disk paths for a theme file, in priority order.
fn theme_search_paths(name: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        paths.push(
            PathBuf::from(home)
                .join(".config/onda/themes")
                .join(format!("{name}.toml")),
        );
    }
    paths.push(PathBuf::from(format!("runtime/themes/{name}.toml")));
    paths
}

/// Spawn a filesystem watcher on a theme file; sends `ThemeReload` on change
/// (100ms leading-edge debounce). Returns the watcher (kept alive by the caller).
fn spawn_theme_watcher(
    path: &std::path::Path,
    bg_tx: mpsc::SyncSender<BgMessage>,
) -> Option<notify::RecommendedWatcher> {
    use notify::Watcher;
    let last = std::sync::Arc::new(std::sync::Mutex::new(
        std::time::Instant::now() - Duration::from_secs(1),
    ));
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if res.is_ok() {
            if let Ok(mut l) = last.lock() {
                if l.elapsed() >= Duration::from_millis(100) {
                    *l = std::time::Instant::now();
                    let _ = bg_tx.try_send(BgMessage::ThemeReload);
                }
            }
        }
    })
    .ok()?;
    watcher
        .watch(path, notify::RecursiveMode::NonRecursive)
        .ok()?;
    Some(watcher)
}

/// Resolve a theme by name. Tries on-disk files first (so they can hot-reload),
/// then the embedded built-in, then falls back to `onda-dark`. Returns the theme and
/// the on-disk path it was loaded from (for the live-reload watcher), if any.
fn load_theme(name: &str) -> (onda_render::Theme, Option<PathBuf>) {
    let normalized = if name.is_empty() || name == "default" {
        "onda-dark"
    } else {
        name
    };
    for path in theme_search_paths(normalized) {
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(theme) = onda_render::Theme::from_toml(normalized, &text) {
                return (theme, Some(path));
            }
        }
    }
    let theme =
        onda_render::Theme::builtin(normalized).unwrap_or_else(onda_render::Theme::default_dark);
    (theme, None)
}

// ── Background file loader ─────────────────────────────────────────────────────

fn load_file_async(path: PathBuf, tx: mpsc::SyncSender<BgMessage>) {
    std::thread::spawn(move || match Document::open(&path) {
        Ok(doc) => {
            let _ = tx.send(BgMessage::FileLoaded { doc });
        }
        Err(e) => {
            let _ = tx.send(BgMessage::FileError {
                path,
                error: e.to_string(),
            });
        }
    });
}

// ── App constructors ──────────────────────────────────────────────────────────

fn make_app<B: Backend>(
    initial_doc: Document,
    backend: B,
    compositor: Compositor,
    bg_tx: mpsc::SyncSender<BgMessage>,
    bg_rx: mpsc::Receiver<BgMessage>,
    config: Config,
    running: bool,
) -> App<B> {
    let win0 = WindowState::new(0);
    let (theme, theme_path) = load_theme(&config.theme);
    let theme_watcher = theme_path
        .as_ref()
        .and_then(|p| spawn_theme_watcher(p, bg_tx.clone()));
    let undo_store = if config.editor.persistent_undo {
        onda_session::UndoStore::default_path().map(onda_session::UndoStore::new)
    } else {
        None
    };
    App {
        docs: vec![initial_doc],
        windows: vec![win0],
        focused_window: 0,
        layout: Layout::single(WindowId(0)),
        next_window_id: 1,
        mode: Mode::Normal,
        keymap: Keymap::new(),
        keymap_state: KeymapState::new(),
        registers: RegisterBank::new(),
        pending_register: None,
        macros: MacroRecorder::new(),
        last_macro_reg: None,
        search: SearchState::new(),
        search_matches: Vec::new(),
        search_input_dir: None,
        marks: MarkStore::new(),
        jumps: JumpList::new(),
        picker: None,
        picker_kind: PickerKind::File,
        syntax_workers: Vec::new(),
        syntax_versions: Vec::new(),
        lang_registry: LanguageRegistry::new(),
        message: Message::None,
        message_history: Vec::new(),
        goal_col: None,
        compositor,
        backend,
        running,
        command_line: CommandLine::new(),
        cmd_completion: None,
        bg_tx,
        bg_rx,
        theme,
        theme_watcher,
        theme_path,
        plugin_highlights: Vec::new(),
        config,
        table_docs: HashMap::new(),
        table_layout: HashMap::new(),
        agent_registry: load_agent_registry(),
        agent_client: None,
        agent_name: None,
        agent_panel_open: false,
        agent_input_focused: false,
        agent_input: String::new(),
        agent_thread: Vec::new(),
        agent_busy: false,
        agent_perms: load_agent_perms(),
        agent_pending_perm: None,
        agent_staging: onda_agent::StagingArea::new(),
        review: None,
        undo_store,
        undo_loaded: std::collections::HashSet::new(),
        lsp_manager: None,
        lsp_event_tx: None,
        diagnostics: HashMap::new(),
        diagnostic_spans: HashMap::new(),
        lsp_request_id: 1,
        hover_float: None,
        completion: None,
        terminal_panes: Vec::new(),
        next_pane_id: 0,
        window_to_pane: HashMap::new(),
        session_manager: SessionManager::new(),
        plugin_host: None,
        plugin_idle_fired: false,
        plugin_decorations: HashMap::new(),
        doc_last_len: HashMap::new(),
        soft_wrap: false,
        #[cfg(feature = "bench")]
        tracer: LatencyTracer::default(),
    }
}

// ── Entry points ──────────────────────────────────────────────────────────────

fn init_tracing() {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_env("ONDA_LOG").unwrap_or_else(|_| EnvFilter::new("warn")))
        .with(fmt::layer().with_writer(std::io::stderr))
        .init();
}

fn run_bench_startup() -> Result<()> {
    let (bg_tx, bg_rx) = mpsc::sync_channel(16);
    let backend = NullBackend::new(120, 40);
    let (width, height) = backend.size();
    let compositor = Compositor::new(width, height);

    let mut doc = Document::new_empty();
    let cs = onda_core::transaction::ChangeSetBuilder::new(0)
        .insert("Hello, onda!\n")
        .build();
    doc.apply(&Transaction::new(cs)).unwrap();

    let mut app = make_app(
        doc,
        backend,
        compositor,
        bg_tx,
        bg_rx,
        Config::default(),
        false,
    );

    app.compositor.buf.invalidate();
    app.render_frame()?;
    Ok(())
}

fn run_editor(paths: Vec<PathBuf>) -> Result<()> {
    init_tracing();

    // A multi-threaded tokio runtime backs all background workers (syntax, LSP,
    // agent). Entering its context lets `tokio::spawn` work from the synchronous
    // main loop; spawned tasks run on the runtime's own threads. The guard (and
    // runtime) must outlive `app.run()`.
    let runtime = tokio::runtime::Runtime::new()?;
    let _rt_guard = runtime.enter();

    let config_result = Config::load();
    let config = config_result.config;

    let (bg_tx, bg_rx) = mpsc::sync_channel::<BgMessage>(256);

    let initial_doc = if let Some(path) = paths.first() {
        match Document::open(path) {
            Ok(doc) => doc,
            Err(e) => {
                eprintln!("onda: cannot open {:?}: {e}", path);
                Document::new_empty()
            }
        }
    } else {
        Document::new_empty()
    };

    // Load remaining files in background
    for path in paths.into_iter().skip(1) {
        load_file_async(path, bg_tx.clone());
    }

    let mut term = TerminalBackend::new()?;
    term.enter()?;

    // Enable mouse capture (T12.3)
    crossterm::execute!(std::io::stdout(), EnableMouseCapture)?;

    let (width, height) = term.size();
    let compositor = Compositor::new(width, height);

    let mut app = make_app(
        initial_doc,
        term,
        compositor,
        bg_tx.clone(),
        bg_rx,
        config,
        true,
    );

    // Spawn the syntax worker for the initial (CLI-opened) document.
    app.try_spawn_syntax_worker_for_doc(0);

    // Show config warning if any
    if let Some(warn) = config_result.warning {
        app.message = Message::Error(warn);
    }

    // Discover + instantiate installed WASM plugins (ADR-002). `init` runs here
    // (registering commands); buffer-open is fired for the initial doc below.
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let (host, startup_calls) = PluginHost::discover(&cwd);
    app.plugin_host = host;
    app.apply_plugin_calls(startup_calls);
    app.fire_plugin_open(0);

    let result = app.run();

    // Disable mouse capture on exit
    let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture);
    app.backend.leave()?;
    result
}

// ── Terminal rendering helper ──────────────────────────────────────────────────

/// Overlay git gutter signs in the leftmost column of the line-number gutter.
/// Draw the diff-review overlay: a bordered box of styled lines (header + hunks),
/// scrolled so the focused hunk stays visible isn't tracked here — the line list is
/// short enough to top-anchor; callers cap content.
fn draw_review_overlay(
    grid: &mut onda_render::Grid,
    width: u16,
    content_height: u16,
    lines: &[(onda_render::Style, String)],
    theme: &onda_render::Theme,
) {
    use onda_render::Style;
    let bw = (width as i32 - 6).clamp(20, 100) as u16;
    let bh = (content_height as i32 - 2).clamp(6, 40) as u16;
    let x = (width.saturating_sub(bw)) / 2;
    let y = 1u16;
    let bg = theme.float_bg();
    let border = theme.float_border();

    // Border + fill.
    grid.fill_rect(x, y, bw, bh, bg);
    let top: String = std::iter::once('┌')
        .chain(std::iter::repeat('─').take(bw.saturating_sub(2) as usize))
        .chain(std::iter::once('┐'))
        .collect();
    let bottom: String = std::iter::once('└')
        .chain(std::iter::repeat('─').take(bw.saturating_sub(2) as usize))
        .chain(std::iter::once('┘'))
        .collect();
    grid.write_str(x, y, &top, border);
    grid.write_str(x, y + bh - 1, &bottom, border);
    for r in 1..bh - 1 {
        grid.set(x, y + r, onda_render::Cell::new("│", border));
        grid.set(x + bw - 1, y + r, onda_render::Cell::new("│", border));
    }

    // Content (top-anchored; scroll to keep the list end visible if it overflows).
    let inner_w = bw.saturating_sub(4) as usize;
    let rows = bh.saturating_sub(2) as usize;
    let start = lines.len().saturating_sub(rows);
    for (i, (style, text)) in lines[start..].iter().take(rows).enumerate() {
        let row = y + 1 + i as u16;
        let clipped: String = text.chars().take(inner_w).collect();
        let st = if *style == Style::default() {
            theme.text()
        } else {
            *style
        };
        grid.write_str(x + 2, row, &clipped, st);
    }
}

/// Parse a plugin color string (`#rrggbb` or a basic name) to a render `Color`.
fn plugin_color(s: &str) -> Option<onda_render::Color> {
    use onda_render::Color;
    let s = s.trim();
    if let Some(hex) = s.strip_prefix('#') {
        if hex.len() == 6 {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            return Some(Color::Rgb(r, g, b));
        }
        return None;
    }
    Some(match s.to_ascii_lowercase().as_str() {
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "white" => Color::White,
        "gray" | "grey" => Color::DarkGray,
        _ => return None,
    })
}

/// Convert a plugin decoration style to a render `Style`.
fn plugin_style(s: &onda_plugin::Style) -> onda_render::Style {
    let mut st = onda_render::Style::default();
    if let Some(fg) = s.fg.as_deref().and_then(plugin_color) {
        st = st.fg(fg);
    }
    if let Some(bg) = s.bg.as_deref().and_then(plugin_color) {
        st = st.bg(bg);
    }
    if s.bold {
        st = st.bold();
    }
    if s.italic {
        st = st.italic();
    }
    st
}

/// Paint plugin gutter signs (one glyph per line in the gutter column).
fn draw_plugin_signs(
    grid: &mut onda_render::Grid,
    rect: &Rect,
    viewport: &Viewport,
    signs: &[(usize, String, onda_plugin::Style)],
) {
    if viewport.line_nr_width == 0 {
        return;
    }
    for (line, glyph, style) in signs {
        if *line < viewport.offset_line {
            continue;
        }
        let screen_row = (*line - viewport.offset_line) as u16;
        if screen_row >= rect.height {
            continue;
        }
        grid.set(
            rect.x,
            rect.y + screen_row,
            onda_render::Cell::new(glyph.clone(), plugin_style(style)),
        );
    }
}

/// Overlay plugin highlight styles onto cells in each char range (grapheme kept).
fn draw_plugin_highlights(
    grid: &mut onda_render::Grid,
    rect: &Rect,
    viewport: &Viewport,
    doc: &Document,
    highlights: &[(usize, usize, onda_plugin::Style)],
) {
    let gutter = viewport.line_nr_width;
    let total = doc.len_chars();
    for (start, end, style) in highlights {
        let st = plugin_style(style);
        let start = (*start).min(total);
        let end = (*end).min(total);
        for ch in start..end {
            let line = doc.char_to_line(ch);
            if line < viewport.offset_line {
                continue;
            }
            let screen_row = (line - viewport.offset_line) as u16;
            if screen_row >= rect.height {
                continue;
            }
            let col = ch - doc.line_to_char(line);
            if col < viewport.offset_col {
                continue;
            }
            let sx = rect.x + gutter + (col - viewport.offset_col) as u16;
            if sx >= rect.x + rect.width {
                continue;
            }
            let row = rect.y + screen_row;
            let grapheme = grid
                .get(sx, row)
                .map(|c| c.grapheme.clone())
                .unwrap_or_else(|| " ".to_string());
            grid.set(sx, row, onda_render::Cell::new(grapheme, st));
        }
    }
}

/// Paint plugin virtual text at the end of its anchor line (inlay-style).
fn draw_plugin_virt_text(
    grid: &mut onda_render::Grid,
    rect: &Rect,
    viewport: &Viewport,
    doc: &Document,
    virt: &[(usize, String, onda_plugin::Style)],
) {
    let gutter = viewport.line_nr_width;
    let total = doc.len_chars();
    for (at, text, style) in virt {
        let at = (*at).min(total);
        let line = doc.char_to_line(at);
        if line < viewport.offset_line {
            continue;
        }
        let screen_row = (line - viewport.offset_line) as u16;
        if screen_row >= rect.height {
            continue;
        }
        let eol_col = doc
            .line_len_no_eol(line)
            .saturating_sub(viewport.offset_col);
        // One space gap after the line content.
        let base = rect.x + gutter + eol_col as u16 + 1;
        let row = rect.y + screen_row;
        let st = plugin_style(style);
        for (i, g) in text.chars().enumerate() {
            let sx = base + i as u16;
            if sx >= rect.x + rect.width {
                break;
            }
            grid.set(sx, row, onda_render::Cell::new(g.to_string(), st));
        }
    }
}

/// Draw the command-line completion popup, bottom-anchored just above `anchor_row`.
/// Shows up to 8 candidates, scrolled to keep the selected item visible.
fn draw_cmd_completion(grid: &mut onda_render::Grid, comp: &CmdCompletion, anchor_row: u16) {
    use onda_render::{Color, Style};

    if comp.candidates.is_empty() {
        return;
    }
    const MAX_VISIBLE: usize = 8;
    let n = comp.candidates.len();
    let visible = MAX_VISIBLE.min(n);

    // Scroll window so the selected item is shown.
    let start = if comp.selected >= visible {
        comp.selected + 1 - visible
    } else {
        0
    };
    let slice = &comp.candidates[start..start + visible];

    // Display the basename portion for paths, but the full candidate otherwise.
    let labels: Vec<&str> = slice
        .iter()
        .map(|c| c.rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or(c))
        .collect();
    let width = labels
        .iter()
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(0)
        .max(8)
        + 2;
    let width = (width as u16).min(grid.width());

    let menu_bg = Style::default().fg(Color::White).bg(Color::DarkGray);
    let sel_bg = Style::default().fg(Color::Black).bg(Color::LightCyan);

    let top = anchor_row.saturating_sub(visible as u16);
    for (i, label) in labels.iter().enumerate() {
        let row = top + i as u16;
        let style = if start + i == comp.selected {
            sel_bg
        } else {
            menu_bg
        };
        grid.fill_rect(0, row, width, 1, style);
        grid.write_str(1, row, label, style);
    }
}

/// Render a CSV/TSV document as an aligned virtual table (view-only; rope untouched).
/// Header row is pinned at the top; columns are padded to their cached widths with
/// `│` separators and per-column rainbow tinting; ragged rows are flagged.
#[allow(clippy::too_many_arguments)]
fn render_table(
    grid: &mut onda_render::Grid,
    doc: &Document,
    sel: &Selection,
    viewport: &Viewport,
    rect: &Rect,
    dialect: onda_data::Dialect,
    layout: &onda_data::ColumnLayout,
    theme: &onda_render::Theme,
) {
    use onda_render::Color;
    const RAINBOW: [Color; 5] = [
        Color::Cyan,
        Color::Green,
        Color::Yellow,
        Color::Magenta,
        Color::LightBlue,
    ];
    let header_style = theme.status_bg();
    let text = theme.text();
    let ragged_style = theme.diag_error();
    let expected = layout.column_count();

    let parse = |line_idx: usize| -> Vec<String> {
        let s = doc.line_to_char(line_idx);
        let len = doc.line_len_no_eol(line_idx);
        let raw = doc.rope().slice(s..s + len).to_string();
        let cleaned = onda_data::csv::clean_line(&raw).to_string();
        onda_data::parse_fields(&cleaned, dialect.delimiter, dialect.quote)
    };

    let draw_row =
        |grid: &mut onda_render::Grid, row: u16, fields: &[String], base: onda_render::Style| {
            let mut col = rect.x;
            let ragged = onda_data::csv::is_ragged(fields.len(), expected);
            for (c, width) in layout.widths.iter().enumerate() {
                if col >= rect.x + rect.width {
                    break;
                }
                let cell = fields.get(c).map(|s| s.as_str()).unwrap_or("");
                let mut style = base;
                if base == text {
                    style = base.fg(RAINBOW[c % RAINBOW.len()]);
                }
                if ragged {
                    style = ragged_style;
                }
                let padded = format!("{cell:<width$}", width = *width);
                col = grid.write_str(col, row, &padded, style);
                if c + 1 < layout.widths.len() && col < rect.x + rect.width {
                    col = grid.write_str(col, row, " │ ", theme.line_nr());
                }
            }
            // Clear the rest of the row.
            if col < rect.x + rect.width {
                grid.fill_rect(
                    col,
                    row,
                    rect.x + rect.width - col,
                    1,
                    onda_render::Style::RESET,
                );
            }
        };

    let total = doc.len_lines();
    let has_header = dialect.has_header && total > 0;
    // Pinned header occupies the first screen row when present.
    let mut screen_row = rect.y;
    if has_header {
        let fields = parse(0);
        draw_row(grid, screen_row, &fields, header_style);
        screen_row += 1;
    }

    // Data rows: skip the header line; honor vertical scroll.
    let first_data = if has_header { 1 } else { 0 };
    let start = first_data + viewport.offset_line;
    let cursor_line = doc.char_to_line(sel.primary().head);
    let last_row = rect.y + rect.height;
    let mut line = start;
    while screen_row < last_row {
        if line >= total {
            grid.fill_rect(rect.x, screen_row, rect.width, 1, onda_render::Style::RESET);
            screen_row += 1;
            line += 1;
            continue;
        }
        let fields = parse(line);
        let base = if line == cursor_line {
            theme.selection()
        } else {
            text
        };
        draw_row(grid, screen_row, &fields, base);
        screen_row += 1;
        line += 1;
    }
}

/// Hard-wrap `text` into chunks of at most `width` chars (char-based, not word-aware
/// — good enough for the narrow agent panel). Always yields at least one chunk.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let chars: Vec<char> = text.chars().collect();
    chars
        .chunks(width.max(1))
        .map(|c| c.iter().collect())
        .collect()
}

/// Map a tree-sitter highlight scope to its theme scope-name suffix.
fn scope_name(scope: onda_syntax::Scope) -> &'static str {
    use onda_syntax::Scope::*;
    match scope {
        Keyword => "keyword",
        Type => "type",
        Function => "function",
        Variable => "variable",
        String => "string",
        Number => "number",
        Comment => "comment",
        Operator => "operator",
        Punctuation => "punctuation",
        Attribute => "attribute",
        Constant => "constant",
        Error => "error",
    }
}

fn render_terminal_pane(grid: &mut onda_render::Grid, screen: &TerminalScreen, rect: &Rect) {
    use onda_render::{Attribute, Cell, Color, Style};

    for row in 0..rect.height {
        let screen_row = row.min(screen.rows().saturating_sub(1));
        let cells = screen.row(screen_row);
        for (col_idx, term_cell) in cells.iter().enumerate() {
            let grid_col = rect.x + col_idx as u16;
            if grid_col >= rect.x + rect.width {
                break;
            }
            let fg = term_cell
                .attrs
                .fg
                .map(|onda_terminal::screen::Rgb(r, g, b)| Color::Rgb(r, g, b))
                .unwrap_or(Color::Reset);
            let bg = term_cell
                .attrs
                .bg
                .map(|onda_terminal::screen::Rgb(r, g, b)| Color::Rgb(r, g, b))
                .unwrap_or(Color::Reset);
            let mut attrs = Attribute::empty();
            if term_cell.attrs.bold {
                attrs |= Attribute::BOLD;
            }
            if term_cell.attrs.italic {
                attrs |= Attribute::ITALIC;
            }
            if term_cell.attrs.underline {
                attrs |= Attribute::UNDERLINE;
            }
            if term_cell.attrs.reverse {
                attrs |= Attribute::REVERSE;
            }
            let style = Style { fg, bg, attrs };
            grid.set(
                grid_col,
                rect.y + row,
                Cell::new(term_cell.ch.to_string(), style),
            );
        }
    }
}

// ── Key → PTY bytes ───────────────────────────────────────────────────────────

fn key_to_pty_bytes(key: &Key) -> Vec<u8> {
    match key {
        Key::Char(c, m) if *m == KeyMod::NONE => c.to_string().into_bytes(),
        Key::Char(c, m) if m.contains(KeyMod::CTRL) => {
            // Ctrl+letter → ASCII control codes
            let b = *c as u8;
            if b.is_ascii_alphabetic() {
                vec![b.to_ascii_lowercase() - b'a' + 1]
            } else {
                c.to_string().into_bytes()
            }
        }
        Key::Enter => vec![b'\r'],
        Key::Backspace => vec![0x7f],
        Key::Delete => vec![0x1b, b'[', b'3', b'~'],
        Key::Esc => vec![0x1b],
        Key::Up => vec![0x1b, b'[', b'A'],
        Key::Down => vec![0x1b, b'[', b'B'],
        Key::Right => vec![0x1b, b'[', b'C'],
        Key::Left => vec![0x1b, b'[', b'D'],
        Key::Tab => vec![b'\t'],
        _ => vec![],
    }
}

// ── Completion kind icon ───────────────────────────────────────────────────────

fn completion_kind_icon(kind: Option<onda_lsp::CompletionItemKind>) -> &'static str {
    use onda_lsp::CompletionItemKind;
    match kind {
        Some(CompletionItemKind::FUNCTION) | Some(CompletionItemKind::METHOD) => "fn ",
        Some(CompletionItemKind::STRUCT) | Some(CompletionItemKind::CLASS) => "st ",
        Some(CompletionItemKind::VARIABLE) | Some(CompletionItemKind::FIELD) => "va ",
        Some(CompletionItemKind::MODULE) | Some(CompletionItemKind::UNIT) => "md ",
        Some(CompletionItemKind::KEYWORD) => "kw ",
        Some(CompletionItemKind::SNIPPET) => "sn ",
        _ => "   ",
    }
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("onda {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    if args.first().map(|s| s.as_str()) == Some("doctor") {
        std::process::exit(doctor::run());
    }

    if args.first().map(|s| s.as_str()) == Some("plugin") {
        std::process::exit(plugin_host::cli(&args[1..]));
    }

    if args.iter().any(|a| a == "--bench-startup") {
        run_bench_startup()?;
        return Ok(());
    }

    let paths: Vec<PathBuf> = args
        .iter()
        .filter(|a| !a.starts_with('-'))
        .map(PathBuf::from)
        .collect();

    run_editor(paths)
}

#[cfg(test)]
mod plugin_render_tests {
    use super::*;
    use onda_plugin::Style;

    fn doc_with(text: &str) -> Document {
        let mut d = Document::new_empty();
        let cs = onda_core::transaction::ChangeSetBuilder::new(0)
            .insert(text)
            .build();
        let _ = d.apply(&Transaction::new(cs));
        d
    }

    fn viewport(line_nr_width: u16) -> Viewport {
        let mut vp = Viewport::new();
        vp.line_nr_width = line_nr_width;
        vp.offset_line = 0;
        vp.offset_col = 0;
        vp
    }

    #[test]
    fn signs_paint_in_gutter_column() {
        let mut grid = onda_render::Grid::new(40, 10);
        let vp = viewport(4);
        let rect = Rect::new(0, 0, 40, 10);
        let signs = vec![(
            1usize,
            "▶".to_string(),
            Style {
                fg: Some("#ff0000".into()),
                ..Default::default()
            },
        )];
        draw_plugin_signs(&mut grid, &rect, &vp, &signs);
        assert_eq!(grid.get(0, 1).unwrap().grapheme, "▶");
        assert_eq!(
            grid.get(0, 1).unwrap().style.fg,
            onda_render::Color::Rgb(255, 0, 0)
        );
    }

    #[test]
    fn highlights_overlay_style_on_range() {
        let doc = doc_with("hello TODO world\n");
        let mut grid = onda_render::Grid::new(40, 10);
        let vp = viewport(0);
        let rect = Rect::new(0, 0, 40, 10);
        // "TODO" is chars 6..10.
        let hl = vec![(
            6usize,
            10usize,
            Style {
                fg: Some("#00ff00".into()),
                bold: true,
                ..Default::default()
            },
        )];
        draw_plugin_highlights(&mut grid, &rect, &vp, &doc, &hl);
        for col in 6..10u16 {
            assert_eq!(
                grid.get(col, 0).unwrap().style.fg,
                onda_render::Color::Rgb(0, 255, 0),
                "col {col} should be highlighted"
            );
        }
        // Outside the range is untouched.
        assert_ne!(
            grid.get(5, 0).unwrap().style.fg,
            onda_render::Color::Rgb(0, 255, 0)
        );
    }

    #[test]
    fn virt_text_renders_after_line_end() {
        let doc = doc_with("abc\n");
        let mut grid = onda_render::Grid::new(40, 10);
        let vp = viewport(0);
        let rect = Rect::new(0, 0, 40, 10);
        let v = vec![(0usize, "Z".to_string(), Style::default())];
        draw_plugin_virt_text(&mut grid, &rect, &vp, &doc, &v);
        // "abc" has len 3; one-space gap → virtual text starts at column 4.
        assert_eq!(grid.get(4, 0).unwrap().grapheme, "Z");
    }
}
