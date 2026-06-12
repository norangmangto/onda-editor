/// Plugin loader — scans `~/.config/onda/plugins/*.lua` and
/// `<project>/.onda/plugins/*.lua` at startup.
///
/// Errors in plugins are logged to the message line; they never crash the editor.
use std::path::{Path, PathBuf};

use tracing::{info, warn};

use crate::runtime::LuaRuntime;

pub struct PluginLoader;

impl PluginLoader {
    /// Load all plugins from the standard locations.
    /// Returns a list of (name, error) for any plugins that failed.
    pub fn load_all(runtime: &LuaRuntime, cwd: &Path) -> Vec<(String, String)> {
        let mut errors = Vec::new();

        let search_paths = plugin_dirs(cwd);
        for dir in &search_paths {
            if !dir.exists() {
                continue;
            }
            let entries = match std::fs::read_dir(dir) {
                Ok(e) => e,
                Err(e) => {
                    warn!("Cannot read plugin dir {:?}: {}", dir, e);
                    continue;
                }
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("lua") {
                    continue;
                }
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("unknown")
                    .to_string();
                match std::fs::read_to_string(&path) {
                    Ok(source) => {
                        info!("Loading plugin: {:?}", path);
                        if let Err(e) = runtime.exec(&source, &name) {
                            errors.push((name, e.to_string()));
                        }
                    }
                    Err(e) => {
                        errors.push((name, e.to_string()));
                    }
                }
            }
        }

        errors
    }
}

fn plugin_dirs(cwd: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    // User plugin dir: ~/.config/onda/plugins/
    if let Some(config_dir) = config_dir() {
        dirs.push(config_dir.join("plugins"));
    }

    // Project plugin dir: <cwd>/.onda/plugins/
    dirs.push(cwd.join(".onda").join("plugins"));

    dirs
}

fn config_dir() -> Option<PathBuf> {
    // XDG_CONFIG_HOME or ~/.config
    std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .ok()
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|h| PathBuf::from(h).join(".config"))
        })
        .map(|d| d.join("onda"))
}
