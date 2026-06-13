//! CSV/TSV virtual-table engine (onda W27).
//!
//! Pure parsing + layout logic for the table *view*. The rope stays the single
//! source of truth (ADR: table mode is view-only); this module turns a line of text
//! into fields and computes per-column display widths and cell ranges so the editor
//! can render aligned columns and run cell/column motions without a parallel data
//! model.

use unicode_width::UnicodeWidthStr;

/// Sniffed CSV dialect for a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dialect {
    pub delimiter: char,
    pub quote: char,
    pub has_header: bool,
}

impl Default for Dialect {
    fn default() -> Self {
        Self {
            delimiter: ',',
            quote: '"',
            has_header: true,
        }
    }
}

/// Candidate delimiters tried during sniffing, in preference order.
const CANDIDATES: [char; 4] = [',', '\t', ';', '|'];

/// Strip a leading UTF-8 BOM and a trailing `\r` from a physical line.
pub fn clean_line(line: &str) -> &str {
    let line = line.strip_prefix('\u{feff}').unwrap_or(line);
    line.strip_suffix('\r').unwrap_or(line)
}

/// Sniff the dialect from a sample (the first chunk of the file).
///
/// Picks the delimiter that yields the most *consistent* field count across the
/// sample's lines (ties broken by candidate order). The header heuristic: a header
/// is likely when the first row is entirely non-numeric but some later row has a
/// numeric field.
pub fn sniff(sample: &str) -> Dialect {
    let lines: Vec<&str> = sample
        .lines()
        .map(clean_line)
        .filter(|l| !l.is_empty())
        .take(20)
        .collect();
    if lines.is_empty() {
        return Dialect::default();
    }

    let mut best = (',', 0i64, 1usize); // (delim, score, field_count)
    for &delim in &CANDIDATES {
        let counts: Vec<usize> = lines
            .iter()
            .map(|l| parse_fields(l, delim, '"').len())
            .collect();
        let max = *counts.iter().max().unwrap_or(&1);
        if max < 2 {
            continue; // this delimiter doesn't split anything
        }
        // Score: reward consistency (rows matching the modal count), penalize raggedness.
        let consistent = counts.iter().filter(|&&c| c == max).count() as i64;
        let score = consistent * 1000 - (lines.len() as i64 - consistent) * 10 + max as i64;
        if score > best.1 {
            best = (delim, score, max);
        }
    }
    let delimiter = if best.1 == 0 { ',' } else { best.0 };

    let has_header = detect_header(&lines, delimiter);
    Dialect {
        delimiter,
        quote: '"',
        has_header,
    }
}

fn detect_header(lines: &[&str], delim: char) -> bool {
    if lines.len() < 2 {
        return true; // default to header for a single-line sample
    }
    let first = parse_fields(lines[0], delim, '"');
    let first_numeric = first.iter().any(|f| is_numeric(f));
    if first_numeric {
        return false; // numeric values in row 0 → probably data, not a header
    }
    // Header likely if a later row contains a numeric field.
    lines[1..]
        .iter()
        .any(|l| parse_fields(l, delim, '"').iter().any(|f| is_numeric(f)))
}

fn is_numeric(s: &str) -> bool {
    let t = s.trim();
    !t.is_empty() && t.parse::<f64>().is_ok()
}

/// Parse one line into field *values* (quotes removed, `""` unescaped).
pub fn parse_fields(line: &str, delim: char, quote: char) -> Vec<String> {
    field_spans(line, delim, quote)
        .into_iter()
        .map(|f| f.value)
        .collect()
}

/// A single parsed field with its character span in the (cleaned) line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldSpan {
    /// Char offset of the field token start (includes a leading quote if quoted).
    pub start: usize,
    /// Char offset one past the field token end (before the delimiter).
    pub end: usize,
    /// The decoded field value.
    pub value: String,
}

/// Parse a line into field spans (char offsets), honoring quoting rules.
///
/// `start..end` covers the raw token between delimiters (including surrounding
/// quotes); `value` is the decoded content. Use spans for `ac`/`ic` cell objects.
pub fn field_spans(line: &str, delim: char, quote: char) -> Vec<FieldSpan> {
    let mut out = Vec::new();
    let chars: Vec<char> = line.chars().collect();
    let n = chars.len();
    let mut i = 0;
    // Empty line → a single empty field (mirrors most CSV readers' row semantics).
    if n == 0 {
        return vec![FieldSpan {
            start: 0,
            end: 0,
            value: String::new(),
        }];
    }
    loop {
        let start = i;
        let mut value = String::new();
        if i < n && chars[i] == quote {
            // Quoted field.
            i += 1;
            while i < n {
                if chars[i] == quote {
                    if i + 1 < n && chars[i + 1] == quote {
                        value.push(quote);
                        i += 2;
                    } else {
                        i += 1; // closing quote
                        break;
                    }
                } else {
                    value.push(chars[i]);
                    i += 1;
                }
            }
        } else {
            while i < n && chars[i] != delim {
                value.push(chars[i]);
                i += 1;
            }
        }
        out.push(FieldSpan {
            start,
            end: i,
            value,
        });
        if i < n && chars[i] == delim {
            i += 1;
            if i == n {
                // Trailing delimiter → a final empty field.
                out.push(FieldSpan {
                    start: n,
                    end: n,
                    value: String::new(),
                });
                break;
            }
        } else {
            break;
        }
    }
    out
}

/// True if `line` ends inside an open quote (the record continues on the next line).
pub fn has_open_quote(line: &str, quote: char) -> bool {
    let mut open = false;
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == quote {
            if open && i + 1 < chars.len() && chars[i + 1] == quote {
                i += 2; // escaped quote inside a quoted field
                continue;
            }
            open = !open;
        }
        i += 1;
    }
    open
}

/// Per-column display widths and column count over a set of rows.
#[derive(Debug, Clone, Default)]
pub struct ColumnLayout {
    /// Display width (unicode) of the widest cell per column.
    pub widths: Vec<usize>,
}

impl ColumnLayout {
    pub fn column_count(&self) -> usize {
        self.widths.len()
    }
}

/// Compute the column layout from already-parsed rows.
pub fn column_layout(rows: &[Vec<String>]) -> ColumnLayout {
    let mut widths: Vec<usize> = Vec::new();
    for row in rows {
        for (c, cell) in row.iter().enumerate() {
            let w = UnicodeWidthStr::width(cell.as_str());
            if c >= widths.len() {
                widths.push(w);
            } else if w > widths[c] {
                widths[c] = w;
            }
        }
    }
    ColumnLayout { widths }
}

/// A row whose field count differs from the header/expected count.
pub fn is_ragged(row_field_count: usize, expected: usize) -> bool {
    row_field_count != expected
}

// ── Cell / column motions & text objects (T27.3) ────────────────────────────────
//
// These operate on line-local char offsets (from `field_spans`). The editor maps a
// returned `(start, end)` to absolute buffer offsets for selection/operators.

/// Index of the field (column) containing `cursor`, a char offset in the line.
pub fn cell_index_at(spans: &[FieldSpan], cursor: usize) -> Option<usize> {
    spans
        .iter()
        .position(|f| cursor >= f.start && cursor <= f.end)
        // A cursor sitting on a delimiter past a field's end belongs to the next field;
        // `<=` above can match two adjacent spans, so prefer the first.
        .or_else(|| spans.iter().rposition(|f| cursor >= f.start))
}

/// Inner cell range (`ic`): the field content, excluding surrounding quotes.
pub fn cell_inner(
    spans: &[FieldSpan],
    idx: usize,
    line: &str,
    quote: char,
) -> Option<(usize, usize)> {
    let f = spans.get(idx)?;
    let chars: Vec<char> = line.chars().collect();
    if f.end > f.start && chars.get(f.start) == Some(&quote) && chars.get(f.end - 1) == Some(&quote)
    {
        // Quoted: strip the surrounding quote chars.
        Some((f.start + 1, f.end.saturating_sub(1)))
    } else {
        Some((f.start, f.end))
    }
}

/// Outer cell range (`ac`): the field token plus its trailing delimiter, or the
/// leading delimiter when it's the last field.
pub fn cell_outer(spans: &[FieldSpan], idx: usize) -> Option<(usize, usize)> {
    let f = spans.get(idx)?;
    if idx + 1 < spans.len() {
        // Include the delimiter that follows this field (one char).
        Some((f.start, f.end + 1))
    } else if idx > 0 {
        // Last field: absorb the preceding delimiter instead.
        let prev = &spans[idx - 1];
        Some((prev.end, f.end))
    } else {
        Some((f.start, f.end))
    }
}

/// `Tab`/`S-Tab` cell motion: the start offset of the next/previous field, if any.
pub fn next_cell_start(spans: &[FieldSpan], cursor: usize) -> Option<usize> {
    let idx = cell_index_at(spans, cursor)?;
    spans.get(idx + 1).map(|f| f.start)
}

pub fn prev_cell_start(spans: &[FieldSpan], cursor: usize) -> Option<usize> {
    let idx = cell_index_at(spans, cursor)?;
    if idx == 0 {
        None
    } else {
        spans.get(idx - 1).map(|f| f.start)
    }
}

/// Inner cell ranges for column `col` across `rows` (for `iC` → multicursor, ADR-006).
/// Each item is `(row_index, (start, end))` with line-local offsets.
pub fn column_cells_inner(
    rows: &[&str],
    col: usize,
    delim: char,
    quote: char,
) -> Vec<(usize, (usize, usize))> {
    let mut out = Vec::new();
    for (r, line) in rows.iter().enumerate() {
        let cleaned = clean_line(line);
        let spans = field_spans(cleaned, delim, quote);
        if let Some(range) = cell_inner(&spans, col, cleaned, quote) {
            out.push((r, range));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(line: &str, d: char) -> Vec<String> {
        parse_fields(line, d, '"')
    }

    #[test]
    fn plain_comma() {
        assert_eq!(fields("a,b,c", ','), vec!["a", "b", "c"]);
    }

    #[test]
    fn quoted_comma_inside_field() {
        assert_eq!(fields(r#"a,"b,c",d"#, ','), vec!["a", "b,c", "d"]);
    }

    #[test]
    fn escaped_quotes() {
        assert_eq!(fields(r#""he said ""hi""""#, ','), vec![r#"he said "hi""#]);
    }

    #[test]
    fn trailing_delimiter_empty_field() {
        assert_eq!(fields("a,b,", ','), vec!["a", "b", ""]);
    }

    #[test]
    fn empty_line_is_one_empty_field() {
        assert_eq!(fields("", ','), vec![""]);
    }

    #[test]
    fn clean_line_strips_bom_and_cr() {
        assert_eq!(clean_line("\u{feff}a,b\r"), "a,b");
    }

    #[test]
    fn sniff_picks_tab() {
        let d = sniff("a\tb\tc\n1\t2\t3\n");
        assert_eq!(d.delimiter, '\t');
    }

    #[test]
    fn sniff_picks_semicolon() {
        let d = sniff("name;age;city\nAlice;30;NYC\n");
        assert_eq!(d.delimiter, ';');
    }

    #[test]
    fn sniff_picks_comma_over_others() {
        let d = sniff("a,b,c\n1,2,3\n4,5,6\n");
        assert_eq!(d.delimiter, ',');
    }

    #[test]
    fn header_detected_when_first_row_textual() {
        let d = sniff("name,age\nAlice,30\nBob,25\n");
        assert!(d.has_header);
    }

    #[test]
    fn header_absent_when_first_row_numeric() {
        let d = sniff("1,2,3\n4,5,6\n");
        assert!(!d.has_header);
    }

    #[test]
    fn field_spans_cover_tokens() {
        let spans = field_spans("a,bb,ccc", ',', '"');
        assert_eq!(
            spans[0],
            FieldSpan {
                start: 0,
                end: 1,
                value: "a".into()
            }
        );
        assert_eq!(
            spans[1],
            FieldSpan {
                start: 2,
                end: 4,
                value: "bb".into()
            }
        );
        assert_eq!(
            spans[2],
            FieldSpan {
                start: 5,
                end: 8,
                value: "ccc".into()
            }
        );
    }

    #[test]
    fn field_spans_quoted_token_includes_quotes() {
        let spans = field_spans(r#"x,"a,b",y"#, ',', '"');
        // The middle token spans the quotes: chars 2..7  ("a,b")
        assert_eq!(spans[1].value, "a,b");
        assert_eq!(
            &r#"x,"a,b",y"#.chars().collect::<String>()[spans[1].start..spans[1].end],
            r#""a,b""#
        );
    }

    #[test]
    fn open_quote_detection() {
        assert!(has_open_quote(r#"a,"unterminated"#, '"'));
        assert!(!has_open_quote(r#"a,"closed",b"#, '"'));
        assert!(!has_open_quote(r#""a""b""#, '"')); // escaped quotes balance
    }

    #[test]
    fn column_widths_unicode() {
        let rows = vec![
            vec!["id".to_string(), "name".to_string()],
            vec!["1".to_string(), "Alice".to_string()],
            vec!["2".to_string(), "李雷".to_string()], // CJK width 2 each = 4
        ];
        let layout = column_layout(&rows);
        assert_eq!(layout.widths, vec![2, 5]); // "name"=4? "Alice"=5; 李雷=4 → max 5
        assert_eq!(layout.column_count(), 2);
    }

    #[test]
    fn ragged_detection() {
        assert!(is_ragged(2, 3));
        assert!(!is_ragged(3, 3));
    }

    #[test]
    fn cell_index_at_cursor() {
        let spans = field_spans("aa,bb,cc", ',', '"');
        assert_eq!(cell_index_at(&spans, 0), Some(0)); // in "aa"
        assert_eq!(cell_index_at(&spans, 4), Some(1)); // in "bb"
        assert_eq!(cell_index_at(&spans, 7), Some(2)); // in "cc"
    }

    #[test]
    fn cell_inner_unquoted_and_quoted() {
        let line = r#"x,"a,b",y"#;
        let spans = field_spans(line, ',', '"');
        assert_eq!(cell_inner(&spans, 0, line, '"'), Some((0, 1))); // x
                                                                    // Quoted middle: inner strips the quotes.
        let (s, e) = cell_inner(&spans, 1, line, '"').unwrap();
        assert_eq!(&line.chars().collect::<String>()[s..e], "a,b");
    }

    #[test]
    fn cell_outer_includes_delimiter() {
        let spans = field_spans("a,b,c", ',', '"');
        // First cell outer = "a," (token + trailing delim).
        assert_eq!(cell_outer(&spans, 0), Some((0, 2)));
        // Last cell outer absorbs the preceding delim = ",c".
        assert_eq!(cell_outer(&spans, 2), Some((3, 5)));
    }

    #[test]
    fn tab_motion_between_cells() {
        let spans = field_spans("aa,bb,cc", ',', '"');
        assert_eq!(next_cell_start(&spans, 0), Some(3)); // → "bb"
        assert_eq!(next_cell_start(&spans, 3), Some(6)); // → "cc"
        assert_eq!(next_cell_start(&spans, 6), None); // last cell
        assert_eq!(prev_cell_start(&spans, 6), Some(3));
        assert_eq!(prev_cell_start(&spans, 0), None);
    }

    #[test]
    fn column_cells_for_multicursor() {
        let rows = ["id,name", "1,Alice", "2,Bob"];
        let cells = column_cells_inner(&rows, 1, ',', '"');
        assert_eq!(cells.len(), 3);
        // Row 1, column 1 inner = "Alice".
        let (_, (s, e)) = cells[1];
        assert_eq!(&rows[1][s..e], "Alice");
    }
}
