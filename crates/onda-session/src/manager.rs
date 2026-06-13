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
