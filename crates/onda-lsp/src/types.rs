use lsp_types::{Diagnostic, Location, Range};
use std::path::PathBuf;

/// Severity mirroring lsp_types::DiagnosticSeverity but without the Option wrapper.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Information,
    Hint,
}

impl From<lsp_types::DiagnosticSeverity> for DiagnosticSeverity {
    fn from(s: lsp_types::DiagnosticSeverity) -> Self {
        match s {
            lsp_types::DiagnosticSeverity::ERROR => Self::Error,
            lsp_types::DiagnosticSeverity::WARNING => Self::Warning,
            lsp_types::DiagnosticSeverity::INFORMATION => Self::Information,
            _ => Self::Hint,
        }
    }
}

/// A diagnostic message delivered to the main loop.
#[derive(Debug, Clone)]
pub struct LspDiagnostic {
    pub path: PathBuf,
    pub range: Range,
    pub severity: DiagnosticSeverity,
    pub message: String,
    pub source: Option<String>,
}

impl LspDiagnostic {
    pub fn from_lsp(path: PathBuf, d: &Diagnostic) -> Self {
        Self {
            path,
            range: d.range,
            severity: d
                .severity
                .map(DiagnosticSeverity::from)
                .unwrap_or(DiagnosticSeverity::Error),
            message: d.message.clone(),
            source: d.source.clone(),
        }
    }
}

/// A flattened document symbol. `DocumentSymbolResponse::Nested` (hierarchical
/// `DocumentSymbol` with `children`) is flattened into a single list — good
/// enough for a jump-to-symbol picker, which doesn't need the tree structure.
#[derive(Debug, Clone)]
pub struct LspSymbol {
    pub name: String,
    pub kind: lsp_types::SymbolKind,
    /// The range to jump to (`DocumentSymbol::selection_range` /
    /// `SymbolInformation::location.range`).
    pub range: Range,
}

/// A code action with a directly-applicable edit. `CodeActionOrCommand::Command`
/// (a server-side command with no `edit`) is filtered out before this type is
/// constructed — executing arbitrary server commands is a BACKLOG follow-up.
#[derive(Debug, Clone)]
pub struct LspCodeAction {
    pub title: String,
    pub edit: Option<lsp_types::WorkspaceEdit>,
}

/// Events emitted from the LSP background worker to the main loop.
#[derive(Debug, Clone)]
pub enum LspEvent {
    /// Server process initialized and ready.
    Ready { root: PathBuf },
    /// Diagnostics published by the server.
    Diagnostics {
        path: PathBuf,
        diagnostics: Vec<LspDiagnostic>,
    },
    /// Hover response arrived.
    HoverResult {
        request_id: u64,
        content: Option<String>,
    },
    /// Definition response arrived.
    DefinitionResult {
        request_id: u64,
        locations: Vec<Location>,
    },
    /// References response arrived.
    ReferencesResult {
        request_id: u64,
        locations: Vec<Location>,
    },
    /// Completion response arrived.
    CompletionResult {
        request_id: u64,
        items: Vec<lsp_types::CompletionItem>,
    },
    /// Rename workspace edit arrived.
    RenameResult {
        request_id: u64,
        edit: Option<lsp_types::WorkspaceEdit>,
    },
    /// Formatting edits arrived.
    FormattingResult {
        request_id: u64,
        edits: Vec<lsp_types::TextEdit>,
    },
    /// Document symbol response arrived.
    SymbolResult {
        request_id: u64,
        symbols: Vec<LspSymbol>,
    },
    /// Code action response arrived.
    CodeActionResult {
        request_id: u64,
        actions: Vec<LspCodeAction>,
    },
    /// Server died or crashed.
    ServerError { root: PathBuf, message: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::{Diagnostic, Position, Range as LspRange};

    fn make_range(sl: u32, sc: u32, el: u32, ec: u32) -> LspRange {
        LspRange {
            start: Position {
                line: sl,
                character: sc,
            },
            end: Position {
                line: el,
                character: ec,
            },
        }
    }

    // ── DiagnosticSeverity ────────────────────────────────────────────────────

    #[test]
    fn severity_from_lsp_error() {
        let s = DiagnosticSeverity::from(lsp_types::DiagnosticSeverity::ERROR);
        assert_eq!(s, DiagnosticSeverity::Error);
    }

    #[test]
    fn severity_from_lsp_warning() {
        let s = DiagnosticSeverity::from(lsp_types::DiagnosticSeverity::WARNING);
        assert_eq!(s, DiagnosticSeverity::Warning);
    }

    #[test]
    fn severity_from_lsp_information() {
        let s = DiagnosticSeverity::from(lsp_types::DiagnosticSeverity::INFORMATION);
        assert_eq!(s, DiagnosticSeverity::Information);
    }

    #[test]
    fn severity_from_lsp_hint() {
        let s = DiagnosticSeverity::from(lsp_types::DiagnosticSeverity::HINT);
        assert_eq!(s, DiagnosticSeverity::Hint);
    }

    #[test]
    fn severity_ordering_error_is_highest() {
        // Error < Warning < Information < Hint in terms of discriminant order
        // as declared in the enum — verify they are distinct values
        let severities = [
            DiagnosticSeverity::Error,
            DiagnosticSeverity::Warning,
            DiagnosticSeverity::Information,
            DiagnosticSeverity::Hint,
        ];
        // All four variants must be pairwise distinct
        for i in 0..severities.len() {
            for j in (i + 1)..severities.len() {
                assert_ne!(
                    severities[i], severities[j],
                    "severity variants should be distinct"
                );
            }
        }
    }

    // ── LspDiagnostic ─────────────────────────────────────────────────────────

    #[test]
    fn lsp_diagnostic_from_lsp_error_severity() {
        let range = make_range(0, 0, 0, 5);
        let d = Diagnostic {
            range,
            severity: Some(lsp_types::DiagnosticSeverity::ERROR),
            message: "use of undeclared variable".into(),
            source: Some("rustc".into()),
            ..Default::default()
        };
        let ld = LspDiagnostic::from_lsp(PathBuf::from("/src/main.rs"), &d);
        assert_eq!(ld.severity, DiagnosticSeverity::Error);
        assert_eq!(ld.message, "use of undeclared variable");
        assert_eq!(ld.source.as_deref(), Some("rustc"));
        assert_eq!(ld.path, PathBuf::from("/src/main.rs"));
    }

    #[test]
    fn lsp_diagnostic_none_severity_defaults_to_error() {
        let range = make_range(1, 2, 1, 8);
        let d = Diagnostic {
            range,
            severity: None,
            message: "unknown issue".into(),
            ..Default::default()
        };
        let ld = LspDiagnostic::from_lsp(PathBuf::from("/a.rs"), &d);
        assert_eq!(
            ld.severity,
            DiagnosticSeverity::Error,
            "missing severity should default to Error"
        );
    }

    #[test]
    fn lsp_diagnostic_range_preserved() {
        let range = make_range(10, 3, 10, 15);
        let d = Diagnostic {
            range,
            severity: Some(lsp_types::DiagnosticSeverity::WARNING),
            message: "unused import".into(),
            ..Default::default()
        };
        let ld = LspDiagnostic::from_lsp(PathBuf::from("/lib.rs"), &d);
        assert_eq!(ld.range.start.line, 10);
        assert_eq!(ld.range.start.character, 3);
        assert_eq!(ld.range.end.character, 15);
    }

    #[test]
    fn lsp_diagnostic_no_source_is_none() {
        let d = Diagnostic {
            range: make_range(0, 0, 0, 1),
            severity: Some(lsp_types::DiagnosticSeverity::HINT),
            message: "consider using x".into(),
            source: None,
            ..Default::default()
        };
        let ld = LspDiagnostic::from_lsp(PathBuf::from("/x.rs"), &d);
        assert!(ld.source.is_none());
        assert_eq!(ld.severity, DiagnosticSeverity::Hint);
    }
}
