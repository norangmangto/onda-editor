//! Command-line completion candidates (onda T18.3).
//!
//! Pure functions that, given the current command-line text, produce a ranked list
//! of completions: command names after a bare `:`, or filesystem paths after a
//! command that takes a file argument (`:e`, `:w`, `:sp`, `:vsp`). The editor owns
//! the cycling/rendering; this module owns the candidate generation so it can be
//! unit-tested without a UI.

use std::path::Path;

use nucleo_matcher::{
    pattern::{Atom, AtomKind, CaseMatching, Normalization},
    Config, Matcher, Utf32String,
};

/// Built-in ex-command names offered for `:`-completion.
pub const COMMAND_NAMES: &[&str] = &[
    "write",
    "quit",
    "wq",
    "wqa",
    "wqall",
    "qa",
    "qall",
    "edit",
    "split",
    "vsplit",
    "noh",
    "set",
    "bnext",
    "bprev",
    "terminal",
    "Format",
    "lnext",
    "lprev",
    "messages",
    "GrammarFetch",
    "ls",
    "buffers",
    "session save",
    "session restore",
    "theme",
    "table",
    "fields",
    "agent",
    "agent-review",
    "agent-export",
];

/// What a `<Tab>` press in the command line should complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Completion {
    /// Complete a command name; `base` is the text to keep before the token.
    Commands {
        base: String,
        token: String,
        candidates: Vec<String>,
    },
    /// Complete a filesystem path; `base` is the text to keep before the token.
    Paths {
        base: String,
        token: String,
        candidates: Vec<String>,
    },
    /// Nothing to complete.
    None,
}

/// Commands whose argument is a file path (so `<Tab>` completes paths).
fn takes_path(cmd: &str) -> bool {
    matches!(
        cmd,
        "e" | "e!" | "edit" | "w" | "write" | "sp" | "split" | "vsp" | "vsplit"
    )
}

/// Analyze `line` (command-line text without the leading `:`) and produce completions.
///
/// `extra_commands` are dynamically-registered command names (e.g. Lua plugins).
pub fn analyze(line: &str, extra_commands: &[String]) -> Completion {
    // Find the first whitespace separating the command from its argument.
    match line.split_once(char::is_whitespace) {
        None => {
            // Still typing the command name → command-name completion.
            let candidates = complete_commands(line, extra_commands);
            if candidates.is_empty() {
                Completion::None
            } else {
                Completion::Commands {
                    base: String::new(),
                    token: line.to_string(),
                    candidates,
                }
            }
        }
        Some((cmd, rest)) => {
            if !takes_path(cmd) {
                return Completion::None;
            }
            // The path token is everything after the last whitespace run.
            let token = rest.trim_start();
            let base_len = line.len() - token.len();
            let base = line[..base_len].to_string();
            let candidates = complete_path(token);
            if candidates.is_empty() {
                Completion::None
            } else {
                Completion::Paths {
                    base,
                    token: token.to_string(),
                    candidates,
                }
            }
        }
    }
}

/// Fuzzy-rank command names (built-in + extra) against `partial`.
pub fn complete_commands(partial: &str, extra: &[String]) -> Vec<String> {
    let all: Vec<String> = COMMAND_NAMES
        .iter()
        .map(|s| s.to_string())
        .chain(extra.iter().cloned())
        .collect();
    if partial.is_empty() {
        return all;
    }
    fuzzy_rank(partial, all)
}

/// List filesystem entries matching `partial` (a possibly-partial path).
///
/// Directories gain a trailing `/`. Returns paths in the same textual form as
/// `partial`'s directory part so they can be substituted directly into the line.
pub fn complete_path(partial: &str) -> Vec<String> {
    // Split into the directory part and the filename prefix.
    let (dir_part, prefix) = match partial.rfind('/') {
        Some(idx) => (&partial[..=idx], &partial[idx + 1..]),
        None => ("", partial),
    };
    let dir_to_read: &Path = if dir_part.is_empty() {
        Path::new(".")
    } else {
        Path::new(dir_part)
    };

    let mut entries: Vec<String> = Vec::new();
    let read = match std::fs::read_dir(dir_to_read) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    for entry in read.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        let display = if is_dir {
            format!("{name}/")
        } else {
            name.clone()
        };
        entries.push(format!("{dir_part}{display}"));
    }

    // Filter / rank by the filename prefix.
    let ranked = if prefix.is_empty() {
        let mut e = entries;
        e.sort();
        e
    } else {
        // Rank by fuzzy score over just the filename portion.
        let scored: Vec<(String, String)> = entries
            .into_iter()
            .map(|full| {
                let fname = full[dir_part.len()..].to_string();
                (full, fname)
            })
            .collect();
        let names: Vec<String> = scored.iter().map(|(_, f)| f.clone()).collect();
        let ranked_names = fuzzy_rank(prefix, names);
        // Map ranked filenames back to full paths, preserving order.
        ranked_names
            .into_iter()
            .filter_map(|rn| {
                scored
                    .iter()
                    .find(|(_, f)| *f == rn)
                    .map(|(full, _)| full.clone())
            })
            .collect()
    };

    ranked
}

/// Generic fuzzy ranker: keep items that match `query`, ordered by descending score.
fn fuzzy_rank(query: &str, items: Vec<String>) -> Vec<String> {
    let mut matcher = Matcher::new(Config::DEFAULT);
    let atom = Atom::new(
        query,
        CaseMatching::Smart,
        Normalization::Smart,
        AtomKind::Fuzzy,
        false,
    );
    let mut scored: Vec<(String, u16)> = items
        .into_iter()
        .filter_map(|item| {
            let haystack = Utf32String::from(item.as_str());
            let score = atom.score(haystack.slice(..), &mut matcher)?;
            Some((item, score))
        })
        .collect();
    scored.sort_by_key(|&(_, s)| std::cmp::Reverse(s));
    scored.into_iter().map(|(i, _)| i).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn command_completion_prefix() {
        let got = complete_commands("se", &[]);
        assert!(got.iter().any(|c| c == "session save"));
        assert!(got.iter().any(|c| c == "set"));
        // Non-matching commands are excluded.
        assert!(!got.iter().any(|c| c == "quit"));
    }

    #[test]
    fn command_completion_includes_extra() {
        let extra = vec!["MyLuaCmd".to_string()];
        let got = complete_commands("My", &extra);
        assert_eq!(got, vec!["MyLuaCmd".to_string()]);
    }

    #[test]
    fn analyze_bare_command() {
        let c = analyze("ag", &[]);
        match c {
            Completion::Commands { candidates, .. } => {
                assert!(candidates.iter().any(|c| c == "agent"));
            }
            other => panic!("expected Commands, got {other:?}"),
        }
    }

    #[test]
    fn analyze_non_path_command_is_none() {
        assert_eq!(analyze("noh extra", &[]), Completion::None);
    }

    #[test]
    fn path_completion_lists_dir() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("README.md"), "x").unwrap();
        let base = dir.path().to_string_lossy().into_owned();

        let partial = format!("{base}/");
        let got = complete_path(&partial);
        assert!(got.iter().any(|p| p.ends_with("src/")));
        assert!(got.iter().any(|p| p.ends_with("README.md")));
    }

    #[test]
    fn path_completion_prefix_filters() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("alpha.txt"), "x").unwrap();
        fs::write(dir.path().join("beta.txt"), "x").unwrap();
        let base = dir.path().to_string_lossy().into_owned();

        let partial = format!("{base}/al");
        let got = complete_path(&partial);
        assert!(got.iter().any(|p| p.ends_with("alpha.txt")));
        assert!(!got.iter().any(|p| p.ends_with("beta.txt")));
    }

    #[test]
    fn path_completion_dir_trailing_slash() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("mydir")).unwrap();
        let base = dir.path().to_string_lossy().into_owned();
        let got = complete_path(&format!("{base}/my"));
        assert!(got.iter().any(|p| p.ends_with("mydir/")));
    }

    #[test]
    fn analyze_edit_path() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("file.rs"), "x").unwrap();
        let base = dir.path().to_string_lossy().into_owned();
        let line = format!("e {base}/fi");
        match analyze(&line, &[]) {
            Completion::Paths {
                base: b,
                candidates,
                ..
            } => {
                assert_eq!(b, "e ");
                assert!(candidates.iter().any(|p| p.ends_with("file.rs")));
            }
            other => panic!("expected Paths, got {other:?}"),
        }
    }
}
