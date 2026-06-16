//! Applying LSP text edits to onda documents (Phase 6 W36).
//!
//! LSP positions are **(line, UTF-16 code-unit column)** — not char offsets — so
//! converting them correctly is essential for non-ASCII buffers (DESIGN §5.4.1).
//! These helpers convert LSP positions/ranges to char offsets and turn a set of
//! `TextEdit`s into a single `ChangeSet` (so a format/rename applies as one undo
//! step). Pure and unit-testable; the editor wires the results in.

use onda_core::transaction::{ChangeSet, ChangeSetBuilder};
use ropey::Rope;

/// Convert an LSP `(line, utf16_col)` position to a char offset in `rope`.
/// Out-of-range lines clamp to the document end; an over-long column clamps to the
/// line end.
pub fn lsp_pos_to_char(rope: &Rope, line: usize, utf16_col: usize) -> usize {
    let total_lines = rope.len_lines();
    if line >= total_lines {
        return rope.len_chars();
    }
    let line_start = rope.line_to_char(line);
    let line_slice = rope.line(line);
    let mut u16_seen = 0usize;
    let mut char_off = 0usize;
    for ch in line_slice.chars() {
        if u16_seen >= utf16_col {
            break;
        }
        // Don't walk past a trailing newline into the next line.
        if ch == '\n' {
            break;
        }
        u16_seen += ch.len_utf16();
        char_off += 1;
    }
    line_start + char_off
}

/// Convert an LSP range (`(start_line, start_col)`, `(end_line, end_col)`) to a
/// `(start_char, end_char)` pair.
pub fn lsp_range_to_chars(
    rope: &Rope,
    start: (usize, usize),
    end: (usize, usize),
) -> (usize, usize) {
    let s = lsp_pos_to_char(rope, start.0, start.1);
    let e = lsp_pos_to_char(rope, end.0, end.1);
    (s.min(e), s.max(e))
}

/// Build a single `ChangeSet` that applies `edits` (each a char range + replacement
/// text) to `rope`. Edits must be non-overlapping (LSP guarantees this); they're
/// sorted by start so order doesn't matter. Overlapping edits are skipped defensively.
pub fn text_edits_to_changeset(rope: &Rope, edits: &[(usize, usize, String)]) -> ChangeSet {
    let len = rope.len_chars();
    let mut sorted: Vec<&(usize, usize, String)> = edits.iter().collect();
    sorted.sort_by_key(|e| e.0);

    let mut b = ChangeSetBuilder::new(len);
    let mut pos = 0usize;
    for (start, end, text) in sorted {
        let (start, end) = (*start.min(end), *start.max(end));
        if start < pos || end > len {
            continue; // overlapping or out-of-bounds — skip
        }
        b = b.retain(start - pos);
        if end > start {
            b = b.delete(end - start);
        }
        if !text.is_empty() {
            b = b.insert(text);
        }
        pos = end;
    }
    b = b.retain(len - pos);
    b.build()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rope(s: &str) -> Rope {
        Rope::from_str(s)
    }

    #[test]
    fn pos_to_char_ascii() {
        let r = rope("hello\nworld\n");
        assert_eq!(lsp_pos_to_char(&r, 0, 0), 0);
        assert_eq!(lsp_pos_to_char(&r, 0, 3), 3);
        assert_eq!(lsp_pos_to_char(&r, 1, 2), 8); // "wo|rld"
    }

    #[test]
    fn pos_to_char_utf16_astral() {
        // 😀 is 1 char but 2 UTF-16 code units; "a😀b".
        let r = rope("a😀b\n");
        assert_eq!(lsp_pos_to_char(&r, 0, 0), 0); // before 'a'
        assert_eq!(lsp_pos_to_char(&r, 0, 1), 1); // after 'a', before 😀
        assert_eq!(lsp_pos_to_char(&r, 0, 3), 2); // after 😀 (2 u16), before 'b'
        assert_eq!(lsp_pos_to_char(&r, 0, 4), 3); // after 'b'
    }

    #[test]
    fn pos_to_char_bmp_cjk() {
        // Hangul is BMP: 1 char == 1 UTF-16 unit, so columns line up.
        let r = rope("가나다\n");
        assert_eq!(lsp_pos_to_char(&r, 0, 2), 2);
    }

    #[test]
    fn pos_clamps_out_of_range() {
        let r = rope("ab\n");
        assert_eq!(lsp_pos_to_char(&r, 9, 0), r.len_chars());
        assert_eq!(lsp_pos_to_char(&r, 0, 99), 2); // clamps to line end (before \n)
    }

    fn apply(rope: &Rope, edits: &[(usize, usize, String)]) -> String {
        let cs = text_edits_to_changeset(rope, edits);
        let mut r = rope.clone();
        cs.apply(&mut r).unwrap();
        r.to_string()
    }

    #[test]
    fn single_replace() {
        let r = rope("hello");
        assert_eq!(apply(&r, &[(0, 1, "H".into())]), "Hello");
    }

    #[test]
    fn multi_edit_unordered() {
        let r = rope("hello world");
        // Replace "world"→"there" and "hello"→"hi", given out of order.
        let edits = vec![(6, 11, "there".into()), (0, 5, "hi".into())];
        assert_eq!(apply(&r, &edits), "hi there");
    }

    #[test]
    fn pure_insertion_and_deletion() {
        let r = rope("abc");
        assert_eq!(apply(&r, &[(1, 1, "X".into())]), "aXbc"); // insert
        assert_eq!(apply(&r, &[(0, 1, String::new())]), "bc"); // delete
    }
}
