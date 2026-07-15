use onda_core::{transaction::ChangeSetBuilder, ChangeSet, Document, Selection, Transaction};

use crate::register::{Register, RegisterKind};

/// The operator type: what we do with a motion's range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operator {
    Delete,
    Change,
    Yank,
    Indent,
    Dedent,
    Lowercase,
    Uppercase,
    ToggleCase,
}

/// How to remap a character's case (`gu`/`gU`/`g~`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseMode {
    Lower,
    Upper,
    Toggle,
}

fn map_case(c: char, mode: CaseMode) -> String {
    match mode {
        CaseMode::Lower => c.to_lowercase().collect(),
        CaseMode::Upper => c.to_uppercase().collect(),
        CaseMode::Toggle => {
            if c.is_uppercase() {
                c.to_lowercase().collect()
            } else if c.is_lowercase() {
                c.to_uppercase().collect()
            } else {
                c.to_string()
            }
        }
    }
}

/// Apply a delete operation to the selection in the document.
///
/// Returns (Transaction, yanked_text) — the transaction can be applied to the document,
/// and the yanked text goes into the register.
pub fn delete(doc: &Document, sel: &Selection) -> (Transaction, Register) {
    let len = doc.len_chars();
    let rope = doc.rope();

    // Collect yanked text
    let mut yanked = String::new();
    for range in sel.ranges() {
        let from = range.from();
        let to = (range.to() + 1).min(len);
        if from < to {
            yanked.push_str(&rope.slice(from..to).to_string());
        }
    }

    let changes = build_delete_changeset(doc, sel);
    let tx = Transaction::new(changes);
    let reg = Register::new(yanked, RegisterKind::Charwise);
    (tx, reg)
}

/// Apply a linewise delete (dd / yy) on the lines that contain the selection.
pub fn delete_lines(doc: &Document, sel: &Selection) -> (Transaction, Register) {
    let rope = doc.rope();
    let len = doc.len_chars();

    let mut ranges_to_delete: Vec<(usize, usize)> = Vec::new();
    let mut yanked = String::new();

    for range in sel.ranges() {
        let line_start = doc.char_to_line(range.from());
        let line_end = doc.char_to_line(range.to());

        let from = doc.line_to_char(line_start);
        let to = if line_end + 1 < rope.len_lines() {
            doc.line_to_char(line_end + 1)
        } else {
            // Last line: include up to end of doc
            len
        };

        yanked.push_str(&rope.slice(from..to).to_string());
        ranges_to_delete.push((from, to));
    }

    let changes = build_delete_changeset_ranges(doc, &ranges_to_delete);
    let tx = Transaction::new(changes);
    let reg = Register::new(yanked, RegisterKind::Linewise);
    (tx, reg)
}

/// Paste charwise after the cursor.
pub fn paste_after(doc: &Document, sel: &Selection, reg: &Register) -> Transaction {
    if reg.text.is_empty() {
        return Transaction::new(ChangeSet::new(doc.len_chars()));
    }
    match reg.kind {
        RegisterKind::Charwise | RegisterKind::Blockwise => {
            let pos = sel.primary().head + 1;
            let pos = pos.min(doc.len_chars());
            let changes = ChangeSetBuilder::new(doc.len_chars())
                .retain(pos)
                .insert(reg.text.clone())
                .retain(doc.len_chars() - pos)
                .build();
            Transaction::new(changes)
        }
        RegisterKind::Linewise => {
            let line = doc.char_to_line(sel.primary().head);
            let next_line_start = if line + 1 < doc.len_lines() {
                doc.line_to_char(line + 1)
            } else {
                doc.len_chars()
            };
            let text = if reg.text.ends_with('\n') {
                reg.text.clone()
            } else {
                format!("{}\n", reg.text)
            };
            let changes = ChangeSetBuilder::new(doc.len_chars())
                .retain(next_line_start)
                .insert(text)
                .retain(doc.len_chars() - next_line_start)
                .build();
            Transaction::new(changes)
        }
    }
}

/// Paste before the cursor.
pub fn paste_before(doc: &Document, sel: &Selection, reg: &Register) -> Transaction {
    if reg.text.is_empty() {
        return Transaction::new(ChangeSet::new(doc.len_chars()));
    }
    match reg.kind {
        RegisterKind::Charwise | RegisterKind::Blockwise => {
            let pos = sel.primary().head;
            let changes = ChangeSetBuilder::new(doc.len_chars())
                .retain(pos)
                .insert(reg.text.clone())
                .retain(doc.len_chars() - pos)
                .build();
            Transaction::new(changes)
        }
        RegisterKind::Linewise => {
            let line = doc.char_to_line(sel.primary().head);
            let line_start = doc.line_to_char(line);
            let text = if reg.text.ends_with('\n') {
                reg.text.clone()
            } else {
                format!("{}\n", reg.text)
            };
            let changes = ChangeSetBuilder::new(doc.len_chars())
                .retain(line_start)
                .insert(text)
                .retain(doc.len_chars() - line_start)
                .build();
            Transaction::new(changes)
        }
    }
}

/// Build a changeset that deletes all characters in the selection ranges.
fn build_delete_changeset(doc: &Document, sel: &Selection) -> ChangeSet {
    let len = doc.len_chars();
    let ranges: Vec<(usize, usize)> = sel
        .ranges()
        .iter()
        .map(|r| {
            let from = r.from();
            let to = (r.to() + 1).min(len);
            (from, to)
        })
        .collect();
    build_delete_changeset_ranges(doc, &ranges)
}

/// Build a changeset that deletes a list of non-overlapping `(from, to)` ranges.
/// Ranges must be sorted.
fn build_delete_changeset_ranges(doc: &Document, ranges: &[(usize, usize)]) -> ChangeSet {
    let len = doc.len_chars();
    let mut builder = ChangeSetBuilder::new(len);
    let mut pos = 0usize;

    for &(from, to) in ranges {
        let from = from.min(len);
        let to = to.min(len);
        if from > pos {
            builder = builder.retain(from - pos);
        }
        if to > from {
            builder = builder.delete(to - from);
        }
        pos = to;
    }
    if pos < len {
        builder = builder.retain(len - pos);
    }
    builder.build()
}

/// Insert text at the cursor position (for insert-mode single char).
pub fn insert_char(doc: &Document, sel: &Selection, c: char) -> Transaction {
    let len = doc.len_chars();
    let pos = sel.primary().head;
    let s = c.to_string();
    let changes = ChangeSetBuilder::new(len)
        .retain(pos)
        .insert(s)
        .retain(len - pos)
        .build();
    Transaction::new(changes)
}

/// Delete the character before the cursor (backspace).
pub fn delete_before_cursor(doc: &Document, sel: &Selection) -> Transaction {
    let _len = doc.len_chars();
    let ranges: Vec<(usize, usize)> = sel
        .ranges()
        .iter()
        .filter_map(|r| {
            let pos = r.head;
            if pos == 0 {
                return None;
            }
            let line = doc.char_to_line(pos);
            let line_start = doc.line_to_char(line);
            if pos <= line_start {
                // Backspace at line start: delete newline
                Some((pos - 1, pos))
            } else {
                Some((pos - 1, pos))
            }
        })
        .collect();
    let changes = build_delete_changeset_ranges(doc, &ranges);
    Transaction::new(changes)
}

/// Delete the character at the cursor (x / Delete).
pub fn delete_char_at_cursor(doc: &Document, sel: &Selection) -> Transaction {
    let len = doc.len_chars();
    let ranges: Vec<(usize, usize)> = sel
        .ranges()
        .iter()
        .filter_map(|r| {
            let pos = r.head;
            if pos >= len {
                return None;
            }
            Some((pos, pos + 1))
        })
        .collect();
    let changes = build_delete_changeset_ranges(doc, &ranges);
    Transaction::new(changes)
}

/// Insert a newline before the cursor (O) or after (o).
pub fn open_line(doc: &Document, sel: &Selection, above: bool) -> (Transaction, Selection) {
    let len = doc.len_chars();
    let line = doc.char_to_line(sel.primary().head);

    // `above`: insert a newline at the line start (pushes the current line down).
    // `below`: insert a newline after the current line's content (before its own
    // newline, or at EOF). In both cases the cursor lands on the new empty line.
    let (insert_pos, cursor_pos) = if above {
        let start = doc.line_to_char(line);
        (start, start)
    } else {
        let end = doc.line_to_char(line) + doc.line_len_no_eol(line);
        (end, end + 1)
    };

    let changes = ChangeSetBuilder::new(len)
        .retain(insert_pos)
        .insert("\n")
        .retain(len - insert_pos)
        .build();

    // After the insert the document has `len + 1` chars, so the new line's start
    // (`end + 1` in the below case) is a valid cursor position.
    let new_sel = Selection::point(cursor_pos.min(len + 1));
    (Transaction::new(changes), new_sel)
}

/// Join line with the next line (J).
pub fn join_line(doc: &Document, sel: &Selection) -> Transaction {
    let len = doc.len_chars();
    let line = doc.char_to_line(sel.primary().head);
    if line + 1 >= doc.len_lines() {
        return Transaction::new(ChangeSet::new(len));
    }

    let this_line_end = doc.line_to_char(line + 1).saturating_sub(1); // newline char
    if this_line_end >= len {
        return Transaction::new(ChangeSet::new(len));
    }

    // Delete the newline, replace with space
    let changes = ChangeSetBuilder::new(len)
        .retain(this_line_end)
        .delete(1)
        .insert(" ")
        .retain(len - this_line_end - 1)
        .build();
    Transaction::new(changes)
}

/// Replace the character under each cursor with `c`.
pub fn replace_char(doc: &Document, sel: &Selection, c: char) -> Transaction {
    let len = doc.len_chars();
    let ranges: Vec<(usize, usize, char)> = sel
        .ranges()
        .iter()
        .filter_map(|r| {
            let pos = r.head;
            if pos >= len {
                return None;
            }
            Some((pos, pos + 1, c))
        })
        .collect();

    let mut builder = ChangeSetBuilder::new(len);
    let mut pos = 0usize;
    for (from, to, ch) in &ranges {
        if *from > pos {
            builder = builder.retain(from - pos);
        }
        builder = builder.delete(to - from).insert(ch.to_string());
        pos = *to;
    }
    if pos < len {
        builder = builder.retain(len - pos);
    }
    Transaction::new(builder.build())
}

/// `~` — toggle the case of `count` characters starting at each cursor, clamped
/// to the end of the line (vim's `~` does not cross lines).
pub fn toggle_case_chars(doc: &Document, sel: &Selection, count: usize) -> Transaction {
    let len = doc.len_chars();
    let rope = doc.rope();
    let mut builder = ChangeSetBuilder::new(len);
    let mut pos = 0usize;
    for range in sel.ranges() {
        let start = range.head;
        if start >= len || start < pos {
            continue;
        }
        let line = doc.char_to_line(start);
        let line_end = doc.line_to_char(line) + doc.line_len_no_eol(line);
        let end = (start + count).min(line_end).min(len);
        if end <= start {
            continue;
        }
        let replacement: String = rope
            .slice(start..end)
            .chars()
            .map(|c| map_case(c, CaseMode::Toggle))
            .collect();
        if start > pos {
            builder = builder.retain(start - pos);
        }
        builder = builder.delete(end - start).insert(replacement);
        pos = end;
    }
    if pos < len {
        builder = builder.retain(len - pos);
    }
    Transaction::new(builder.build())
}

/// `gu{motion}`/`gU{motion}`/`g~{motion}` (+ `guu`/`gUU`/`g~~` linewise doubling)
/// — remap the case of every character in the targeted range(s). `linewise`
/// mirrors [`delete`]/[`delete_lines`]: charwise ranges are `[from, to]`
/// inclusive; linewise expands to the full lines the ranges touch.
pub fn change_case(doc: &Document, sel: &Selection, mode: CaseMode, linewise: bool) -> Transaction {
    let len = doc.len_chars();
    let rope = doc.rope();

    let ranges: Vec<(usize, usize)> = if linewise {
        let mut lines: Vec<usize> = Vec::new();
        for range in sel.ranges() {
            let start_line = doc.char_to_line(range.from());
            let end_line = doc.char_to_line(range.to());
            lines.extend(start_line..=end_line);
        }
        lines.sort_unstable();
        lines.dedup();
        lines
            .into_iter()
            .map(|line| {
                let from = doc.line_to_char(line);
                let to = if line + 1 < rope.len_lines() {
                    doc.line_to_char(line + 1)
                } else {
                    len
                };
                (from, to)
            })
            .collect()
    } else {
        sel.ranges()
            .iter()
            .map(|r| (r.from(), (r.to() + 1).min(len)))
            .collect()
    };

    let mut builder = ChangeSetBuilder::new(len);
    let mut pos = 0usize;
    for (from, to) in ranges {
        if from >= to || from < pos {
            continue;
        }
        let replacement: String = rope
            .slice(from..to)
            .chars()
            .map(|c| map_case(c, mode))
            .collect();
        if from > pos {
            builder = builder.retain(from - pos);
        }
        builder = builder.delete(to - from).insert(replacement);
        pos = to;
    }
    if pos < len {
        builder = builder.retain(len - pos);
    }
    Transaction::new(builder.build())
}

/// `Ctrl-a`/`Ctrl-x` — adjust the nearest number on the cursor's line by `delta`
/// (positive to increment, negative to decrement). vim's rule: use the digit run
/// the cursor is inside, or the next digit run after the cursor on the same
/// line; a `-` immediately before the digits is treated as its sign. Returns
/// `(Transaction, new_cursor_head)` — the transaction is empty and the cursor
/// `None` if the line has no number at-or-after the cursor.
pub fn increment_number(
    doc: &Document,
    sel: &Selection,
    delta: i64,
) -> (Transaction, Option<usize>) {
    let len = doc.len_chars();
    let rope = doc.rope();
    let pos = sel.primary().head;
    if pos >= len {
        return (Transaction::new(ChangeSet::new(len)), None);
    }

    let line = doc.char_to_line(pos);
    let line_start = doc.line_to_char(line);
    let line_end = line_start + doc.line_len_no_eol(line);
    let line_chars: Vec<char> = rope.slice(line_start..line_end).chars().collect();
    let cursor_col = pos - line_start;

    let mut run: Option<(usize, usize)> = None;
    let mut i = 0usize;
    while i < line_chars.len() {
        if line_chars[i].is_ascii_digit() {
            let start = i;
            while i < line_chars.len() && line_chars[i].is_ascii_digit() {
                i += 1;
            }
            if i > cursor_col {
                run = Some((start, i));
                break;
            }
        } else {
            i += 1;
        }
    }
    let Some((mut start, end)) = run else {
        return (Transaction::new(ChangeSet::new(len)), None);
    };
    if start > 0 && line_chars[start - 1] == '-' {
        start -= 1;
    }
    let digits: String = line_chars[start..end].iter().collect();
    let Ok(value) = digits.parse::<i64>() else {
        return (Transaction::new(ChangeSet::new(len)), None);
    };
    let new_str = value.saturating_add(delta).to_string();

    let abs_from = line_start + start;
    let abs_to = line_start + end;
    let changes = ChangeSetBuilder::new(len)
        .retain(abs_from)
        .delete(abs_to - abs_from)
        .insert(new_str.clone())
        .retain(len - abs_to)
        .build();
    let new_head = abs_from + new_str.chars().count() - 1;
    (Transaction::new(changes), Some(new_head))
}

/// Indent (`>>`/`>{motion}`) or dedent (`<<`/`<{motion}`) every line spanned by
/// `sel` by one `unit` (a shiftwidth's worth of spaces, or a single tab char).
/// Dedent removes up to `unit`'s char-width of leading whitespace, whatever is
/// actually there (never more than the line has). Always linewise — vim forces
/// `<`/`>` to operate on whole lines regardless of the motion's own type.
pub fn indent_lines(doc: &Document, sel: &Selection, dedent: bool, unit: &str) -> Transaction {
    let rope = doc.rope();
    let len = doc.len_chars();
    let unit_width = unit.chars().count();

    let mut lines: Vec<usize> = Vec::new();
    for range in sel.ranges() {
        let start_line = doc.char_to_line(range.from());
        let end_line = doc.char_to_line(range.to());
        lines.extend(start_line..=end_line);
    }
    lines.sort_unstable();
    lines.dedup();

    let mut builder = ChangeSetBuilder::new(len);
    let mut pos = 0usize;
    for line in lines {
        let line_start = doc.line_to_char(line);
        if dedent {
            let line_str = rope.line(line).to_string();
            let remove = line_str
                .chars()
                .take_while(|c| *c == ' ' || *c == '\t')
                .count()
                .min(unit_width);
            if remove == 0 {
                continue;
            }
            if line_start > pos {
                builder = builder.retain(line_start - pos);
            }
            builder = builder.delete(remove);
            pos = line_start + remove;
        } else {
            if line_start > pos {
                builder = builder.retain(line_start - pos);
            }
            builder = builder.insert(unit.to_string());
            pos = line_start;
        }
    }
    if pos < len {
        builder = builder.retain(len - pos);
    }
    Transaction::new(builder.build())
}

#[cfg(test)]
mod tests {
    use super::*;
    use onda_core::{Document, Range, Selection, Transaction};

    fn doc(s: &str) -> Document {
        let mut d = Document::new_empty();
        let cs = ChangeSetBuilder::new(0).insert(s).build();
        d.apply(&Transaction::new(cs)).unwrap();
        d
    }

    #[test]
    fn delete_selection() {
        let mut d = doc("hello world");
        let sel = Selection::new(vec![Range::new(0, 4)], 0);
        let (tx, reg) = delete(&d, &sel);
        d.apply(&tx).unwrap();
        assert_eq!(d.rope().to_string(), " world");
        assert_eq!(reg.text, "hello");
    }

    #[test]
    fn paste_after_charwise() {
        let mut d = doc("hello");
        let sel = Selection::point(4);
        let reg = Register::new("!".to_string(), RegisterKind::Charwise);
        let tx = paste_after(&d, &sel, &reg);
        d.apply(&tx).unwrap();
        assert_eq!(d.rope().to_string(), "hello!");
    }

    #[test]
    fn insert_char_test() {
        let mut d = doc("helo");
        let sel = Selection::point(3);
        let tx = insert_char(&d, &sel, 'l');
        d.apply(&tx).unwrap();
        assert_eq!(d.rope().to_string(), "hello");
    }

    #[test]
    fn delete_before_cursor_basic() {
        let mut d = doc("hello");
        let sel = Selection::point(3);
        let tx = delete_before_cursor(&d, &sel);
        d.apply(&tx).unwrap();
        assert_eq!(d.rope().to_string(), "helo");
    }

    #[test]
    fn join_line_basic() {
        let mut d = doc("hello\nworld");
        let sel = Selection::point(0);
        let tx = join_line(&d, &sel);
        d.apply(&tx).unwrap();
        assert_eq!(d.rope().to_string(), "hello world");
    }

    /// Apply `open_line` and return (resulting text, cursor head).
    fn open(text: &str, cursor: usize, above: bool) -> (String, usize) {
        let mut d = doc(text);
        let sel = Selection::point(cursor);
        let (tx, new_sel) = open_line(&d, &sel, above);
        d.apply(&tx).unwrap();
        (d.rope().to_string(), new_sel.primary().head)
    }

    #[test]
    fn open_below_midfile_cursor_on_new_line() {
        // Regression: cursor must land on the *new* empty line, not the line below it.
        let (text, cur) = open("first\nsecond\n", 0, false);
        assert_eq!(text, "first\n\nsecond\n");
        assert_eq!(cur, 6); // start of the new empty line (line 1)
    }

    #[test]
    fn open_below_from_midline_cursor() {
        // Cursor in the middle of a line: open below still goes after the whole line.
        let (text, cur) = open("first\nsecond\n", 2, false); // cursor on 'r' of first
        assert_eq!(text, "first\n\nsecond\n");
        assert_eq!(cur, 6);
    }

    #[test]
    fn open_below_last_line_no_trailing_newline() {
        let (text, cur) = open("first\nsecond", 8, false); // cursor on last line
        assert_eq!(text, "first\nsecond\n");
        assert_eq!(cur, 13); // start of the new empty trailing line (after the \n)
    }

    #[test]
    fn open_below_empty_doc() {
        let (text, cur) = open("", 0, false);
        assert_eq!(text, "\n");
        assert_eq!(cur, 1);
    }

    #[test]
    fn open_above_first_line() {
        let (text, cur) = open("first\nsecond\n", 0, true);
        assert_eq!(text, "\nfirst\nsecond\n");
        assert_eq!(cur, 0);
    }

    #[test]
    fn open_above_second_line() {
        let (text, cur) = open("first\nsecond\n", 6, true); // start of "second"
        assert_eq!(text, "first\n\nsecond\n");
        assert_eq!(cur, 6);
    }

    #[test]
    fn indent_single_line() {
        let mut d = doc("foo\nbar\n");
        let sel = Selection::point(0); // on "foo"
        let tx = indent_lines(&d, &sel, false, "  ");
        d.apply(&tx).unwrap();
        assert_eq!(d.rope().to_string(), "  foo\nbar\n");
    }

    #[test]
    fn indent_multiple_lines_from_range_selection() {
        let mut d = doc("foo\nbar\nbaz\n");
        // range spans line 0 through line 1
        let sel = Selection::new(vec![Range::new(0, 5)], 0);
        let tx = indent_lines(&d, &sel, false, "  ");
        d.apply(&tx).unwrap();
        assert_eq!(d.rope().to_string(), "  foo\n  bar\nbaz\n");
    }

    #[test]
    fn dedent_removes_existing_whitespace() {
        let mut d = doc("    foo\nbar\n");
        let sel = Selection::point(0);
        let tx = indent_lines(&d, &sel, true, "  "); // unit width 2
        d.apply(&tx).unwrap();
        assert_eq!(d.rope().to_string(), "  foo\nbar\n");
    }

    #[test]
    fn dedent_never_removes_more_than_present() {
        let mut d = doc(" foo\n"); // only one leading space
        let sel = Selection::point(0);
        let tx = indent_lines(&d, &sel, true, "    "); // unit width 4
        d.apply(&tx).unwrap();
        assert_eq!(d.rope().to_string(), "foo\n");
    }

    #[test]
    fn dedent_noop_with_no_leading_whitespace() {
        let mut d = doc("foo\n");
        let sel = Selection::point(0);
        let tx = indent_lines(&d, &sel, true, "  ");
        assert!(tx.changes.is_empty());
        d.apply(&tx).unwrap();
        assert_eq!(d.rope().to_string(), "foo\n");
    }

    #[test]
    fn indent_tab_unit() {
        let mut d = doc("foo\n");
        let sel = Selection::point(0);
        let tx = indent_lines(&d, &sel, false, "\t");
        d.apply(&tx).unwrap();
        assert_eq!(d.rope().to_string(), "\tfoo\n");
    }

    #[test]
    fn toggle_case_single_char() {
        let mut d = doc("hello\n");
        let sel = Selection::point(0);
        let tx = toggle_case_chars(&d, &sel, 1);
        d.apply(&tx).unwrap();
        assert_eq!(d.rope().to_string(), "Hello\n");
    }

    #[test]
    fn toggle_case_count_clamped_to_line() {
        let mut d = doc("ab\ncd\n");
        let sel = Selection::point(0);
        let tx = toggle_case_chars(&d, &sel, 10); // way past line end
        d.apply(&tx).unwrap();
        assert_eq!(d.rope().to_string(), "AB\ncd\n"); // doesn't cross into next line
    }

    #[test]
    fn toggle_case_non_alpha_unchanged() {
        let mut d = doc("a1!\n");
        let sel = Selection::point(0);
        let tx = toggle_case_chars(&d, &sel, 3);
        d.apply(&tx).unwrap();
        assert_eq!(d.rope().to_string(), "A1!\n");
    }

    #[test]
    fn change_case_lowercase_charwise() {
        let mut d = doc("FOO bar\n");
        let sel = Selection::new(vec![Range::new(0, 2)], 0); // "FOO"
        let tx = change_case(&d, &sel, CaseMode::Lower, false);
        d.apply(&tx).unwrap();
        assert_eq!(d.rope().to_string(), "foo bar\n");
    }

    #[test]
    fn change_case_uppercase_charwise() {
        let mut d = doc("foo bar\n");
        let sel = Selection::new(vec![Range::new(0, 2)], 0); // "foo"
        let tx = change_case(&d, &sel, CaseMode::Upper, false);
        d.apply(&tx).unwrap();
        assert_eq!(d.rope().to_string(), "FOO bar\n");
    }

    #[test]
    fn change_case_toggle_charwise() {
        let mut d = doc("Foo Bar\n");
        let sel = Selection::new(vec![Range::new(0, 6)], 0); // whole line minus \n
        let tx = change_case(&d, &sel, CaseMode::Toggle, false);
        d.apply(&tx).unwrap();
        assert_eq!(d.rope().to_string(), "fOO bAR\n");
    }

    #[test]
    fn increment_cursor_inside_number() {
        let mut d = doc("val 41 end\n");
        let sel = Selection::point(5); // '1' of 41
        let (tx, head) = increment_number(&d, &sel, 1);
        d.apply(&tx).unwrap();
        assert_eq!(d.rope().to_string(), "val 42 end\n");
        assert_eq!(head, Some(5)); // last digit of "42"
    }

    #[test]
    fn decrement_cursor_before_number() {
        let mut d = doc("count=10\n");
        let sel = Selection::point(0); // before the number
        let (tx, head) = increment_number(&d, &sel, -1);
        d.apply(&tx).unwrap();
        assert_eq!(d.rope().to_string(), "count=9\n");
        assert_eq!(head, Some(6));
    }

    #[test]
    fn increment_skips_past_number_before_cursor() {
        // cursor is after the first number, so `Ctrl-a` targets the *next* one.
        let mut d = doc("1 and 2\n");
        let sel = Selection::point(2); // on "and"
        let (tx, head) = increment_number(&d, &sel, 1);
        d.apply(&tx).unwrap();
        assert_eq!(d.rope().to_string(), "1 and 3\n");
        assert_eq!(head, Some(6));
    }

    #[test]
    fn increment_negative_number_treats_sign() {
        let mut d = doc("-5\n");
        let sel = Selection::point(0);
        let (tx, head) = increment_number(&d, &sel, 1);
        d.apply(&tx).unwrap();
        assert_eq!(d.rope().to_string(), "-4\n");
        assert_eq!(head, Some(1));
    }

    #[test]
    fn increment_no_number_on_line_is_noop() {
        let mut d = doc("no digits here\n");
        let sel = Selection::point(0);
        let (tx, head) = increment_number(&d, &sel, 1);
        assert!(tx.changes.is_empty());
        assert_eq!(head, None);
        d.apply(&tx).unwrap();
        assert_eq!(d.rope().to_string(), "no digits here\n");
    }

    #[test]
    fn increment_grows_digit_width() {
        let mut d = doc("99\n");
        let sel = Selection::point(0);
        let (tx, head) = increment_number(&d, &sel, 1);
        d.apply(&tx).unwrap();
        assert_eq!(d.rope().to_string(), "100\n");
        assert_eq!(head, Some(2));
    }

    #[test]
    fn change_case_linewise_doubling() {
        let mut d = doc("Foo\nBar\nBaz\n");
        // linewise range spanning lines 0-1 (mirrors how OperatorLine builds its range)
        let sel = Selection::new(vec![Range::new(0, 4)], 0);
        let tx = change_case(&d, &sel, CaseMode::Upper, true);
        d.apply(&tx).unwrap();
        assert_eq!(d.rope().to_string(), "FOO\nBAR\nBaz\n");
    }
}
