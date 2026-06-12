use onda_core::{Document, Selection};

use crate::{
    backend::{Backend, CursorShape, RenderError},
    grid::{Attribute, Cell, Color, DoubleBuffer, Grid, Style},
};

// ── Palette ────────────────────────────────────────────────────────────────────

mod palette {
    use super::{Attribute, Color, Style};

    pub const TEXT: Style = Style {
        fg: Color::Reset,
        bg: Color::Reset,
        attrs: Attribute::empty(),
    };
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
    pub const LINE_NR: Style = Style {
        fg: Color::DarkGray,
        bg: Color::Reset,
        attrs: Attribute::empty(),
    };
    pub const LINE_NR_CURRENT: Style = Style {
        fg: Color::Yellow,
        bg: Color::Reset,
        attrs: Attribute::empty(),
    };
    pub const STATUS_NORMAL: Style = Style {
        fg: Color::Black,
        bg: Color::Green,
        attrs: Attribute::empty(),
    };
    pub const STATUS_INSERT: Style = Style {
        fg: Color::Black,
        bg: Color::LightCyan,
        attrs: Attribute::empty(),
    };
    pub const STATUS_VISUAL: Style = Style {
        fg: Color::Black,
        bg: Color::Yellow,
        attrs: Attribute::empty(),
    };
    pub const STATUS_BG: Style = Style {
        fg: Color::White,
        bg: Color::DarkGray,
        attrs: Attribute::empty(),
    };
    pub const MSG_ERROR: Style = Style {
        fg: Color::LightRed,
        bg: Color::Reset,
        attrs: Attribute::empty(),
    };
    pub const MSG_INFO: Style = Style {
        fg: Color::Reset,
        bg: Color::Reset,
        attrs: Attribute::empty(),
    };
    pub const STATUS_TERMINAL: Style = Style {
        fg: Color::Black,
        bg: Color::LightBlue,
        attrs: Attribute::empty(),
    };
    pub const DIAG_ERROR: Style = Style {
        fg: Color::LightRed,
        bg: Color::Reset,
        attrs: Attribute::UNDERLINE,
    };
    pub const DIAG_WARNING: Style = Style {
        fg: Color::Yellow,
        bg: Color::Reset,
        attrs: Attribute::UNDERLINE,
    };
    pub const DIAG_INFO: Style = Style {
        fg: Color::LightCyan,
        bg: Color::Reset,
        attrs: Attribute::UNDERLINE,
    };
    pub const GUTTER_ERROR: Style = Style {
        fg: Color::LightRed,
        bg: Color::Reset,
        attrs: Attribute::empty(),
    };
    pub const GUTTER_WARNING: Style = Style {
        fg: Color::Yellow,
        bg: Color::Reset,
        attrs: Attribute::empty(),
    };
    pub const FLOAT_BG: Style = Style {
        fg: Color::White,
        bg: Color::DarkGray,
        attrs: Attribute::empty(),
    };
    pub const FLOAT_BORDER: Style = Style {
        fg: Color::LightCyan,
        bg: Color::DarkGray,
        attrs: Attribute::empty(),
    };
    pub const COMPLETION_SELECTED: Style = Style {
        fg: Color::Black,
        bg: Color::LightCyan,
        attrs: Attribute::empty(),
    };
}

/// The mode label shown in the statusline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeIndicator {
    Normal,
    Insert,
    Visual,
    VisualLine,
    Command,
    Terminal,
    TerminalScroll,
}

impl ModeIndicator {
    pub fn label(self) -> &'static str {
        match self {
            ModeIndicator::Normal => "NORMAL",
            ModeIndicator::Insert => "INSERT",
            ModeIndicator::Visual => "VISUAL",
            ModeIndicator::VisualLine => "VISUAL LINE",
            ModeIndicator::Command => "COMMAND",
            ModeIndicator::Terminal => "TERMINAL",
            ModeIndicator::TerminalScroll => "TERMINAL SCROLL",
        }
    }

    fn style(self) -> Style {
        match self {
            ModeIndicator::Normal | ModeIndicator::Command => palette::STATUS_NORMAL,
            ModeIndicator::Insert => palette::STATUS_INSERT,
            ModeIndicator::Visual | ModeIndicator::VisualLine => palette::STATUS_VISUAL,
            ModeIndicator::Terminal | ModeIndicator::TerminalScroll => palette::STATUS_TERMINAL,
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
        Self {
            offset_line: 0,
            offset_col: 0,
            scrolloff: 5,
            line_nr_width: 4,
        }
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
        } else if cursor_line >= self.offset_line + viewport_height.saturating_sub(self.scrolloff) {
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

/// Placeholder type used in `render_with_highlights` until a real syntax-highlight
/// type is introduced (avoids a dependency on `onda-syntax` in Phase 1).
pub struct HighlightsPlaceholder;

/// Renders the document content into a grid region.
pub struct DocumentView;

impl DocumentView {
    /// Render visible lines of `doc` into `grid`, with optional search-match highlighting.
    ///
    /// `_highlights` is reserved for future syntax-highlight data (currently unused).
    /// `search_matches` is a slice of char-index ranges to highlight with reversed style.
    #[allow(clippy::too_many_arguments)]
    pub fn render_with_highlights(
        grid: &mut Grid,
        doc: &Document,
        sel: &Selection,
        viewport: &Viewport,
        mode: ModeIndicator,
        row_offset: u16,
        height: u16,
        _highlights: Option<&HighlightsPlaceholder>,
        search_matches: &[onda_core::Range],
    ) {
        let text_col_start = viewport.line_nr_width;
        let text_width = grid.width().saturating_sub(text_col_start) as usize;
        let total_lines = doc.len_lines();

        for screen_row in 0..height {
            let doc_line = viewport.offset_line + screen_row as usize;
            let abs_row = row_offset + screen_row;

            if doc_line >= total_lines {
                grid.set(
                    0,
                    abs_row,
                    Cell::new("~", Style::default().fg(Color::DarkGray)),
                );
                grid.fill_rect(1, abs_row, grid.width() - 1, 1, Style::RESET);
                continue;
            }

            if viewport.line_nr_width > 0 {
                let is_cursor_line = sel
                    .ranges()
                    .iter()
                    .any(|r| doc.char_to_line(r.head) == doc_line);
                let nr_style = if is_cursor_line {
                    palette::LINE_NR_CURRENT
                } else {
                    palette::LINE_NR
                };
                let nr_str = format!(
                    "{:>width$} ",
                    doc_line + 1,
                    width = (viewport.line_nr_width as usize).saturating_sub(1)
                );
                grid.write_str(0, abs_row, &nr_str, nr_style);
            }

            let line_start_char = doc.line_to_char(doc_line);
            let line_len = doc.line_len_no_eol(doc_line);
            let line_rope = doc
                .rope()
                .slice(line_start_char..line_start_char + line_len);
            let line_str: String = line_rope
                .chars()
                .skip(viewport.offset_col)
                .take(text_width)
                .collect();

            let row_char_start = line_start_char + viewport.offset_col;
            let mut col = text_col_start;

            for (i, ch) in line_str.chars().enumerate() {
                if col >= grid.width() {
                    break;
                }
                let char_idx = row_char_start + i;
                let mut style = Self::char_style(char_idx, sel, mode);

                // Apply search-match highlight (reversed style) if in a match range.
                let in_match = search_matches
                    .iter()
                    .any(|r| char_idx >= r.from() && char_idx < r.to());
                if in_match && style == palette::TEXT {
                    // Reverse: swap fg/bg for the match cells
                    style = Style {
                        fg: palette::CURSOR_NORMAL.bg,
                        bg: palette::CURSOR_NORMAL.fg,
                        attrs: style.attrs,
                    };
                }

                let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1) as u16;
                grid.set(
                    col,
                    abs_row,
                    Cell {
                        grapheme: ch.to_string(),
                        width: w as u8,
                        style,
                    },
                );
                col += w;
            }

            if col < grid.width() {
                grid.fill_rect(col, abs_row, grid.width() - col, 1, Style::RESET);
            }
        }
    }

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
                let is_cursor_line = sel
                    .ranges()
                    .iter()
                    .any(|r| doc.char_to_line(r.head) == doc_line);
                let nr_style = if is_cursor_line {
                    palette::LINE_NR_CURRENT
                } else {
                    palette::LINE_NR
                };
                let nr_str = format!(
                    "{:>width$} ",
                    doc_line + 1,
                    width = (viewport.line_nr_width as usize).saturating_sub(1)
                );
                grid.write_str(0, abs_row, &nr_str, nr_style);
            }

            // Document line text
            let line_start_char = doc.line_to_char(doc_line);
            let line_len = doc.line_len_no_eol(doc_line);
            let line_rope = doc
                .rope()
                .slice(line_start_char..line_start_char + line_len);

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
                grid.set(
                    col,
                    abs_row,
                    Cell {
                        grapheme: ch.to_string(),
                        width: w as u8,
                        style,
                    },
                );
                col += w;
            }

            // Fill remainder of row
            if col < grid.width() {
                grid.fill_rect(col, abs_row, grid.width() - col, 1, Style::RESET);
            }
        }
    }

    /// Render visible lines with diagnostic underline spans.
    #[allow(clippy::too_many_arguments)]
    pub fn render_with_diagnostics(
        grid: &mut Grid,
        doc: &Document,
        sel: &Selection,
        viewport: &Viewport,
        mode: ModeIndicator,
        row_offset: u16,
        height: u16,
        search_matches: &[onda_core::Range],
        diagnostics: &[DiagnosticSpan],
    ) {
        // Render base content first
        Self::render_with_highlights(
            grid,
            doc,
            sel,
            viewport,
            mode,
            row_offset,
            height,
            None,
            search_matches,
        );

        // Overlay diagnostic underlines
        let text_col_start = viewport.line_nr_width;
        for screen_row in 0..height {
            let doc_line = viewport.offset_line + screen_row as usize;
            let abs_row = row_offset + screen_row;
            if doc_line >= doc.len_lines() {
                continue;
            }
            let line_start = doc.line_to_char(doc_line);
            let line_len = doc.line_len_no_eol(doc_line);

            for span in diagnostics {
                // Skip spans that don't overlap this line
                if span.to <= line_start || span.from >= line_start + line_len {
                    continue;
                }
                let span_style = match span.severity {
                    0 => palette::DIAG_ERROR,
                    1 => palette::DIAG_WARNING,
                    _ => palette::DIAG_INFO,
                };
                // Add gutter sign in the line-number column
                if viewport.line_nr_width >= 2 {
                    let sign = match span.severity {
                        0 => "E",
                        1 => "W",
                        _ => "I",
                    };
                    let gutter_style = match span.severity {
                        0 => palette::GUTTER_ERROR,
                        1 => palette::GUTTER_WARNING,
                        _ => palette::DIAG_INFO,
                    };
                    grid.write_str(0, abs_row, sign, gutter_style);
                }
                // Underline the span columns
                let col_from = span.from.max(line_start) - line_start;
                let col_to = (span.to.min(line_start + line_len)) - line_start;
                let visible_from = col_from.saturating_sub(viewport.offset_col);
                let visible_to = col_to.saturating_sub(viewport.offset_col);
                for col_idx in visible_from..visible_to {
                    let screen_col = text_col_start + col_idx as u16;
                    if screen_col >= grid.width() {
                        break;
                    }
                    if let Some(cell) = grid.get_mut(screen_col, abs_row) {
                        cell.style.attrs |= Attribute::UNDERLINE;
                        if cell.style.fg == palette::TEXT.fg {
                            cell.style.fg = span_style.fg;
                        }
                    }
                }
            }
        }
    }

    fn char_style(char_idx: usize, sel: &Selection, mode: ModeIndicator) -> Style {
        let primary = sel.primary();
        let is_cursor = char_idx == primary.head;

        match mode {
            ModeIndicator::Normal
            | ModeIndicator::Command
            | ModeIndicator::Terminal
            | ModeIndicator::TerminalScroll => {
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
        macro_recording: Option<char>,
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
        let mut x = grid.write_str(x, row, &name, bg_style);

        // Macro recording indicator
        if let Some(reg) = macro_recording {
            let rec_label = format!(" recording @{reg} ");
            x = grid.write_str(x, row, &rec_label, palette::STATUS_VISUAL);
        }

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

// ── Picker overlay ────────────────────────────────────────────────────────────

/// Draw a floating picker overlay onto `grid`.
///
/// The overlay is centred horizontally (clamped to grid bounds) at row 2.
/// Layout (top to bottom):
///   - Top border + title
///   - Prompt line: `> {query}`
///   - `items` rows: each item's text; `(text, selected)` — selected items get
///     a highlighted background
///   - Bottom border
///
/// `width` and `height` are the outer dimensions of the box (including borders).
pub fn render_picker(
    grid: &mut Grid,
    title: &str,
    query: &str,
    items: &[(&str, bool)],
    width: u16,
    height: u16,
) {
    let grid_w = grid.width();
    let grid_h = grid.height();

    // Clamp to grid
    let width = width.min(grid_w);
    let height = height.min(grid_h);

    // Centre horizontally; start at row 2 (leave room for potential top decoration)
    let x = grid_w.saturating_sub(width) / 2;
    let y: u16 = 2;

    let inner_w = width.saturating_sub(2); // subtract left+right border cols

    let picker_bg = Style {
        fg: Color::White,
        bg: Color::DarkGray,
        attrs: Attribute::empty(),
    };
    let picker_border = Style {
        fg: Color::White,
        bg: Color::DarkGray,
        attrs: Attribute::empty(),
    };
    let picker_selected = Style {
        fg: Color::Black,
        bg: Color::LightCyan,
        attrs: Attribute::empty(),
    };
    let picker_prompt = Style {
        fg: Color::LightCyan,
        bg: Color::DarkGray,
        attrs: Attribute::empty(),
    };

    // Top border
    {
        let title_truncated: String = title.chars().take(inner_w as usize).collect();
        let top = format!(
            "┌{:─<width$}┐",
            format!(" {} ", title_truncated),
            width = inner_w as usize
        );
        grid.write_str(x, y, &top, picker_border);
    }

    // Prompt line
    if height > 2 {
        let prompt_str = format!("> {}", query);
        let prompt_display: String = format!("│{:<width$}│", prompt_str, width = inner_w as usize);
        grid.write_str(x, y + 1, &prompt_display, picker_prompt);
    }

    // Item rows
    let item_start_row = y + 2;
    let max_items = height.saturating_sub(3) as usize; // top + prompt + bottom
    for (i, (text, selected)) in items.iter().take(max_items).enumerate() {
        let row = item_start_row + i as u16;
        if row + 1 >= y + height {
            break;
        }
        let style = if *selected {
            picker_selected
        } else {
            picker_bg
        };
        let cell_text: String = text.chars().take(inner_w as usize).collect();
        let line = format!("│{:<width$}│", cell_text, width = inner_w as usize);
        grid.write_str(x, row, &line, style);
    }

    // Fill empty item rows
    let used_item_rows = items.len().min(max_items);
    let empty_start = item_start_row + used_item_rows as u16;
    let bottom_row = y + height.saturating_sub(1);
    for row in empty_start..bottom_row {
        let line = format!("│{:<width$}│", "", width = inner_w as usize);
        grid.write_str(x, row, &line, picker_bg);
    }

    // Bottom border
    if height > 1 {
        let bottom = format!("└{:─<width$}┘", "", width = inner_w as usize);
        grid.write_str(x, bottom_row, &bottom, picker_border);
    }
}

// ── Diagnostic data ───────────────────────────────────────────────────────────

/// A diagnostic span for rendering (char-index based, resolved from LSP line/col).
#[derive(Debug, Clone)]
pub struct DiagnosticSpan {
    /// First char index in the buffer.
    pub from: usize,
    /// Last char index (exclusive).
    pub to: usize,
    /// Severity: 0=error, 1=warning, 2=info/hint.
    pub severity: u8,
}

// ── Floating window ────────────────────────────────────────────────────────────

/// Render a floating window (hover, documentation, completion detail).
///
/// The float is drawn over the grid at (`col`, `row`) with a border.
/// Lines that are too long are truncated to fit `width`.
pub fn render_float(grid: &mut Grid, title: &str, lines: &[&str], col: u16, row: u16, width: u16) {
    let grid_w = grid.width();
    let grid_h = grid.height();
    if grid_w == 0 || grid_h == 0 {
        return;
    }

    // Compute actual height: 2 border rows + content
    let height = (lines.len() as u16 + 2).min(grid_h.saturating_sub(row));
    if height < 2 {
        return;
    }
    let width = width.min(grid_w.saturating_sub(col));
    if width < 4 {
        return;
    }
    let inner_w = width.saturating_sub(2) as usize;

    // Top border
    {
        let title_truncated: String = title.chars().take(inner_w.saturating_sub(2)).collect();
        let top = if title_truncated.is_empty() {
            format!("╭{:─<w$}╮", "", w = inner_w)
        } else {
            format!(
                "╭─ {:─<w$}─╮",
                title_truncated,
                w = inner_w.saturating_sub(title_truncated.len() + 3)
            )
        };
        grid.write_str(col, row, &top, palette::FLOAT_BORDER);
    }

    // Content rows
    for (i, line) in lines.iter().take(height as usize - 2).enumerate() {
        let content: String = line.chars().take(inner_w).collect();
        let row_str = format!("│{:<w$}│", content, w = inner_w);
        grid.write_str(col, row + 1 + i as u16, &row_str, palette::FLOAT_BG);
    }

    // Bottom border
    if height >= 2 {
        let bottom = format!("╰{:─<w$}╯", "", w = inner_w);
        grid.write_str(col, row + height - 1, &bottom, palette::FLOAT_BORDER);
    }
}

// ── Completion menu ────────────────────────────────────────────────────────────

/// Render a completion popup menu below the cursor position.
pub fn render_completion_menu(
    grid: &mut Grid,
    items: &[(&str, &str)], // (label, kind_icon)
    selected: usize,
    cursor_col: u16,
    cursor_row: u16,
    max_visible: usize,
) {
    let grid_w = grid.width();
    let grid_h = grid.height();
    if items.is_empty() || grid_w == 0 || grid_h == 0 {
        return;
    }

    let width: u16 = 40.min(grid_w.saturating_sub(cursor_col));
    let visible = max_visible
        .min(items.len())
        .min(grid_h.saturating_sub(cursor_row + 1) as usize);
    if visible == 0 {
        return;
    }

    let inner_w = width.saturating_sub(2) as usize;
    let start_row = cursor_row + 1;

    for (i, (label, kind)) in items.iter().enumerate().take(visible) {
        let row = start_row + i as u16;
        if row >= grid_h {
            break;
        }
        let is_selected = i == selected;
        let style = if is_selected {
            palette::COMPLETION_SELECTED
        } else {
            palette::FLOAT_BG
        };
        let kind_str = if kind.is_empty() { "   " } else { kind };
        let label_part: String = label.chars().take(inner_w.saturating_sub(4)).collect();
        let line = format!(
            "{} {:<w$}",
            kind_str,
            label_part,
            w = inner_w.saturating_sub(4)
        );
        grid.write_str(cursor_col, row, &line, style);
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
        self.buf
            .current_mut()
            .write_str(0, 0, &s, palette::DEBUG_OVERLAY);
    }
}
