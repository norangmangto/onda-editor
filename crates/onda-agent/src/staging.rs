//! Proposed-change staging with rebase over concurrent user edits (T24.1).
//!
//! Agent file edits never touch buffers directly: they accumulate here, keyed by
//! file, recorded against the *base* content the agent saw (what we served via
//! `fs/read_text_file`). When the user edits the same file before the review is
//! applied, the agent's change is rebased over the user's via a line-level 3-way
//! merge — clean when the two sets of changes don't overlap, otherwise the file is
//! marked **stale** rather than corrupted (the agent is told on apply).
//!
//! This is the phase's load-bearing invariant, so it lives in pure, heavily-tested
//! logic with no I/O.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// One agent-proposed file change, recorded against the content it was based on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposedEdit {
    /// File content the agent edited against (served from buffer state).
    pub base: String,
    /// The agent's proposed new content.
    pub proposed: String,
    /// Set once a rebase against current user content fails.
    pub stale: bool,
}

/// Result of resolving a proposed edit against the current file content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// No effective change (proposed == current).
    Unchanged,
    /// A clean merge; apply this content.
    Clean(String),
    /// Agent and user edits conflict; the edit is stale and must be re-proposed.
    Stale,
}

/// Per-session collection of proposed edits.
#[derive(Debug, Default)]
pub struct StagingArea {
    files: HashMap<PathBuf, ProposedEdit>,
}

impl StagingArea {
    pub fn new() -> Self {
        Self::default()
    }

    /// Stage (or replace) a proposed edit for `path`.
    pub fn stage(&mut self, path: impl Into<PathBuf>, base: String, proposed: String) {
        self.files.insert(
            path.into(),
            ProposedEdit {
                base,
                proposed,
                stale: false,
            },
        );
    }

    pub fn get(&self, path: &Path) -> Option<&ProposedEdit> {
        self.files.get(path)
    }

    pub fn files(&self) -> impl Iterator<Item = (&PathBuf, &ProposedEdit)> {
        self.files.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    pub fn remove(&mut self, path: &Path) -> Option<ProposedEdit> {
        self.files.remove(path)
    }

    /// Resolve the staged edit for `path` against the file's `current` content,
    /// rebasing over any concurrent user edits. Marks the edit stale on conflict.
    pub fn resolve(&mut self, path: &Path, current: &str) -> Option<Resolution> {
        let edit = self.files.get_mut(path)?;
        let res = three_way_merge(&edit.base, &edit.proposed, current);
        if matches!(res, Resolution::Stale) {
            edit.stale = true;
        }
        Some(res)
    }
}

/// Line-level 3-way merge. Returns:
/// - `Unchanged` when the proposal makes no change to `current`,
/// - `Clean(merged)` when agent and user edits are disjoint,
/// - `Stale` when they overlap.
pub fn three_way_merge(base: &str, proposed: &str, current: &str) -> Resolution {
    if proposed == current {
        return Resolution::Unchanged;
    }
    // Fast path: the user hasn't touched the file since the agent read it.
    if current == base {
        return Resolution::Clean(proposed.to_string());
    }

    let base_lines = split_lines(base);
    let agent_hunks = diff_hunks(&base_lines, &split_lines(proposed));
    let user_hunks = diff_hunks(&base_lines, &split_lines(current));

    // If the agent made no real change, keep the user's content.
    if agent_hunks.is_empty() {
        return Resolution::Unchanged;
    }

    // Conflict if any agent hunk's base range overlaps any user hunk's base range.
    for a in &agent_hunks {
        for u in &user_hunks {
            if ranges_overlap(a, u) {
                return Resolution::Stale;
            }
        }
    }

    // Disjoint: apply both hunk sets to the base, ordered by base position.
    let mut all: Vec<&Hunk> = agent_hunks.iter().chain(user_hunks.iter()).collect();
    all.sort_by_key(|h| h.base_start);
    let merged = apply_hunks(&base_lines, &all);
    Resolution::Clean(merged)
}

/// A change region in *base* coordinates: replace `base[start..start+len]` with
/// `replacement` lines.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Hunk {
    base_start: usize,
    base_len: usize,
    replacement: Vec<String>,
}

/// Two hunks conflict if their base ranges overlap. Insertions (zero-length) at the
/// same anchor are treated as a conflict (conservative).
fn ranges_overlap(a: &Hunk, b: &Hunk) -> bool {
    let (a0, a1) = (a.base_start, a.base_start + a.base_len);
    let (b0, b1) = (b.base_start, b.base_start + b.base_len);
    if a.base_len == 0 && b.base_len == 0 {
        return a0 == b0;
    }
    // Half-open overlap; an insertion strictly inside a replacement conflicts.
    a0 < b1 && b0 < a1
}

/// Split into lines preserving the count; a trailing newline yields a final empty
/// element so round-tripping is exact.
fn split_lines(s: &str) -> Vec<String> {
    s.split('\n').map(|l| l.to_string()).collect()
}

fn join_lines(lines: &[String]) -> String {
    lines.join("\n")
}

/// LCS-based line diff: change hunks transforming `base` into `other`.
fn diff_hunks(base: &[String], other: &[String]) -> Vec<Hunk> {
    let n = base.len();
    let m = other.len();
    // LCS length table.
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if base[i] == other[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }

    let mut hunks = Vec::new();
    let mut cur: Option<Hunk> = None;
    let (mut i, mut j) = (0usize, 0usize);

    let flush = |cur: &mut Option<Hunk>, hunks: &mut Vec<Hunk>| {
        if let Some(h) = cur.take() {
            hunks.push(h);
        }
    };

    while i < n && j < m {
        if base[i] == other[j] {
            flush(&mut cur, &mut hunks);
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            // Delete base[i].
            let h = cur.get_or_insert(Hunk {
                base_start: i,
                base_len: 0,
                replacement: Vec::new(),
            });
            h.base_len += 1;
            i += 1;
        } else {
            // Insert other[j].
            let h = cur.get_or_insert(Hunk {
                base_start: i,
                base_len: 0,
                replacement: Vec::new(),
            });
            h.replacement.push(other[j].clone());
            j += 1;
        }
    }
    // Trailing deletions.
    while i < n {
        let h = cur.get_or_insert(Hunk {
            base_start: i,
            base_len: 0,
            replacement: Vec::new(),
        });
        h.base_len += 1;
        i += 1;
    }
    // Trailing insertions.
    while j < m {
        let h = cur.get_or_insert(Hunk {
            base_start: i,
            base_len: 0,
            replacement: Vec::new(),
        });
        h.replacement.push(other[j].clone());
        j += 1;
    }
    flush(&mut cur, &mut hunks);
    hunks
}

/// Apply non-overlapping hunks (sorted by `base_start`) to `base`.
fn apply_hunks(base: &[String], hunks: &[&Hunk]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut pos = 0usize;
    for h in hunks {
        // Copy unchanged lines up to the hunk.
        while pos < h.base_start && pos < base.len() {
            out.push(base[pos].clone());
            pos += 1;
        }
        // Emit the replacement, skip the replaced base lines.
        out.extend(h.replacement.iter().cloned());
        pos += h.base_len;
    }
    while pos < base.len() {
        out.push(base[pos].clone());
        pos += 1;
    }
    join_lines(&out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_when_user_untouched() {
        let base = "a\nb\nc\n";
        let proposed = "a\nB\nc\n";
        assert_eq!(
            three_way_merge(base, proposed, base),
            Resolution::Clean(proposed.into())
        );
    }

    #[test]
    fn unchanged_when_proposed_equals_current() {
        let base = "a\nb\n";
        assert_eq!(
            three_way_merge(base, "a\nb\n", "a\nb\n"),
            Resolution::Unchanged
        );
    }

    #[test]
    fn disjoint_edits_merge_cleanly() {
        // Agent edits line 1; user edits line 3 — non-overlapping.
        let base = "one\ntwo\nthree\nfour\n";
        let agent = "ONE\ntwo\nthree\nfour\n";
        let user = "one\ntwo\nthree\nFOUR\n";
        match three_way_merge(base, agent, user) {
            Resolution::Clean(merged) => {
                assert_eq!(merged, "ONE\ntwo\nthree\nFOUR\n");
            }
            other => panic!("expected clean merge, got {other:?}"),
        }
    }

    #[test]
    fn overlapping_edits_are_stale() {
        // Both change line 2 differently.
        let base = "one\ntwo\nthree\n";
        let agent = "one\nAGENT\nthree\n";
        let user = "one\nUSER\nthree\n";
        assert_eq!(three_way_merge(base, agent, user), Resolution::Stale);
    }

    #[test]
    fn agent_insert_user_edit_disjoint() {
        // Agent inserts after line 1; user edits line 3.
        let base = "a\nb\nc\n";
        let agent = "a\nNEW\nb\nc\n";
        let user = "a\nb\nC\n";
        match three_way_merge(base, agent, user) {
            Resolution::Clean(m) => assert_eq!(m, "a\nNEW\nb\nC\n"),
            other => panic!("expected clean, got {other:?}"),
        }
    }

    #[test]
    fn both_append_at_end_conflicts() {
        // Both append different lines at EOF — same anchor → conservative stale.
        let base = "a\nb\n";
        let agent = "a\nb\nAGENT\n";
        let user = "a\nb\nUSER\n";
        assert_eq!(three_way_merge(base, agent, user), Resolution::Stale);
    }

    #[test]
    fn staging_area_resolve_marks_stale() {
        let mut area = StagingArea::new();
        area.stage("src/main.rs", "one\ntwo\n".into(), "one\nAGENT\n".into());
        // User changed the same line.
        let res = area
            .resolve(Path::new("src/main.rs"), "one\nUSER\n")
            .unwrap();
        assert_eq!(res, Resolution::Stale);
        assert!(area.get(Path::new("src/main.rs")).unwrap().stale);
    }

    #[test]
    fn staging_area_clean_apply() {
        let mut area = StagingArea::new();
        area.stage("f", "a\nb\nc\n".into(), "A\nb\nc\n".into());
        let res = area.resolve(Path::new("f"), "a\nb\nc\n").unwrap();
        assert_eq!(res, Resolution::Clean("A\nb\nc\n".into()));
        assert!(!area.get(Path::new("f")).unwrap().stale);
    }

    #[test]
    fn diff_hunks_roundtrip_property() {
        // Applying base→other hunks to base reproduces other, for several cases.
        let cases = [
            ("a\nb\nc", "a\nx\nc"),
            ("a\nb\nc", "a\nb\nc\nd"),
            ("a\nb\nc\nd", "a\nd"),
            ("", "hello"),
            ("keep\nme", "keep\nme"),
            ("1\n2\n3\n4\n5", "1\n9\n3\n4\n8"),
        ];
        for (base, other) in cases {
            let bl = split_lines(base);
            let hunks = diff_hunks(&bl, &split_lines(other));
            let refs: Vec<&Hunk> = hunks.iter().collect();
            assert_eq!(
                apply_hunks(&bl, &refs),
                other,
                "roundtrip {base:?}->{other:?}"
            );
        }
    }

    #[test]
    fn multiple_disjoint_files_independent() {
        let mut area = StagingArea::new();
        area.stage("a", "x\n".into(), "X\n".into());
        area.stage("b", "y\n".into(), "Y\n".into());
        assert_eq!(
            area.resolve(Path::new("a"), "x\n").unwrap(),
            Resolution::Clean("X\n".into())
        );
        assert_eq!(
            area.resolve(Path::new("b"), "y\n").unwrap(),
            Resolution::Clean("Y\n".into())
        );
    }
}
