use std::io::{self, BufWriter, Write};

use crossterm::{
    cursor, execute, queue,
    style::{
        Attribute as CAttribute, Color as CColor, Print, ResetColor, SetAttribute,
        SetBackgroundColor, SetForegroundColor,
    },
    terminal::{
        self, disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
    },
};
use thiserror::Error;

use crate::grid::{Attribute, Cell, Color, Style};

#[derive(Debug, Error)]
pub enum RenderError {
    #[error("IO error: {0}")]
    Io(#[from] io::Error),
}

/// Cursor shape hint (for mode indication).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorShape {
    /// Blinking block (normal mode)
    Block,
    /// Blinking bar (insert mode)
    Bar,
    /// Underline
    Underline,
}

/// Abstraction over a terminal output surface.
///
/// The `NullBackend` implements this trait for testing and benchmarking without
/// a real terminal.
pub trait Backend {
    fn draw_cells<'a>(
        &mut self,
        cells: impl Iterator<Item = (u16, u16, &'a Cell)>,
    ) -> Result<(), RenderError>;
    fn hide_cursor(&mut self) -> Result<(), RenderError>;
    fn show_cursor(&mut self, shape: CursorShape) -> Result<(), RenderError>;
    fn set_cursor_position(&mut self, col: u16, row: u16) -> Result<(), RenderError>;
    fn flush(&mut self) -> Result<(), RenderError>;
    fn size(&self) -> (u16, u16);
    fn clear(&mut self) -> Result<(), RenderError>;
}

// ── TerminalBackend ────────────────────────────────────────────────────────────

/// Crossterm-based backend for a real terminal.
pub struct TerminalBackend {
    out: BufWriter<io::Stdout>,
    width: u16,
    height: u16,
}

impl TerminalBackend {
    pub fn new() -> Result<Self, RenderError> {
        let (width, height) = terminal::size()?;
        Ok(Self {
            out: BufWriter::with_capacity(64 * 1024, io::stdout()),
            width,
            height,
        })
    }

    /// Enter raw mode + alternate screen. Also registers a panic hook to restore
    /// the terminal if we crash (so the shell is never left broken).
    pub fn enter(&mut self) -> Result<(), RenderError> {
        enable_raw_mode()?;
        execute!(self.out, EnterAlternateScreen, cursor::Hide)?;
        self.install_panic_hook();
        Ok(())
    }

    /// Restore the terminal to its prior state.
    pub fn leave(&mut self) -> Result<(), RenderError> {
        execute!(self.out, LeaveAlternateScreen, cursor::Show)?;
        disable_raw_mode()?;
        Ok(())
    }

    pub fn update_size(&mut self) -> Result<(), RenderError> {
        let (w, h) = terminal::size()?;
        self.width = w;
        self.height = h;
        Ok(())
    }

    fn install_panic_hook(&self) {
        let original = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            // Best-effort restore: ignore errors since we're already panicking.
            let _ = disable_raw_mode();
            let _ = execute!(io::stdout(), LeaveAlternateScreen, cursor::Show);
            original(info);
        }));
    }

    fn crossterm_color(color: Color) -> CColor {
        match color {
            Color::Reset => CColor::Reset,
            Color::Black => CColor::Black,
            Color::Red => CColor::DarkRed,
            Color::Green => CColor::DarkGreen,
            Color::Yellow => CColor::DarkYellow,
            Color::Blue => CColor::DarkBlue,
            Color::Magenta => CColor::DarkMagenta,
            Color::Cyan => CColor::DarkCyan,
            Color::White => CColor::White,
            Color::DarkGray => CColor::DarkGrey,
            Color::LightRed => CColor::Red,
            Color::LightGreen => CColor::Green,
            Color::LightYellow => CColor::Yellow,
            Color::LightBlue => CColor::Blue,
            Color::LightMagenta => CColor::Magenta,
            Color::LightCyan => CColor::Cyan,
            Color::Gray => CColor::Grey,
            Color::Rgb(r, g, b) => CColor::Rgb { r, g, b },
            Color::Indexed(i) => CColor::AnsiValue(i),
        }
    }

    fn apply_style(&mut self, style: &Style) -> io::Result<()> {
        queue!(self.out, ResetColor)?;
        if style.fg != Color::Reset {
            queue!(
                self.out,
                SetForegroundColor(Self::crossterm_color(style.fg))
            )?;
        }
        if style.bg != Color::Reset {
            queue!(
                self.out,
                SetBackgroundColor(Self::crossterm_color(style.bg))
            )?;
        }
        if style.attrs.contains(Attribute::BOLD) {
            queue!(self.out, SetAttribute(CAttribute::Bold))?;
        }
        if style.attrs.contains(Attribute::ITALIC) {
            queue!(self.out, SetAttribute(CAttribute::Italic))?;
        }
        if style.attrs.contains(Attribute::UNDERLINE) {
            queue!(self.out, SetAttribute(CAttribute::Underlined))?;
        }
        if style.attrs.contains(Attribute::REVERSE) {
            queue!(self.out, SetAttribute(CAttribute::Reverse))?;
        }
        Ok(())
    }
}

impl Backend for TerminalBackend {
    fn draw_cells<'a>(
        &mut self,
        cells: impl Iterator<Item = (u16, u16, &'a Cell)>,
    ) -> Result<(), RenderError> {
        let mut last_pos: Option<(u16, u16)> = None;
        let mut last_style: Option<Style> = None;

        for (col, row, cell) in cells {
            // Only move cursor when not at the expected next position
            if last_pos != Some((col, row)) {
                queue!(self.out, cursor::MoveTo(col, row))?;
            }

            // Only re-emit style when it changes
            if last_style != Some(cell.style) {
                self.apply_style(&cell.style)?;
                last_style = Some(cell.style);
            }

            queue!(self.out, Print(&cell.grapheme))?;
            last_pos = Some((col + cell.width as u16, row));
        }
        Ok(())
    }

    fn hide_cursor(&mut self) -> Result<(), RenderError> {
        queue!(self.out, cursor::Hide)?;
        Ok(())
    }

    fn show_cursor(&mut self, shape: CursorShape) -> Result<(), RenderError> {
        let ct_shape = match shape {
            CursorShape::Block => cursor::SetCursorStyle::SteadyBlock,
            CursorShape::Bar => cursor::SetCursorStyle::BlinkingBar,
            CursorShape::Underline => cursor::SetCursorStyle::SteadyUnderScore,
        };
        queue!(self.out, cursor::Show, ct_shape)?;
        Ok(())
    }

    fn set_cursor_position(&mut self, col: u16, row: u16) -> Result<(), RenderError> {
        queue!(self.out, cursor::MoveTo(col, row))?;
        Ok(())
    }

    fn flush(&mut self) -> Result<(), RenderError> {
        self.out.flush()?;
        Ok(())
    }

    fn size(&self) -> (u16, u16) {
        (self.width, self.height)
    }

    fn clear(&mut self) -> Result<(), RenderError> {
        queue!(self.out, terminal::Clear(terminal::ClearType::All))?;
        Ok(())
    }
}

// ── NullBackend ────────────────────────────────────────────────────────────────

/// A backend that discards all output. Used for testing and benchmarking.
pub struct NullBackend {
    pub width: u16,
    pub height: u16,
    /// Number of cells drawn across all flush calls (for bench verification).
    pub cells_drawn: usize,
    /// Number of flush calls.
    pub flush_count: usize,
    /// Last cursor position.
    pub cursor_pos: (u16, u16),
    pub cursor_visible: bool,
}

impl NullBackend {
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            width,
            height,
            cells_drawn: 0,
            flush_count: 0,
            cursor_pos: (0, 0),
            cursor_visible: false,
        }
    }

    pub fn reset_stats(&mut self) {
        self.cells_drawn = 0;
        self.flush_count = 0;
    }
}

impl Backend for NullBackend {
    fn draw_cells<'a>(
        &mut self,
        cells: impl Iterator<Item = (u16, u16, &'a Cell)>,
    ) -> Result<(), RenderError> {
        self.cells_drawn += cells.count();
        Ok(())
    }

    fn hide_cursor(&mut self) -> Result<(), RenderError> {
        self.cursor_visible = false;
        Ok(())
    }

    fn show_cursor(&mut self, _shape: CursorShape) -> Result<(), RenderError> {
        self.cursor_visible = true;
        Ok(())
    }

    fn set_cursor_position(&mut self, col: u16, row: u16) -> Result<(), RenderError> {
        self.cursor_pos = (col, row);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), RenderError> {
        self.flush_count += 1;
        Ok(())
    }

    fn size(&self) -> (u16, u16) {
        (self.width, self.height)
    }

    fn clear(&mut self) -> Result<(), RenderError> {
        Ok(())
    }
}
