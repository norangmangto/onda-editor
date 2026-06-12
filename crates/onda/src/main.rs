use std::{path::PathBuf, sync::mpsc, time::Duration};

#[cfg(feature = "bench")]
use std::time::Instant;

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyEventKind};
use onda_config::Config;
use onda_core::{Document, DocumentId, Selection, Transaction, UndoHistory};
use onda_modal::{
    build_buffer_picker, build_file_picker, find_all, find_next, find_prev, Action, CommandLine,
    ExCommand, JumpList, Key, KeyMod, Keymap, KeymapState, MacroRecorder, MarkStore, Mode, Motion,
    Operator, PendingResult, Picker, Register, RegisterBank, SearchState,
};
use onda_render::{
    draw_borders, render_picker, Backend, Compositor, DocumentView, Layout, Message, MessageLine,
    ModeIndicator, NullBackend, Rect, RenderError, Statusline, TerminalBackend, Viewport, WindowId,
};
use onda_syntax::{LanguageRegistry, SyntaxWorker};
use tracing::debug;

// ── Background message channel ─────────────────────────────────────────────────

enum BgMessage {
    FileLoaded { doc: Document },
    FileError { path: PathBuf, error: String },
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
    goal_col: Option<usize>,
    compositor: Compositor,
    backend: B,
    running: bool,
    command_line: CommandLine,
    bg_rx: mpsc::Receiver<BgMessage>,

    // ── Config ────────────────────────────────────────────────────────────────
    #[allow(dead_code)]
    config: Config,

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
            Mode::VisualBlock => ModeIndicator::Visual, // render same as Visual for now
            Mode::Command => ModeIndicator::Command,
        }
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
                let doc_idx = self.windows[win_idx].doc_idx;
                if doc_idx >= self.docs.len() {
                    continue;
                }
                let doc = &self.docs[doc_idx];
                let sel = &self.windows[win_idx].selection;
                let viewport = &self.windows[win_idx].viewport;
                let is_focused = *win_id == focused_win_id;
                let matches: &[onda_core::Range] = if is_focused { &search_matches } else { &[] };

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
                self.handle_key(key)?;

                // Clear info messages on any keypress in normal mode
                if self.mode == Mode::Normal && matches!(self.message, Message::Info(_)) {
                    self.message = Message::None;
                }
            }
            Event::Resize(w, h) => {
                self.compositor.resize(w, h);
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
            ExCommand::Set(_key, _value) => {
                // TODO T5.1: apply config settings at runtime
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
                // TODO T6.3: macro replay — get keys and replay via handle_key loop
                // Stubbed: just show a message to avoid complexity
                let _keys: Option<Vec<Key>> = self.macros.get_macro(c).map(|s| s.to_vec());
                // TODO: actually replay keys
                debug!("PlayMacro({c}) — stub, not yet replaying");
            }
            Action::PlayLastMacro => {
                // TODO T6.3: macro replay — stub
                debug!("PlayLastMacro — stub, not yet replaying");
            }
            Action::DotRepeat => {
                // TODO T6.3: dot-repeat — stub
                debug!("DotRepeat — stub, not yet replaying");
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
                // Close all but the focused window — TODO T6.6: full impl
                // For now just keep the current layout
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
        }
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
            }
        }
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
            self.render_frame().context("render frame")?;
        }

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
        goal_col: None,
        compositor,
        backend,
        running,
        command_line: CommandLine::new(),
        bg_rx,
        config,
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
    let (_, bg_rx) = mpsc::sync_channel(16);
    let backend = NullBackend::new(120, 40);
    let (width, height) = backend.size();
    let compositor = Compositor::new(width, height);

    let mut doc = Document::new_empty();
    let cs = onda_core::transaction::ChangeSetBuilder::new(0)
        .insert("Hello, onda!\n")
        .build();
    doc.apply(&Transaction::new(cs)).unwrap();

    let mut app = make_app(doc, backend, compositor, bg_rx, Config::default(), false);

    app.compositor.buf.invalidate();
    app.render_frame()?;
    Ok(())
}

fn run_editor(paths: Vec<PathBuf>) -> Result<()> {
    init_tracing();

    let config_result = Config::load();
    let config = config_result.config;

    let (bg_tx, bg_rx) = mpsc::sync_channel(16);

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

    let (width, height) = term.size();
    let compositor = Compositor::new(width, height);

    let mut app = make_app(initial_doc, term, compositor, bg_rx, config, true);

    // Show config warning if any
    if let Some(warn) = config_result.warning {
        app.message = Message::Error(warn);
    }

    // Try to start syntax worker for the initial doc (requires tokio runtime).
    // The runtime is only available when started via tokio main; for Phase 1 we
    // run in a synchronous context, so skip if it would panic.
    // TODO T6.4: start a tokio runtime and spawn syntax workers.

    let result = app.run();
    app.backend.leave()?;
    result
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
