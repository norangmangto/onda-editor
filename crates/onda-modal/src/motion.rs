use onda_core::{Range, Selection};
use ropey::Rope;
use smallvec::SmallVec;
use unicode_segmentation::UnicodeSegmentation;

/// A motion transforms a range to a new range.
///
/// All motions are pure functions: `(text, range, count) -> Range`.
/// They are applied to each range in a [`Selection`] independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    WordForward,        // w
    WordBackward,       // b
    WordEnd,            // e
    BigWordForward,     // W
    BigWordBackward,    // B
    BigWordEnd,         // E
    LineStart,          // 0
    LineFirstNonBlank,  // ^
    LineEnd,            // $
    DocumentStart,      // gg
    DocumentEnd,        // G
    FindChar(char),     // f<char>
    TillChar(char),     // t<char>
    FindCharBack(char), // F<char>
    TillCharBack(char), // T<char>
    ParagraphForward,   // }
    ParagraphBackward,  // {
    HalfPageDown,       // Ctrl-d
    HalfPageUp,         // Ctrl-u
}

impl Motion {
    /// Whether this motion is *inclusive* when used as an operator target (vim
    /// semantics): the character at the target position is part of the operated
    /// span. Inclusive motions are `e`/`E`, `$`, and forward `f`/`t`. Everything
    /// else (`w`, `b`, `0`, `^`, …) is exclusive — the target char is not deleted.
    pub fn is_inclusive(self) -> bool {
        matches!(
            self,
            Motion::WordEnd
                | Motion::BigWordEnd
                | Motion::LineEnd
                | Motion::FindChar(_)
                | Motion::TillChar(_)
        )
    }

    /// Whether this motion is *linewise* when used as an operator target (vim
    /// semantics): `dj`/`dk`/`dG`/`dgg` operate on whole lines, not a charwise
    /// span. Line-oriented vertical motions are linewise; everything else is
    /// charwise.
    pub fn is_linewise(self) -> bool {
        matches!(
            self,
            Motion::Up
                | Motion::Down
                | Motion::DocumentStart
                | Motion::DocumentEnd
                | Motion::HalfPageDown
                | Motion::HalfPageUp
        )
    }

    /// Apply this motion to a single range, returning the new range.
    ///
    /// `count` is the repetition count (≥ 1).
    /// `goal_col` is the sticky column for vertical motions (in grapheme units).
    pub fn apply(
        self,
        rope: &Rope,
        range: Range,
        count: usize,
        goal_col: Option<usize>,
        viewport_height: usize,
    ) -> (Range, Option<usize>) {
        let count = count.max(1);
        match self {
            Motion::Left => (move_left(rope, range, count), None),
            Motion::Right => (move_right(rope, range, count), None),
            Motion::Up => move_vertical(rope, range, -(count as isize), goal_col, viewport_height),
            Motion::Down => move_vertical(rope, range, count as isize, goal_col, viewport_height),
            Motion::WordForward => (word_forward(rope, range, count, false), None),
            Motion::WordBackward => (word_backward(rope, range, count, false), None),
            Motion::WordEnd => (word_end(rope, range, count, false), None),
            Motion::BigWordForward => (word_forward(rope, range, count, true), None),
            Motion::BigWordBackward => (word_backward(rope, range, count, true), None),
            Motion::BigWordEnd => (word_end(rope, range, count, true), None),
            Motion::LineStart => (line_start(rope, range), None),
            Motion::LineFirstNonBlank => (line_first_non_blank(rope, range), None),
            Motion::LineEnd => (line_end(rope, range, count), None),
            Motion::DocumentStart => (doc_start(range), None),
            Motion::DocumentEnd => (doc_end(rope, range), None),
            Motion::FindChar(c) => (find_char(rope, range, c, count, false, false), None),
            Motion::TillChar(c) => (find_char(rope, range, c, count, true, false), None),
            Motion::FindCharBack(c) => (find_char(rope, range, c, count, false, true), None),
            Motion::TillCharBack(c) => (find_char(rope, range, c, count, true, true), None),
            Motion::ParagraphForward => (paragraph_forward(rope, range, count), None),
            Motion::ParagraphBackward => (paragraph_backward(rope, range, count), None),
            Motion::HalfPageDown => move_vertical(
                rope,
                range,
                (viewport_height / 2) as isize,
                goal_col,
                viewport_height,
            ),
            Motion::HalfPageUp => move_vertical(
                rope,
                range,
                -((viewport_height / 2) as isize),
                goal_col,
                viewport_height,
            ),
        }
    }

    /// Apply this motion to every range in `sel`, updating the selection.
    pub fn apply_to_selection(
        self,
        rope: &Rope,
        sel: &Selection,
        count: usize,
        goal_col: Option<usize>,
        viewport_height: usize,
    ) -> (Selection, Option<usize>) {
        let mut new_goal = goal_col;
        let ranges: SmallVec<[Range; 1]> = sel
            .ranges()
            .iter()
            .map(|&r| {
                let (new_r, g) = self.apply(rope, r, count, new_goal, viewport_height);
                if g.is_some() {
                    new_goal = g;
                }
                new_r
            })
            .collect();
        let primary = sel.primary_index();
        (Selection::new(ranges, primary), new_goal)
    }
}

// ── Primitive movement functions ──────────────────────────────────────────────

#[allow(dead_code)]
fn clamp_char(rope: &Rope, pos: usize) -> usize {
    pos.min(rope.len_chars().saturating_sub(1))
}

fn move_left(rope: &Rope, range: Range, count: usize) -> Range {
    let pos = range.head;
    let line = rope.char_to_line(pos);
    let line_start = rope.line_to_char(line);
    let new_pos = pos.saturating_sub(count).max(line_start);
    Range::point(new_pos)
}

fn move_right(rope: &Rope, range: Range, count: usize) -> Range {
    let pos = range.head;
    let line = rope.char_to_line(pos);
    let line_end = line_end_char(rope, line);
    let new_pos = (pos + count).min(line_end);
    Range::point(new_pos)
}

/// Returns the last valid cursor position on a line (before the newline).
fn line_end_char(rope: &Rope, line: usize) -> usize {
    let line_start = rope.line_to_char(line);
    let line_len = rope.line(line).len_chars();
    if line_len == 0 {
        return line_start;
    }
    let last = rope.line(line).char(line_len - 1);
    let eol_len = if last == '\n' { 1 } else { 0 };
    line_start + line_len.saturating_sub(eol_len).saturating_sub(1)
}

fn move_vertical(
    rope: &Rope,
    range: Range,
    delta: isize,
    goal_col: Option<usize>,
    _viewport_height: usize,
) -> (Range, Option<usize>) {
    let pos = range.head;
    let cur_line = rope.char_to_line(pos) as isize;
    let target_line = (cur_line + delta).clamp(0, rope.len_lines() as isize - 1) as usize;

    // Compute current grapheme column if no goal column set
    let cur_line_start = rope.line_to_char(cur_line as usize);
    let cur_col = grapheme_col(rope, cur_line_start, pos);
    let goal = goal_col.unwrap_or(cur_col);

    let target_line_start = rope.line_to_char(target_line);
    let target_line_len = rope.line(target_line).len_chars();
    let eol = if target_line_len > 0 && rope.line(target_line).char(target_line_len - 1) == '\n' {
        1
    } else {
        0
    };
    let target_line_max_col = target_line_len.saturating_sub(eol).saturating_sub(1);

    let target_col = goal.min(target_max_grapheme(rope, target_line, target_line_max_col));
    let new_pos = grapheme_col_to_char(rope, target_line_start, target_col);

    (
        Range::point(new_pos.min(rope.len_chars().saturating_sub(1))),
        Some(goal),
    )
}

/// Count grapheme clusters from `line_start` to `char_pos`.
fn grapheme_col(rope: &Rope, line_start: usize, char_pos: usize) -> usize {
    if char_pos <= line_start {
        return 0;
    }
    let slice: String = rope.slice(line_start..char_pos).to_string();
    slice.graphemes(true).count()
}

/// Max grapheme column for a line (given max char count).
fn target_max_grapheme(rope: &Rope, line: usize, max_chars: usize) -> usize {
    let line_start = rope.line_to_char(line);
    let slice: String = rope.slice(line_start..line_start + max_chars).to_string();
    slice.graphemes(true).count()
}

/// Convert a grapheme column index to a char index.
fn grapheme_col_to_char(rope: &Rope, line_start: usize, col: usize) -> usize {
    let line_len = {
        let line_idx = rope.char_to_line(line_start);
        rope.line(line_idx).len_chars()
    };
    let slice: String = rope.slice(line_start..line_start + line_len).to_string();
    let mut char_count = 0usize;
    for (i, g) in slice.graphemes(true).enumerate() {
        if i >= col {
            break;
        }
        char_count += g.chars().count();
    }
    line_start + char_count
}

fn line_start(rope: &Rope, range: Range) -> Range {
    let line = rope.char_to_line(range.head);
    Range::point(rope.line_to_char(line))
}

fn line_first_non_blank(rope: &Rope, range: Range) -> Range {
    let line = rope.char_to_line(range.head);
    let line_start = rope.line_to_char(line);
    let line_str: String = rope.line(line).to_string();
    let offset = line_str
        .char_indices()
        .find(|(_, c)| !c.is_whitespace())
        .map(|(i, _)| {
            // i is a byte offset; convert to char count
            line_str[..i].chars().count()
        })
        .unwrap_or(0);
    Range::point(line_start + offset)
}

fn line_end(rope: &Rope, range: Range, _count: usize) -> Range {
    let line = rope.char_to_line(range.head);
    Range::point(line_end_char(rope, line))
}

fn doc_start(_range: Range) -> Range {
    Range::point(0)
}

fn doc_end(rope: &Rope, _range: Range) -> Range {
    // Find the last non-empty line (skip trailing empty line from trailing newline)
    let total_lines = rope.len_lines();
    let mut last_line = total_lines.saturating_sub(1);
    // If the last line is empty (trailing newline), use the one before it
    while last_line > 0 && rope.line(last_line).len_chars() == 0 {
        last_line -= 1;
    }
    Range::point(line_end_char(rope, last_line))
}

fn is_word_char(c: char, big: bool) -> bool {
    if big {
        !c.is_whitespace()
    } else {
        c.is_alphanumeric() || c == '_'
    }
}

fn word_forward(rope: &Rope, range: Range, count: usize, big: bool) -> Range {
    let mut pos = range.head;
    let len = rope.len_chars();
    for _ in 0..count {
        if pos >= len {
            break;
        }
        let ch = rope.char(pos);
        if is_word_char(ch, big) {
            // Skip current word
            while pos < len && is_word_char(rope.char(pos), big) {
                pos += 1;
            }
        } else if !ch.is_whitespace() {
            // Skip current punctuation run
            while pos < len && !is_word_char(rope.char(pos), big) && !rope.char(pos).is_whitespace()
            {
                pos += 1;
            }
        }
        // Skip whitespace
        while pos < len && rope.char(pos).is_whitespace() {
            pos += 1;
        }
    }
    Range::point(pos.min(len.saturating_sub(1)))
}

fn word_backward(rope: &Rope, range: Range, count: usize, big: bool) -> Range {
    let mut pos = range.head;
    for _ in 0..count {
        if pos == 0 {
            break;
        }
        pos = pos.saturating_sub(1);
        // Skip whitespace backwards
        while pos > 0 && rope.char(pos).is_whitespace() {
            pos -= 1;
        }
        if pos == 0 {
            break;
        }
        // Skip word chars backwards
        while pos > 0 && is_word_char(rope.char(pos - 1), big) {
            pos -= 1;
        }
        // If on punctuation, skip it
        while pos > 0 && !is_word_char(rope.char(pos), big) && !rope.char(pos).is_whitespace() {
            pos -= 1;
        }
    }
    Range::point(pos)
}

fn word_end(rope: &Rope, range: Range, count: usize, big: bool) -> Range {
    let mut pos = range.head;
    let len = rope.len_chars();
    for _ in 0..count {
        if pos + 1 >= len {
            break;
        }
        pos += 1;
        // Skip whitespace
        while pos < len && rope.char(pos).is_whitespace() {
            pos += 1;
        }
        // Skip to end of word
        while pos + 1 < len && is_word_char(rope.char(pos + 1), big) {
            pos += 1;
        }
    }
    Range::point(pos.min(len.saturating_sub(1)))
}

fn find_char(
    rope: &Rope,
    range: Range,
    target: char,
    count: usize,
    till: bool,
    back: bool,
) -> Range {
    let pos = range.head;
    let len = rope.len_chars();

    if back {
        let mut found = pos;
        let mut hits = 0;
        let start = if pos == 0 {
            return range;
        } else {
            pos - 1
        };
        let mut i = start;
        loop {
            if rope.char(i) == target {
                hits += 1;
                if hits >= count {
                    found = if till { i + 1 } else { i };
                    break;
                }
            }
            if i == 0 {
                break;
            }
            i -= 1;
        }
        Range::point(found)
    } else {
        let mut found = pos;
        let mut hits = 0;
        for i in (pos + 1)..len {
            if rope.char(i) == target {
                hits += 1;
                if hits >= count {
                    found = if till { i.saturating_sub(1) } else { i };
                    break;
                }
            }
        }
        Range::point(found)
    }
}

fn paragraph_forward(rope: &Rope, range: Range, count: usize) -> Range {
    let mut line = rope.char_to_line(range.head);
    let total_lines = rope.len_lines();
    for _ in 0..count {
        // Skip non-blank lines
        while line < total_lines && rope.line(line).len_chars() > 1 {
            line += 1;
        }
        // Skip blank lines
        while line < total_lines && rope.line(line).len_chars() <= 1 {
            line += 1;
        }
    }
    Range::point(rope.line_to_char(line.min(total_lines.saturating_sub(1))))
}

fn paragraph_backward(rope: &Rope, range: Range, count: usize) -> Range {
    let mut line = rope.char_to_line(range.head) as isize;
    for _ in 0..count {
        line -= 1;
        while line > 0 && rope.line(line as usize).len_chars() > 1 {
            line -= 1;
        }
        while line > 0 && rope.line(line as usize).len_chars() <= 1 {
            line -= 1;
        }
    }
    let line = line.max(0) as usize;
    Range::point(rope.line_to_char(line))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ropey::Rope;

    fn rope(s: &str) -> Rope {
        Rope::from_str(s)
    }

    fn pt(pos: usize) -> Range {
        Range::point(pos)
    }

    #[test]
    fn move_right_stays_on_line() {
        let r = rope("hello\nworld\n");
        // pos 4 = 'o', pos 5 = '\n'
        let result = Motion::Right.apply(&r, pt(4), 3, None, 10);
        assert_eq!(result.0.head, 4); // can't go past end of line
    }

    #[test]
    fn move_left_clamps_to_line_start() {
        let r = rope("hello\nworld\n");
        let result = Motion::Left.apply(&r, pt(6), 10, None, 10); // 6 = 'w'
        assert_eq!(result.0.head, 6); // stays at line start
    }

    #[test]
    fn move_down_preserves_goal_col() {
        let r = rope("hello\nhi\nworld\n");
        // Start at col 4 ('o')
        let (r1, goal) = Motion::Down.apply(&r, pt(4), 1, None, 10);
        // Line 1 is "hi\n" (len 2), so clamped to col 1
        assert_eq!(r1.head, 7); // 'i' on line 1
        assert_eq!(goal, Some(4)); // goal col preserved

        let (r2, _) = Motion::Down.apply(&r, r1, 1, goal, 10);
        // Line 2 "world\n" col 4 = 'l' at char 9+4=13? Let's verify
        assert_eq!(r2.head, 13); // should land at col 4 on "world"
    }

    #[test]
    fn word_forward_basic() {
        let r = rope("hello world foo");
        let result = Motion::WordForward.apply(&r, pt(0), 1, None, 10);
        assert_eq!(result.0.head, 6); // start of "world"
    }

    #[test]
    fn line_end_motion() {
        let r = rope("hello\nworld\n");
        let result = Motion::LineEnd.apply(&r, pt(0), 1, None, 10);
        assert_eq!(result.0.head, 4); // 'o' is last char on line 0
    }

    #[test]
    fn doc_end_motion() {
        let r = rope("abc\ndef\n");
        let result = Motion::DocumentEnd.apply(&r, pt(0), 1, None, 10);
        assert_eq!(result.0.head, 6); // 'f' is last char
    }

    #[test]
    fn find_char_forward() {
        let r = rope("hello world");
        let result = Motion::FindChar('o').apply(&r, pt(0), 1, None, 10);
        assert_eq!(result.0.head, 4);
    }

    #[test]
    fn multi_count_word_forward() {
        let r = rope("a b c d e");
        // 3w from 'a': w1→'b', w2→'c', w3→'d'
        let result = Motion::WordForward.apply(&r, pt(0), 3, None, 10);
        assert_eq!(result.0.head, 6); // "d"
    }

    #[test]
    fn is_inclusive_classification() {
        // Inclusive: the target char is part of an operator span.
        for m in [
            Motion::WordEnd,
            Motion::BigWordEnd,
            Motion::LineEnd,
            Motion::FindChar('x'),
            Motion::TillChar('x'),
        ] {
            assert!(m.is_inclusive(), "{m:?} should be inclusive");
        }
        // Exclusive: target char is not deleted by an operator.
        for m in [
            Motion::Left,
            Motion::Right,
            Motion::WordForward,
            Motion::WordBackward,
            Motion::BigWordForward,
            Motion::LineStart,
            Motion::LineFirstNonBlank,
            Motion::FindCharBack('x'),
            Motion::TillCharBack('x'),
            Motion::ParagraphForward,
        ] {
            assert!(!m.is_inclusive(), "{m:?} should be exclusive");
        }
    }

    #[test]
    fn word_end_lands_on_last_char() {
        let r = rope("foo bar");
        // `e` from start of "foo" → last char 'o' (index 2), inclusive.
        let result = Motion::WordEnd.apply(&r, pt(0), 1, None, 10);
        assert_eq!(result.0.head, 2);
    }

    #[test]
    fn line_start_and_end() {
        let r = rope("  hello\n");
        assert_eq!(Motion::LineStart.apply(&r, pt(5), 1, None, 10).0.head, 0);
        // `$` → last non-newline char.
        assert_eq!(Motion::LineEnd.apply(&r, pt(0), 1, None, 10).0.head, 6);
        // `^` → first non-blank.
        assert_eq!(
            Motion::LineFirstNonBlank
                .apply(&r, pt(6), 1, None, 10)
                .0
                .head,
            2
        );
    }
}
