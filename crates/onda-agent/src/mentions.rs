//! Context mentions & assembly (T25.1/T25.2).
//!
//! The input box accepts `@file`, `@buffer`, `@selection`, `@diagnostics`, and
//! `@terminal` mentions. This module parses them from the prompt text and turns
//! editor-supplied raw content into ACP context blocks, applying size guards (a line
//! cap with a visible truncation notice) so a fat `@file` can't silently blow the
//! prompt. The editor owns *fetching* the content; the pure assembly + guards live
//! here so they're unit-testable.

use crate::protocol::{ContentBlock, EmbeddedResource};

/// Default line cap applied to a single attached context resource.
pub const DEFAULT_MAX_LINES: usize = 1000;

/// A mention kind recognized in the input box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MentionKind {
    File,
    Buffer,
    Selection,
    Diagnostics,
    Terminal,
    Unknown,
}

impl MentionKind {
    fn from_word(w: &str) -> Self {
        match w {
            "file" => MentionKind::File,
            "buffer" => MentionKind::Buffer,
            "selection" => MentionKind::Selection,
            "diagnostics" => MentionKind::Diagnostics,
            "terminal" => MentionKind::Terminal,
            _ => MentionKind::Unknown,
        }
    }
}

/// A parsed mention from the prompt text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mention {
    pub kind: MentionKind,
    /// Optional argument after `:` (path for `@file:...`, count for `@terminal:50`, …).
    pub arg: Option<String>,
    /// The exact token as typed (e.g. `@file:src/main.rs`), for completion/replacement.
    pub raw: String,
}

/// Parse all `@mention[:arg]` tokens from `text`, in order of appearance.
pub fn parse_mentions(text: &str) -> Vec<Mention> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'@' {
            i += 1;
            continue;
        }
        // `@` must start a token (beginning of string or preceded by whitespace).
        if i > 0 && !bytes[i - 1].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        i += 1; // past '@'
        let word_start = i;
        while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
            i += 1;
        }
        if i == word_start {
            continue; // bare '@'
        }
        let word = &text[word_start..i];
        let mut arg = None;
        if i < bytes.len() && bytes[i] == b':' {
            i += 1;
            let arg_start = i;
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i > arg_start {
                arg = Some(text[arg_start..i].to_string());
            }
        }
        out.push(Mention {
            kind: MentionKind::from_word(word),
            arg,
            raw: text[start..i].to_string(),
        });
    }
    out
}

/// Severity levels for `@diagnostics` filtering (mirrors LSP).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Error = 0,
    Warning = 1,
    Info = 2,
    Hint = 3,
}

/// A single diagnostic item for assembly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticItem {
    pub line: usize,
    pub col: usize,
    pub severity: Severity,
    pub message: String,
}

/// A resolved context resource ready to attach to a prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedContext {
    pub block: ContentBlock,
    pub truncated: bool,
    pub original_lines: usize,
    pub included_lines: usize,
}

/// Build a context resource from raw `content`, capping at `max_lines` and appending
/// a visible truncation notice when content is dropped.
pub fn build_context(
    uri: impl Into<String>,
    mime_type: Option<&str>,
    content: &str,
    max_lines: usize,
) -> ResolvedContext {
    let lines: Vec<&str> = content.split('\n').collect();
    let original_lines = lines.len();

    let (text, truncated, included_lines) = if original_lines > max_lines {
        let kept = &lines[..max_lines];
        let dropped = original_lines - max_lines;
        let mut t = kept.join("\n");
        t.push_str(&format!(
            "\n… [truncated {dropped} of {original_lines} lines]"
        ));
        (t, true, max_lines)
    } else {
        (content.to_string(), false, original_lines)
    };

    ResolvedContext {
        block: ContentBlock::Resource {
            resource: EmbeddedResource {
                uri: uri.into(),
                mime_type: mime_type.map(|s| s.to_string()),
                text,
            },
        },
        truncated,
        original_lines,
        included_lines,
    }
}

/// Format diagnostics at or above `min_severity` into a compact text block.
pub fn format_diagnostics(items: &[DiagnosticItem], min_severity: Severity) -> String {
    let mut out = String::new();
    for d in items.iter().filter(|d| d.severity <= min_severity) {
        let sev = match d.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "info",
            Severity::Hint => "hint",
        };
        out.push_str(&format!(
            "{}:{}: {}: {}\n",
            d.line + 1,
            d.col + 1,
            sev,
            d.message
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_each_mention_kind() {
        let m = parse_mentions("fix @file:src/main.rs and @selection then @diagnostics");
        assert_eq!(m.len(), 3);
        assert_eq!(m[0].kind, MentionKind::File);
        assert_eq!(m[0].arg.as_deref(), Some("src/main.rs"));
        assert_eq!(m[1].kind, MentionKind::Selection);
        assert_eq!(m[1].arg, None);
        assert_eq!(m[2].kind, MentionKind::Diagnostics);
    }

    #[test]
    fn parse_buffer_and_terminal_args() {
        let m = parse_mentions("@buffer:lib.rs @terminal:50");
        assert_eq!(m[0].kind, MentionKind::Buffer);
        assert_eq!(m[0].arg.as_deref(), Some("lib.rs"));
        assert_eq!(m[1].kind, MentionKind::Terminal);
        assert_eq!(m[1].arg.as_deref(), Some("50"));
    }

    #[test]
    fn email_like_at_is_not_a_mention() {
        // `@` mid-word (e.g. an email) is ignored.
        let m = parse_mentions("mail me at foo@bar.com");
        assert!(m.is_empty());
    }

    #[test]
    fn unknown_kind_marked_unknown() {
        let m = parse_mentions("@wat:thing");
        assert_eq!(m[0].kind, MentionKind::Unknown);
    }

    #[test]
    fn raw_token_preserved_for_completion() {
        let m = parse_mentions("see @file:a/b.rs done");
        assert_eq!(m[0].raw, "@file:a/b.rs");
    }

    #[test]
    fn small_content_not_truncated() {
        let ctx = build_context("file://a.rs", Some("text/rust"), "one\ntwo\n", 1000);
        assert!(!ctx.truncated);
        assert_eq!(ctx.included_lines, ctx.original_lines);
        if let ContentBlock::Resource { resource } = &ctx.block {
            assert_eq!(resource.uri, "file://a.rs");
            assert!(resource.text.contains("one"));
        } else {
            panic!("expected resource block");
        }
    }

    #[test]
    fn oversize_content_truncated_with_notice() {
        let content: String = (0..50).map(|i| format!("line{i}\n")).collect();
        let ctx = build_context("file://big.rs", None, &content, 10);
        assert!(ctx.truncated);
        assert_eq!(ctx.included_lines, 10);
        if let ContentBlock::Resource { resource } = &ctx.block {
            assert!(resource.text.contains("truncated"));
            assert!(resource.text.contains("line0"));
            assert!(!resource.text.contains("line40"));
        } else {
            panic!("expected resource block");
        }
    }

    #[test]
    fn diagnostics_severity_filter() {
        let items = vec![
            DiagnosticItem {
                line: 0,
                col: 0,
                severity: Severity::Error,
                message: "boom".into(),
            },
            DiagnosticItem {
                line: 4,
                col: 2,
                severity: Severity::Hint,
                message: "tidy".into(),
            },
        ];
        let only_errors = format_diagnostics(&items, Severity::Error);
        assert!(only_errors.contains("1:1: error: boom"));
        assert!(!only_errors.contains("tidy"));

        let all = format_diagnostics(&items, Severity::Hint);
        assert!(all.contains("boom") && all.contains("tidy"));
    }

    #[test]
    fn selection_context_uses_custom_uri() {
        let ctx = build_context("onda-selection://main.rs#L1-L3", None, "sel", 1000);
        if let ContentBlock::Resource { resource } = &ctx.block {
            assert_eq!(resource.uri, "onda-selection://main.rs#L1-L3");
        } else {
            panic!("expected resource block");
        }
    }
}
