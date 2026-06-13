use std::{collections::HashMap, path::PathBuf, sync::mpsc, time::Duration};

#[cfg(feature = "bench")]
use std::time::Instant;

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
use onda_lua::{LuaApiCall, LuaRuntime, PluginLoader};
use onda_modal::{
    build_buffer_picker, build_file_picker, find_all, find_next, find_prev, Action, CommandLine,
    ExCommand, JumpList, Key, KeyMod, Keymap, KeymapState, MacroRecorder, MarkStore, Mode, Motion,
    Operator, PendingResult, Picker, Register, RegisterBank, SearchState,
};
use onda_render::{
    draw_borders, render_completion_menu, render_float, render_picker, Backend, Compositor,
    DiagnosticSpan, DocumentView, Layout, Message, MessageLine, ModeIndicator, NullBackend, Rect,
    RenderError, Statusline, TerminalBackend, Viewport, WindowId,
};
use onda_session::{Session, SessionManager};
use onda_syntax::{LanguageRegistry, SyntaxWorker};
use onda_terminal::{PtyEvent, PtyProcess, TerminalScreen};
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
    bg_tx: mpsc::SyncSender<BgMessage>,
    bg_rx: mpsc::Receiver<BgMessage>,

    // ── Config ────────────────────────────────────────────────────────────────
    #[allow(dead_code)]
    config: Config,

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

    // ── Lua plugins ────────────────────────────────────────────────────────────
    /// Lua runtime (None when in bench / non-tokio mode).
    lua_runtime: Option<LuaRuntime>,
    /// Custom Lua commands registered via `onda.cmd.create`.
    lua_commands: HashMap<String, u64>,

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

        // ── Phase 1: update viewports (no grid access yet) ────────────────────
        let content_area = Rect::new(0, 0, width, content_height);
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

        // ── Phase 3: render into grid ─────────────────────────────────────────
        {
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

                if diag_spans.is_empty() {
                    DocumentView::render_with_highlights(
                        grid,
                        doc,
                        sel,
                        viewport,
                        mode_ind,
                        rect.y,
                        rect.height,
                        None,
                        matches,
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
                        matches,
                        diag_spans,
                    );
                }
            }

            // Statusline
            {
                let focused_doc_idx = self.windows[self.focused_window].doc_idx;
                let doc = &self.docs[focused_doc_idx];
                let sel = &self.windows[self.focused_window].selection;
                Statusline::render(grid, status_row, mode_ind, doc, sel, macro_recording);
            }

            // Message line
            MessageLine::render(grid, msg_row, &msg);

            // Picker overlay
            if let Some((title, query, items, pw, ph)) = picker_data {
                let items_ref: Vec<(&str, bool)> =
                    items.iter().map(|(s, b)| (s.as_str(), *b)).collect();
                render_picker(grid, &title, &query, &items_ref, pw, ph);
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
                );
            }

            // Completion menu
            if let Some(ref comp) = self.completion {
                let items_ref: Vec<(&str, &str)> = comp
                    .items
                    .iter()
                    .map(|(l, k)| (l.as_str(), k.as_str()))
                    .collect();
                render_completion_menu(grid, &items_ref, comp.selected, cursor_col, cursor_row, 10);
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
        match &key {
            Key::Esc => {
                self.command_line.clear();
                self.search_input_dir = None;
                self.mode = Mode::Normal;
                self.message = Message::None;
            }
            Key::Enter => {
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
                        Err(e) => {
                            self.message = Message::Error(format!("E: {e}"));
                            self.mode = Mode::Normal;
                        }
                    }
                }
            }
            Key::Backspace => {
                if self.command_line.as_str().is_empty() {
                    self.search_input_dir = None;
                    self.mode = Mode::Normal;
                    self.message = Message::None;
                } else {
                    self.command_line.backspace();
                }
            }
            Key::Char(c, _) => {
                self.command_line.push_char(*c);
            }
            _ => {}
        }
        Ok(())
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
            ExCommand::LuaCommand(name, args) => {
                if let Some(runtime) = &self.lua_runtime {
                    let args_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                    runtime.fire_command(&name, &args_refs);
                }
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

    // ── Lua API call drain ────────────────────────────────────────────────────

    fn drain_lua_calls(&mut self) {
        let calls = if let Some(rt) = &self.lua_runtime {
            rt.drain_calls()
        } else {
            return;
        };

        for call in calls {
            match call {
                LuaApiCall::Notify { msg, level } => {
                    self.message_history.push(msg.clone());
                    self.message = match level {
                        onda_lua::api::NotifyLevel::Error => Message::Error(msg),
                        _ => Message::Info(msg),
                    };
                }
                LuaApiCall::BufSetLines {
                    buf_id,
                    start,
                    end,
                    lines,
                } => {
                    if buf_id < self.docs.len() {
                        let doc = &self.docs[buf_id];
                        let line_start = if start < doc.len_lines() {
                            doc.line_to_char(start)
                        } else {
                            doc.len_chars()
                        };
                        let line_end = if end < doc.len_lines() {
                            doc.line_to_char(end)
                        } else {
                            doc.len_chars()
                        };
                        let new_text = lines.join("\n");
                        let len = doc.len_chars();
                        let cs = onda_core::transaction::ChangeSetBuilder::new(len)
                            .retain(line_start)
                            .delete(line_end - line_start)
                            .insert(&new_text)
                            .build();
                        let tx = Transaction::new(cs);
                        let _ = self.docs[buf_id].apply(&tx);
                    }
                }
                LuaApiCall::WinSetCursor { win_id, row, col } => {
                    if win_id < self.windows.len() {
                        let doc_idx = self.windows[win_id].doc_idx;
                        if doc_idx < self.docs.len() {
                            let doc = &self.docs[doc_idx];
                            let line = row.min(doc.len_lines().saturating_sub(1));
                            let line_start = doc.line_to_char(line);
                            let line_len = doc.line_len_no_eol(line);
                            let char_pos = line_start + col.min(line_len);
                            self.windows[win_id].selection = Selection::point(char_pos);
                        }
                    }
                }
                LuaApiCall::CmdCreate {
                    name, callback_id, ..
                } => {
                    self.lua_commands.insert(name, callback_id);
                }
                LuaApiCall::UiFloat {
                    title: _title,
                    lines,
                    width: _width,
                    height: _height,
                } => {
                    let lines_str: Vec<String> = lines;
                    self.hover_float = Some(HoverFloat {
                        lines: lines_str,
                        col: 4,
                        row: 4,
                    });
                }
                _ => {}
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
            if event::poll(Duration::from_millis(8))? {
                let ev = event::read()?;
                self.handle_event(ev)?;
            }

            self.drain_bg_channel();
            self.drain_lua_calls();
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
        bg_tx,
        bg_rx,
        config,
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
        lua_runtime: None,
        lua_commands: HashMap::new(),
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

    // Show config warning if any
    if let Some(warn) = config_result.warning {
        app.message = Message::Error(warn);
    }

    // Initialize Lua runtime and load plugins (T13.1)
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    match LuaRuntime::new() {
        Ok(runtime) => {
            let errors = PluginLoader::load_all(&runtime, &cwd);
            for (name, err) in errors {
                app.message_history
                    .push(format!("Plugin '{name}' error: {err}"));
            }
            app.lua_runtime = Some(runtime);
        }
        Err(e) => {
            app.message_history
                .push(format!("Lua runtime init error: {e}"));
        }
    }

    let result = app.run();

    // Disable mouse capture on exit
    let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture);
    app.backend.leave()?;
    result
}

// ── Terminal rendering helper ──────────────────────────────────────────────────

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
