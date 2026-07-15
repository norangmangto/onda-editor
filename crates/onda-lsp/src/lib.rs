pub mod client;
pub mod manager;
pub mod transport;
pub mod types;

pub use client::{LspClient, LspError, RequestId};
pub use manager::{LspManager, LspServerConfig};
pub use types::{DiagnosticSeverity, LspCodeAction, LspDiagnostic, LspEvent, LspSymbol};

/// Re-export key lsp-types for consumers.
pub use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionList, Diagnostic, GotoDefinitionResponse, Hover,
    HoverContents, Location, MarkedString, MarkupContent, MarkupKind, Position, Range, SymbolKind,
    TextEdit, WorkspaceEdit,
};
