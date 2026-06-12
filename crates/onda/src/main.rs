use std::{
    path::PathBuf,
    sync::mpsc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyEventKind};
use onda_config::Config;
use onda_core::{Document, Selection, Transaction, UndoHistory};
use onda_modal::{
    Action, CommandLine, ExCommand, Key, Keymap, KeymapState, Mode, Motion, Operator, PendingResult,
};
use onda_render::{
    Backend, Compositor, DocumentView, MessageLine, Message, ModeIndicator, NullBackend,
    RenderError, Statusline, TerminalBackend, Viewport,
};
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
        println!("Latency p50={p50}µs p95={p95}µs p99={p99}µs (n={})", s.len());
    }
}

// ── App ────────────────────────────────────────────────────────────────────────

struct App<B: Backend> {
    docs: Vec<Document>,
    current: usize,
    selection: Selection,
    mode: Mode,
    viewport: Viewport,
    undo: UndoHistory,
    keymap: Keymap,
    keymap_state: KeymapState,
    register: Option<onda_modal::operator::Register>,
    message: Message,
    goal_col: Option<usize>,
    compositor: Compositor,
    backend: B,
    running: bool,
    command_line: CommandLine,
    bg_rx: mpsc::Receiver<BgMessage>,
    config: Config,
    #[cfg(feature = "bench")]
    tracer: LatencyTracer,
}

impl<B: Backend> App<B> {
    fn doc(&self) -> &Document {
        &self.docs[self.current]
    }

    fn doc_mut(&mut self) -> &mut Document {
        &mut self.docs[self.current]
    }

    fn mode_indicator(&self) -> ModeIndicator {
        match self.mode {
            Mode::Normal => ModeIndicator::Normal,
            Mode::Insert => ModeIndicator::Insert,
            Mode::Visual => ModeIndicator::Visual,
            Mode::VisualLine => ModeIndicator::VisualLine,
            Mode::Command => ModeIndicator::Command,
        }
    }

    fn render_frame(&mut self) -> Result<(), RenderError> {
        let (width, height) = self.backend.size();
        if width == 0 || height == 0 {
            return Ok(());
        }

        let doc_height = height.saturating_sub(2); // statusline + messageline
        let status_row = height.saturating_sub(2);
        let msg_row = height.saturating_sub(1);

        let mode_ind = self.mode_indicator();
        let doc = &self.docs[self.current];
        let cursor_line = doc.char_to_line(self.selection.primary().head);
        self.viewport.scroll_to(cursor_line, doc_height as usize);

        let grid = self.compositor.buf.current_mut();

        // Document view
        DocumentView::render(grid, doc, &self.selection, &self.viewport, mode_ind, 0, doc_height);

        // Statusline
        Statusline::render(grid, status_row, mode_ind, doc, &self.selection);

        // Message / command line
        let msg = if self.mode == Mode::Command {
            Message::Command(self.command_line.as_str().to_string())
        } else {
            self.message.clone()
        };
        MessageLine::render(grid, msg_row, &msg);

        // Cursor position
        let (cursor_col, cursor_row) = self.cursor_screen_pos();
        self.compositor.cursor_col = cursor_col;
        self.compositor.cursor_row = cursor_row;

        #[cfg(feature = "debug-overlay")]
        self.compositor.render_debug_overlay();

        self.compositor.flush(&mut self.backend, mode_ind)?;

        #[cfg(feature = "bench")]
        self.tracer.mark_frame();

        Ok(())
    }

    fn cursor_screen_pos(&self) -> (u16, u16) {
        let doc = self.doc();
        let head = self.selection.primary().head;
        let (line, col) = doc.char_to_visual_pos(head);

        if self.mode == Mode::Command {
            let (width, height) = self.backend.size();
            let cmd_col = (self.command_line.as_str().len() + 1) as u16; // +1 for ':'
            return (cmd_col.min(width - 1), height.saturating_sub(1));
        }

        let screen_row = line.saturating_sub(self.viewport.offset_line) as u16;
        let screen_col = (col.saturating_sub(self.viewport.offset_col) as u16)
            .saturating_add(self.viewport.line_nr_width);
        (screen_col, screen_row)
    }

    fn handle_event(&mut self, event: Event) -> Result<()> {
        match event {
            Event::Key(ev) if ev.kind == KeyEventKind::Press => {
                #[cfg(feature = "bench")]
                self.tracer.mark_key();

                let key = Key::from_event(&ev);
                self.handle_key(key)?;

                // Clear info messages on any keypress in normal mode
                if self.mode == Mode::Normal {
                    if matches!(self.message, Message::Info(_)) {
                        self.message = Message::None;
                    }
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
        match self.mode {
            Mode::Insert => self.handle_insert_key(key),
            Mode::Command => self.handle_command_key(key),
            Mode::Normal | Mode::Visual | Mode::VisualLine => self.handle_normal_key(key),
        }
    }

    fn handle_insert_key(&mut self, key: Key) -> Result<()> {
        match &key {
            Key::Esc => {
                // Move cursor left by one when leaving insert mode (vim behaviour)
                let doc = self.doc();
                let pos = self.selection.primary().head;
                let line = doc.char_to_line(pos);
                let line_start = doc.line_to_char(line);
                if pos > line_start {
                    self.selection = Selection::point(pos - 1);
                }
                self.undo.end_group();
                self.mode = Mode::Normal;
                self.keymap_state.reset();
            }
            Key::Enter => {
                self.apply_insert(|doc, sel| {
                    onda_modal::operator::insert_char(doc, sel, '\n')
                })?;
                // Move cursor to new line
                let new_pos = self.selection.primary().head + 1;
                self.selection = Selection::point(new_pos.min(self.doc().len_chars()));
            }
            Key::Backspace => {
                let before = self.selection.clone();
                let tx = onda_modal::operator::delete_before_cursor(self.doc(), &self.selection);
                if !tx.changes.is_empty() {
                    let sel_before = self.selection.clone();
                    let inv = self.doc_mut().apply(&tx)?;
                    let new_pos = self.selection.primary().head.saturating_sub(1);
                    self.selection = Selection::point(new_pos);
                    self.undo.push(tx, inv, sel_before, self.selection.clone());
                    self.undo.begin_group();
                }
            }
            Key::Delete => {
                let tx = onda_modal::operator::delete_char_at_cursor(self.doc(), &self.selection);
                if !tx.changes.is_empty() {
                    let sel_before = self.selection.clone();
                    let inv = self.doc_mut().apply(&tx)?;
                    self.undo.push(tx, inv, sel_before, self.selection.clone());
                    self.undo.begin_group();
                }
            }
            Key::Char(c, _) => {
                let ch = *c;
                let sel_before = self.selection.clone();
                let tx = onda_modal::operator::insert_char(self.doc(), &self.selection, ch);
                let inv = self.doc_mut().apply(&tx)?;
                let new_pos = self.selection.primary().head + 1;
                self.selection = Selection::point(new_pos.min(self.doc().len_chars()));
                self.undo.push(tx, inv, sel_before, self.selection.clone());
                self.undo.begin_group();
            }
            _ => {}
        }
        Ok(())
    }

    fn apply_insert<F>(&mut self, build: F) -> Result<()>
    where
        F: FnOnce(&Document, &Selection) -> Transaction,
    {
        let tx = build(self.doc(), &self.selection);
        if !tx.changes.is_empty() {
            let sel_before = self.selection.clone();
            let inv = self.doc_mut().apply(&tx)?;
            self.selection = self.selection.map(&tx.changes);
            self.undo.push(tx, inv, sel_before, self.selection.clone());
        }
        Ok(())
    }

    fn handle_command_key(&mut self, key: Key) -> Result<()> {
        match &key {
            Key::Esc => {
                self.command_line.clear();
                self.mode = Mode::Normal;
                self.message = Message::None;
            }
            Key::Enter => {
                match self.command_line.submit() {
                    Ok(cmd) => self.execute_ex_command(cmd)?,
                    Err(e) => {
                        self.message = Message::Error(format!("E: {e}"));
                        self.mode = Mode::Normal;
                    }
                }
            }
            Key::Backspace => {
                if self.command_line.as_str().is_empty() {
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
                        self.message =
                            Message::Info(format!("\"{}\" written", self.doc().name()));
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
            ExCommand::WriteQuit => {
                match self.doc().save() {
                    Ok(()) => {
                        self.doc_mut().mark_saved();
                        self.running = false;
                    }
                    Err(e) => {
                        self.message = Message::Error(format!("E: {e}"));
                    }
                }
            }
            ExCommand::Edit(path) => {
                match Document::open(&path) {
                    Ok(doc) => {
                        self.docs.push(doc);
                        self.current = self.docs.len() - 1;
                        self.selection = Selection::point(0);
                        self.viewport = Viewport::new();
                    }
                    Err(e) => {
                        self.message = Message::Error(format!("E: {e}"));
                    }
                }
            }
            ExCommand::NextBuffer => {
                if self.docs.len() > 1 {
                    self.current = (self.current + 1) % self.docs.len();
                    self.selection = Selection::point(0);
                }
            }
            ExCommand::PrevBuffer => {
                if self.docs.len() > 1 {
                    self.current =
                        (self.current + self.docs.len() - 1) % self.docs.len();
                    self.selection = Selection::point(0);
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
    fn execute_action(&mut self, action: Action, count: usize, viewport_height: usize) -> Result<()> {
        match action {
            Action::EnterInsert => {
                self.mode = Mode::Insert;
                self.undo.begin_group();
            }
            Action::EnterInsertLineStart => {
                let doc = self.doc();
                let line = doc.char_to_line(self.selection.primary().head);
                let line_start = doc.line_to_char(line);
                self.selection = Selection::point(line_start);
                self.mode = Mode::Insert;
                self.undo.begin_group();
            }
            Action::EnterInsertAfter => {
                let pos = self.selection.primary().head;
                let doc = self.doc();
                let len = doc.len_chars();
                let line = doc.char_to_line(pos);
                let line_end = {
                    let line_len = doc.line_len_no_eol(line);
                    doc.line_to_char(line) + line_len
                };
                self.selection = Selection::point((pos + 1).min(line_end).min(len));
                self.mode = Mode::Insert;
                self.undo.begin_group();
            }
            Action::EnterInsertLineEnd => {
                let doc = self.doc();
                let line = doc.char_to_line(self.selection.primary().head);
                let line_end = doc.line_to_char(line) + doc.line_len_no_eol(line);
                self.selection = Selection::point(line_end.min(doc.len_chars()));
                self.mode = Mode::Insert;
                self.undo.begin_group();
            }
            Action::EnterInsertNewLineBelow => {
                let (tx, new_sel) =
                    onda_modal::operator::open_line(self.doc(), &self.selection, false);
                let sel_before = self.selection.clone();
                let inv = self.doc_mut().apply(&tx)?;
                self.selection = new_sel;
                self.undo.push(tx, inv, sel_before, self.selection.clone());
                self.mode = Mode::Insert;
                self.undo.begin_group();
            }
            Action::EnterInsertNewLineAbove => {
                let (tx, new_sel) =
                    onda_modal::operator::open_line(self.doc(), &self.selection, true);
                let sel_before = self.selection.clone();
                let inv = self.doc_mut().apply(&tx)?;
                self.selection = new_sel;
                self.undo.push(tx, inv, sel_before, self.selection.clone());
                self.mode = Mode::Insert;
                self.undo.begin_group();
            }
            Action::EnterNormal => {
                if self.mode == Mode::Insert {
                    self.undo.end_group();
                }
                self.mode = Mode::Normal;
                self.selection = self.selection.collapse_to_head();
            }
            Action::EnterVisual => {
                self.mode = Mode::Visual;
            }
            Action::EnterVisualLine => {
                self.mode = Mode::VisualLine;
            }
            Action::EnterCommand => {
                self.mode = Mode::Command;
                self.command_line.clear();
            }
            Action::Move(motion) => {
                let rope = self.doc().rope().clone();
                let (new_sel, new_goal) = motion.apply_to_selection(
                    &rope,
                    &self.selection,
                    count,
                    self.goal_col,
                    viewport_height,
                );
                // In visual mode: extend selection
                if self.mode.is_visual() {
                    let primary = self.selection.primary();
                    let new_head = new_sel.primary().head;
                    self.selection = Selection::new(
                        vec![onda_core::Range::new(primary.anchor, new_head)],
                        0,
                    );
                } else {
                    self.selection = new_sel;
                }
                self.goal_col = new_goal;
            }
            Action::ApplyOperatorMotion(op, motion) => {
                // Apply motion to get the range, then apply operator to that range
                let rope = self.doc().rope().clone();
                let (motion_sel, _) = motion.apply_to_selection(
                    &rope,
                    &self.selection,
                    count,
                    self.goal_col,
                    viewport_height,
                );
                let primary = self.selection.primary();
                let motion_head = motion_sel.primary().head;
                let op_range = onda_core::Range::new(
                    primary.head.min(motion_head),
                    primary.head.max(motion_head),
                );
                let op_sel = Selection::new(vec![op_range], 0);
                self.apply_operator(op, &op_sel, false)?;
            }
            Action::OperatorLine(op) => {
                let sel = self.selection.clone();
                self.apply_operator(op, &sel, true)?;
            }
            Action::OperatorSelection(op) => {
                let sel = self.selection.clone();
                self.apply_operator(op, &sel, false)?;
                self.mode = Mode::Normal;
            }
            Action::DeleteChar => {
                let tx =
                    onda_modal::operator::delete_char_at_cursor(self.doc(), &self.selection);
                if !tx.changes.is_empty() {
                    let sel_before = self.selection.clone();
                    let inv = self.doc_mut().apply(&tx)?;
                    self.selection = self.selection.map(&tx.changes);
                    // Clamp to valid position
                    let len = self.doc().len_chars();
                    let head = self.selection.primary().head.min(len.saturating_sub(1));
                    self.selection = Selection::point(head);
                    self.undo.push(tx, inv, sel_before, self.selection.clone());
                }
            }
            Action::ReplaceChar(c) => {
                let tx = onda_modal::operator::replace_char(self.doc(), &self.selection, c);
                if !tx.changes.is_empty() {
                    let sel_before = self.selection.clone();
                    let inv = self.doc_mut().apply(&tx)?;
                    self.undo.push(tx, inv, sel_before, self.selection.clone());
                }
            }
            Action::ChangeToEnd => {
                let rope = self.doc().rope().clone();
                let (end_sel, _) = Motion::LineEnd.apply_to_selection(
                    &rope,
                    &self.selection,
                    1,
                    None,
                    viewport_height,
                );
                let primary = self.selection.primary();
                let end = end_sel.primary().head;
                let range = onda_core::Range::new(primary.head, end);
                let del_sel = Selection::new(vec![range], 0);
                self.apply_operator(Operator::Change, &del_sel, false)?;
            }
            Action::DeleteToEnd => {
                let rope = self.doc().rope().clone();
                let (end_sel, _) = Motion::LineEnd.apply_to_selection(
                    &rope,
                    &self.selection,
                    1,
                    None,
                    viewport_height,
                );
                let primary = self.selection.primary();
                let end = end_sel.primary().head;
                let range = onda_core::Range::new(primary.head, end);
                let del_sel = Selection::new(vec![range], 0);
                self.apply_operator(Operator::Delete, &del_sel, false)?;
            }
            Action::PasteAfter => {
                if let Some(ref reg) = self.register.clone() {
                    let tx = onda_modal::operator::paste_after(self.doc(), &self.selection, reg);
                    if !tx.changes.is_empty() {
                        let sel_before = self.selection.clone();
                        let inv = self.doc_mut().apply(&tx)?;
                        self.selection = self.selection.map(&tx.changes);
                        self.undo.push(tx, inv, sel_before, self.selection.clone());
                    }
                }
            }
            Action::PasteBefore => {
                if let Some(ref reg) = self.register.clone() {
                    let tx =
                        onda_modal::operator::paste_before(self.doc(), &self.selection, reg);
                    if !tx.changes.is_empty() {
                        let sel_before = self.selection.clone();
                        let inv = self.doc_mut().apply(&tx)?;
                        self.selection = self.selection.map(&tx.changes);
                        self.undo.push(tx, inv, sel_before, self.selection.clone());
                    }
                }
            }
            Action::JoinLine => {
                let tx = onda_modal::operator::join_line(self.doc(), &self.selection);
                if !tx.changes.is_empty() {
                    let sel_before = self.selection.clone();
                    let inv = self.doc_mut().apply(&tx)?;
                    self.undo.push(tx, inv, sel_before, self.selection.clone());
                }
            }
            Action::Undo => {
                for _ in 0..count {
                    let doc = &mut self.docs[self.current];
                    match self.undo.undo(doc) {
                        Ok(sel) => {
                            self.selection = sel;
                        }
                        Err(_) => {
                            self.message = Message::Info("Already at oldest change".to_string());
                            break;
                        }
                    }
                }
            }
            Action::Redo => {
                for _ in 0..count {
                    let doc = &mut self.docs[self.current];
                    match self.undo.redo(doc) {
                        Ok(sel) => {
                            self.selection = sel;
                        }
                        Err(_) => {
                            self.message = Message::Info("Already at newest change".to_string());
                            break;
                        }
                    }
                }
            }
            Action::SwapAnchorHead => {
                self.selection = self.selection.transform(|r| r.flip());
            }
            // Ex commands dispatched through command mode, not here
            Action::WriteFile
            | Action::Quit
            | Action::QuitForce
            | Action::WriteQuit
            | Action::EditFile(_)
            | Action::NextBuffer
            | Action::PrevBuffer => {}
            Action::PendingOperator(_) => {} // handled by KeymapState
        }

        // Reset goal_col unless motion set it
        if !matches!(&action, Action::Move(m)
            if matches!(m, Motion::Up | Motion::Down | Motion::HalfPageDown | Motion::HalfPageUp))
        {
            // Only non-vertical motions reset the goal column
            match &action {
                Action::Move(Motion::Up)
                | Action::Move(Motion::Down)
                | Action::Move(Motion::HalfPageDown)
                | Action::Move(Motion::HalfPageUp) => {}
                _ => self.goal_col = None,
            }
        }

        Ok(())
    }

    fn apply_operator(
        &mut self,
        op: Operator,
        sel: &Selection,
        linewise: bool,
    ) -> Result<()> {
        let (tx, reg) = if linewise {
            onda_modal::operator::delete_lines(self.doc(), sel)
        } else {
            onda_modal::operator::delete(self.doc(), sel)
        };

        match op {
            Operator::Yank => {
                self.register = Some(reg);
                // Yank doesn't modify the document
                return Ok(());
            }
            Operator::Delete | Operator::Change => {
                self.register = Some(reg);
                if !tx.changes.is_empty() {
                    let sel_before = self.selection.clone();
                    let inv = self.doc_mut().apply(&tx)?;
                    // Move cursor to deletion point
                    let new_pos =
                        tx.changes.map_pos(sel.primary().from(), onda_core::Assoc::After);
                    let new_pos = new_pos.min(self.doc().len_chars().saturating_sub(1));
                    self.selection = Selection::point(new_pos);
                    self.undo.push(tx, inv, sel_before, self.selection.clone());
                }
                if op == Operator::Change {
                    self.mode = Mode::Insert;
                    self.undo.begin_group();
                }
            }
        }
        Ok(())
    }

    fn drain_bg_channel(&mut self) {
        while let Ok(msg) = self.bg_rx.try_recv() {
            match msg {
                BgMessage::FileLoaded { doc } => {
                    let name = doc.name().to_string();
                    self.docs.push(doc);
                    self.current = self.docs.len() - 1;
                    self.selection = Selection::point(0);
                    self.viewport = Viewport::new();
                    self.message = Message::Info(format!("Loaded: {name}"));
                }
                BgMessage::FileError { path, error } => {
                    self.message =
                        Message::Error(format!("{}: {error}", path.display()));
                }
            }
        }
    }

    fn run(&mut self) -> Result<()> {
        // Initial render
        self.compositor.buf.invalidate();
        self.render_frame().context("initial render")?;

        while self.running {
            // Poll for input with a short timeout to also drain background tasks
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
    std::thread::spawn(move || {
        match Document::open(&path) {
            Ok(doc) => {
                let _ = tx.send(BgMessage::FileLoaded { doc });
            }
            Err(e) => {
                let _ = tx.send(BgMessage::FileError { path, error: e.to_string() });
            }
        }
    });
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
    let mut backend = NullBackend::new(120, 40);
    let (width, height) = backend.size();
    let compositor = Compositor::new(width, height);

    let mut doc = Document::new_empty();
    let cs = onda_core::transaction::ChangeSetBuilder::new(0)
        .insert("Hello, onda!\n")
        .build();
    doc.apply(&Transaction::new(cs)).unwrap();

    let mut app = App {
        docs: vec![doc],
        current: 0,
        selection: Selection::point(0),
        mode: Mode::Normal,
        viewport: Viewport::new(),
        undo: UndoHistory::new(),
        keymap: Keymap::new(),
        keymap_state: KeymapState::new(),
        register: None,
        message: Message::None,
        goal_col: None,
        compositor,
        backend,
        running: false,
        command_line: CommandLine::new(),
        bg_rx,
        config: Config::default(),
        #[cfg(feature = "bench")]
        tracer: LatencyTracer::default(),
    };

    app.compositor.buf.invalidate();
    app.render_frame()?;
    Ok(())
}

fn run_editor(paths: Vec<PathBuf>) -> Result<()> {
    init_tracing();

    let (bg_tx, bg_rx) = mpsc::sync_channel(16);

    // Open first doc synchronously (so the editor is ready immediately)
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

    let mut app = App {
        docs: vec![initial_doc],
        current: 0,
        selection: Selection::point(0),
        mode: Mode::Normal,
        viewport: Viewport::new(),
        undo: UndoHistory::new(),
        keymap: Keymap::new(),
        keymap_state: KeymapState::new(),
        register: None,
        message: Message::None,
        goal_col: None,
        compositor,
        backend: term,
        running: true,
        command_line: CommandLine::new(),
        bg_rx,
        config: Config::default(),
        #[cfg(feature = "bench")]
        tracer: LatencyTracer::default(),
    };

    let result = app.run();
    app.backend.leave()?;
    result
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // --version
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("onda {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    // --bench-startup: init, render one frame to null backend, exit
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
