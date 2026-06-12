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
    Quit { force: bool },
    WriteQuit,
    Edit(String),
    NextBuffer,
    PrevBuffer,
}

impl ExCommand {
    /// Parse a command-line string (without the leading `:`).
    pub fn parse(input: &str) -> Result<Self, CommandError> {
        let input = input.trim();
        match input {
            "w" => Ok(ExCommand::Write(None)),
            "q" => Ok(ExCommand::Quit { force: false }),
            "q!" => Ok(ExCommand::Quit { force: true }),
            "wq" | "x" => Ok(ExCommand::WriteQuit),
            "bn" => Ok(ExCommand::NextBuffer),
            "bp" => Ok(ExCommand::PrevBuffer),
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
                // Force re-read
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
        assert_eq!(ExCommand::parse("q").unwrap(), ExCommand::Quit { force: false });
        assert_eq!(ExCommand::parse("q!").unwrap(), ExCommand::Quit { force: true });
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
