pub mod highlight;
pub mod language;
pub mod textobjects;
pub mod worker;

// highlight re-exports
pub use highlight::{
    parse_highlights, Highlight, HighlightConfig, HighlightError, HighlightEvent, Highlights,
    Scope, Span,
};

// textobject re-exports
pub use textobjects::{text_object, TextObjectKind, TextObjectScope};

// language re-exports
pub use language::{Language, LanguageConfig, LanguageError, LanguageRegistry};

// worker re-exports
pub use worker::{SyntaxWorker, SyntaxWorkerHandle, WorkerCmd, WorkerRequest, WorkerResponse};
