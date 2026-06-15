//! Plugin manager (W19): install / list / remove + lockfile.
//!
//! A plugin's package is a git repo (or a local directory) containing an
//! `onda-plugin.toml` manifest and the compiled `entry` component. The manager
//! materializes packages under a store dir and records resolved sources in a
//! lockfile (`plugins.lock`) so installs are reproducible.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::manifest::{Manifest, ManifestError};

const MANIFEST_FILE: &str = "onda-plugin.toml";
const LOCKFILE: &str = "plugins.lock";

#[derive(Debug, Error)]
pub enum ManagerError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("manifest: {0}")]
    Manifest(#[from] ManifestError),
    #[error("git: {0}")]
    Git(#[from] git2::Error),
    #[error("lockfile: {0}")]
    Lock(String),
    #[error("invalid source: {0}")]
    Source(String),
    #[error("plugin not found: {0}")]
    NotFound(String),
    #[error("entry component `{0}` missing from package")]
    MissingEntry(String),
}

/// Where a plugin came from. Recorded in the lockfile for reproducible installs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A git repository at `url`, optionally pinned to `rev` (branch/tag/sha).
    Git { url: String, rev: Option<String> },
    /// A local directory (copied into the store).
    Local(PathBuf),
}

impl Source {
    /// Parse `github:user/repo[@rev]`, a `https://…`/`git@…` URL, or a path.
    pub fn parse(spec: &str) -> Result<Source, ManagerError> {
        if let Some(rest) = spec.strip_prefix("github:") {
            let (path, rev) = match rest.split_once('@') {
                Some((p, r)) => (p, Some(r.to_string())),
                None => (rest, None),
            };
            if path.split('/').count() != 2 {
                return Err(ManagerError::Source(format!(
                    "expected github:user/repo, got `{spec}`"
                )));
            }
            return Ok(Source::Git {
                url: format!("https://github.com/{path}.git"),
                rev,
            });
        }
        if spec.starts_with("https://")
            || spec.starts_with("git@")
            || spec.starts_with("file://")
            || spec.ends_with(".git")
        {
            let (url, rev) = match spec.split_once('@').filter(|_| !spec.starts_with("git@")) {
                Some((u, r)) => (u.to_string(), Some(r.to_string())),
                None => (spec.to_string(), None),
            };
            return Ok(Source::Git { url, rev });
        }
        let p = PathBuf::from(spec);
        if p.exists() {
            return Ok(Source::Local(p));
        }
        Err(ManagerError::Source(format!(
            "unrecognized source `{spec}`"
        )))
    }
}

/// One installed plugin's lockfile entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockEntry {
    pub name: String,
    pub version: String,
    /// `git:<url>` / `local:<path>`.
    pub source: String,
    /// Resolved commit sha for git sources (empty for local).
    #[serde(default)]
    pub revision: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Lockfile {
    #[serde(default, rename = "plugin")]
    plugins: Vec<LockEntry>,
}

/// Manages the on-disk plugin store + lockfile.
pub struct PluginManager {
    root: PathBuf,
}

impl PluginManager {
    /// Create/open a store rooted at `root` (e.g. `~/.config/onda/plugins`).
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, ManagerError> {
        let root = root.into();
        std::fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    fn lock_path(&self) -> PathBuf {
        self.root.join(LOCKFILE)
    }

    fn read_lock(&self) -> Result<Lockfile, ManagerError> {
        match std::fs::read_to_string(self.lock_path()) {
            Ok(s) => toml::from_str(&s).map_err(|e| ManagerError::Lock(e.to_string())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Lockfile::default()),
            Err(e) => Err(e.into()),
        }
    }

    fn write_lock(&self, lock: &Lockfile) -> Result<(), ManagerError> {
        let s = toml::to_string_pretty(lock).map_err(|e| ManagerError::Lock(e.to_string()))?;
        std::fs::write(self.lock_path(), s)?;
        Ok(())
    }

    /// Directory where plugin `name` is materialized.
    pub fn plugin_dir(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    /// Install a plugin from `spec`. Returns its lockfile entry.
    pub fn install(&self, spec: &str) -> Result<LockEntry, ManagerError> {
        let source = Source::parse(spec)?;
        // Materialize into a temp area first so a bad manifest can't leave a
        // half-installed plugin in the store.
        let staging = self.root.join(format!(".staging-{}", sanitize(spec)));
        let _ = std::fs::remove_dir_all(&staging);

        let (source_str, revision) = match &source {
            Source::Local(path) => {
                copy_dir(path, &staging)?;
                (format!("local:{}", path.display()), String::new())
            }
            Source::Git { url, rev } => {
                let repo = git2::Repository::clone(url, &staging)?;
                if let Some(rev) = rev {
                    let obj = repo.revparse_single(rev)?;
                    repo.checkout_tree(&obj, None)?;
                    repo.set_head_detached(obj.id())?;
                }
                let head = repo.head()?.peel_to_commit()?.id().to_string();
                (format!("git:{url}"), head)
            }
        };

        let manifest = self.read_manifest(&staging)?;
        // Verify the entry component is present.
        if !staging.join(&manifest.plugin.entry).exists() {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(ManagerError::MissingEntry(manifest.plugin.entry.clone()));
        }

        // Promote staging → final dir (atomic-ish: remove old, rename).
        let dest = self.plugin_dir(&manifest.plugin.name);
        let _ = std::fs::remove_dir_all(&dest);
        std::fs::rename(&staging, &dest)?;

        let entry = LockEntry {
            name: manifest.plugin.name.clone(),
            version: manifest.plugin.version.clone(),
            source: source_str,
            revision,
        };

        let mut lock = self.read_lock()?;
        lock.plugins.retain(|p| p.name != entry.name);
        lock.plugins.push(entry.clone());
        lock.plugins.sort_by(|a, b| a.name.cmp(&b.name));
        self.write_lock(&lock)?;

        Ok(entry)
    }

    fn read_manifest(&self, dir: &Path) -> Result<Manifest, ManagerError> {
        let src = std::fs::read_to_string(dir.join(MANIFEST_FILE))?;
        Ok(Manifest::parse(&src)?)
    }

    /// All installed plugins, from the lockfile.
    pub fn list(&self) -> Result<Vec<LockEntry>, ManagerError> {
        Ok(self.read_lock()?.plugins)
    }

    /// Remove an installed plugin by name.
    pub fn remove(&self, name: &str) -> Result<(), ManagerError> {
        let mut lock = self.read_lock()?;
        let before = lock.plugins.len();
        lock.plugins.retain(|p| p.name != name);
        if lock.plugins.len() == before {
            return Err(ManagerError::NotFound(name.to_string()));
        }
        let _ = std::fs::remove_dir_all(self.plugin_dir(name));
        self.write_lock(&lock)?;
        Ok(())
    }

    /// Resolve the absolute path to an installed plugin's entry component.
    pub fn entry_path(&self, name: &str) -> Result<PathBuf, ManagerError> {
        let dir = self.plugin_dir(name);
        let manifest = self.read_manifest(&dir)?;
        Ok(dir.join(manifest.plugin.entry))
    }
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect()
}

fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            // Skip VCS metadata when copying local sources.
            if entry.file_name() == ".git" {
                continue;
            }
            copy_dir(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_github_source() {
        let s = Source::parse("github:onda-editor/todo-highlighter@v0.1.0").unwrap();
        assert_eq!(
            s,
            Source::Git {
                url: "https://github.com/onda-editor/todo-highlighter.git".into(),
                rev: Some("v0.1.0".into()),
            }
        );
    }

    #[test]
    fn install_remove_local_roundtrip() {
        let store = tempfile::tempdir().unwrap();
        let pkg = tempfile::tempdir().unwrap();
        std::fs::write(
            pkg.path().join("onda-plugin.toml"),
            r#"
            [plugin]
            name = "demo"
            version = "0.2.0"
            entry = "demo.wasm"
            [permissions]
            buffer = "read"
            "#,
        )
        .unwrap();
        std::fs::write(pkg.path().join("demo.wasm"), b"\0asm").unwrap();

        let mgr = PluginManager::new(store.path()).unwrap();
        let entry = mgr.install(pkg.path().to_str().unwrap()).unwrap();
        assert_eq!(entry.name, "demo");
        assert_eq!(entry.version, "0.2.0");
        assert!(entry.source.starts_with("local:"));

        // Materialized + listed + entry resolvable.
        assert!(mgr.plugin_dir("demo").join("demo.wasm").exists());
        assert_eq!(mgr.list().unwrap().len(), 1);
        assert!(mgr.entry_path("demo").unwrap().ends_with("demo.wasm"));

        // Lockfile persists across manager re-open.
        let mgr2 = PluginManager::new(store.path()).unwrap();
        assert_eq!(mgr2.list().unwrap()[0].name, "demo");

        mgr2.remove("demo").unwrap();
        assert!(mgr2.list().unwrap().is_empty());
        assert!(!mgr2.plugin_dir("demo").exists());
        assert!(matches!(
            mgr2.remove("demo"),
            Err(ManagerError::NotFound(_))
        ));
    }

    #[test]
    fn install_rejects_missing_entry_component() {
        let store = tempfile::tempdir().unwrap();
        let pkg = tempfile::tempdir().unwrap();
        std::fs::write(
            pkg.path().join("onda-plugin.toml"),
            r#"
            [plugin]
            name = "noentry"
            version = "0.1.0"
            entry = "missing.wasm"
            "#,
        )
        .unwrap();
        let mgr = PluginManager::new(store.path()).unwrap();
        assert!(matches!(
            mgr.install(pkg.path().to_str().unwrap()),
            Err(ManagerError::MissingEntry(_))
        ));
        // Failed install leaves nothing behind.
        assert!(mgr.list().unwrap().is_empty());
        assert!(!mgr.plugin_dir("noentry").exists());
    }

    #[test]
    fn install_clones_local_git_repo() {
        let store = tempfile::tempdir().unwrap();
        let repo_dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(repo_dir.path()).unwrap();
        std::fs::write(
            repo_dir.path().join("onda-plugin.toml"),
            r#"
            [plugin]
            name = "gitdemo"
            version = "0.1.0"
            entry = "p.wasm"
            "#,
        )
        .unwrap();
        std::fs::write(repo_dir.path().join("p.wasm"), b"\0asm").unwrap();
        // Commit the files.
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("t", "t@e.com").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();

        let mgr = PluginManager::new(store.path()).unwrap();
        let url = format!("file://{}", repo_dir.path().display());
        let entry = mgr.install(&url).unwrap();
        assert_eq!(entry.name, "gitdemo");
        assert!(entry.source.starts_with("git:"));
        assert_eq!(entry.revision.len(), 40, "resolved commit sha recorded");
        assert!(mgr.plugin_dir("gitdemo").join("p.wasm").exists());
    }
}
