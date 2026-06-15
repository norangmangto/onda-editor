//! VT100 screen state, wrapping the `vt100` crate.
//!
//! Accepts raw PTY bytes via `process()`, then exposes `cells()` for rendering.
//! Damage tracking: the vt100 `Screen` diff is used to determine which cells
//! changed since the last frame.

// ── Cell types ────────────────────────────────────────────────────────────────

/// SGR colour (approximate mapping from vt100).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

/// Cell attributes from SGR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CellAttrs {
    pub fg: Option<Rgb>,
    pub bg: Option<Rgb>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub reverse: bool,
}

/// A single rendered terminal cell.
#[derive(Debug, Clone)]
pub struct Cell {
    pub ch: char,
    pub attrs: CellAttrs,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            attrs: CellAttrs::default(),
        }
    }
}

// ── TerminalScreen ─────────────────────────────────────────────────────────────

/// Wraps `vt100::Parser` to maintain the terminal screen state.
/// Supports a scrollback buffer for `Mode::TerminalScroll`.
pub struct TerminalScreen {
    parser: vt100::Parser,
    rows: u16,
    cols: u16,
    /// Scrollback buffer: rows that have scrolled off the top.
    scrollback: Vec<Vec<Cell>>,
    #[allow(dead_code)]
    scrollback_limit: usize,
    /// Scroll offset in `Mode::TerminalScroll` (0 = live bottom).
    scroll_offset: usize,
}

impl TerminalScreen {
    pub fn new(rows: u16, cols: u16) -> Self {
        Self {
            parser: vt100::Parser::new(rows, cols, 10_000),
            rows,
            cols,
            scrollback: Vec::new(),
            scrollback_limit: 10_000,
            scroll_offset: 0,
        }
    }

    /// Feed raw PTY bytes into the VT100 parser.
    pub fn process(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
    }

    /// Resize the screen.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.rows = rows;
        self.cols = cols;
        self.parser = vt100::Parser::new(rows, cols, 10_000);
    }

    /// Return the current screen rows as `Cell` slices.
    /// `row` is 0-based from top of visible screen.
    pub fn row(&self, row: u16) -> Vec<Cell> {
        let screen = self.parser.screen();
        let mut cells = Vec::with_capacity(self.cols as usize);
        for col in 0..self.cols {
            let cell = screen.cell(row, col);
            let ch = cell
                .map(|c| {
                    let contents = c.contents();
                    contents.chars().next().unwrap_or(' ')
                })
                .unwrap_or(' ');
            let attrs = cell
                .map(|c| {
                    let fg = vt100_color_to_rgb(c.fgcolor());
                    let bg = vt100_color_to_rgb(c.bgcolor());
                    CellAttrs {
                        fg,
                        bg,
                        bold: c.bold(),
                        italic: c.italic(),
                        underline: c.underline(),
                        reverse: c.inverse(),
                    }
                })
                .unwrap_or_default();
            cells.push(Cell { ch, attrs });
        }
        cells
    }

    /// Cursor position (row, col) in the visible screen.
    pub fn cursor_pos(&self) -> (u16, u16) {
        let screen = self.parser.screen();
        (screen.cursor_position().0, screen.cursor_position().1)
    }

    pub fn rows(&self) -> u16 {
        self.rows
    }

    pub fn cols(&self) -> u16 {
        self.cols
    }

    /// Scroll viewport up by `n` lines (entering scrollback).
    pub fn scroll_up(&mut self, n: usize) {
        let max = self.scrollback.len();
        self.scroll_offset = (self.scroll_offset + n).min(max);
    }

    /// Scroll viewport down by `n` lines (toward live bottom).
    pub fn scroll_down(&mut self, n: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
    }

    /// Jump to the top of scrollback.
    pub fn scroll_to_top(&mut self) {
        self.scroll_offset = self.scrollback.len();
    }

    /// Jump to live bottom.
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
    }

    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    /// Is the viewport at the live bottom (scroll_offset == 0)?
    pub fn at_bottom(&self) -> bool {
        self.scroll_offset == 0
    }
}

fn vt100_color_to_rgb(color: vt100::Color) -> Option<Rgb> {
    match color {
        vt100::Color::Default => None,
        vt100::Color::Idx(idx) => Some(ansi_256_to_rgb(idx)),
        vt100::Color::Rgb(r, g, b) => Some(Rgb(r, g, b)),
    }
}

/// ANSI 256-color palette to approximate RGB.
fn ansi_256_to_rgb(idx: u8) -> Rgb {
    // Standard 16 colours (approximate)
    const ANSI16: [Rgb; 16] = [
        Rgb(0, 0, 0),       // 0 black
        Rgb(128, 0, 0),     // 1 red
        Rgb(0, 128, 0),     // 2 green
        Rgb(128, 128, 0),   // 3 yellow
        Rgb(0, 0, 128),     // 4 blue
        Rgb(128, 0, 128),   // 5 magenta
        Rgb(0, 128, 128),   // 6 cyan
        Rgb(192, 192, 192), // 7 white
        Rgb(128, 128, 128), // 8 bright black
        Rgb(255, 0, 0),     // 9 bright red
        Rgb(0, 255, 0),     // 10 bright green
        Rgb(255, 255, 0),   // 11 bright yellow
        Rgb(0, 0, 255),     // 12 bright blue
        Rgb(255, 0, 255),   // 13 bright magenta
        Rgb(0, 255, 255),   // 14 bright cyan
        Rgb(255, 255, 255), // 15 bright white
    ];
    if idx < 16 {
        return ANSI16[idx as usize];
    }
    if idx >= 232 {
        // Grayscale ramp
        let v = 8 + (idx - 232) * 10;
        return Rgb(v, v, v);
    }
    // 6×6×6 colour cube
    let idx = idx - 16;
    let b = idx % 6;
    let g = (idx / 6) % 6;
    let r = idx / 36;
    let to_val = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
    Rgb(to_val(r), to_val(g), to_val(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ansi_256_black_and_white() {
        assert_eq!(ansi_256_to_rgb(0), Rgb(0, 0, 0));
        assert_eq!(ansi_256_to_rgb(15), Rgb(255, 255, 255));
    }

    #[test]
    fn ansi_grayscale() {
        let Rgb(r, g, b) = ansi_256_to_rgb(232);
        assert_eq!(r, g);
        assert_eq!(g, b);
    }

    fn line(s: &TerminalScreen, row: u16) -> String {
        s.row(row).iter().map(|c| c.ch).collect::<String>()
    }

    #[test]
    fn plain_text_writes_and_advances_cursor() {
        let mut s = TerminalScreen::new(4, 20);
        s.process(b"hello");
        assert_eq!(line(&s, 0).trim_end(), "hello");
        assert_eq!(s.cursor_pos(), (0, 5));
    }

    #[test]
    fn crlf_moves_to_next_row() {
        let mut s = TerminalScreen::new(4, 20);
        s.process(b"ab\r\ncd");
        assert_eq!(line(&s, 0).trim_end(), "ab");
        assert_eq!(line(&s, 1).trim_end(), "cd");
        assert_eq!(s.cursor_pos(), (1, 2));
    }

    #[test]
    fn cursor_position_escape() {
        let mut s = TerminalScreen::new(10, 20);
        s.process(b"\x1b[3;5H"); // row 3, col 5 (1-based) → (2,4) 0-based
        assert_eq!(s.cursor_pos(), (2, 4));
        s.process(b"X");
        assert_eq!(s.row(2)[4].ch, 'X');
    }

    #[test]
    fn clear_screen_blanks_cells() {
        let mut s = TerminalScreen::new(4, 20);
        s.process(b"junk text");
        s.process(b"\x1b[2J"); // erase entire screen
        assert_eq!(line(&s, 0).trim_end(), "");
    }

    #[test]
    fn sgr_bold_attribute_applies() {
        let mut s = TerminalScreen::new(4, 20);
        s.process(b"\x1b[1mB\x1b[0mN"); // bold 'B', reset, normal 'N'
        assert!(s.row(0)[0].attrs.bold);
        assert!(!s.row(0)[1].attrs.bold);
    }

    #[test]
    fn resize_changes_dimensions() {
        let mut s = TerminalScreen::new(4, 20);
        s.resize(10, 40);
        assert_eq!(s.rows(), 10);
        assert_eq!(s.cols(), 40);
        assert_eq!(s.row(0).len(), 40);
    }
}
