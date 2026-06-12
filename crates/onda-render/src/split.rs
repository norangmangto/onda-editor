//! Window split layout for onda-render.
//!
//! Manages a binary tree of splits, computing screen [`Rect`]s for each
//! [`WindowId`] leaf and providing cursor-cycling / node-removal helpers.
//!
//! Drawing conventions
//! -------------------
//! - `:sp`  (`:split`)  → `SplitDir::Horizontal` — two panes stacked top/bottom,
//!   divided by a horizontal rule.
//! - `:vsp` (`:vsplit`) → `SplitDir::Vertical`   — two panes side by side,
//!   divided by a vertical bar.
//!
//! The naming follows Vim's terminology: "horizontal split" creates a horizontal
//! dividing line (panes above and below), "vertical split" creates a vertical
//! dividing line (panes left and right).

use crate::grid::{Cell, Color, Grid, Style};

// ── WindowId ──────────────────────────────────────────────────────────────────

/// An opaque handle for a single editor window pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WindowId(pub usize);

// ── SplitDir ──────────────────────────────────────────────────────────────────

/// Direction of the dividing line between two panes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitDir {
    /// Panes stacked top / bottom — divided by a horizontal rule (`:sp`).
    Horizontal,
    /// Panes side by side — divided by a vertical bar (`:vsp`).
    Vertical,
}

// ── Rect ──────────────────────────────────────────────────────────────────────

/// An axis-aligned rectangle in terminal cell coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    /// Column of the top-left corner (0-indexed).
    pub x: u16,
    /// Row of the top-left corner (0-indexed).
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl Rect {
    pub const fn new(x: u16, y: u16, width: u16, height: u16) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }
}

// ── Layout ────────────────────────────────────────────────────────────────────

/// A binary tree that describes how the terminal area is divided into panes.
///
/// Every internal node is a `Split`; every leaf is a single `WindowId`.
#[derive(Debug, Clone)]
pub enum Layout {
    Leaf(WindowId),
    Split {
        dir: SplitDir,
        first: Box<Layout>,
        second: Box<Layout>,
        /// Fraction of the parent's dimension allocated to `first` (0.0–1.0).
        ratio: f32,
    },
}

impl Layout {
    // ── Constructors ──────────────────────────────────────────────────────────

    /// Create a layout containing a single window.
    pub fn single(id: WindowId) -> Self {
        Layout::Leaf(id)
    }

    /// Split the current layout horizontally (top / bottom), adding `new_id`
    /// below the existing content.  The existing content takes the top half.
    pub fn split_h(self, new_id: WindowId) -> Self {
        Layout::Split {
            dir: SplitDir::Horizontal,
            first: Box::new(self),
            second: Box::new(Layout::Leaf(new_id)),
            ratio: 0.5,
        }
    }

    /// Split the current layout vertically (left / right), adding `new_id`
    /// to the right of the existing content.  The existing content takes the
    /// left half.
    pub fn split_v(self, new_id: WindowId) -> Self {
        Layout::Split {
            dir: SplitDir::Vertical,
            first: Box::new(self),
            second: Box::new(Layout::Leaf(new_id)),
            ratio: 0.5,
        }
    }

    // ── Geometry ──────────────────────────────────────────────────────────────

    /// Compute the [`Rect`] for every window in the tree, given the total
    /// terminal area `total`.
    ///
    /// The returned `Vec` is in tree-traversal order (depth-first, first child
    /// before second child).
    pub fn rects(&self, total: Rect) -> Vec<(WindowId, Rect)> {
        let mut out = Vec::new();
        self.collect_rects(total, &mut out);
        out
    }

    fn collect_rects(&self, area: Rect, out: &mut Vec<(WindowId, Rect)>) {
        match self {
            Layout::Leaf(id) => out.push((*id, area)),
            Layout::Split {
                dir,
                first,
                second,
                ratio,
            } => {
                let (r1, r2) = split_rect(area, *dir, *ratio);
                first.collect_rects(r1, out);
                second.collect_rects(r2, out);
            }
        }
    }

    // ── Window enumeration ────────────────────────────────────────────────────

    /// Return all window IDs in depth-first order.
    pub fn all_windows(&self) -> Vec<WindowId> {
        let mut out = Vec::new();
        self.collect_windows(&mut out);
        out
    }

    fn collect_windows(&self, out: &mut Vec<WindowId>) {
        match self {
            Layout::Leaf(id) => out.push(*id),
            Layout::Split { first, second, .. } => {
                first.collect_windows(out);
                second.collect_windows(out);
            }
        }
    }

    // ── Cursor cycling ────────────────────────────────────────────────────────

    /// Return the window after `current` in depth-first order, wrapping around.
    /// Returns `None` only when the layout contains no windows (should never
    /// happen in practice).
    pub fn cycle_next(&self, current: WindowId) -> Option<WindowId> {
        let windows = self.all_windows();
        if windows.is_empty() {
            return None;
        }
        let pos = windows.iter().position(|&w| w == current);
        match pos {
            None => windows.first().copied(),
            Some(i) => windows.get((i + 1) % windows.len()).copied(),
        }
    }

    /// Return the window before `current` in depth-first order, wrapping around.
    /// Returns `None` only when the layout is empty.
    pub fn cycle_prev(&self, current: WindowId) -> Option<WindowId> {
        let windows = self.all_windows();
        if windows.is_empty() {
            return None;
        }
        let pos = windows.iter().position(|&w| w == current);
        match pos {
            None => windows.last().copied(),
            Some(0) => windows.last().copied(),
            Some(i) => windows.get(i - 1).copied(),
        }
    }

    // ── Removal ───────────────────────────────────────────────────────────────

    /// Remove `target` from the layout.
    ///
    /// - Returns `None` if this node **is** `target` (the caller should drop it).
    /// - Returns `Some(simplified)` otherwise, collapsing any single-child
    ///   split nodes.
    pub fn remove(self, target: WindowId) -> Option<Layout> {
        match self {
            Layout::Leaf(id) => {
                if id == target {
                    None // this leaf is the one to remove
                } else {
                    Some(Layout::Leaf(id))
                }
            }
            Layout::Split {
                dir,
                first,
                second,
                ratio,
            } => {
                let first_new = first.remove(target);
                let second_new = second.remove(target);
                match (first_new, second_new) {
                    // Both branches survived — rebuild the split.
                    (Some(f), Some(s)) => Some(Layout::Split {
                        dir,
                        first: Box::new(f),
                        second: Box::new(s),
                        ratio,
                    }),
                    // First branch was fully removed — promote the second.
                    (None, Some(s)) => Some(s),
                    // Second branch was fully removed — promote the first.
                    (Some(f), None) => Some(f),
                    // Both gone (target appeared in both — impossible in a valid tree,
                    // but handle gracefully).
                    (None, None) => None,
                }
            }
        }
    }
}

// ── split_rect ────────────────────────────────────────────────────────────────

/// Divide `r` into two sub-rectangles along `dir`, giving `first` a fraction
/// `ratio` of the relevant dimension.
///
/// A one-cell gutter is reserved between the two panes for the border
/// character.  Each sub-rect is clamped to fit within `r`.
fn split_rect(r: Rect, dir: SplitDir, ratio: f32) -> (Rect, Rect) {
    let ratio = ratio.clamp(0.0, 1.0);
    match dir {
        SplitDir::Horizontal => {
            // Divide height; the border occupies one row.
            let usable = r.height.saturating_sub(1); // 1 row for border
            let first_h = ((usable as f32 * ratio).round() as u16).min(usable);
            let second_h = usable.saturating_sub(first_h);
            let r1 = Rect::new(r.x, r.y, r.width, first_h);
            // `r.y + first_h` is the border row; second pane starts one below.
            let r2 = Rect::new(r.x, r.y + first_h + 1, r.width, second_h);
            (r1, r2)
        }
        SplitDir::Vertical => {
            // Divide width; the border occupies one column.
            let usable = r.width.saturating_sub(1); // 1 col for border
            let first_w = ((usable as f32 * ratio).round() as u16).min(usable);
            let second_w = usable.saturating_sub(first_w);
            let r1 = Rect::new(r.x, r.y, first_w, r.height);
            // `r.x + first_w` is the border column; second pane starts one right.
            let r2 = Rect::new(r.x + first_w + 1, r.y, second_w, r.height);
            (r1, r2)
        }
    }
}

// ── Border style ──────────────────────────────────────────────────────────────

/// Style applied to split-border cells.
const BORDER_STYLE: Style = Style {
    fg: Color::DarkGray,
    bg: Color::Reset,
    attrs: crate::grid::Attribute::empty(),
};

// ── draw_borders ──────────────────────────────────────────────────────────────

/// Draw the dividing lines between all window panes onto `grid`.
///
/// For a [`SplitDir::Vertical`] split the border is a column of `│` characters
/// drawn between adjacent panes.  For a [`SplitDir::Horizontal`] split it is a
/// row of `─` characters.
///
/// `rects` is the output of [`Layout::rects`].  The `dir` parameter is used to
/// select the box-drawing character; call this function once per split node, or
/// rely on the fact that mixing directions in one call draws both axis borders
/// with sensible characters (the caller is responsible for passing the correct
/// `dir` for each split level when the tree has mixed directions).
///
/// # Usage in a mixed-split tree
///
/// Walk the [`Layout`] tree yourself and call `draw_borders` for each
/// `Split` node, passing its `dir` and only the two child rects.  For simple
/// demos it is fine to call once with all rects and the top-level `dir`.
pub fn draw_borders(grid: &mut Grid, rects: &[(WindowId, Rect)], dir: SplitDir) {
    match dir {
        SplitDir::Vertical => {
            // Find columns that sit in the gutter (between adjacent pane rects).
            // A gutter column x exists when some pane ends at x-1 and another
            // starts at x+1 (or the next pane starts at x+1).
            let mut gutter_cols: Vec<(u16, u16, u16)> = Vec::new(); // (col, y, height)
            for (_, r) in rects {
                // Right edge of this pane + 1 is a candidate gutter column.
                let gutter_col = r.x + r.width;
                // Check if another pane starts immediately after the gutter.
                let neighbor_exists = rects.iter().any(|(_, other)| other.x == gutter_col + 1);
                if neighbor_exists {
                    gutter_cols.push((gutter_col, r.y, r.height));
                }
            }
            for (col, y, height) in gutter_cols {
                for row in y..y + height {
                    grid.set(col, row, Cell::new("│", BORDER_STYLE));
                }
            }
        }
        SplitDir::Horizontal => {
            // Find rows that sit in the gutter between vertically adjacent panes.
            let mut gutter_rows: Vec<(u16, u16, u16)> = Vec::new(); // (row, x, width)
            for (_, r) in rects {
                let gutter_row = r.y + r.height;
                let neighbor_exists = rects.iter().any(|(_, other)| other.y == gutter_row + 1);
                if neighbor_exists {
                    gutter_rows.push((gutter_row, r.x, r.width));
                }
            }
            for (row, x, width) in gutter_rows {
                for col in x..x + width {
                    grid.set(col, row, Cell::new("─", BORDER_STYLE));
                }
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn w(n: usize) -> WindowId {
        WindowId(n)
    }

    // ── rects ──────────────────────────────────────────────────────────────

    #[test]
    fn single_window_fills_total() {
        let layout = Layout::single(w(0));
        let total = Rect::new(0, 0, 80, 24);
        let rects = layout.rects(total);
        assert_eq!(rects.len(), 1);
        assert_eq!(rects[0], (w(0), total));
    }

    #[test]
    fn horizontal_split_two_windows() {
        // 80x24 → top pane + 1 border row + bottom pane
        let layout = Layout::single(w(0)).split_h(w(1));
        let total = Rect::new(0, 0, 80, 24);
        let rects = layout.rects(total);

        assert_eq!(rects.len(), 2, "expected exactly two window rects");

        let (id0, r0) = rects[0];
        let (id1, r1) = rects[1];

        assert_eq!(id0, w(0));
        assert_eq!(id1, w(1));

        // Both panes share the full width.
        assert_eq!(r0.width, 80);
        assert_eq!(r1.width, 80);

        // Both panes start at column 0.
        assert_eq!(r0.x, 0);
        assert_eq!(r1.x, 0);

        // Top pane starts at row 0.
        assert_eq!(r0.y, 0);

        // usable = 24 - 1 = 23; each half = round(23 * 0.5) = 12 and 11.
        assert_eq!(r0.height, 12, "top pane height");
        assert_eq!(r1.height, 11, "bottom pane height");

        // Border row is between r0 and r1.
        assert_eq!(
            r1.y,
            r0.y + r0.height + 1,
            "bottom pane must start after 1-row border"
        );

        // Heights + border = total height.
        assert_eq!(r0.height + 1 + r1.height, 24);
    }

    #[test]
    fn vertical_split_two_windows() {
        let layout = Layout::single(w(0)).split_v(w(1));
        let total = Rect::new(0, 0, 80, 24);
        let rects = layout.rects(total);

        assert_eq!(rects.len(), 2);

        let (id0, r0) = rects[0];
        let (id1, r1) = rects[1];

        assert_eq!(id0, w(0));
        assert_eq!(id1, w(1));

        // Both panes share the full height.
        assert_eq!(r0.height, 24);
        assert_eq!(r1.height, 24);

        // usable = 80 - 1 = 79; left = round(79 * 0.5) = 40, right = 39.
        assert_eq!(r0.width, 40, "left pane width");
        assert_eq!(r1.width, 39, "right pane width");

        assert_eq!(r0.x, 0);
        assert_eq!(
            r1.x,
            r0.x + r0.width + 1,
            "right pane must start after 1-col border"
        );

        assert_eq!(r0.width + 1 + r1.width, 80);
    }

    // ── all_windows ────────────────────────────────────────────────────────

    #[test]
    fn all_windows_depth_first() {
        let layout = Layout::single(w(0)).split_h(w(1));
        let windows = layout.all_windows();
        assert_eq!(windows, vec![w(0), w(1)]);
    }

    #[test]
    fn all_windows_three_panes() {
        // Build: single(0) split_h(1) then the whole thing split_v(2)
        let layout = Layout::single(w(0)).split_h(w(1)).split_v(w(2));
        let windows = layout.all_windows();
        assert_eq!(windows, vec![w(0), w(1), w(2)]);
    }

    // ── cycle_next / cycle_prev ────────────────────────────────────────────

    #[test]
    fn cycle_next_wraps() {
        let layout = Layout::single(w(0)).split_h(w(1));
        assert_eq!(layout.cycle_next(w(0)), Some(w(1)));
        assert_eq!(layout.cycle_next(w(1)), Some(w(0))); // wrap
    }

    #[test]
    fn cycle_prev_wraps() {
        let layout = Layout::single(w(0)).split_h(w(1));
        assert_eq!(layout.cycle_prev(w(1)), Some(w(0)));
        assert_eq!(layout.cycle_prev(w(0)), Some(w(1))); // wrap
    }

    #[test]
    fn cycle_unknown_id_returns_first() {
        let layout = Layout::single(w(0)).split_h(w(1));
        assert_eq!(layout.cycle_next(w(99)), Some(w(0)));
    }

    // ── remove ─────────────────────────────────────────────────────────────

    #[test]
    fn remove_last_window_returns_none() {
        let layout = Layout::single(w(0));
        assert!(layout.remove(w(0)).is_none());
    }

    #[test]
    fn remove_promotes_sibling() {
        let layout = Layout::single(w(0)).split_h(w(1));
        // Remove first pane — second pane should be promoted to root.
        let remaining = layout.remove(w(0)).expect("should have remaining layout");
        let windows = remaining.all_windows();
        assert_eq!(windows, vec![w(1)]);
    }

    #[test]
    fn remove_second_promotes_first() {
        let layout = Layout::single(w(0)).split_h(w(1));
        let remaining = layout.remove(w(1)).expect("should have remaining layout");
        let windows = remaining.all_windows();
        assert_eq!(windows, vec![w(0)]);
    }

    #[test]
    fn remove_nonexistent_id_leaves_layout_intact() {
        let layout = Layout::single(w(0)).split_h(w(1));
        let remaining = layout.remove(w(99)).expect("layout unchanged");
        assert_eq!(remaining.all_windows(), vec![w(0), w(1)]);
    }

    // ── draw_borders ───────────────────────────────────────────────────────

    #[test]
    fn draw_borders_horizontal_places_dashes() {
        let layout = Layout::single(w(0)).split_h(w(1));
        let total = Rect::new(0, 0, 10, 6);
        let rects = layout.rects(total);
        let mut grid = crate::grid::Grid::new(10, 6);
        draw_borders(&mut grid, &rects, SplitDir::Horizontal);

        // usable = 5; first_h = round(5 * 0.5) = 3; border row = 3
        let border_row = rects[0].1.height;
        for col in 0..10 {
            let cell = grid.get(col, border_row).expect("cell should exist");
            assert_eq!(
                cell.grapheme, "─",
                "col {col} row {border_row} should be a horizontal border"
            );
        }
    }

    #[test]
    fn draw_borders_vertical_places_pipes() {
        let layout = Layout::single(w(0)).split_v(w(1));
        let total = Rect::new(0, 0, 11, 4);
        let rects = layout.rects(total);
        let mut grid = crate::grid::Grid::new(11, 4);
        draw_borders(&mut grid, &rects, SplitDir::Vertical);

        // usable = 10; first_w = round(10 * 0.5) = 5; border col = 5
        let border_col = rects[0].1.width;
        for row in 0..4 {
            let cell = grid.get(border_col, row).expect("cell should exist");
            assert_eq!(
                cell.grapheme, "│",
                "col {border_col} row {row} should be a vertical border"
            );
        }
    }
}
