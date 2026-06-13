use thiserror::Error;

#[derive(Debug, Error)]
pub enum CommandError {
    #[error("unknown command: {0}")]
    Unknown(String),
    #[error("no file name")]
    NoFileName,
    #[error("unsaved changes (use :q! to force)")]
    UnsavedChanges,
}

/// A parsed Ex command.
#[derive(Debug, Clone, PartialEq)]
pub enum ExCommand {
    Write(Option<String>),
    Quit {
        force: bool,
    },
    /// `:wqa` / `:wqall` — write all and quit (triggers session auto-save).
    WriteQuitAll,
    WriteQuit,
    Edit(String),
    NextBuffer,
    PrevBuffer,
    /// `:sp [file]` — horizontal split.
    Split(Option<String>),
    /// `:vsp [file]` — vertical split.
    VSplit(Option<String>),
    /// `:noh` — clear search highlight.
    NoHighlight,
    /// `:set option [value]`.
    Set(String, Option<String>),
    /// `:[%]s/pattern/replacement/[flags]`
    Substitute {
        range_all: bool,
        pattern: String,
        replacement: String,
        flags_global: bool,
        flags_case_insensitive: bool,
    },
    // ── Phase 2 commands ──────────────────────────────────────────────────────
    /// `:terminal` — open a terminal pane.
    Terminal,
    /// `:session save [name]` — save a session.
    SessionSave(Option<String>),
    /// `:session restore [name]` — restore a session.
    SessionRestore(Option<String>),
    /// `:Format` — format the current buffer via LSP.
    Format,
    /// `:lnext` — go to next diagnostic.
    LspNext,
    /// `:lprev` — go to previous diagnostic.
    LspPrev,
    /// `:messages` — show message history.
    Messages,
    /// `:GrammarFetch` — fetch tree-sitter grammars.
    GrammarFetch,
    /// `:ls` — list buffers.
    ListBuffers,
    /// `:GitStatus` — open a picker of modified/untracked/staged files.
    GitStatus,
    /// `:GitStage` — stage the current file.
    GitStage,
    /// `:GitUnstage` — unstage the current file.
    GitUnstage,
    /// `:GitDiscard` — discard worktree changes to the current file.
    GitDiscard,
    /// Custom command registered by a Lua plugin.
    LuaCommand(String, Vec<String>),
}

impl ExCommand {
    /// Parse a command-line string (without the leading `:`).
    pub fn parse(input: &str) -> Result<Self, CommandError> {
        let input = input.trim();

        // Check session commands before substitute (both start with 's')
        if input.starts_with("session") {
            if let Some(rest) = input.strip_prefix("session save") {
                let name = rest.trim();
                return Ok(ExCommand::SessionSave(if name.is_empty() {
                    None
                } else {
                    Some(name.to_string())
                }));
            }
            if let Some(rest) = input.strip_prefix("session restore") {
                let name = rest.trim();
                return Ok(ExCommand::SessionRestore(if name.is_empty() {
                    None
                } else {
                    Some(name.to_string())
                }));
            }
            return Ok(ExCommand::SessionSave(None));
        }

        // Check for substitute: [%]s/pat/rep/[flags]
        {
            let (range_all, rest) = if let Some(without_pct) = input.strip_prefix('%') {
                (true, without_pct)
            } else {
                (false, input)
            };
            if let Some(after_s) = rest.strip_prefix('s') {
                if let Some(delim) = after_s.chars().next() {
                    // Only treat as substitute if delimiter is a punctuation char, not a letter
                    // (to avoid matching 'sp', 'split', 'session', etc.)
                    if !delim.is_alphabetic() && !delim.is_whitespace() {
                        let parts: Vec<&str> =
                            after_s[delim.len_utf8()..].splitn(3, delim).collect();
                        if parts.len() >= 2 {
                            let pattern = parts[0].to_string();
                            let replacement = parts[1].to_string();
                            let flags = parts.get(2).copied().unwrap_or("");
                            let flags_global = flags.contains('g');
                            let flags_case_insensitive = flags.contains('i');
                            return Ok(ExCommand::Substitute {
                                range_all,
                                pattern,
                                replacement,
                                flags_global,
                                flags_case_insensitive,
                            });
                        }
                    }
                }
            }
        }

        match input {
            "w" => Ok(ExCommand::Write(None)),
            "q" => Ok(ExCommand::Quit { force: false }),
            "q!" => Ok(ExCommand::Quit { force: true }),
            "wq" | "x" => Ok(ExCommand::WriteQuit),
            "wqa" | "wqall" => Ok(ExCommand::WriteQuitAll),
            "bn" => Ok(ExCommand::NextBuffer),
            "bp" => Ok(ExCommand::PrevBuffer),
            "sp" | "split" => Ok(ExCommand::Split(None)),
            "vsp" | "vsplit" => Ok(ExCommand::VSplit(None)),
            "noh" | "nohlsearch" => Ok(ExCommand::NoHighlight),
            "terminal" | "term" => Ok(ExCommand::Terminal),
            "Format" | "format" => Ok(ExCommand::Format),
            "lnext" => Ok(ExCommand::LspNext),
            "lprev" => Ok(ExCommand::LspPrev),
            "messages" | "mes" => Ok(ExCommand::Messages),
            "GrammarFetch" | "grammars" => Ok(ExCommand::GrammarFetch),
            "ls" | "buffers" => Ok(ExCommand::ListBuffers),
            "GitStatus" | "gitstatus" | "Gs" => Ok(ExCommand::GitStatus),
            "GitStage" | "gitstage" => Ok(ExCommand::GitStage),
            "GitUnstage" | "gitunstage" => Ok(ExCommand::GitUnstage),
            "GitDiscard" | "gitdiscard" => Ok(ExCommand::GitDiscard),
            s if s.starts_with("sp ") || s.starts_with("split ") => {
                let path = s
                    .split_once(' ')
                    .map(|(_, r)| r)
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if path.is_empty() {
                    Ok(ExCommand::Split(None))
                } else {
                    Ok(ExCommand::Split(Some(path)))
                }
            }
            s if s.starts_with("vsp ") || s.starts_with("vsplit ") => {
                let path = s
                    .split_once(' ')
                    .map(|(_, r)| r)
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if path.is_empty() {
                    Ok(ExCommand::VSplit(None))
                } else {
                    Ok(ExCommand::VSplit(Some(path)))
                }
            }
            s if s.starts_with("set ") => {
                let rest = s[4..].trim();
                if let Some((key, val)) = rest.split_once('=') {
                    Ok(ExCommand::Set(
                        key.trim().to_string(),
                        Some(val.trim().to_string()),
                    ))
                } else if let Some((key, val)) = rest.split_once(' ') {
                    Ok(ExCommand::Set(
                        key.trim().to_string(),
                        Some(val.trim().to_string()),
                    ))
                } else {
                    Ok(ExCommand::Set(rest.to_string(), None))
                }
            }
            s if s.starts_with("w ") => {
                let path = s[2..].trim().to_string();
                if path.is_empty() {
                    Err(CommandError::NoFileName)
                } else {
                    Ok(ExCommand::Write(Some(path)))
                }
            }
            s if s.starts_with("e ") => {
                let path = s[2..].trim().to_string();
                if path.is_empty() {
                    Err(CommandError::NoFileName)
                } else {
                    Ok(ExCommand::Edit(path))
                }
            }
            s if s.starts_with("e!") => {
                let path = s[2..].trim().to_string();
                if path.is_empty() {
                    Err(CommandError::NoFileName)
                } else {
                    Ok(ExCommand::Edit(path))
                }
            }
            other => Err(CommandError::Unknown(other.to_string())),
        }
    }
}

/// In-session command-line editor.
#[derive(Debug, Default, Clone)]
pub struct CommandLine {
    /// Current input buffer (without leading `:`).
    pub buffer: String,
    /// Simple session history (most recent last).
    history: Vec<String>,
    history_pos: Option<usize>,
}

impl CommandLine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_char(&mut self, c: char) {
        self.buffer.push(c);
        self.history_pos = None;
    }

    pub fn backspace(&mut self) {
        self.buffer.pop();
        self.history_pos = None;
    }

    pub fn clear(&mut self) {
        self.buffer.clear();
        self.history_pos = None;
    }

    /// Accept the current input, pushing it onto history and returning the parsed command.
    pub fn submit(&mut self) -> Result<ExCommand, CommandError> {
        let input = self.buffer.trim().to_string();
        if !input.is_empty() {
            self.history.push(input.clone());
        }
        self.buffer.clear();
        self.history_pos = None;
        ExCommand::parse(&input)
    }

    pub fn as_str(&self) -> &str {
        &self.buffer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_write() {
        assert_eq!(ExCommand::parse("w").unwrap(), ExCommand::Write(None));
        assert_eq!(
            ExCommand::parse("w /tmp/foo.txt").unwrap(),
            ExCommand::Write(Some("/tmp/foo.txt".to_string()))
        );
    }

    #[test]
    fn parse_quit() {
        assert_eq!(
            ExCommand::parse("q").unwrap(),
            ExCommand::Quit { force: false }
        );
        assert_eq!(
            ExCommand::parse("q!").unwrap(),
            ExCommand::Quit { force: true }
        );
        assert_eq!(ExCommand::parse("wqa").unwrap(), ExCommand::WriteQuitAll);
    }

    #[test]
    fn parse_phase2_commands() {
        assert_eq!(ExCommand::parse("terminal").unwrap(), ExCommand::Terminal);
        assert_eq!(ExCommand::parse("Format").unwrap(), ExCommand::Format);
        assert_eq!(ExCommand::parse("lnext").unwrap(), ExCommand::LspNext);
        assert_eq!(
            ExCommand::parse("session save myproject").unwrap(),
            ExCommand::SessionSave(Some("myproject".to_string()))
        );
    }

    #[test]
    fn parse_edit() {
        assert_eq!(
            ExCommand::parse("e src/main.rs").unwrap(),
            ExCommand::Edit("src/main.rs".to_string())
        );
    }

    #[test]
    fn parse_unknown() {
        assert!(ExCommand::parse("zz").is_err());
    }

    #[test]
    fn command_line_edit() {
        let mut cl = CommandLine::new();
        cl.push_char('w');
        cl.push_char('q');
        assert_eq!(cl.as_str(), "wq");
        cl.backspace();
        assert_eq!(cl.as_str(), "w");
        let cmd = cl.submit().unwrap();
        assert_eq!(cmd, ExCommand::Write(None));
        assert_eq!(cl.as_str(), "");
    }
}
