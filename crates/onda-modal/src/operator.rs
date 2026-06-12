use onda_core::{transaction::ChangeSetBuilder, ChangeSet, Document, Selection, Transaction};

use crate::register::{Register, RegisterKind};

/// The operator type: what we do with a motion's range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operator {
    Delete,
    Change,
    Yank,
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

    let insert_pos = if above {
        doc.line_to_char(line)
    } else if line + 1 < doc.len_lines() {
        doc.line_to_char(line + 1)
    } else {
        len
    };

    let changes = ChangeSetBuilder::new(len)
        .retain(insert_pos)
        .insert("\n")
        .retain(len - insert_pos)
        .build();

    let cursor_pos = if above { insert_pos } else { insert_pos + 1 };
    let new_sel = Selection::point(cursor_pos.min(len));
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
}
