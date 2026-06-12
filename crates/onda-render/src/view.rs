use onda_core::{Document, Range, Selection};
use unicode_width::UnicodeWidthStr;

use crate::{
    backend::{Backend, CursorShape, RenderError},
    grid::{Attribute, Cell, Color, DoubleBuffer, Grid, Style},
};

// ── Palette ────────────────────────────────────────────────────────────────────

mod palette {
    use super::{Attribute, Color, Style};

    pub const TEXT: Style = Style { fg: Color::Reset, bg: Color::Reset, attrs: Attribute::empty() };
    pub const CURSOR_NORMAL: Style = Style {
        fg: Color::Black,
        bg: Color::White,
        attrs: Attribute::empty(),
    };
    pub const CURSOR_INSERT: Style = Style {
        fg: Color::Black,
        bg: Color::LightCyan,
        attrs: Attribute::empty(),
    };
    pub const SELECTION: Style = Style {
        fg: Color::Black,
        bg: Color::LightBlue,
        attrs: Attribute::empty(),
    };
    pub const LINE_NR: Style = Style { fg: Color::DarkGray, bg: Color::Reset, attrs: Attribute::empty() };
    pub const LINE_NR_CURRENT: Style = Style {
        fg: Color::Yellow,
        bg: Color::Reset,
        attrs: Attribute::empty(),
    };
    pub const STATUS_NORMAL: Style =
        Style { fg: Color::Black, bg: Color::Green, attrs: Attribute::empty() };
    pub const STATUS_INSERT: Style =
        Style { fg: Color::Black, bg: Color::LightCyan, attrs: Attribute::empty() };
    pub const STATUS_VISUAL: Style =
        Style { fg: Color::Black, bg: Color::Yellow, attrs: Attribute::empty() };
    pub const STATUS_BG: Style =
        Style { fg: Color::White, bg: Color::DarkGray, attrs: Attribute::empty() };
    pub const MSG_ERROR: Style = Style {
        fg: Color::LightRed,
        bg: Color::Reset,
        attrs: Attribute::empty(),
    };
    pub const MSG_INFO: Style =
        Style { fg: Color::Reset, bg: Color::Reset, attrs: Attribute::empty() };
    pub const DEBUG_OVERLAY: Style =
        Style { fg: Color::LightYellow, bg: Color::Black, attrs: Attribute::empty() };
}

/// The mode label shown in the statusline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeIndicator {
    Normal,
    Insert,
    Visual,
    VisualLine,
    Command,
}

impl ModeIndicator {
    pub fn label(self) -> &'static str {
        match self {
            ModeIndicator::Normal => "NORMAL",
            ModeIndicator::Insert => "INSERT",
            ModeIndicator::Visual => "VISUAL",
            ModeIndicator::VisualLine => "VISUAL LINE",
            ModeIndicator::Command => "COMMAND",
        }
    }

    fn style(self) -> Style {
        match self {
            ModeIndicator::Normal => palette::STATUS_NORMAL,
            ModeIndicator::Insert => palette::STATUS_INSERT,
            ModeIndicator::Visual | ModeIndicator::VisualLine => palette::STATUS_VISUAL,
            ModeIndicator::Command => palette::STATUS_NORMAL,
        }
    }

    pub fn cursor_shape(self) -> CursorShape {
        match self {
            ModeIndicator::Insert => CursorShape::Bar,
            _ => CursorShape::Block,
        }
    }
}

// ── Viewport ──────────────────────────────────────────────────────────────────

/// Manages the visible portion of the document.
#[derive(Debug, Clone)]
pub struct Viewport {
    /// First visible line (0-indexed).
    pub offset_line: usize,
    /// First visible column character offset (for horizontal scroll).
    pub offset_col: usize,
    /// Number of lines to keep visible above/below the cursor.
    pub scrolloff: usize,
    /// Width of the line-number column (0 = disabled).
    pub line_nr_width: u16,
}

impl Viewport {
    pub fn new() -> Self {
        Self { offset_line: 0, offset_col: 0, scrolloff: 5, line_nr_width: 4 }
    }

    /// Scroll the viewport so that `cursor_line` is visible.
    pub fn scroll_to(&mut self, cursor_line: usize, viewport_height: usize) {
        let effective_height = viewport_height.saturating_sub(self.scrolloff * 2);
        if effective_height == 0 {
            self.offset_line = cursor_line;
            return;
        }

        if cursor_line < self.offset_line + self.scrolloff {
            self.offset_line = cursor_line.saturating_sub(self.scrolloff);
        } else if cursor_line >= self.offset_line + viewport_height.saturating_sub(self.scrolloff)
        {
            self.offset_line =
                cursor_line + self.scrolloff + 1 - viewport_height.min(cursor_line + 1);
        }
    }
}

impl Default for Viewport {
    fn default() -> Self {
        Self::new()
    }
}

// ── DocumentView ──────────────────────────────────────────────────────────────

/// Renders the document content into a grid region.
pub struct DocumentView;

impl DocumentView {
    /// Render visible lines of `doc` into `grid`.
    ///
    /// Only the rows from `row_offset` to `row_offset + height` are written.
    /// Only rope slices for visible lines are accessed (critical for the 1GB demo).
    pub fn render(
        grid: &mut Grid,
        doc: &Document,
        sel: &Selection,
        viewport: &Viewport,
        mode: ModeIndicator,
        row_offset: u16,
        height: u16,
    ) {
        let text_col_start = viewport.line_nr_width;
        let text_width = grid.width().saturating_sub(text_col_start) as usize;
        let total_lines = doc.len_lines();

        for screen_row in 0..height {
            let doc_line = viewport.offset_line + screen_row as usize;
            let abs_row = row_offset + screen_row;

            if doc_line >= total_lines {
                // Past end of document: draw tilde
                grid.set(
                    0,
                    abs_row,
                    Cell::new("~", Style::default().fg(Color::DarkGray)),
                );
                grid.fill_rect(1, abs_row, grid.width() - 1, 1, Style::RESET);
                continue;
            }

            // Line number column
            if viewport.line_nr_width > 0 {
                let is_cursor_line = sel.ranges().iter().any(|r| {
                    doc.char_to_line(r.head) == doc_line
                });
                let nr_style =
                    if is_cursor_line { palette::LINE_NR_CURRENT } else { palette::LINE_NR };
                let nr_str = format!("{:>width$} ", doc_line + 1, width = (viewport.line_nr_width as usize).saturating_sub(1));
                grid.write_str(0, abs_row, &nr_str, nr_style);
            }

            // Document line text
            let line_start_char = doc.line_to_char(doc_line);
            let line_len = doc.line_len_no_eol(doc_line);
            let line_rope = doc.rope().slice(line_start_char..line_start_char + line_len);

            // Convert rope slice to a string (only visible chars)
            let line_str: String = line_rope
                .chars()
                .skip(viewport.offset_col)
                .take(text_width)
                .collect();

            // Determine which char indices are selected
            let row_char_start = line_start_char + viewport.offset_col;

            let mut col = text_col_start;
            for (i, ch) in line_str.chars().enumerate() {
                if col >= grid.width() {
                    break;
                }
                let char_idx = row_char_start + i;
                let style = Self::char_style(char_idx, sel, mode);
                let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1) as u16;
                grid.set(col, abs_row, Cell {
                    grapheme: ch.to_string(),
                    width: w as u8,
                    style,
                });
                col += w;
            }

            // Fill remainder of row
            if col < grid.width() {
                grid.fill_rect(col, abs_row, grid.width() - col, 1, Style::RESET);
            }
        }
    }

    fn char_style(char_idx: usize, sel: &Selection, mode: ModeIndicator) -> Style {
        let primary = sel.primary();
        let is_cursor = char_idx == primary.head;

        match mode {
            ModeIndicator::Normal | ModeIndicator::Command => {
                if is_cursor {
                    palette::CURSOR_NORMAL
                } else {
                    palette::TEXT
                }
            }
            ModeIndicator::Insert => {
                if is_cursor {
                    palette::CURSOR_INSERT
                } else {
                    palette::TEXT
                }
            }
            ModeIndicator::Visual | ModeIndicator::VisualLine => {
                let in_selection = sel.ranges().iter().any(|r| r.contains_inclusive(char_idx));
                if is_cursor {
                    palette::CURSOR_NORMAL
                } else if in_selection {
                    palette::SELECTION
                } else {
                    palette::TEXT
                }
            }
        }
    }
}

// ── Statusline ────────────────────────────────────────────────────────────────

/// Renders a one-line statusline at the bottom of the screen.
pub struct Statusline;

impl Statusline {
    pub fn render(
        grid: &mut Grid,
        row: u16,
        mode: ModeIndicator,
        doc: &Document,
        sel: &Selection,
    ) {
        let width = grid.width() as usize;
        if width == 0 {
            return;
        }

        let mode_label = format!(" {} ", mode.label());
        let mode_style = mode.style();
        let bg_style = palette::STATUS_BG;

        // Left: mode indicator
        let x = grid.write_str(0, row, &mode_label, mode_style);

        // Filename + modified
        let modified = if doc.is_modified() { " [+]" } else { "" };
        let name = format!(" {}{} ", doc.name(), modified);
        let x = grid.write_str(x, row, &name, bg_style);

        // Right: position
        let (line, col) = doc.char_to_visual_pos(sel.primary().head);
        let pct = if doc.len_lines() <= 1 {
            "All".to_string()
        } else {
            let p = line * 100 / (doc.len_lines() - 1);
            format!("{p}%")
        };
        let right = format!(" {}:{} {} ", line + 1, col + 1, pct);
        let right_x = width.saturating_sub(right.len()) as u16;

        // Fill gap
        if right_x > x {
            grid.fill_rect(x, row, right_x - x, 1, bg_style);
        }

        if right_x < grid.width() {
            grid.write_str(right_x, row, &right, bg_style);
        }
    }
}

// ── MessageLine ───────────────────────────────────────────────────────────────

/// The bottom message/command line.
pub struct MessageLine;

#[derive(Debug, Clone)]
pub enum Message {
    Info(String),
    Error(String),
    Command(String),
    None,
}

impl Message {
    pub fn is_none(&self) -> bool {
        matches!(self, Message::None)
    }
}

impl MessageLine {
    pub fn render(grid: &mut Grid, row: u16, message: &Message) {
        let width = grid.width();
        match message {
            Message::None => {
                grid.fill_rect(0, row, width, 1, Style::RESET);
            }
            Message::Info(s) => {
                let x = grid.write_str(0, row, s, palette::MSG_INFO);
                grid.fill_rect(x, row, width.saturating_sub(x), 1, Style::RESET);
            }
            Message::Error(s) => {
                let x = grid.write_str(0, row, s, palette::MSG_ERROR);
                grid.fill_rect(x, row, width.saturating_sub(x), 1, Style::RESET);
            }
            Message::Command(s) => {
                let prompt = format!(":{}", s);
                let x = grid.write_str(0, row, &prompt, Style::RESET);
                grid.fill_rect(x, row, width.saturating_sub(x), 1, Style::RESET);
            }
        }
    }
}

// ── Compositor ────────────────────────────────────────────────────────────────

/// Owns the double-buffer and drives the full render pipeline.
pub struct Compositor {
    pub buf: DoubleBuffer,
    /// Track the last cursor position so backends can position the cursor last.
    pub cursor_col: u16,
    pub cursor_row: u16,
    #[cfg(feature = "debug-overlay")]
    pub last_diff_count: usize,
    #[cfg(feature = "debug-overlay")]
    pub last_frame_us: u64,
}

impl Compositor {
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            buf: DoubleBuffer::new(width, height),
            cursor_col: 0,
            cursor_row: 0,
            #[cfg(feature = "debug-overlay")]
            last_diff_count: 0,
            #[cfg(feature = "debug-overlay")]
            last_frame_us: 0,
        }
    }

    pub fn resize(&mut self, width: u16, height: u16) {
        self.buf.resize(width, height);
        self.buf.invalidate();
    }

    /// Flush the current frame to the backend, writing only changed cells.
    pub fn flush<B: Backend>(
        &mut self,
        backend: &mut B,
        mode: ModeIndicator,
    ) -> Result<(), RenderError> {
        #[cfg(feature = "debug-overlay")]
        let diff_count = self.buf.diff_count();

        backend.draw_cells(self.buf.diff())?;

        #[cfg(feature = "debug-overlay")]
        {
            self.last_diff_count = diff_count;
        }

        // Position cursor and show it
        backend.set_cursor_position(self.cursor_col, self.cursor_row)?;
        backend.show_cursor(mode.cursor_shape())?;
        backend.flush()?;
        self.buf.commit();
        Ok(())
    }

    #[cfg(feature = "debug-overlay")]
    pub fn render_debug_overlay(&mut self) {
        let s = format!(
            " DAMAGE:{} FRAME:{}µs ",
            self.last_diff_count, self.last_frame_us
        );
        self.buf.current_mut().write_str(0, 0, &s, palette::DEBUG_OVERLAY);
    }
}
