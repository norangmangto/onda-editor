pub mod document;
pub mod selection;
pub mod transaction;
pub mod undo;

pub use document::{Document, DocumentError, DocumentId, LineEnding};
pub use selection::{Assoc, Range, Selection};
pub use transaction::{ChangeSet, ChangeSetError, Transaction};
pub use undo::{UndoHistory, UndoHistoryError, UndoTree};
