use std::{
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU32, Ordering},
};

use ropey::Rope;
use thiserror::Error;
use unicode_segmentation::UnicodeSegmentation;

use crate::transaction::{ChangeSet, Transaction};

static NEXT_ID: AtomicU32 = AtomicU32::new(1);

/// Stable identifier for an open document (never reused).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DocumentId(u32);

impl DocumentId {
    pub fn new() -> Self {
        Self(NEXT_ID.fetch_add(1, Ordering::Relaxed))
    }
}

impl Default for DocumentId {
    fn default() -> Self {
        Self::new()
    }
}

/// Line-ending convention detected/preserved for a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEnding {
    Lf,
    CrLf,
}

impl LineEnding {
    pub fn as_str(self) -> &'static str {
        match self {
            LineEnding::Lf => "\n",
            LineEnding::CrLf => "\r\n",
        }
    }

    fn detect(text: &str) -> Self {
        if text.contains("\r\n") {
            LineEnding::CrLf
        } else {
            LineEnding::Lf
        }
    }
}

#[derive(Debug, Error)]
pub enum DocumentError {
    #[error("IO error: {0}")]
    Io(#[from] io::Error),
    #[error("changeset error: {0}")]
    ChangeSet(#[from] crate::transaction::ChangeSetError),
    #[error("document is read-only")]
    ReadOnly,
}

/// An open text document backed by a [`Rope`].
///
/// All mutations must go through [`Document::apply`] — direct rope mutation is not exposed.
pub struct Document {
    id: DocumentId,
    rope: Rope,
    path: Option<PathBuf>,
    line_ending: LineEnding,
    /// `true` when the in-memory content differs from what's on disk.
    modified: bool,
    /// If the file was loaded with lossy UTF-8 conversion, this is set.
    pub lossy: bool,
    /// Monotonic counter bumped on every applied [`Transaction`]. Lets callers
    /// cheaply detect whether a command mutated the buffer (e.g. dot-repeat).
    rev: u64,
}

impl Document {
    /// Create a new empty document (no path, not modified).
    pub fn new_empty() -> Self {
        Self {
            id: DocumentId::new(),
            rope: Rope::new(),
            path: None,
            line_ending: LineEnding::Lf,
            modified: false,
            lossy: false,
            rev: 0,
        }
    }

    /// Open a file from disk synchronously.
    ///
    /// UTF-8 is assumed; falls back to lossy conversion and sets `self.lossy = true`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, DocumentError> {
        let path = path.as_ref();
        let mut file = std::fs::File::open(path)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;

        let (text, lossy) = match String::from_utf8(bytes.clone()) {
            Ok(s) => (s, false),
            Err(_) => (String::from_utf8_lossy(&bytes).into_owned(), true),
        };

        let line_ending = LineEnding::detect(&text);
        let rope = Rope::from_str(&text);

        Ok(Self {
            id: DocumentId::new(),
            rope,
            path: Some(path.to_path_buf()),
            line_ending,
            modified: false,
            lossy,
            rev: 0,
        })
    }

    /// Save the document to its current path atomically (write temp, rename).
    pub fn save(&self) -> Result<(), DocumentError> {
        let path = self.path.as_deref().ok_or_else(|| {
            DocumentError::Io(io::Error::new(io::ErrorKind::NotFound, "no path set"))
        })?;
        self.save_to(path)
    }

    /// Save the document to a specific path atomically.
    pub fn save_to(&self, path: impl AsRef<Path>) -> Result<(), DocumentError> {
        let path = path.as_ref();
        let dir = path.parent().unwrap_or(Path::new("."));
        let tmp_path = dir.join(format!(
            ".onda-{}.tmp",
            path.file_name().unwrap_or_default().to_string_lossy()
        ));

        {
            let mut file = std::fs::File::create(&tmp_path)?;
            for chunk in self.rope.chunks() {
                file.write_all(chunk.as_bytes())?;
            }
            file.flush()?;
            file.sync_all()?;
        }

        std::fs::rename(tmp_path, path)?;
        Ok(())
    }

    /// Apply a transaction, mutating the document.
    ///
    /// Returns the inverse transaction (for undo).
    pub fn apply(&mut self, tx: &Transaction) -> Result<Transaction, DocumentError> {
        let inverse = tx.invert(&self.rope);
        tx.changes.apply(&mut self.rope)?;
        self.modified = true;
        self.rev = self.rev.wrapping_add(1);
        Ok(inverse)
    }

    /// Monotonic revision counter (bumped on every applied transaction).
    pub fn rev(&self) -> u64 {
        self.rev
    }

    // ── Accessors ──────────────────────────────────────────────────────────

    pub fn id(&self) -> DocumentId {
        self.id
    }

    pub fn rope(&self) -> &Rope {
        &self.rope
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn set_path(&mut self, path: PathBuf) {
        self.path = Some(path);
    }

    pub fn line_ending(&self) -> LineEnding {
        self.line_ending
    }

    pub fn is_modified(&self) -> bool {
        self.modified
    }

    pub fn mark_saved(&mut self) {
        self.modified = false;
    }

    /// Total character count.
    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    /// Total line count.
    pub fn len_lines(&self) -> usize {
        self.rope.len_lines()
    }

    /// Display name (filename or "[No Name]").
    pub fn name(&self) -> &str {
        self.path
            .as_deref()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("[No Name]")
    }

    // ── Position utilities ──────────────────────────────────────────────────

    /// Convert a char index to a (line, grapheme_col) pair.
    ///
    /// `grapheme_col` counts Unicode grapheme clusters (not bytes or chars).
    pub fn char_to_visual_pos(&self, char_idx: usize) -> (usize, usize) {
        let char_idx = char_idx.min(self.rope.len_chars());
        let line = self.rope.char_to_line(char_idx);
        let line_start = self.rope.line_to_char(line);
        let line_slice: String = self.rope.slice(line_start..char_idx).to_string();
        let col = line_slice.graphemes(true).count();
        (line, col)
    }

    /// Convert a char index to a (line, display_col) pair, where `display_col` sums
    /// the terminal cell widths of preceding characters on the line (wide/CJK glyphs
    /// count as 2). Use this for cursor placement; [`char_to_visual_pos`] returns a
    /// grapheme count, which under-counts columns when wide characters precede the
    /// cursor.
    pub fn char_to_display_col(&self, char_idx: usize) -> (usize, usize) {
        use unicode_width::UnicodeWidthChar;
        let char_idx = char_idx.min(self.rope.len_chars());
        let line = self.rope.char_to_line(char_idx);
        let line_start = self.rope.line_to_char(line);
        let col: usize = self
            .rope
            .slice(line_start..char_idx)
            .chars()
            .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
            .sum();
        (line, col)
    }

    /// Convert a (line, grapheme_col) pair to a char index.
    pub fn visual_pos_to_char(&self, line: usize, col: usize) -> usize {
        let line = line.min(self.rope.len_lines().saturating_sub(1));
        let line_start = self.rope.line_to_char(line);
        let line_str: String = self.rope.line(line).to_string();
        let mut offset = 0usize;
        for (i, g) in line_str.graphemes(true).enumerate() {
            if i >= col {
                break;
            }
            offset += g.chars().count();
        }
        (line_start + offset).min(self.rope.len_chars())
    }

    /// First char index of `line`.
    pub fn line_to_char(&self, line: usize) -> usize {
        self.rope.line_to_char(line.min(self.rope.len_lines()))
    }

    /// Line that contains `char_idx`.
    pub fn char_to_line(&self, char_idx: usize) -> usize {
        self.rope.char_to_line(char_idx.min(self.rope.len_chars()))
    }

    /// Length of `line` in characters (including the line ending).
    pub fn line_len_chars(&self, line: usize) -> usize {
        if line >= self.rope.len_lines() {
            return 0;
        }
        self.rope.line(line).len_chars()
    }

    /// Length of `line` in characters, excluding the trailing newline.
    pub fn line_len_no_eol(&self, line: usize) -> usize {
        let len = self.line_len_chars(line);
        if len == 0 {
            return 0;
        }
        let last = self.rope.line(line).char(len - 1);
        if last == '\n' {
            len.saturating_sub(if len >= 2 && self.rope.line(line).char(len - 2) == '\r' {
                2
            } else {
                1
            })
        } else {
            len
        }
    }

    /// Build a [`ChangeSet`] that inserts `text` at `char_idx`.
    pub fn changeset_insert(&self, char_idx: usize, text: &str) -> ChangeSet {
        crate::transaction::ChangeSetBuilder::new(self.rope.len_chars())
            .retain(char_idx)
            .insert(text)
            .retain(self.rope.len_chars() - char_idx)
            .build()
    }

    /// Build a [`ChangeSet`] that deletes `from..to`.
    pub fn changeset_delete(&self, from: usize, to: usize) -> ChangeSet {
        let from = from.min(self.rope.len_chars());
        let to = to.min(self.rope.len_chars());
        if from >= to {
            return ChangeSet::new(self.rope.len_chars());
        }
        crate::transaction::ChangeSetBuilder::new(self.rope.len_chars())
            .retain(from)
            .delete(to - from)
            .retain(self.rope.len_chars() - to)
            .build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn doc_from_str(s: &str) -> Document {
        let mut d = Document::new_empty();
        let cs = crate::transaction::ChangeSetBuilder::new(0)
            .insert(s)
            .build();
        let tx = Transaction::new(cs);
        d.apply(&tx).unwrap();
        d.modified = false;
        d
    }

    #[test]
    fn open_and_round_trip() {
        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(b"hello\nworld\n").unwrap();
        tmp.flush().unwrap();

        let doc = Document::open(tmp.path()).unwrap();
        assert_eq!(doc.len_lines(), 3); // ropey counts the empty final line
        assert!(!doc.lossy);

        let out = NamedTempFile::new().unwrap();
        doc.save_to(out.path()).unwrap();
        let contents = std::fs::read(out.path()).unwrap();
        assert_eq!(contents, b"hello\nworld\n");
    }

    #[test]
    fn char_to_visual_and_back() {
        let doc = doc_from_str("hello\nworld\n");
        let (line, col) = doc.char_to_visual_pos(7); // 'o' in "world"
        assert_eq!(line, 1);
        assert_eq!(col, 1);
        let back = doc.visual_pos_to_char(1, 1);
        assert_eq!(back, 7);
    }

    #[test]
    fn multibyte_char_and_line_conversions() {
        // Each Hangul syllable is one char (3 UTF-8 bytes, 2 display cells).
        let doc = doc_from_str("가나다\n라마\n");
        assert_eq!(doc.len_lines(), 3); // two lines + empty final
        assert_eq!(doc.len_chars(), 7); // 3 + \n + 2 + \n
        assert_eq!(doc.line_len_no_eol(0), 3);
        assert_eq!(doc.line_len_no_eol(1), 2);
        assert_eq!(doc.char_to_line(0), 0);
        assert_eq!(doc.char_to_line(3), 0); // the newline char of line 0
        assert_eq!(doc.char_to_line(4), 1); // first char of "라마"
        assert_eq!(doc.line_to_char(1), 4);
    }

    #[test]
    fn display_col_accounts_for_wide_chars() {
        let doc = doc_from_str("가나X\n");
        // grapheme columns under-count; display columns count wide glyphs as 2.
        assert_eq!(doc.char_to_visual_pos(2), (0, 2)); // grapheme count before 'X'
        assert_eq!(doc.char_to_display_col(2), (0, 4)); // 가(2) + 나(2)
                                                        // ASCII line is unaffected.
        let ascii = doc_from_str("abc\n");
        assert_eq!(ascii.char_to_display_col(2), (0, 2));
    }

    #[test]
    fn display_col_mixed_width_line() {
        let doc = doc_from_str("a가b나\n"); // widths: 1,2,1,2
        assert_eq!(doc.char_to_display_col(0), (0, 0));
        assert_eq!(doc.char_to_display_col(1), (0, 1)); // after 'a'
        assert_eq!(doc.char_to_display_col(2), (0, 3)); // after 'a가'
        assert_eq!(doc.char_to_display_col(3), (0, 4)); // after 'a가b'
        assert_eq!(doc.char_to_display_col(4), (0, 6)); // after 'a가b나'
    }

    #[test]
    fn modified_flag() {
        let mut doc = Document::new_empty();
        assert!(!doc.is_modified());
        let cs = crate::transaction::ChangeSetBuilder::new(0)
            .insert("x")
            .build();
        doc.apply(&Transaction::new(cs)).unwrap();
        assert!(doc.is_modified());
        doc.mark_saved();
        assert!(!doc.is_modified());
    }

    #[test]
    fn apply_and_undo() {
        let mut doc = Document::new_empty();
        let cs = crate::transaction::ChangeSetBuilder::new(0)
            .insert("hello")
            .build();
        let inv = doc.apply(&Transaction::new(cs)).unwrap();
        assert_eq!(doc.rope().to_string(), "hello");

        doc.apply(&inv).unwrap();
        assert_eq!(doc.rope().to_string(), "");
    }
}
