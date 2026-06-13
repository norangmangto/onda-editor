/// Session data model.
///
/// A session captures:
/// - The list of open buffers (file paths + any unsaved content)
/// - The split/window layout tree
/// - Per-window cursor and viewport positions
/// - Current working directory
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("TOML serialize error: {0}")]
    TomlSer(#[from] toml::ser::Error),
    #[error("TOML deserialize error: {0}")]
    TomlDe(#[from] toml::de::Error),
}

// ── Data types ─────────────────────────────────────────────────────────────────

/// Cursor + viewport position for a window.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct CursorPos {
    /// Char offset in the buffer.
    pub char_offset: usize,
    /// Viewport top line offset.
    pub viewport_line: usize,
}

/// A single open buffer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BufferEntry {
    /// Buffer index (used by window entries to reference this buffer).
    pub id: usize,
    /// Filesystem path, if any.
    pub path: Option<PathBuf>,
    /// Name (e.g. `[scratch]` for path-less buffers).
    pub name: String,
    /// Unsaved content (only stored for scratch/unsaved buffers).
    pub unsaved_content: Option<String>,
}

/// A window in the split layout.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowEntry {
    pub id: usize,
    pub buffer_id: usize,
    pub cursor: CursorPos,
}

/// A node in the split layout tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum SplitEntry {
    /// A single window leaf.
    Window { window_id: usize },
    /// A horizontal split (top/bottom children).
    Horizontal { children: Vec<SplitEntry> },
    /// A vertical split (left/right children).
    Vertical { children: Vec<SplitEntry> },
}

// ── Session ────────────────────────────────────────────────────────────────────

/// The complete editor session state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// Format version for forward-compat.
    pub version: u32,
    /// Absolute working directory when the session was saved.
    pub cwd: PathBuf,
    /// Open buffers.
    pub buffers: Vec<BufferEntry>,
    /// Windows (cursor/viewport per window).
    pub windows: Vec<WindowEntry>,
    /// Layout tree.
    pub layout: SplitEntry,
    /// Index of the focused window.
    pub focused_window: usize,
}

impl Session {
    pub const CURRENT_VERSION: u32 = 1;

    /// Serialize to TOML bytes.
    pub fn to_toml(&self) -> Result<String, SessionError> {
        Ok(toml::to_string_pretty(self)?)
    }

    /// Deserialize from TOML bytes.
    pub fn from_toml(s: &str) -> Result<Self, SessionError> {
        Ok(toml::from_str(s)?)
    }

    /// Write to a file.
    pub fn save_to(&self, path: &PathBuf) -> Result<(), SessionError> {
        let toml = self.to_toml()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, toml.as_bytes())?;
        Ok(())
    }

    /// Read from a file.
    pub fn load_from(path: &PathBuf) -> Result<Self, SessionError> {
        let bytes = std::fs::read_to_string(path)?;
        Self::from_toml(&bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_session() -> Session {
        Session {
            version: Session::CURRENT_VERSION,
            cwd: PathBuf::from("/tmp"),
            buffers: vec![BufferEntry {
                id: 0,
                path: Some(PathBuf::from("/tmp/hello.rs")),
                name: "hello.rs".into(),
                unsaved_content: None,
            }],
            windows: vec![WindowEntry {
                id: 0,
                buffer_id: 0,
                cursor: CursorPos {
                    char_offset: 42,
                    viewport_line: 5,
                },
            }],
            layout: SplitEntry::Window { window_id: 0 },
            focused_window: 0,
        }
    }

    #[test]
    fn roundtrip_toml() {
        let s = make_session();
        let toml = s.to_toml().unwrap();
        let s2 = Session::from_toml(&toml).unwrap();
        assert_eq!(s.buffers[0].name, s2.buffers[0].name);
        assert_eq!(
            s.windows[0].cursor.char_offset,
            s2.windows[0].cursor.char_offset
        );
    }

    #[test]
    fn roundtrip_file() {
        // Session save/load to file covered by integration tests.
        let s = make_session();
        let toml = s.to_toml().unwrap();
        let s2 = Session::from_toml(&toml).unwrap();
        assert_eq!(s.focused_window, s2.focused_window);
    }

    #[test]
    fn buffer_entry_with_unsaved_content_roundtrips() {
        let session = Session {
            version: Session::CURRENT_VERSION,
            cwd: PathBuf::from("/tmp"),
            buffers: vec![BufferEntry {
                id: 1,
                path: None,
                name: "[scratch]".into(),
                unsaved_content: Some("fn main() {}\n".into()),
            }],
            windows: vec![WindowEntry {
                id: 0,
                buffer_id: 1,
                cursor: CursorPos::default(),
            }],
            layout: SplitEntry::Window { window_id: 0 },
            focused_window: 0,
        };

        let toml = session.to_toml().expect("to_toml");
        let restored = Session::from_toml(&toml).expect("from_toml");

        assert_eq!(restored.buffers.len(), 1);
        let buf = &restored.buffers[0];
        assert_eq!(buf.id, 1);
        assert_eq!(buf.name, "[scratch]");
        assert!(buf.path.is_none(), "scratch buffer should have no path");
        assert_eq!(
            buf.unsaved_content.as_deref(),
            Some("fn main() {}\n"),
            "unsaved content should survive round-trip"
        );
    }

    #[test]
    fn cursor_pos_serialization() {
        let cursor = CursorPos {
            char_offset: 123,
            viewport_line: 45,
        };

        // PartialEq is derived, so direct equality must hold
        let cloned = cursor.clone();
        assert_eq!(cursor, cloned);

        // Round-trip through a session that embeds the cursor
        let session = Session {
            version: Session::CURRENT_VERSION,
            cwd: PathBuf::from("/tmp"),
            buffers: vec![BufferEntry {
                id: 0,
                path: Some(PathBuf::from("/tmp/a.rs")),
                name: "a.rs".into(),
                unsaved_content: None,
            }],
            windows: vec![WindowEntry {
                id: 0,
                buffer_id: 0,
                cursor: cursor.clone(),
            }],
            layout: SplitEntry::Window { window_id: 0 },
            focused_window: 0,
        };

        let toml = session.to_toml().expect("to_toml");
        let restored = Session::from_toml(&toml).expect("from_toml");
        assert_eq!(restored.windows[0].cursor.char_offset, 123);
        assert_eq!(restored.windows[0].cursor.viewport_line, 45);
    }
}
