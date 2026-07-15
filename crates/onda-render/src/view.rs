use onda_core::{Document, Selection};

use crate::{
    backend::{Backend, CursorShape, RenderError},
    grid::{Attribute, Cell, Color, DoubleBuffer, Grid, Style},
};

use crate::theme::Theme;

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

    fn style(self, theme: &Theme) -> Style {
        match self {
            ModeIndicator::Normal | ModeIndicator::Command => theme.status_normal(),
            ModeIndicator::Insert => theme.status_insert(),
            ModeIndicator::Visual | ModeIndicator::VisualLine => theme.status_visual(),
            ModeIndicator::Terminal | ModeIndicator::TerminalScroll => theme.status_terminal(),
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

    /// Center the viewport vertically on `cursor_line` (vim's `zz`).
    pub fn center_on(&mut self, cursor_line: usize, viewport_height: usize) {
        self.offset_line = cursor_line.saturating_sub(viewport_height / 2);
    }
}

impl Default for Viewport {
    fn default() -> Self {
        Self::new()
    }
}

// ── DocumentView ──────────────────────────────────────────────────────────────

/// A pre-resolved syntax-highlight span: char range `[start, end)` painted with
/// `style`. The binary resolves tree-sitter scopes to theme styles and passes these
/// in, so `onda-render` stays free of an `onda-syntax` dependency. Spans must be
/// sorted by `start` and non-overlapping (innermost-wins resolved by the caller).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HlSpan {
    pub start: usize,
    pub end: usize,
    pub style: Style,
}

/// Advancing cursor over a sorted, non-overlapping `&[HlSpan]`. Because the render
/// pass visits char indices monotonically, a single moving pointer resolves the
/// active style in amortized O(1) per char.
struct HlCursor<'a> {
    spans: &'a [HlSpan],
    idx: usize,
}

impl<'a> HlCursor<'a> {
    fn new(spans: &'a [HlSpan]) -> Self {
        Self { spans, idx: 0 }
    }

    /// Style for `char_idx` (monotonically non-decreasing across calls), if any span
    /// covers it.
    fn style_at(&mut self, char_idx: usize) -> Option<Style> {
        while self.idx < self.spans.len() && self.spans[self.idx].end <= char_idx {
            self.idx += 1;
        }
        let s = self.spans.get(self.idx)?;
        if s.start <= char_idx && char_idx < s.end {
            Some(s.style)
        } else {
            None
        }
    }
}

// ── Row layout (soft wrap) ──────────────────────────────────────────────────────

/// One screen row's worth of a document line. Without soft wrap every row is a
/// whole line (`seg_start = 0`, `seg_len` = the full line, `continuation =
/// false`) — identical to the pre-wrap 1:1 `doc_line = offset_line + row`
/// mapping. With soft wrap a long line is split into multiple consecutive
/// `RowSlice`s; every row after the first for a given line has `continuation
/// = true` (used to blank the line-number gutter on those rows).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowSlice {
    pub doc_line: usize,
    /// Char offset into the line where this segment starts.
    pub seg_start: usize,
    /// Number of chars in this segment.
    pub seg_len: usize,
    pub continuation: bool,
}

/// Split one line into `(seg_start, seg_len)` segments that each fit within
/// `text_width` display cells (wide/CJK chars count as 2, matching
/// `Document::char_to_display_col`). Character-boundary wrapping only — no
/// word-boundary (greedy) wrapping. Always returns at least one segment, even
/// for an empty line.
fn wrap_line_segments(doc: &Document, line: usize, text_width: usize) -> Vec<(usize, usize)> {
    let line_start = doc.line_to_char(line);
    let line_len = doc.line_len_no_eol(line);
    if line_len == 0 || text_width == 0 {
        return vec![(0, line_len)];
    }
    let rope = doc.rope();
    let line_slice = rope.slice(line_start..line_start + line_len);

    let mut segments = Vec::new();
    let mut seg_start = 0usize;
    let mut width_acc = 0usize;
    let mut count = 0usize;
    for (i, ch) in line_slice.chars().enumerate() {
        let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1);
        if width_acc + w > text_width && count > 0 {
            segments.push((seg_start, count));
            seg_start = i;
            width_acc = 0;
            count = 0;
        }
        width_acc += w;
        count += 1;
    }
    segments.push((seg_start, count));
    segments
}

/// Build the `height`-row layout for the visible window starting at
/// `viewport.offset_line`. With `soft_wrap` off this is exactly the old 1:1
/// `doc_line = offset_line + row` mapping; with it on, long lines expand into
/// multiple rows via [`wrap_line_segments`]. Stops early (fewer than `height`
/// rows) at end of document — callers render `~` for the remainder, as before.
pub fn build_row_layout(
    doc: &Document,
    viewport: &Viewport,
    height: u16,
    text_width: usize,
    soft_wrap: bool,
) -> Vec<RowSlice> {
    let total_lines = doc.len_lines();
    let mut rows = Vec::with_capacity(height as usize);
    let mut doc_line = viewport.offset_line;
    while rows.len() < height as usize && doc_line < total_lines {
        if soft_wrap {
            for (idx, (seg_start, seg_len)) in wrap_line_segments(doc, doc_line, text_width)
                .into_iter()
                .enumerate()
            {
                if rows.len() >= height as usize {
                    break;
                }
                rows.push(RowSlice {
                    doc_line,
                    seg_start,
                    seg_len,
                    continuation: idx > 0,
                });
            }
        } else {
            rows.push(RowSlice {
                doc_line,
                seg_start: 0,
                seg_len: doc.line_len_no_eol(doc_line),
                continuation: false,
            });
        }
        doc_line += 1;
    }
    rows
}

/// Locate `char_idx` within a row layout: returns `(row_index, display_col)`
/// (display_col accounts for wide/CJK chars, matching the render loop's `col`
/// advance). A non-final segment of a wrapped line is exclusive of its end
/// (that position belongs to the *next* row); the line's last segment is
/// inclusive (matches the unwrapped "cursor after the last char" position).
pub fn locate_in_layout(
    rows: &[RowSlice],
    doc: &Document,
    char_idx: usize,
) -> Option<(usize, usize)> {
    for (i, row) in rows.iter().enumerate() {
        let line_start = doc.line_to_char(row.doc_line);
        let seg_char_start = line_start + row.seg_start;
        let seg_char_end = seg_char_start + row.seg_len;
        let is_last_segment_of_line = rows
            .get(i + 1)
            .map(|n| n.doc_line != row.doc_line)
            .unwrap_or(true);
        let in_row = if is_last_segment_of_line {
            char_idx >= seg_char_start && char_idx <= seg_char_end
        } else {
            char_idx >= seg_char_start && char_idx < seg_char_end
        };
        if in_row {
            let col: usize = doc
                .rope()
                .slice(seg_char_start..char_idx)
                .chars()
                .map(|c| unicode_width::UnicodeWidthChar::width(c).unwrap_or(1))
                .sum();
            return Some((i, col));
        }
    }
    None
}

/// Renders the document content into a grid region.
pub struct DocumentView;

impl DocumentView {
    /// Render visible lines of `doc` into `grid`, applying syntax `highlights`,
    /// selection/cursor styling, and search-match highlighting.
    ///
    /// `highlights` are pre-resolved styled char spans (sorted, non-overlapping);
    /// `search_matches` is a slice of char-index ranges shown reversed.
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    pub fn render_with_highlights(
        grid: &mut Grid,
        doc: &Document,
        sel: &Selection,
        viewport: &Viewport,
        mode: ModeIndicator,
        row_offset: u16,
        height: u16,
        col_offset: u16,
        area_width: u16,
        highlights: &[HlSpan],
        search_matches: &[onda_core::Range],
        theme: &Theme,
        soft_wrap: bool,
    ) {
        // Horizontal window bounds: the editor owns [col_offset, col_end) so it never
        // paints over left chrome (sidebar) or a neighbouring vertical split.
        let col_end = col_offset.saturating_add(area_width).min(grid.width());
        let text_col_start = col_offset + viewport.line_nr_width;
        let text_width = area_width.saturating_sub(viewport.line_nr_width) as usize;
        let mut hl = HlCursor::new(highlights);
        let rows = build_row_layout(doc, viewport, height, text_width, soft_wrap);

        for screen_row in 0..height {
            let abs_row = row_offset + screen_row;
            let Some(row) = rows.get(screen_row as usize) else {
                grid.set(
                    col_offset,
                    abs_row,
                    Cell::new("~", Style::default().fg(Color::DarkGray)),
                );
                grid.fill_rect(
                    col_offset + 1,
                    abs_row,
                    col_end.saturating_sub(col_offset + 1),
                    1,
                    Style::RESET,
                );
                continue;
            };
            let doc_line = row.doc_line;

            if viewport.line_nr_width > 0 {
                if row.continuation {
                    grid.fill_rect(
                        col_offset,
                        abs_row,
                        viewport.line_nr_width,
                        1,
                        theme.line_nr(),
                    );
                } else {
                    let is_cursor_line = sel
                        .ranges()
                        .iter()
                        .any(|r| doc.char_to_line(r.head) == doc_line);
                    let nr_style = if is_cursor_line {
                        theme.line_nr_current()
                    } else {
                        theme.line_nr()
                    };
                    let nr_str = format!(
                        "{:>width$} ",
                        doc_line + 1,
                        width = (viewport.line_nr_width as usize).saturating_sub(1)
                    );
                    grid.write_str(col_offset, abs_row, &nr_str, nr_style);
                }
            }

            let line_start_char = doc.line_to_char(doc_line);
            let seg_start_char = line_start_char + row.seg_start;
            let line_str: String = if soft_wrap {
                doc.rope()
                    .slice(seg_start_char..seg_start_char + row.seg_len)
                    .to_string()
            } else {
                let line_len = doc.line_len_no_eol(doc_line);
                doc.rope()
                    .slice(line_start_char..line_start_char + line_len)
                    .chars()
                    .skip(viewport.offset_col)
                    .take(text_width)
                    .collect()
            };

            let row_char_start = if soft_wrap {
                seg_start_char
            } else {
                line_start_char + viewport.offset_col
            };
            let mut col = text_col_start;

            for (i, ch) in line_str.chars().enumerate() {
                if col >= col_end {
                    break;
                }
                let char_idx = row_char_start + i;
                // Syntax style is the base; cursor/selection override it.
                let base = hl.style_at(char_idx).unwrap_or_else(|| theme.text());
                let mut style = Self::char_style(char_idx, sel, mode, theme, base);

                // Apply search-match highlight (reversed style) if in a match range.
                let in_match = search_matches
                    .iter()
                    .any(|r| char_idx >= r.from() && char_idx < r.to());
                if in_match && style == base {
                    // Reverse: swap fg/bg for the match cells
                    let cursor = theme.cursor_normal();
                    style = Style {
                        fg: cursor.bg,
                        bg: cursor.fg,
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
                // Mark a wide char's trailing column so a later narrow overwrite
                // redraws it instead of leaving a ghosted right half.
                if w == 2 && col + 1 < col_end {
                    grid.set(col + 1, abs_row, Cell::wide_continuation(style));
                }
                col += w;
            }

            if col < col_end {
                grid.fill_rect(col, abs_row, col_end - col, 1, Style::RESET);
            }
        }
    }

    /// Render visible lines of `doc` into `grid`.
    ///
    /// Only the rows from `row_offset` to `row_offset + height` are written.
    /// Only rope slices for visible lines are accessed (critical for the 1GB demo).
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        grid: &mut Grid,
        doc: &Document,
        sel: &Selection,
        viewport: &Viewport,
        mode: ModeIndicator,
        row_offset: u16,
        height: u16,
        theme: &Theme,
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
                    theme.line_nr_current()
                } else {
                    theme.line_nr()
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
                let style = Self::char_style(char_idx, sel, mode, theme, theme.text());
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
                // Mark a wide char's trailing column so a later narrow overwrite
                // redraws it instead of leaving a ghosted right half.
                if w == 2 && col + 1 < grid.width() {
                    grid.set(col + 1, abs_row, Cell::wide_continuation(style));
                }
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
        col_offset: u16,
        area_width: u16,
        highlights: &[HlSpan],
        search_matches: &[onda_core::Range],
        diagnostics: &[DiagnosticSpan],
        theme: &Theme,
        soft_wrap: bool,
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
            col_offset,
            area_width,
            highlights,
            search_matches,
            theme,
            soft_wrap,
        );

        // Overlay diagnostic underlines — reuse the identical (deterministic)
        // row layout so a wrapped line's underline lands on the right sub-row.
        let col_end = col_offset.saturating_add(area_width).min(grid.width());
        let text_col_start = col_offset + viewport.line_nr_width;
        let text_width = area_width.saturating_sub(viewport.line_nr_width) as usize;
        let rows = build_row_layout(doc, viewport, height, text_width, soft_wrap);
        for screen_row in 0..height {
            let abs_row = row_offset + screen_row;
            let Some(row) = rows.get(screen_row as usize) else {
                continue;
            };
            let doc_line = row.doc_line;
            let line_start_all = doc.line_to_char(doc_line);
            let seg_start = line_start_all + row.seg_start;
            let seg_end = seg_start + row.seg_len;

            for span in diagnostics {
                // Skip spans that don't overlap this row's segment.
                if span.to <= seg_start || span.from >= seg_end {
                    continue;
                }
                let span_style = match span.severity {
                    0 => theme.diag_error(),
                    1 => theme.diag_warning(),
                    _ => theme.diag_info(),
                };
                // Add gutter sign in the line-number column (first segment only).
                if viewport.line_nr_width >= 2 && !row.continuation {
                    let sign = match span.severity {
                        0 => "E",
                        1 => "W",
                        _ => "I",
                    };
                    let gutter_style = match span.severity {
                        0 => theme.gutter_error(),
                        1 => theme.gutter_warning(),
                        _ => theme.diag_info(),
                    };
                    grid.write_str(col_offset, abs_row, sign, gutter_style);
                }
                // Underline the span columns within this segment.
                let col_from = span.from.max(seg_start) - seg_start;
                let col_to = span.to.min(seg_end) - seg_start;
                let (visible_from, visible_to) = if soft_wrap {
                    (col_from, col_to)
                } else {
                    (
                        col_from.saturating_sub(viewport.offset_col),
                        col_to.saturating_sub(viewport.offset_col),
                    )
                };
                for col_idx in visible_from..visible_to {
                    let screen_col = text_col_start + col_idx as u16;
                    if screen_col >= col_end {
                        break;
                    }
                    if let Some(cell) = grid.get_mut(screen_col, abs_row) {
                        cell.style.attrs |= Attribute::UNDERLINE;
                        if cell.style.fg == theme.text().fg {
                            cell.style.fg = span_style.fg;
                        }
                    }
                }
            }
        }
    }

    /// Resolve a cell's style: cursor/selection override `base` (the syntax/text
    /// style); otherwise `base` shows through.
    fn char_style(
        char_idx: usize,
        sel: &Selection,
        mode: ModeIndicator,
        theme: &Theme,
        base: Style,
    ) -> Style {
        let primary = sel.primary();
        let is_cursor = char_idx == primary.head;

        match mode {
            ModeIndicator::Normal
            | ModeIndicator::Command
            | ModeIndicator::Terminal
            | ModeIndicator::TerminalScroll => {
                if is_cursor {
                    theme.cursor_normal()
                } else {
                    base
                }
            }
            ModeIndicator::Insert => {
                if is_cursor {
                    theme.cursor_insert()
                } else {
                    base
                }
            }
            ModeIndicator::Visual | ModeIndicator::VisualLine => {
                let in_selection = sel.ranges().iter().any(|r| r.contains_inclusive(char_idx));
                if is_cursor {
                    theme.cursor_normal()
                } else if in_selection {
                    theme.selection()
                } else {
                    base
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
        theme: &Theme,
    ) {
        let width = grid.width() as usize;
        if width == 0 {
            return;
        }

        let mode_label = format!(" {} ", mode.label());
        let mode_style = mode.style(theme);
        let bg_style = theme.status_bg();

        // Left: mode indicator
        let x = grid.write_str(0, row, &mode_label, mode_style);

        // Filename + modified
        let modified = if doc.is_modified() { " [+]" } else { "" };
        let name = format!(" {}{} ", doc.name(), modified);
        let mut x = grid.write_str(x, row, &name, bg_style);

        // Macro recording indicator
        if let Some(reg) = macro_recording {
            let rec_label = format!(" recording @{reg} ");
            x = grid.write_str(x, row, &rec_label, theme.status_visual());
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
    pub fn render(grid: &mut Grid, row: u16, message: &Message, theme: &Theme) {
        let width = grid.width();
        match message {
            Message::None => {
                grid.fill_rect(0, row, width, 1, Style::RESET);
            }
            Message::Info(s) => {
                let x = grid.write_str(0, row, s, theme.msg_info());
                grid.fill_rect(x, row, width.saturating_sub(x), 1, Style::RESET);
            }
            Message::Error(s) => {
                let x = grid.write_str(0, row, s, theme.msg_error());
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
    theme: &Theme,
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

    let picker_bg = theme.menu();
    let picker_border = theme.float_border();
    let picker_selected = theme.menu_selected();
    let picker_prompt = theme.float_border();

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
pub fn render_float(
    grid: &mut Grid,
    title: &str,
    lines: &[&str],
    col: u16,
    row: u16,
    width: u16,
    theme: &Theme,
) {
    let grid_w = grid.width();
    let grid_h = grid.height();
    if grid_w == 0 || grid_h == 0 {
        return;
    }
    let float_border = theme.float_border();
    let float_bg = theme.float_bg();

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
        grid.write_str(col, row, &top, float_border);
    }

    // Content rows
    for (i, line) in lines.iter().take(height as usize - 2).enumerate() {
        let content: String = line.chars().take(inner_w).collect();
        let row_str = format!("│{:<w$}│", content, w = inner_w);
        grid.write_str(col, row + 1 + i as u16, &row_str, float_bg);
    }

    // Bottom border
    if height >= 2 {
        let bottom = format!("╰{:─<w$}╯", "", w = inner_w);
        grid.write_str(col, row + height - 1, &bottom, float_border);
    }
}

// ── Completion menu ────────────────────────────────────────────────────────────

/// Render a completion popup menu below the cursor position.
#[allow(clippy::too_many_arguments)]
pub fn render_completion_menu(
    grid: &mut Grid,
    items: &[(&str, &str)], // (label, kind_icon)
    selected: usize,
    cursor_col: u16,
    cursor_row: u16,
    max_visible: usize,
    theme: &Theme,
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
            theme.menu_selected()
        } else {
            theme.float_bg()
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

// ── Agent panel ──────────────────────────────────────────────────────────────

/// Draw the right-side agent panel: a title bar (with a busy spinner), the
/// conversation thread (pre-styled lines, scrolled to the bottom), and an input
/// line. `lines` are `(style, text)` pairs the binary formats from the thread; this
/// keeps `onda-render` free of an `onda-agent` dependency.
#[allow(clippy::too_many_arguments)]
pub fn render_agent_panel(
    grid: &mut Grid,
    x: u16,
    y: u16,
    width: u16,
    height: u16,
    title: &str,
    lines: &[(Style, String)],
    input: &str,
    busy: bool,
    theme: &Theme,
) {
    if width < 2 || height < 3 {
        return;
    }
    let bg = theme.float_bg();
    let border = theme.float_border();
    let title_style = theme.status_bg();

    // Left separator column.
    for r in 0..height {
        grid.set(x, y + r, Cell::new("│", border));
    }
    let content_x = x + 1;
    let content_w = width - 1;

    // Title bar.
    let spinner = if busy { "  ◐ thinking…" } else { "" };
    let title_text = format!(" {title}{spinner}");
    grid.fill_rect(content_x, y, content_w, 1, title_style);
    grid.write_str(content_x, y, &title_text, title_style);

    // Input line at the bottom.
    let input_row = y + height - 1;
    grid.fill_rect(content_x, input_row, content_w, 1, bg);
    let prompt = format!("> {input}");
    grid.write_str(content_x, input_row, &prompt, bg);

    // Thread body between title and input, scrolled so the tail is visible.
    let body_top = y + 1;
    let body_rows = height.saturating_sub(2) as usize;
    let start = lines.len().saturating_sub(body_rows);
    for (i, (style, text)) in lines[start..].iter().enumerate() {
        let row = body_top + i as u16;
        grid.fill_rect(content_x, row, content_w, 1, bg);
        let clipped: String = text.chars().take(content_w as usize).collect();
        grid.write_str(content_x, row, &clipped, *style);
    }
    // Clear any remaining body rows.
    for i in lines[start..].len()..body_rows {
        grid.fill_rect(content_x, body_top + i as u16, content_w, 1, bg);
    }
}

/// Render a buffer tabline at `(x, y)` spanning `width` cells. Each tab is a
/// `(name, active)` pair; the active tab uses the visual style, others a dim style.
/// Returns the starting column of each rendered tab (for click hit-testing).
pub fn render_tabline(
    grid: &mut Grid,
    x: u16,
    y: u16,
    width: u16,
    tabs: &[(String, bool)],
    theme: &Theme,
) -> Vec<u16> {
    let bg = theme.status_bg();
    let active = theme.status_visual();
    let inactive = theme.line_nr();
    grid.fill_rect(x, y, width, 1, bg);
    let mut col = x;
    let mut starts = Vec::with_capacity(tabs.len());
    for (name, is_active) in tabs {
        let label = format!(" {name} ");
        let w = label.chars().count() as u16;
        if col + w > x + width {
            break;
        }
        starts.push(col);
        let style = if *is_active { active } else { inactive };
        grid.fill_rect(col, y, w, 1, style);
        grid.write_str(col, y, &label, style);
        col += w;
    }
    starts
}

/// Render the IDE shell's left chrome: a vertical activity bar (view switcher)
/// at `[0, activity_w)` plus a sidebar panel at `[activity_w, activity_w + width)`.
///
/// `views` are short activity-bar labels (1–2 cells); `active` is the selected
/// index. `title`/`body` fill the sidebar; `focused` highlights it. A `│` border
/// closes the right edge so the editor area abuts cleanly.
#[allow(clippy::too_many_arguments)]
pub fn render_sidebar(
    grid: &mut Grid,
    activity_w: u16,
    width: u16,
    height: u16,
    views: &[&str],
    active: usize,
    title: &str,
    body: &[(Style, String)],
    focused: bool,
    theme: &Theme,
) {
    if width < 2 || height == 0 {
        return;
    }
    let bg = theme.float_bg();
    let border = theme.float_border();
    let active_style = theme.status_visual();
    let inactive = theme.line_nr();

    // Activity bar (far left): one label per row.
    for r in 0..height {
        grid.fill_rect(0, r, activity_w, 1, bg);
    }
    for (i, label) in views.iter().enumerate() {
        if i as u16 >= height {
            break;
        }
        let style = if i == active { active_style } else { inactive };
        grid.fill_rect(0, i as u16, activity_w, 1, style);
        grid.write_str(0, i as u16, label, style);
    }

    // Sidebar panel.
    let sx = activity_w;
    let sw = width;
    let title_style = if focused {
        theme.status_visual()
    } else {
        theme.status_bg()
    };
    grid.fill_rect(sx, 0, sw, 1, title_style);
    let header: String = format!(" {title}").chars().take(sw as usize).collect();
    grid.write_str(sx, 0, &header, title_style);

    let body_top = 1u16;
    let body_rows = height.saturating_sub(1) as usize;
    for r in 0..body_rows {
        grid.fill_rect(sx, body_top + r as u16, sw, 1, bg);
    }
    for (i, (style, text)) in body.iter().take(body_rows).enumerate() {
        let clipped: String = text.chars().take(sw.saturating_sub(1) as usize).collect();
        grid.write_str(sx, body_top + i as u16, &clipped, *style);
    }

    // Right border column.
    let bx = sx + sw - 1;
    for r in 0..height {
        grid.set(bx, r, Cell::new("│", border));
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
        let overlay = Style {
            fg: Color::Black,
            bg: Color::LightYellow,
            attrs: Attribute::empty(),
        };
        self.buf.current_mut().write_str(0, 0, &s, overlay);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use onda_core::{Document, Selection};

    fn doc_with(text: &str) -> Document {
        let mut d = Document::new_empty();
        let cs = onda_core::transaction::ChangeSetBuilder::new(0)
            .insert(text)
            .build();
        d.apply(&onda_core::Transaction::new(cs)).unwrap();
        d
    }

    #[test]
    fn hl_cursor_resolves_styles_monotonically() {
        let red = Style::default().fg(Color::Red);
        let blue = Style::default().fg(Color::Blue);
        let spans = [
            HlSpan {
                start: 0,
                end: 3,
                style: red,
            },
            HlSpan {
                start: 5,
                end: 8,
                style: blue,
            },
        ];
        let mut c = HlCursor::new(&spans);
        assert_eq!(c.style_at(0), Some(red));
        assert_eq!(c.style_at(2), Some(red));
        assert_eq!(c.style_at(3), None); // gap
        assert_eq!(c.style_at(4), None);
        assert_eq!(c.style_at(5), Some(blue));
        assert_eq!(c.style_at(9), None); // past end
    }

    #[test]
    fn agent_panel_draws_title_thread_and_input() {
        let mut grid = Grid::new(60, 10);
        let theme = Theme::default_dark();
        let s = Style::default();
        let lines = vec![
            (s, "you: hello".to_string()),
            (s, "agent: hi there".to_string()),
        ];
        render_agent_panel(
            &mut grid, 40, 0, 20, 10, "Agent", &lines, "type…", true, &theme,
        );
        // Separator column.
        assert_eq!(grid.get(40, 0).unwrap().grapheme, "│");
        // Title row contains "Agent" and the busy spinner.
        let title: String = (41..60)
            .filter_map(|c| grid.get(c, 0).map(|x| x.grapheme.clone()))
            .collect();
        assert!(title.contains("Agent"));
        assert!(title.contains("thinking"));
        // Input row (last) shows the prompt.
        let input: String = (41..60)
            .filter_map(|c| grid.get(c, 9).map(|x| x.grapheme.clone()))
            .collect();
        assert!(input.contains("> type"));
    }

    #[test]
    fn render_applies_syntax_style_to_cells() {
        let mut grid = Grid::new(40, 4);
        let doc = doc_with("fn main\n");
        let sel = Selection::point(100); // cursor off-screen so it doesn't override
        let vp = Viewport {
            offset_line: 0,
            offset_col: 0,
            scrolloff: 0,
            line_nr_width: 0,
        };
        let theme = Theme::default_dark();
        let kw = theme.syntax("keyword"); // syntax.keyword from onda-dark (magenta)
                                          // Highlight "fn" (chars 0..2) as keyword.
        let spans = [HlSpan {
            start: 0,
            end: 2,
            style: kw,
        }];
        let gw = grid.width();
        DocumentView::render_with_highlights(
            &mut grid,
            &doc,
            &sel,
            &vp,
            ModeIndicator::Normal,
            0,
            4,
            0,
            gw,
            &spans,
            &[],
            &theme,
            false,
        );
        // Cell at col 0 ('f') carries the keyword fg; col 3 ('m') does not.
        assert_eq!(grid.get(0, 0).unwrap().style.fg, kw.fg);
        assert_ne!(grid.get(3, 0).unwrap().style.fg, kw.fg);
    }

    fn plain_vp() -> Viewport {
        Viewport {
            offset_line: 0,
            offset_col: 0,
            scrolloff: 0,
            line_nr_width: 0,
        }
    }

    fn render(grid: &mut Grid, doc: &Document, sel: &Selection, theme: &Theme) {
        let h = grid.height();
        let w = grid.width();
        DocumentView::render_with_highlights(
            grid,
            doc,
            sel,
            &plain_vp(),
            ModeIndicator::Normal,
            0,
            h,
            0,
            w,
            &[],
            &[],
            theme,
            false,
        );
    }

    #[test]
    fn wide_chars_emit_width0_continuation_columns() {
        let mut grid = Grid::new(20, 2);
        let doc = doc_with("가나\n"); // two width-2 graphemes
        let sel = Selection::point(100); // off-screen
        render(&mut grid, &doc, &sel, &Theme::default_dark());
        assert_eq!(grid.get(0, 0).unwrap().grapheme, "가");
        assert_eq!(grid.get(0, 0).unwrap().width, 2);
        assert_eq!(grid.get(1, 0).unwrap().width, 0); // continuation
        assert_eq!(grid.get(2, 0).unwrap().grapheme, "나");
        assert_eq!(grid.get(2, 0).unwrap().width, 2);
        assert_eq!(grid.get(3, 0).unwrap().width, 0); // continuation
    }

    #[test]
    fn wide_to_narrow_redraws_trailing_column() {
        // Regression for the ghosting bug: when a line of wide chars is replaced by
        // narrow content, the wide chars' trailing columns must register as damage.
        let theme = Theme::default_dark();
        let off = Selection::point(100);
        let mut grid = Grid::new(20, 2);
        render(&mut grid, &doc_with("가나\n"), &off, &theme);
        let prev = grid.clone();
        // Same buffer now holds a short ASCII line where the wide chars were.
        render(&mut grid, &doc_with("x\n"), &off, &theme);
        let changed: Vec<(u16, u16)> = grid.diff(&prev).map(|(c, r, _)| (c, r)).collect();
        // col 1 was the right half of "가"; it must be redrawn (cleared), not ghosted.
        assert!(
            changed.contains(&(1, 0)),
            "trailing column must be redrawn, got {changed:?}"
        );
    }

    #[test]
    fn sidebar_lays_out_activity_bar_title_and_border() {
        let theme = Theme::default_dark();
        let mut grid = Grid::new(40, 10);
        let views = ["E", "S", "G", "R", "A"];
        render_sidebar(
            &mut grid,
            3,
            20,
            10,
            &views,
            2,
            "SOURCE CONTROL",
            &[],
            false,
            &theme,
        );
        // Activity bar: each label on its own row at column 0.
        assert_eq!(grid.get(0, 0).unwrap().grapheme, "E");
        assert_eq!(grid.get(0, 2).unwrap().grapheme, "G");
        assert_eq!(grid.get(0, 4).unwrap().grapheme, "A");
        // Sidebar title starts just after the activity bar (col 3).
        let title: String = (3..23)
            .filter_map(|c| grid.get(c, 0).map(|x| x.grapheme.clone()))
            .collect();
        assert!(title.contains("SOURCE CONTROL"), "got {title:?}");
        // Right border column at activity_w + width - 1 = 3 + 20 - 1 = 22.
        assert_eq!(grid.get(22, 5).unwrap().grapheme, "│");
    }

    #[test]
    fn tabline_lays_out_tabs_and_returns_starts() {
        let theme = Theme::default_dark();
        let mut grid = Grid::new(40, 2);
        let tabs = vec![("a.rs".to_string(), true), ("b.rs".to_string(), false)];
        let starts = render_tabline(&mut grid, 0, 0, 40, &tabs, &theme);
        assert_eq!(starts, vec![0, 6]); // " a.rs " is 6 cells wide
        let row: String = (0..14)
            .filter_map(|c| grid.get(c, 0).map(|x| x.grapheme.clone()))
            .collect();
        assert!(row.contains("a.rs") && row.contains("b.rs"), "got {row:?}");
    }

    #[test]
    fn sidebar_renders_body_lines() {
        let theme = Theme::default_dark();
        let mut grid = Grid::new(40, 10);
        let body = vec![(Style::default(), "  hello.rs".to_string())];
        render_sidebar(
            &mut grid,
            3,
            20,
            10,
            &["E"],
            0,
            "EXPLORER",
            &body,
            true,
            &theme,
        );
        let row1: String = (3..23)
            .filter_map(|c| grid.get(c, 1).map(|x| x.grapheme.clone()))
            .collect();
        assert!(row1.contains("hello.rs"), "got {row1:?}");
    }

    #[test]
    fn center_on_puts_cursor_line_mid_viewport() {
        let mut vp = plain_vp();
        // 40 lines tall, cursor deep in the file: center → offset = line - height/2.
        vp.center_on(100, 40);
        assert_eq!(vp.offset_line, 80);
    }

    #[test]
    fn center_on_clamps_near_document_start() {
        let mut vp = plain_vp();
        // Cursor near the top: centering must not underflow past line 0.
        vp.center_on(3, 40);
        assert_eq!(vp.offset_line, 0);
    }

    // ── Soft wrap ────────────────────────────────────────────────────────────

    #[test]
    fn row_layout_without_wrap_matches_old_1to1_mapping() {
        let doc = doc_with("one\ntwo\nthree\n");
        let vp = plain_vp();
        let rows = build_row_layout(&doc, &vp, 3, 80, false);
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows[0],
            RowSlice {
                doc_line: 0,
                seg_start: 0,
                seg_len: 3,
                continuation: false
            }
        );
        assert_eq!(
            rows[1],
            RowSlice {
                doc_line: 1,
                seg_start: 0,
                seg_len: 3,
                continuation: false
            }
        );
        assert_eq!(
            rows[2],
            RowSlice {
                doc_line: 2,
                seg_start: 0,
                seg_len: 5,
                continuation: false
            }
        );
    }

    #[test]
    fn row_layout_stops_at_end_of_document() {
        // No trailing newline, so this is exactly 2 lines (a trailing "\n" would
        // add a real (empty) 3rd line, per how ropey/the renderer already count
        // lines — not this function's concern).
        let doc = doc_with("one\ntwo");
        let vp = plain_vp();
        let rows = build_row_layout(&doc, &vp, 10, 80, false);
        assert_eq!(rows.len(), 2); // fewer than height; caller draws `~` for the rest
    }

    #[test]
    fn wrap_line_segments_splits_long_line_by_width() {
        let doc = doc_with("abcdefghij\n");
        let segs = wrap_line_segments(&doc, 0, 4);
        assert_eq!(segs, vec![(0, 4), (4, 4), (8, 2)]);
    }

    #[test]
    fn wrap_line_segments_short_line_is_one_segment() {
        let doc = doc_with("abc\n");
        let segs = wrap_line_segments(&doc, 0, 80);
        assert_eq!(segs, vec![(0, 3)]);
    }

    #[test]
    fn wrap_line_segments_respects_wide_char_display_width() {
        // "가" is 2 cells wide; a width-4 budget fits exactly 2 of them.
        let doc = doc_with("가나다\n");
        let segs = wrap_line_segments(&doc, 0, 4);
        assert_eq!(segs, vec![(0, 2), (2, 1)]);
    }

    #[test]
    fn wrap_line_segments_empty_line() {
        let doc = doc_with("\nx\n");
        let segs = wrap_line_segments(&doc, 0, 10);
        assert_eq!(segs, vec![(0, 0)]);
    }

    #[test]
    fn row_layout_with_wrap_expands_long_line_into_continuation_rows() {
        // No trailing newline after "short", so the doc has exactly 2 lines.
        let doc = doc_with("abcdefghij\nshort");
        let vp = plain_vp();
        let rows = build_row_layout(&doc, &vp, 5, 6, true);
        // Line 0 (10 chars) wraps into 2 rows at width 6 (6+4); line 1 ("short",
        // 5 chars) fits in one row. Only 3 rows produced — the doc ends there.
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows[0],
            RowSlice {
                doc_line: 0,
                seg_start: 0,
                seg_len: 6,
                continuation: false
            }
        );
        assert_eq!(
            rows[1],
            RowSlice {
                doc_line: 0,
                seg_start: 6,
                seg_len: 4,
                continuation: true
            }
        );
        assert_eq!(
            rows[2],
            RowSlice {
                doc_line: 1,
                seg_start: 0,
                seg_len: 5,
                continuation: false
            }
        );
    }

    #[test]
    fn row_layout_with_wrap_truncates_at_viewport_height() {
        let doc = doc_with("abcdefghij\n");
        let vp = plain_vp();
        // Only 2 rows of screen space: the 3rd wrapped segment doesn't fit.
        let rows = build_row_layout(&doc, &vp, 2, 4, true);
        assert_eq!(rows.len(), 2);
        assert!(rows[1].continuation);
    }

    #[test]
    fn locate_in_layout_start_of_wrapped_line() {
        let doc = doc_with("abcdefghij\n");
        let rows = build_row_layout(&doc, &plain_vp(), 3, 4, true);
        assert_eq!(locate_in_layout(&rows, &doc, 0), Some((0, 0)));
        assert_eq!(locate_in_layout(&rows, &doc, 3), Some((0, 3)));
    }

    #[test]
    fn locate_in_layout_wrap_boundary_belongs_to_next_row() {
        // char 4 is the boundary between segment 0 ([0,4)) and segment 1 ([4,8)).
        // It's not the end of the line, so it must resolve to the *next* row.
        let doc = doc_with("abcdefghij\n");
        let rows = build_row_layout(&doc, &plain_vp(), 3, 4, true);
        assert_eq!(locate_in_layout(&rows, &doc, 4), Some((1, 0)));
    }

    #[test]
    fn locate_in_layout_end_of_line_is_inclusive_on_last_segment() {
        // char 10 (== line_len) is the "cursor after last char" position — it must
        // stay on the *last* segment (row 2), matching unwrapped EOL behavior.
        let doc = doc_with("abcdefghij\n");
        let rows = build_row_layout(&doc, &plain_vp(), 3, 4, true);
        assert_eq!(locate_in_layout(&rows, &doc, 10), Some((2, 2)));
    }

    #[test]
    fn locate_in_layout_second_line_after_wrapped_first() {
        let doc = doc_with("abcdefghij\nz\n");
        let rows = build_row_layout(&doc, &plain_vp(), 4, 4, true);
        // "z" is the first char of doc_line 1, which starts at row 3.
        assert_eq!(locate_in_layout(&rows, &doc, 11), Some((3, 0)));
    }

    #[test]
    fn render_with_soft_wrap_forces_wrap_on_narrow_grid() {
        let mut grid = Grid::new(4, 3);
        let doc = doc_with("abcdefgh\n");
        let sel = Selection::point(100);
        let vp = plain_vp();
        let theme = Theme::default_dark();
        let gw = grid.width();
        DocumentView::render_with_highlights(
            &mut grid,
            &doc,
            &sel,
            &vp,
            ModeIndicator::Normal,
            0,
            3,
            0,
            gw,
            &[],
            &[],
            &theme,
            true,
        );
        let row0: String = (0..4)
            .filter_map(|c| grid.get(c, 0).map(|x| x.grapheme.clone()))
            .collect();
        let row1: String = (0..4)
            .filter_map(|c| grid.get(c, 1).map(|x| x.grapheme.clone()))
            .collect();
        assert_eq!(row0, "abcd");
        assert_eq!(row1, "efgh");
    }

    #[test]
    fn render_without_soft_wrap_still_truncates_long_lines() {
        // Regression: soft_wrap=false must behave exactly as before (truncate,
        // not wrap) — the second row shows the *next document line*, not overflow.
        let mut grid = Grid::new(4, 2);
        let doc = doc_with("abcdefgh\nZZ\n");
        let sel = Selection::point(100);
        let vp = plain_vp();
        let theme = Theme::default_dark();
        let gw = grid.width();
        DocumentView::render_with_highlights(
            &mut grid,
            &doc,
            &sel,
            &vp,
            ModeIndicator::Normal,
            0,
            2,
            0,
            gw,
            &[],
            &[],
            &theme,
            false,
        );
        let row0: String = (0..4)
            .filter_map(|c| grid.get(c, 0).map(|x| x.grapheme.clone()))
            .collect();
        let row1: String = (0..4)
            .filter_map(|c| grid.get(c, 1).map(|x| x.grapheme.clone()))
            .collect();
        assert_eq!(row0, "abcd"); // truncated, not wrapped
        assert_eq!(row1.trim_end(), "ZZ");
    }
}
