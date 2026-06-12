//! Text search utilities for onda-modal.
//!
//! Provides Vim-compatible regex searching over a `ropey::Rope`, including
//! forward/backward find, find-all, and `:s` substitution with capture-group
//! references.

use onda_core::Range;
use regex::Regex;
use ropey::Rope;

// ── Vim pattern translation ───────────────────────────────────────────────────

/// Translate a Vim-style search pattern to a Rust `regex`-crate pattern.
///
/// Translations performed:
/// - `\<` and `\>` word boundaries → `\b`
/// - `\.` literal dot → `\.` (already correct)
/// - `\(` / `\)` grouping (very-magic-off) → `(` / `)`
/// - `\|` alternation → `|`
/// - `\+` one-or-more → `+`
/// - `\?` optional → `?`
/// - `\{n,m\}` repetition → `{n,m}`
///
/// Additionally, if `smartcase` is `true` and the entire pattern is lowercase,
/// `(?i)` is prepended to enable case-insensitive matching.
pub fn vim_pattern_to_regex(pattern: &str, smartcase: bool) -> Result<Regex, regex::Error> {
    let mut out = String::with_capacity(pattern.len() + 16);

    if smartcase && pattern.chars().all(|c| !c.is_uppercase()) {
        out.push_str("(?i)");
    }

    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('<') | Some('>') => out.push_str("\\b"),
                Some('(') => out.push('('),
                Some(')') => out.push(')'),
                Some('|') => out.push('|'),
                Some('+') => out.push('+'),
                Some('?') => out.push('?'),
                Some('{') => {
                    // Collect until closing \}
                    out.push('{');
                    loop {
                        match chars.next() {
                            None => break,
                            Some('\\') => {
                                // Peek by checking the next char
                                match chars.next() {
                                    Some('}') => break, // end of \{...\}
                                    Some(c2) => {
                                        out.push('\\');
                                        out.push(c2);
                                    }
                                    None => {
                                        out.push('\\');
                                        break;
                                    }
                                }
                            }
                            Some(inner) => out.push(inner),
                        }
                    }
                    out.push('}');
                }
                Some(nc) => {
                    out.push('\\');
                    out.push(nc);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }

    Regex::new(&out)
}

// ── SearchDir / SearchState ───────────────────────────────────────────────────

/// Direction for incremental search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchDir {
    Forward,
    Backward,
}

/// Persistent search state carried by the editor.
#[derive(Debug, Clone)]
pub struct SearchState {
    pub pattern: String,
    pub regex: Option<Regex>,
    pub direction: SearchDir,
    pub highlight: bool,
}

impl SearchState {
    pub fn new() -> Self {
        Self {
            pattern: String::new(),
            regex: None,
            direction: SearchDir::Forward,
            highlight: true,
        }
    }

    /// Update the pattern, recompile the regex, and apply smartcase.
    pub fn set_pattern(&mut self, pattern: impl Into<String>, smartcase: bool) {
        self.pattern = pattern.into();
        self.regex = vim_pattern_to_regex(&self.pattern, smartcase).ok();
    }
}

impl Default for SearchState {
    fn default() -> Self {
        Self::new()
    }
}

// ── Find helpers ──────────────────────────────────────────────────────────────

/// Convert a rope char-offset to a byte offset in the rope's string
/// representation.  We materialise the rope to a `String` once per call; the
/// caller may cache it for performance-critical paths.
fn char_to_byte(text: &str, char_idx: usize) -> usize {
    text.char_indices()
        .nth(char_idx)
        .map(|(b, _)| b)
        .unwrap_or(text.len())
}

fn byte_to_char(text: &str, byte_idx: usize) -> usize {
    text[..byte_idx].chars().count()
}

/// Find the next match of `regex` in `rope` starting at char position
/// `from_char`.  Wraps around to the beginning of the document if no match is
/// found after `from_char`.
pub fn find_next(rope: &Rope, regex: &Regex, from_char: usize) -> Option<Range> {
    let text = rope.to_string();
    let len_chars = rope.len_chars();

    // Search from from_char onwards.
    let start_byte = char_to_byte(&text, from_char.min(len_chars));
    if let Some(m) = regex.find(&text[start_byte..]) {
        let anchor = byte_to_char(&text, start_byte + m.start());
        let head = byte_to_char(&text, start_byte + m.end());
        return Some(Range::new(anchor, head));
    }

    // Wrap: search from the beginning up to from_char.
    if from_char > 0 {
        let end_byte = char_to_byte(&text, from_char.min(len_chars));
        if let Some(m) = regex.find(&text[..end_byte]) {
            let anchor = byte_to_char(&text, m.start());
            let head = byte_to_char(&text, m.end());
            return Some(Range::new(anchor, head));
        }
    }

    None
}

/// Find the previous match of `regex` in `rope` before char position
/// `from_char`.  Wraps around to the end of the document if no match is found
/// before `from_char`.
pub fn find_prev(rope: &Rope, regex: &Regex, from_char: usize) -> Option<Range> {
    let text = rope.to_string();

    // Collect all matches in the document, then return the last one before from_char.
    let all: Vec<_> = regex.find_iter(&text).collect();

    // Last match whose start char is < from_char.
    let prev = all
        .iter()
        .rev()
        .find(|m| byte_to_char(&text, m.start()) < from_char);

    if let Some(m) = prev {
        let anchor = byte_to_char(&text, m.start());
        let head = byte_to_char(&text, m.end());
        return Some(Range::new(anchor, head));
    }

    // Wrap: last match anywhere.
    if let Some(m) = all.last() {
        let anchor = byte_to_char(&text, m.start());
        let head = byte_to_char(&text, m.end());
        return Some(Range::new(anchor, head));
    }

    None
}

/// Return all matches of `regex` in `rope` as a `Vec<Range>`.
pub fn find_all(rope: &Rope, regex: &Regex) -> Vec<Range> {
    let text = rope.to_string();
    regex
        .find_iter(&text)
        .map(|m| {
            let anchor = byte_to_char(&text, m.start());
            let head = byte_to_char(&text, m.end());
            Range::new(anchor, head)
        })
        .collect()
}

// ── Substitution ─────────────────────────────────────────────────────────────

/// Translate a Vim-style replacement string (using `\1`..`\9` for back-references
/// and `\\` for a literal backslash) into a `regex`-crate replacement string
/// (which uses `$1`..`$9`).
fn translate_replacement(vim_repl: &str) -> String {
    let mut out = String::with_capacity(vim_repl.len());
    let mut chars = vim_repl.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.peek() {
                Some(&d) if d.is_ascii_digit() => {
                    out.push('$');
                    out.push(chars.next().unwrap());
                }
                Some(&'\\') => {
                    out.push('\\');
                    chars.next();
                }
                Some(&'n') => {
                    out.push('\n');
                    chars.next();
                }
                Some(&'t') => {
                    out.push('\t');
                    chars.next();
                }
                _ => {
                    out.push('\\');
                }
            }
        } else if c == '$' {
            // Escape bare `$` so it is treated literally by the regex replacer.
            out.push_str("$$");
        } else {
            out.push(c);
        }
    }
    out
}

/// Perform a substitution on the slice of `rope` between char indices
/// `line_start` and `line_end` (exclusive).
///
/// - `global`: replace all occurrences in the range; otherwise only the first.
/// - Returns `Some((new_text, count))` where `new_text` replaces the original
///   range and `count` is the number of substitutions made.
/// - Returns `None` if the regex does not match at all.
pub fn substitute(
    rope: &Rope,
    regex: &Regex,
    replacement: &str,
    global: bool,
    line_start: usize,
    line_end: usize,
) -> Option<(String, usize)> {
    let text = rope.to_string();

    // Convert char bounds to byte bounds.
    let len_chars = rope.len_chars();
    let start_byte = char_to_byte(&text, line_start.min(len_chars));
    let end_byte = char_to_byte(&text, line_end.min(len_chars));

    let slice = &text[start_byte..end_byte];
    let repl = translate_replacement(replacement);

    let mut count = 0usize;
    let result: String = if global {
        regex
            .replace_all(slice, |caps: &regex::Captures<'_>| {
                count += 1;
                caps.expand(&repl, &mut String::new());
                let mut expanded = String::new();
                caps.expand(&repl, &mut expanded);
                expanded
            })
            .into_owned()
    } else {
        let replaced = regex.replace(slice, |caps: &regex::Captures<'_>| {
            count += 1;
            let mut expanded = String::new();
            caps.expand(&repl, &mut expanded);
            expanded
        });
        replaced.into_owned()
    };

    if count == 0 {
        return None;
    }
    Some((result, count))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ropey::Rope;

    fn rope(s: &str) -> Rope {
        Rope::from_str(s)
    }

    #[test]
    fn smartcase_lowercase_insensitive() {
        let re = vim_pattern_to_regex("hello", true).unwrap();
        assert!(re.is_match("HELLO"));
        assert!(re.is_match("Hello"));
    }

    #[test]
    fn smartcase_mixed_case_sensitive() {
        let re = vim_pattern_to_regex("Hello", true).unwrap();
        assert!(re.is_match("Hello"));
        assert!(!re.is_match("hello"));
    }

    #[test]
    fn word_boundary_translation() {
        let re = vim_pattern_to_regex("\\<word\\>", false).unwrap();
        assert!(re.is_match("a word here"));
        assert!(!re.is_match("awordhere"));
    }

    #[test]
    fn find_next_basic() {
        // "hello world hello"
        //  0123456789012345678
        // Searching from char 1: the suffix "ello world hello" contains "hello"
        // starting at position 12 (the second occurrence).
        let r = rope("hello world hello");
        let re = Regex::new("hello").unwrap();
        let m = find_next(&r, &re, 1).expect("should find");
        assert_eq!(m.from(), 12);
        assert_eq!(m.to(), 17);
    }

    #[test]
    fn find_next_wraps() {
        let r = rope("hello world");
        let re = Regex::new("hello").unwrap();
        // from_char = 5 (past the only "hello"), should wrap around
        let m = find_next(&r, &re, 5).expect("wrap around");
        assert_eq!(m.from(), 0);
        assert_eq!(m.to(), 5);
    }

    #[test]
    fn find_prev_basic() {
        let r = rope("hello world hello");
        let re = Regex::new("hello").unwrap();
        // from_char = 13 (inside second hello), prev match should be at 0
        let m = find_prev(&r, &re, 12).expect("prev match");
        assert_eq!(m.from(), 0);
    }

    #[test]
    fn find_prev_wraps() {
        let r = rope("hello world hello");
        let re = Regex::new("hello").unwrap();
        // from_char = 0, nothing before it, wraps to last match (char 12)
        let m = find_prev(&r, &re, 0).expect("wrap prev");
        assert_eq!(m.from(), 12);
    }

    #[test]
    fn find_all_basic() {
        let r = rope("aaa bbb aaa");
        let re = Regex::new("aaa").unwrap();
        let matches = find_all(&r, &re);
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].from(), 0);
        assert_eq!(matches[1].from(), 8);
    }

    #[test]
    fn substitute_global() {
        let r = rope("foo bar foo");
        let re = Regex::new("foo").unwrap();
        let (new_text, count) = substitute(&r, &re, "baz", true, 0, 11).unwrap();
        assert_eq!(new_text, "baz bar baz");
        assert_eq!(count, 2);
    }

    #[test]
    fn substitute_first_only() {
        let r = rope("foo bar foo");
        let re = Regex::new("foo").unwrap();
        let (new_text, count) = substitute(&r, &re, "baz", false, 0, 11).unwrap();
        assert_eq!(new_text, "baz bar foo");
        assert_eq!(count, 1);
    }

    #[test]
    fn substitute_capture_group() {
        let r = rope("hello world");
        let re = Regex::new("(hello) (world)").unwrap();
        let (new_text, count) = substitute(&r, &re, "\\2 \\1", false, 0, 11).unwrap();
        assert_eq!(new_text, "world hello");
        assert_eq!(count, 1);
    }

    #[test]
    fn substitute_no_match_returns_none() {
        let r = rope("hello");
        let re = Regex::new("xyz").unwrap();
        assert!(substitute(&r, &re, "abc", true, 0, 5).is_none());
    }

    #[test]
    fn search_state_set_pattern() {
        let mut state = SearchState::new();
        state.set_pattern("foo", true);
        assert!(state.regex.is_some());
        assert_eq!(state.pattern, "foo");
    }
}
