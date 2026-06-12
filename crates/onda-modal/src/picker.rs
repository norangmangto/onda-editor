//! Fuzzy-picker widget for onda-modal.
//!
//! Provides a generic `Picker` backed by [`nucleo_matcher`] for scoring, plus
//! two convenience constructors: `build_file_picker` (walks the filesystem
//! respecting `.gitignore`) and `build_buffer_picker` (wraps a list of open
//! buffer names).

use std::path::Path;

use nucleo_matcher::{
    pattern::{Atom, AtomKind, CaseMatching, Normalization},
    Config, Matcher, Utf32String,
};

// ── Data types ────────────────────────────────────────────────────────────────

/// A single item that can appear in a `Picker` list.
#[derive(Debug, Clone)]
pub struct PickerItem {
    /// Text shown to the user in the picker UI.
    pub display: String,
    /// The underlying value that is returned when an item is selected
    /// (e.g. a file path or buffer id).
    pub value: String,
}

impl PickerItem {
    pub fn new(display: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            display: display.into(),
            value: value.into(),
        }
    }
}

// ── Picker ────────────────────────────────────────────────────────────────────

/// A fuzzy-searchable list picker.
///
/// Maintains a full `items` list and a `filtered` sub-list (indices into
/// `items`) that is recomputed whenever the query changes.
pub struct Picker {
    /// All items loaded into the picker.
    items: Vec<PickerItem>,
    /// Indices of `items` that match the current `query`, sorted by score.
    filtered: Vec<usize>,
    /// Current search query typed by the user.
    query: String,
    /// Currently highlighted row within `filtered`.
    selected: usize,
    /// Whether the picker is visible.
    visible: bool,
    /// Title shown in the picker header.
    title: String,
}

impl Picker {
    // ── Constructor / lifecycle ───────────────────────────────────────────────

    /// Create a new, empty picker with the given title.  Call [`open`] to
    /// populate and show it.
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            items: Vec::new(),
            filtered: Vec::new(),
            query: String::new(),
            selected: 0,
            visible: false,
            title: title.into(),
        }
    }

    /// Populate the picker with `items` and make it visible.
    /// The query is reset to empty and all items are shown.
    pub fn open(&mut self, items: Vec<PickerItem>) {
        self.items = items;
        self.query.clear();
        self.selected = 0;
        self.visible = true;
        self.refilter();
    }

    /// Hide the picker and clear its state.
    pub fn close(&mut self) {
        self.visible = false;
        self.selected = 0;
    }

    // ── Query editing ─────────────────────────────────────────────────────────

    /// Append a character to the query and refilter.
    pub fn push_char(&mut self, c: char) {
        self.query.push(c);
        self.selected = 0;
        self.refilter();
    }

    /// Remove the last character from the query and refilter.
    pub fn pop_char(&mut self) {
        self.query.pop();
        self.selected = 0;
        self.refilter();
    }

    // ── Navigation ────────────────────────────────────────────────────────────

    /// Move the selection down by one row (wraps at the end).
    pub fn move_down(&mut self) {
        if self.filtered.is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % self.filtered.len();
    }

    /// Move the selection up by one row (wraps at the start).
    pub fn move_up(&mut self) {
        if self.filtered.is_empty() {
            return;
        }
        if self.selected == 0 {
            self.selected = self.filtered.len() - 1;
        } else {
            self.selected -= 1;
        }
    }

    // ── Accessors ─────────────────────────────────────────────────────────────

    /// Return the currently highlighted item, or `None` if the list is empty.
    pub fn selected_item(&self) -> Option<&PickerItem> {
        self.filtered.get(self.selected).map(|&i| &self.items[i])
    }

    /// The current search query string.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// An iterator over the filtered items in score order.
    pub fn filtered_items(&self) -> impl Iterator<Item = &PickerItem> {
        self.filtered.iter().map(|&i| &self.items[i])
    }

    /// Number of items currently passing the filter.
    pub fn filtered_count(&self) -> usize {
        self.filtered.len()
    }

    /// Whether the picker is currently visible.
    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// The picker title.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Index of the selected row within `filtered`.
    pub fn selected_index(&self) -> usize {
        self.selected
    }

    // ── Internal ──────────────────────────────────────────────────────────────

    /// Recompute `filtered` using nucleo-matcher fuzzy scoring.
    ///
    /// When the query is empty every item passes with a score of 0 and the
    /// original insertion order is preserved.  When the query is non-empty
    /// items are ranked by descending score; items with no match are excluded.
    fn refilter(&mut self) {
        if self.query.is_empty() {
            self.filtered = (0..self.items.len()).collect();
            return;
        }

        let mut matcher = Matcher::new(Config::DEFAULT);

        // Build a nucleo Atom from the query string.
        let atom = Atom::new(
            &self.query,
            CaseMatching::Smart,
            Normalization::Smart,
            AtomKind::Fuzzy,
            false,
        );

        let mut scored: Vec<(usize, u16)> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(idx, item)| {
                let haystack = Utf32String::from(item.display.as_str());
                let score = atom.score(haystack.slice(..), &mut matcher)?;
                Some((idx, score))
            })
            .collect();

        // Sort descending by score; ties preserve insertion order (stable via
        // sort_by which is stable).
        scored.sort_by_key(|&(_, score)| std::cmp::Reverse(score));

        self.filtered = scored.into_iter().map(|(i, _)| i).collect();
    }
}

// ── File picker ───────────────────────────────────────────────────────────────

/// Build a `Picker` populated with all files under `root`, respecting
/// `.gitignore` and other standard ignore rules via the `ignore` crate.
///
/// Each item's `display` and `value` are the path relative to `root`
/// (UTF-8 lossy).
pub fn build_file_picker(root: &Path) -> Picker {
    use ignore::WalkBuilder;

    let mut items = Vec::new();
    for entry in WalkBuilder::new(root)
        .hidden(false)   // include hidden files (user can filter with query)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .build()
        .flatten()
    {
        if entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            let path = entry.path();
            let rel = path.strip_prefix(root).unwrap_or(path);
            let display = rel.to_string_lossy().into_owned();
            items.push(PickerItem::new(display.clone(), display));
        }
    }

    let mut picker = Picker::new("Files");
    picker.open(items);
    picker
}

/// Build a `Picker` populated with the given buffer names.
pub fn build_buffer_picker(names: &[String]) -> Picker {
    let items = names
        .iter()
        .map(|n| PickerItem::new(n.clone(), n.clone()))
        .collect();

    let mut picker = Picker::new("Buffers");
    picker.open(items);
    picker
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_picker(items: &[&str]) -> Picker {
        let mut p = Picker::new("test");
        p.open(items.iter().map(|s| PickerItem::new(*s, *s)).collect());
        p
    }

    #[test]
    fn empty_query_shows_all() {
        let p = make_picker(&["alpha", "beta", "gamma"]);
        assert_eq!(p.filtered_count(), 3);
    }

    #[test]
    fn query_filters_results() {
        let mut p = make_picker(&["alpha", "beta", "gamma"]);
        p.push_char('a');
        p.push_char('l');
        // "al" should match "alpha" but not "beta" or "gamma" (fuzzy: may vary)
        let items: Vec<_> = p.filtered_items().map(|i| i.display.as_str()).collect();
        assert!(
            items.contains(&"alpha"),
            "alpha should be in results, got {items:?}"
        );
    }

    #[test]
    fn pop_char_restores_results() {
        let mut p = make_picker(&["alpha", "beta", "gamma"]);
        p.push_char('z'); // no match
        assert_eq!(p.filtered_count(), 0);
        p.pop_char();
        assert_eq!(p.filtered_count(), 3);
    }

    #[test]
    fn navigation_wraps() {
        let mut p = make_picker(&["a", "b", "c"]);
        assert_eq!(p.selected_index(), 0);
        p.move_up();
        assert_eq!(p.selected_index(), 2); // wrap to last
        p.move_down();
        assert_eq!(p.selected_index(), 0); // wrap to first
    }

    #[test]
    fn selected_item_returns_correct() {
        let p = make_picker(&["alpha", "beta"]);
        let item = p.selected_item().expect("item");
        assert_eq!(item.display, "alpha");
    }

    #[test]
    fn close_hides_picker() {
        let mut p = make_picker(&["a"]);
        assert!(p.is_visible());
        p.close();
        assert!(!p.is_visible());
    }

    #[test]
    fn build_buffer_picker_basic() {
        let names: Vec<String> = vec!["main.rs".into(), "lib.rs".into()];
        let p = build_buffer_picker(&names);
        assert_eq!(p.filtered_count(), 2);
        assert_eq!(p.title(), "Buffers");
    }
}
