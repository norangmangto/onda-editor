/// Session persistence manager.
///
/// Knows where to save/load sessions:
/// - Named sessions: `~/.local/share/onda/sessions/<name>.toml`
/// - Project-local: `.onda/session.toml` in the cwd (if opt-in)
use std::path::{Path, PathBuf};

use tracing::{info, warn};

use crate::session::{Session, SessionError};

pub struct SessionManager {
    /// Base directory for named sessions.
    sessions_dir: PathBuf,
}

impl SessionManager {
    /// Create with the default sessions directory.
    pub fn new() -> Self {
        let dir = dirs_path().join("sessions");
        Self { sessions_dir: dir }
    }

    /// Save a named session.
    pub fn save(&self, name: &str, session: &Session) -> Result<(), SessionError> {
        let path = self.sessions_dir.join(format!("{name}.toml"));
        session.save_to(&path)?;
        info!("Session saved to {:?}", path);
        Ok(())
    }

    /// Load a named session.
    pub fn load(&self, name: &str) -> Result<Session, SessionError> {
        let path = self.sessions_dir.join(format!("{name}.toml"));
        let session = Session::load_from(&path)?;
        info!("Session loaded from {:?}", path);
        Ok(session)
    }

    /// Save the default session (used for auto-save on quit).
    pub fn auto_save(&self, session: &Session) -> Result<(), SessionError> {
        self.save("default", session)
    }

    /// Load the default session (if it exists).
    pub fn auto_load(&self) -> Option<Session> {
        match self.load("default") {
            Ok(s) => Some(s),
            Err(SessionError::Io(_)) => None,
            Err(e) => {
                warn!("Session auto-load error: {e}");
                None
            }
        }
    }

    /// Load a project-local session from `.onda/session.toml` in `cwd`.
    pub fn load_project_local(cwd: &Path) -> Option<Session> {
        let path = cwd.join(".onda").join("session.toml");
        if !path.exists() {
            return None;
        }
        match Session::load_from(&path.to_path_buf()) {
            Ok(s) => {
                info!("Project session loaded from {:?}", path);
                Some(s)
            }
            Err(e) => {
                warn!("Project session load error: {e}");
                None
            }
        }
    }

    /// Save a project-local session to `.onda/session.toml` in `cwd`.
    pub fn save_project_local(cwd: &Path, session: &Session) -> Result<(), SessionError> {
        let path = cwd.join(".onda").join("session.toml");
        session.save_to(&path.to_path_buf())?;
        info!("Project session saved to {:?}", path);
        Ok(())
    }

    /// List available named sessions.
    pub fn list_sessions(&self) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(&self.sessions_dir) else {
            return vec![];
        };
        entries
            .filter_map(|e| {
                let e = e.ok()?;
                let name = e.file_name().into_string().ok()?;
                name.strip_suffix(".toml").map(|n| n.to_string())
            })
            .collect()
    }
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

fn dirs_path() -> PathBuf {
    // XDG_DATA_HOME or ~/.local/share/onda
    std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs_home()
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join(".local")
                .join("share")
        })
        .join("onda")
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var("HOME").ok().map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{BufferEntry, CursorPos, SplitEntry, WindowEntry};

    fn mgr_in(dir: &Path) -> SessionManager {
        SessionManager {
            sessions_dir: dir.join("sessions"),
        }
    }

    fn sample(cwd: &str) -> Session {
        Session {
            version: Session::CURRENT_VERSION,
            cwd: PathBuf::from(cwd),
            buffers: vec![BufferEntry {
                id: 0,
                path: Some(PathBuf::from(format!("{cwd}/a.rs"))),
                name: "a.rs".into(),
                unsaved_content: None,
            }],
            windows: vec![WindowEntry {
                id: 0,
                buffer_id: 0,
                cursor: CursorPos {
                    char_offset: 7,
                    viewport_line: 0,
                },
            }],
            layout: SplitEntry::Window { window_id: 0 },
            focused_window: 0,
        }
    }

    #[test]
    fn named_save_load_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let m = mgr_in(tmp.path());
        m.save("work", &sample("/p")).unwrap();
        let loaded = m.load("work").unwrap();
        assert_eq!(loaded.cwd, PathBuf::from("/p"));
        assert_eq!(loaded.buffers[0].name, "a.rs");
    }

    #[test]
    fn load_missing_session_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let m = mgr_in(tmp.path());
        assert!(m.load("nope").is_err());
    }

    #[test]
    fn auto_load_returns_none_without_default() {
        let tmp = tempfile::tempdir().unwrap();
        let m = mgr_in(tmp.path());
        assert!(m.auto_load().is_none());
        m.auto_save(&sample("/x")).unwrap();
        assert!(m.auto_load().is_some());
    }

    #[test]
    fn list_sessions_returns_saved_names() {
        let tmp = tempfile::tempdir().unwrap();
        let m = mgr_in(tmp.path());
        m.save("alpha", &sample("/a")).unwrap();
        m.save("beta", &sample("/b")).unwrap();
        let mut names = m.list_sessions();
        names.sort();
        assert_eq!(names, vec!["alpha".to_string(), "beta".to_string()]);
    }

    #[test]
    fn project_local_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        SessionManager::save_project_local(tmp.path(), &sample("/proj")).unwrap();
        let loaded = SessionManager::load_project_local(tmp.path()).expect("loads");
        assert_eq!(loaded.cwd, PathBuf::from("/proj"));
    }

    #[test]
    fn project_local_absent_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(SessionManager::load_project_local(tmp.path()).is_none());
    }
}
