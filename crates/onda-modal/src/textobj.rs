//! Text object queries for onda-modal.
//!
//! All functions take `(rope: &Rope, pos: usize)` and return an `Option<Range>`.
//! They follow the same inner/outer semantics as Vim text objects.

use onda_core::Range;
use ropey::Rope;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Returns `true` for characters that count as word characters (alphanumeric or `_`).
#[inline]
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

// ── Bracket / delimiter pairs ─────────────────────────────────────────────────

/// For an *asymmetric* bracket pair `(open, close)`, scan backwards from `pos`
/// (exclusive) to find the matching opening delimiter, honouring nesting.
/// Returns the char index of the opening delimiter, or `None`.
fn find_open(rope: &Rope, pos: usize, open: char, close: char) -> Option<usize> {
    if pos == 0 {
        return None;
    }
    let mut depth = 0usize;
    let mut i = pos; // exclusive upper bound
    while i > 0 {
        i -= 1;
        let c = rope.char(i);
        if c == close {
            depth += 1;
        } else if c == open {
            if depth == 0 {
                return Some(i);
            }
            depth -= 1;
        }
    }
    None
}

/// For an *asymmetric* bracket pair `(open, close)`, scan forwards from `pos`
/// (exclusive) to find the matching closing delimiter, honouring nesting.
/// Returns the char index of the closing delimiter, or `None`.
fn find_close(rope: &Rope, pos: usize, open: char, close: char) -> Option<usize> {
    let len = rope.len_chars();
    let mut depth = 0usize;
    for i in pos..len {
        let c = rope.char(i);
        if c == open {
            depth += 1;
        } else if c == close {
            if depth == 0 {
                return Some(i);
            }
            depth -= 1;
        }
    }
    None
}

/// For a *symmetric* delimiter (quote character), scan backwards for the nearest
/// occurrence of `delim` before `pos`.
fn find_sym_open(rope: &Rope, pos: usize, delim: char) -> Option<usize> {
    if pos == 0 {
        return None;
    }
    let mut i = pos;
    while i > 0 {
        i -= 1;
        if rope.char(i) == delim {
            return Some(i);
        }
    }
    None
}

/// For a *symmetric* delimiter, scan forwards from `pos` (exclusive) for the
/// next occurrence of `delim`.
fn find_sym_close(rope: &Rope, pos: usize, delim: char) -> Option<usize> {
    let len = rope.len_chars();
    (pos..len).find(|&i| rope.char(i) == delim)
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Find the inner range of an asymmetric delimiter pair surrounding `pos`.
///
/// "Inner" = characters strictly between the delimiters: `[open+1, close)`.
pub fn inner_pair(rope: &Rope, pos: usize, open: char, close: char) -> Option<Range> {
    // First check whether `pos` itself is already past an opening delimiter or
    // before a closing one; scan backwards for the open.
    let open_pos = find_open(rope, pos, open, close)?;
    let close_pos = find_close(rope, open_pos + 1, open, close)?;
    if open_pos + 1 > close_pos {
        return None;
    }
    Some(Range::new(open_pos + 1, close_pos))
}

/// Find the outer range of an asymmetric delimiter pair surrounding `pos`.
///
/// "Outer" = the delimiters themselves included: `[open, close+1)`.
pub fn outer_pair(rope: &Rope, pos: usize, open: char, close: char) -> Option<Range> {
    let open_pos = find_open(rope, pos, open, close)?;
    let close_pos = find_close(rope, open_pos + 1, open, close)?;
    Some(Range::new(open_pos, close_pos + 1))
}

/// Find the inner range of a symmetric delimiter (quote) surrounding `pos`.
///
/// "Inner" = characters strictly between the two delimiters: `[open+1, close)`.
pub fn inner_sym_pair(rope: &Rope, pos: usize, delim: char) -> Option<Range> {
    // If pos itself is the delimiter, treat it as opening.
    let open_pos = if rope.char(pos) == delim {
        pos
    } else {
        find_sym_open(rope, pos, delim)?
    };
    let close_pos = find_sym_close(rope, open_pos + 1, delim)?;
    if open_pos + 1 > close_pos {
        return None;
    }
    Some(Range::new(open_pos + 1, close_pos))
}

/// Find the outer range of a symmetric delimiter (quote) surrounding `pos`.
///
/// "Outer" = delimiters included: `[open, close+1)`.
pub fn outer_sym_pair(rope: &Rope, pos: usize, delim: char) -> Option<Range> {
    let open_pos = if rope.char(pos) == delim {
        pos
    } else {
        find_sym_open(rope, pos, delim)?
    };
    let close_pos = find_sym_close(rope, open_pos + 1, delim)?;
    Some(Range::new(open_pos, close_pos + 1))
}

// ── Convenience wrappers ──────────────────────────────────────────────────────

pub fn inner_parens(rope: &Rope, pos: usize) -> Option<Range> {
    inner_pair(rope, pos, '(', ')')
}
pub fn outer_parens(rope: &Rope, pos: usize) -> Option<Range> {
    outer_pair(rope, pos, '(', ')')
}
pub fn inner_brackets(rope: &Rope, pos: usize) -> Option<Range> {
    inner_pair(rope, pos, '[', ']')
}
pub fn outer_brackets(rope: &Rope, pos: usize) -> Option<Range> {
    outer_pair(rope, pos, '[', ']')
}
pub fn inner_braces(rope: &Rope, pos: usize) -> Option<Range> {
    inner_pair(rope, pos, '{', '}')
}
pub fn outer_braces(rope: &Rope, pos: usize) -> Option<Range> {
    outer_pair(rope, pos, '{', '}')
}
pub fn inner_double_quote(rope: &Rope, pos: usize) -> Option<Range> {
    inner_sym_pair(rope, pos, '"')
}
pub fn outer_double_quote(rope: &Rope, pos: usize) -> Option<Range> {
    outer_sym_pair(rope, pos, '"')
}
pub fn inner_single_quote(rope: &Rope, pos: usize) -> Option<Range> {
    inner_sym_pair(rope, pos, '\'')
}
pub fn outer_single_quote(rope: &Rope, pos: usize) -> Option<Range> {
    outer_sym_pair(rope, pos, '\'')
}
pub fn inner_backtick(rope: &Rope, pos: usize) -> Option<Range> {
    inner_sym_pair(rope, pos, '`')
}
pub fn outer_backtick(rope: &Rope, pos: usize) -> Option<Range> {
    outer_sym_pair(rope, pos, '`')
}

// ── Word objects ──────────────────────────────────────────────────────────────

/// Inner word: contiguous run of alphanumeric+underscore characters around `pos`.
/// Returns `None` if `pos` is not on a word character.
pub fn inner_word(rope: &Rope, pos: usize) -> Option<Range> {
    let len = rope.len_chars();
    if pos >= len || !is_word_char(rope.char(pos)) {
        return None;
    }
    // Expand left
    let mut start = pos;
    while start > 0 && is_word_char(rope.char(start - 1)) {
        start -= 1;
    }
    // Expand right
    let mut end = pos + 1;
    while end < len && is_word_char(rope.char(end)) {
        end += 1;
    }
    Some(Range::new(start, end))
}

/// Outer word: inner word + trailing whitespace (or leading if at end of line).
pub fn outer_word(rope: &Rope, pos: usize) -> Option<Range> {
    let inner = inner_word(rope, pos)?;
    let len = rope.len_chars();
    let mut start = inner.from();
    let mut end = inner.to();
    // Prefer trailing whitespace
    if end < len && rope.char(end).is_whitespace() {
        while end < len && rope.char(end).is_whitespace() {
            end += 1;
        }
    } else {
        // Leading whitespace
        while start > 0 && rope.char(start - 1).is_whitespace() {
            start -= 1;
        }
    }
    Some(Range::new(start, end))
}

/// Inner WORD: contiguous run of non-whitespace characters around `pos`.
/// Returns `None` if `pos` is on whitespace.
pub fn inner_big_word(rope: &Rope, pos: usize) -> Option<Range> {
    let len = rope.len_chars();
    if pos >= len || rope.char(pos).is_whitespace() {
        return None;
    }
    let mut start = pos;
    while start > 0 && !rope.char(start - 1).is_whitespace() {
        start -= 1;
    }
    let mut end = pos + 1;
    while end < len && !rope.char(end).is_whitespace() {
        end += 1;
    }
    Some(Range::new(start, end))
}

/// Outer WORD: inner WORD + trailing (or leading) whitespace.
pub fn outer_big_word(rope: &Rope, pos: usize) -> Option<Range> {
    let inner = inner_big_word(rope, pos)?;
    let len = rope.len_chars();
    let mut start = inner.from();
    let mut end = inner.to();
    if end < len && rope.char(end).is_whitespace() {
        while end < len && rope.char(end).is_whitespace() {
            end += 1;
        }
    } else {
        while start > 0 && rope.char(start - 1).is_whitespace() {
            start -= 1;
        }
    }
    Some(Range::new(start, end))
}

// ── Paragraph objects ─────────────────────────────────────────────────────────

/// Returns `true` when a line contains only a newline (i.e. is blank).
fn is_blank_line(rope: &Rope, line: usize) -> bool {
    let s = rope.line(line);
    let len = s.len_chars();
    len == 0 || (len == 1 && s.char(0) == '\n')
}

/// Inner paragraph: the block of non-blank lines surrounding `pos`.
/// Returns the range from the start of the first non-blank line to the end of
/// the last non-blank line (exclusive of the trailing newline that separates
/// paragraphs).
pub fn inner_paragraph(rope: &Rope, pos: usize) -> Option<Range> {
    let total_lines = rope.len_lines();
    let cur_line = rope.char_to_line(pos);

    // If already on a blank line there is no paragraph here.
    if is_blank_line(rope, cur_line) {
        return None;
    }

    // Expand upward to find the first non-blank line.
    let mut first_line = cur_line;
    while first_line > 0 && !is_blank_line(rope, first_line - 1) {
        first_line -= 1;
    }

    // Expand downward to find the last non-blank line.
    let mut last_line = cur_line;
    while last_line + 1 < total_lines && !is_blank_line(rope, last_line + 1) {
        last_line += 1;
    }

    let start = rope.line_to_char(first_line);
    // Include the content of the last line but not a trailing separator blank line.
    let end = rope.line_to_char(last_line) + rope.line(last_line).len_chars();
    Some(Range::new(start, end))
}

/// Outer paragraph: inner paragraph + surrounding blank lines.
pub fn outer_paragraph(rope: &Rope, pos: usize) -> Option<Range> {
    let total_lines = rope.len_lines();
    let cur_line = rope.char_to_line(pos);

    // Find the paragraph block first (skip any leading blanks at cursor).
    let mut first_line = cur_line;
    // Move down past blanks if we're on them.
    while first_line < total_lines && is_blank_line(rope, first_line) {
        first_line += 1;
    }
    if first_line >= total_lines {
        return None;
    }
    // Expand upward to include leading blank lines.
    let mut outer_start = first_line;
    while outer_start > 0 && is_blank_line(rope, outer_start - 1) {
        outer_start -= 1;
    }
    // Also walk back past the non-blank block.
    while outer_start > 0 && !is_blank_line(rope, outer_start - 1) {
        outer_start -= 1;
    }

    // Expand downward past the non-blank block.
    let mut last_line = first_line;
    while last_line + 1 < total_lines && !is_blank_line(rope, last_line + 1) {
        last_line += 1;
    }
    // Include trailing blank lines.
    let mut outer_end_line = last_line;
    while outer_end_line + 1 < total_lines && is_blank_line(rope, outer_end_line + 1) {
        outer_end_line += 1;
    }

    let start = rope.line_to_char(outer_start);
    let end = rope.line_to_char(outer_end_line) + rope.line(outer_end_line).len_chars();
    Some(Range::new(start, end))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ropey::Rope;

    fn r(s: &str) -> Rope {
        Rope::from_str(s)
    }

    // inner_parens: "foo(bar,baz)" pos=6 → Range(4,11)
    #[test]
    fn inner_parens_basic() {
        let rope = r("foo(bar,baz)");
        // pos 6 = 'a' inside "(bar,baz)"
        // open at 3, close at 11 → inner = [4, 11)
        let result = inner_parens(&rope, 6).expect("should find parens");
        assert_eq!(result.from(), 4);
        assert_eq!(result.to(), 11);
    }

    #[test]
    fn outer_parens_basic() {
        let rope = r("foo(bar,baz)");
        let result = outer_parens(&rope, 6).expect("should find parens");
        assert_eq!(result.from(), 3);
        assert_eq!(result.to(), 12);
    }

    #[test]
    fn inner_parens_nested() {
        let rope = r("(a(b)c)");
        // pos=3 is 'b', inside inner parens at [2,4]
        let result = inner_parens(&rope, 3).expect("nested inner parens");
        assert_eq!(result.from(), 3);
        assert_eq!(result.to(), 4);
    }

    #[test]
    fn inner_brackets_basic() {
        let rope = r("vec[1, 2]");
        let result = inner_brackets(&rope, 5).expect("brackets");
        assert_eq!(result.from(), 4);
        assert_eq!(result.to(), 8);
    }

    #[test]
    fn inner_braces_basic() {
        let rope = r("fn foo() { body }");
        let result = inner_braces(&rope, 12).expect("braces");
        assert_eq!(result.from(), 10);
        assert_eq!(result.to(), 16);
    }

    #[test]
    fn inner_double_quote_basic() {
        let rope = r(r#"say "hello world" now"#);
        // pos=9 = 'l' inside "hello world"
        let result = inner_double_quote(&rope, 9).expect("double quote");
        assert_eq!(result.from(), 5);
        assert_eq!(result.to(), 16);
    }

    #[test]
    fn outer_double_quote_basic() {
        let rope = r(r#"say "hello" now"#);
        let result = outer_double_quote(&rope, 6).expect("outer dquote");
        assert_eq!(result.from(), 4);
        assert_eq!(result.to(), 11);
    }

    #[test]
    fn inner_word_basic() {
        let rope = r("hello world");
        let result = inner_word(&rope, 2).expect("inner word");
        assert_eq!(result.from(), 0);
        assert_eq!(result.to(), 5);
    }

    #[test]
    fn inner_word_middle() {
        let rope = r("foo bar baz");
        let result = inner_word(&rope, 5).expect("middle word");
        assert_eq!(result.from(), 4);
        assert_eq!(result.to(), 7);
    }

    #[test]
    fn inner_word_none_on_space() {
        let rope = r("foo bar");
        assert!(inner_word(&rope, 3).is_none());
    }

    #[test]
    fn outer_word_trailing_space() {
        let rope = r("foo bar");
        // pos 1 = 'o', outer word should consume "foo " (4 chars)
        let result = outer_word(&rope, 1).expect("outer word");
        assert_eq!(result.from(), 0);
        assert_eq!(result.to(), 4);
    }

    #[test]
    fn inner_big_word_basic() {
        let rope = r("foo.bar baz");
        // pos=3 = '.', not whitespace → WORD is "foo.bar"
        let result = inner_big_word(&rope, 3).expect("big word");
        assert_eq!(result.from(), 0);
        assert_eq!(result.to(), 7);
    }

    #[test]
    fn inner_paragraph_basic() {
        let rope = r("line1\nline2\n\nline3\n");
        // pos inside "line1" (char 2)
        let result = inner_paragraph(&rope, 2).expect("paragraph");
        assert_eq!(result.from(), 0);
        // inner paragraph covers "line1\nline2\n" = 12 chars
        assert_eq!(result.to(), 12);
    }

    #[test]
    fn inner_paragraph_none_on_blank() {
        let rope = r("line1\n\nline2\n");
        // pos at the blank line (char 6 = '\n')
        assert!(inner_paragraph(&rope, 6).is_none());
    }
}
