use bitflags::bitflags;
use unicode_width::UnicodeWidthStr;

/// ANSI/true-color value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Color {
    #[default]
    Reset,
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    White,
    DarkGray,
    LightRed,
    LightGreen,
    LightYellow,
    LightBlue,
    LightMagenta,
    LightCyan,
    Gray,
    Rgb(u8, u8, u8),
    Indexed(u8),
}

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct Attribute: u8 {
        const BOLD      = 0b0000_0001;
        const DIM       = 0b0000_0010;
        const ITALIC    = 0b0000_0100;
        const UNDERLINE = 0b0000_1000;
        const REVERSE   = 0b0001_0000;
    }
}

/// Visual style for a cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Style {
    pub fg: Color,
    pub bg: Color,
    pub attrs: Attribute,
}

impl Style {
    pub const RESET: Style = Style {
        fg: Color::Reset,
        bg: Color::Reset,
        attrs: Attribute::empty(),
    };

    pub fn fg(mut self, color: Color) -> Self {
        self.fg = color;
        self
    }

    pub fn bg(mut self, color: Color) -> Self {
        self.bg = color;
        self
    }

    pub fn bold(mut self) -> Self {
        self.attrs |= Attribute::BOLD;
        self
    }

    pub fn italic(mut self) -> Self {
        self.attrs |= Attribute::ITALIC;
        self
    }

    pub fn reversed(mut self) -> Self {
        self.attrs |= Attribute::REVERSE;
        self
    }
}

/// A single terminal cell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    /// The grapheme cluster displayed in this cell (usually a single char).
    pub grapheme: String,
    /// Display width (1 for normal, 2 for CJK wide chars).
    pub width: u8,
    pub style: Style,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            grapheme: " ".to_string(),
            width: 1,
            style: Style::RESET,
        }
    }
}

impl Cell {
    pub fn new(grapheme: impl Into<String>, style: Style) -> Self {
        let grapheme = grapheme.into();
        let width = grapheme.width().max(1) as u8;
        Self {
            grapheme,
            width,
            style,
        }
    }

    pub fn blank(style: Style) -> Self {
        Self {
            grapheme: " ".to_string(),
            width: 1,
            style,
        }
    }

    /// The trailing column occupied by a wide (width-2) grapheme. It carries
    /// `width: 0` as a sentinel: the backend skips it when flushing (the wide
    /// grapheme already paints both columns), but it still participates in the
    /// damage diff so that when the wide char is replaced by narrow content the
    /// stale right-half is redrawn (otherwise it ghosts).
    pub fn wide_continuation(style: Style) -> Self {
        Self {
            grapheme: " ".to_string(),
            width: 0,
            style,
        }
    }

    pub fn set_grapheme(&mut self, g: impl Into<String>) {
        let g = g.into();
        self.width = g.width().max(1) as u8;
        self.grapheme = g;
    }
}

/// A 2-D grid of cells, indexed by `(col, row)` (x, y).
#[derive(Debug, Clone)]
pub struct Grid {
    width: u16,
    height: u16,
    cells: Vec<Cell>,
}

impl Grid {
    pub fn new(width: u16, height: u16) -> Self {
        let cells = vec![Cell::default(); (width as usize) * (height as usize)];
        Self {
            width,
            height,
            cells,
        }
    }

    pub fn width(&self) -> u16 {
        self.width
    }

    pub fn height(&self) -> u16 {
        self.height
    }

    #[inline]
    fn idx(&self, col: u16, row: u16) -> usize {
        row as usize * self.width as usize + col as usize
    }

    pub fn get(&self, col: u16, row: u16) -> Option<&Cell> {
        if col >= self.width || row >= self.height {
            return None;
        }
        Some(&self.cells[self.idx(col, row)])
    }

    pub fn get_mut(&mut self, col: u16, row: u16) -> Option<&mut Cell> {
        if col >= self.width || row >= self.height {
            return None;
        }
        let idx = self.idx(col, row);
        Some(&mut self.cells[idx])
    }

    pub fn set(&mut self, col: u16, row: u16, cell: Cell) {
        if col >= self.width || row >= self.height {
            return;
        }
        let idx = self.idx(col, row);
        self.cells[idx] = cell;
    }

    /// Write a string starting at `(col, row)`, advancing horizontally.
    /// Returns the column after the last written character.
    pub fn write_str(&mut self, col: u16, row: u16, s: &str, style: Style) -> u16 {
        use unicode_width::UnicodeWidthChar;
        let mut x = col;
        for ch in s.chars() {
            if x >= self.width {
                break;
            }
            let w = ch.width().unwrap_or(1);
            self.set(
                x,
                row,
                Cell {
                    grapheme: ch.to_string(),
                    width: w as u8,
                    style,
                },
            );
            // Mark the wide char's trailing column as a continuation sentinel so a
            // later narrow overwrite redraws it (avoids ghosting the right half).
            if w == 2 && x + 1 < self.width {
                self.set(x + 1, row, Cell::wide_continuation(style));
            }
            x += w as u16;
        }
        x
    }

    /// Fill a rectangular region with blanks of `style`.
    pub fn fill_rect(&mut self, col: u16, row: u16, w: u16, h: u16, style: Style) {
        for r in row..row.saturating_add(h).min(self.height) {
            for c in col..col.saturating_add(w).min(self.width) {
                self.set(c, r, Cell::blank(style));
            }
        }
    }

    /// Resize the grid, filling new cells with default.
    pub fn resize(&mut self, width: u16, height: u16) {
        self.width = width;
        self.height = height;
        self.cells
            .resize((width as usize) * (height as usize), Cell::default());
    }

    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }

    /// Produce a list of `(col, row, cell)` pairs that differ between `self` and `previous`.
    pub fn diff<'a>(&'a self, previous: &'a Grid) -> impl Iterator<Item = (u16, u16, &'a Cell)> {
        let w = self.width as usize;
        let h = self.height as usize;
        let prev_cells = &previous.cells;
        self.cells
            .iter()
            .enumerate()
            .filter_map(move |(idx, cell)| {
                let col = (idx % w) as u16;
                let row = (idx / w) as u16;
                if row >= h as u16 {
                    return None;
                }
                let prev = prev_cells.get(idx)?;
                if cell != prev {
                    Some((col, row, cell))
                } else {
                    None
                }
            })
    }
}

/// Double-buffer compositor. Keeps a "current" and "previous" grid; flushing only
/// sends the diff to the backend.
pub struct DoubleBuffer {
    current: Grid,
    previous: Grid,
}

impl DoubleBuffer {
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            current: Grid::new(width, height),
            previous: Grid::new(width, height),
        }
    }

    pub fn current(&self) -> &Grid {
        &self.current
    }

    pub fn current_mut(&mut self) -> &mut Grid {
        &mut self.current
    }

    /// Mark all cells as dirty (force full redraw on next flush).
    ///
    /// Fills `previous` with a sentinel that differs from any real drawn cell so
    /// `diff()` reports every position as changed.
    pub fn invalidate(&mut self) {
        let sentinel = Cell {
            grapheme: "\x00".to_string(),
            width: 1,
            style: Style::RESET,
        };
        for cell in self.previous.cells.iter_mut() {
            cell.clone_from(&sentinel);
        }
    }

    /// Resize both grids.
    pub fn resize(&mut self, width: u16, height: u16) {
        self.current.resize(width, height);
        self.previous.resize(width, height);
    }

    /// Record the flushed frame: copy current → previous so diff() returns 0
    /// until the next frame's writes diverge from the just-sent state.
    pub fn commit(&mut self) {
        self.previous.cells.clone_from(&self.current.cells);
    }

    /// Iterate cells that changed since last commit.
    pub fn diff(&self) -> impl Iterator<Item = (u16, u16, &Cell)> {
        self.current.diff(&self.previous)
    }

    /// Count of cells that changed (for debug overlay).
    pub fn diff_count(&self) -> usize {
        self.diff().count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_set_get() {
        let mut grid = Grid::new(10, 5);
        let style = Style::default().fg(Color::Red);
        grid.set(3, 2, Cell::new("A", style));
        let cell = grid.get(3, 2).unwrap();
        assert_eq!(cell.grapheme, "A");
        assert_eq!(cell.style.fg, Color::Red);
    }

    #[test]
    fn double_buffer_diff_count() {
        let mut db = DoubleBuffer::new(10, 5);
        db.current_mut().set(0, 0, Cell::new("X", Style::default()));
        assert_eq!(db.diff().count(), 1);
        db.commit();
        // After commit, current is cleared; no diff
        assert_eq!(db.diff().count(), 0);
    }

    #[test]
    fn write_str_clips() {
        let mut grid = Grid::new(5, 1);
        let col = grid.write_str(3, 0, "HELLO", Style::default());
        assert!(col <= 5);
        assert_eq!(grid.get(3, 0).unwrap().grapheme, "H");
        assert_eq!(grid.get(4, 0).unwrap().grapheme, "E");
    }

    #[test]
    fn double_buffer_invalidate() {
        let mut db = DoubleBuffer::new(5, 5);
        db.commit(); // previous = current
        assert_eq!(db.diff().count(), 0);
        db.invalidate();
        // Now all cells are "changed"
        assert!(db.diff().count() > 0);
    }

    #[test]
    fn wide_char_marks_continuation_column() {
        let mut g = Grid::new(10, 1);
        g.write_str(0, 0, "가", Style::default()); // width-2 grapheme
        assert_eq!(g.get(0, 0).unwrap().width, 2);
        // Trailing column is a width-0 sentinel, not a normal space.
        assert_eq!(g.get(1, 0).unwrap().width, 0);
    }

    #[test]
    fn replacing_wide_char_with_narrow_redraws_right_half() {
        // Regression: a wide char (cols 0–1) replaced by a narrow char at col 0
        // must register the trailing column as changed so the terminal clears the
        // stale right half instead of ghosting it.
        let mut g = Grid::new(10, 1);
        g.write_str(0, 0, "가", Style::default());
        let prev = g.clone(); // the wide-char frame

        // Now the line is just "x": narrow char + cleared tail.
        g.set(0, 0, Cell::new("x", Style::default()));
        g.fill_rect(1, 0, g.width() - 1, 1, Style::RESET);

        let changed: Vec<(u16, u16)> = g.diff(&prev).map(|(c, r, _)| (c, r)).collect();
        assert!(
            changed.contains(&(1, 0)),
            "continuation column must be redrawn, got {changed:?}"
        );
    }
}
