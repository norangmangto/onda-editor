//! Permission/capability model (DESIGN §5.5, PHASE3 T17.3 / T18.3).
//!
//! A manifest *requests* capabilities; the user *grants* them; the host wires the
//! corresponding WIT interface into the instance only when granted. The set of
//! granted capabilities is the enforcement boundary — a plugin cannot import a
//! capability it was not granted (it fails to link, T17.3 acceptance).

use std::path::{Path, PathBuf};

use crate::manifest::{BufferAccess, Permissions};

/// A single capability a plugin may hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Capability {
    BufferRead,
    BufferWrite,
    /// Filesystem access scoped to a project-root-relative path (preopen).
    Fs(PathBuf),
    Network,
    // Shell is intentionally absent: v0 always denies it.
}

/// The user's decision on a capability prompt (persisted per plugin+capability).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    AllowOnce,
    AllowAlways,
    Deny,
    DenyAlways,
}

/// The capabilities a plugin has been granted for this session. Built from the
/// manifest's *requests* intersected with the user's *grants*.
#[derive(Debug, Clone, Default)]
pub struct GrantedCaps {
    buffer: BufferAccess,
    fs_roots: Vec<PathBuf>,
    network: bool,
}

impl GrantedCaps {
    /// Build the granted set from manifest permissions, resolving fs paths against
    /// `project_root` and rejecting `..` escapes. `approve` decides each requested
    /// capability (the host calls this with the persisted/prompted decision).
    pub fn resolve(
        perms: &Permissions,
        project_root: &Path,
        mut approve: impl FnMut(&Capability) -> bool,
    ) -> Self {
        let mut g = GrantedCaps::default();

        match perms.buffer {
            BufferAccess::None => {}
            BufferAccess::Read => {
                if approve(&Capability::BufferRead) {
                    g.buffer = BufferAccess::Read;
                }
            }
            BufferAccess::Write => {
                if approve(&Capability::BufferWrite) {
                    g.buffer = BufferAccess::Write;
                }
            }
        }

        for raw in &perms.filesystem {
            if let Some(resolved) = resolve_within(project_root, raw) {
                if approve(&Capability::Fs(resolved.clone())) {
                    g.fs_roots.push(resolved);
                }
            }
            // `..`-escaping or absolute paths outside the root are silently
            // dropped — never granted (T18.3 escape-attempt rule).
        }

        if perms.network && approve(&Capability::Network) {
            g.network = true;
        }

        g
    }

    pub fn can_read_buffer(&self) -> bool {
        matches!(self.buffer, BufferAccess::Read | BufferAccess::Write)
    }
    pub fn can_write_buffer(&self) -> bool {
        matches!(self.buffer, BufferAccess::Write)
    }
    pub fn network(&self) -> bool {
        self.network
    }
    pub fn fs_roots(&self) -> &[PathBuf] {
        &self.fs_roots
    }

    /// Whether a host fs access to `path` is permitted (must be under a preopen).
    pub fn fs_allows(&self, path: &Path) -> bool {
        self.fs_roots.iter().any(|root| path.starts_with(root))
    }
}

/// Resolve `rel` against `root`, returning `None` if it escapes the root via
/// `..` or is an absolute path pointing elsewhere. Pure lexical check (no IO) so
/// it is safe to call on the main thread (rule 2).
fn resolve_within(root: &Path, rel: &str) -> Option<PathBuf> {
    let candidate = Path::new(rel);
    // Reject absolute paths that are not already under root.
    let joined = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        root.join(candidate)
    };

    // Lexically normalize, refusing any `..` that would climb above root.
    let mut out = PathBuf::new();
    for comp in joined.components() {
        use std::path::Component::*;
        match comp {
            ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    if out.starts_with(root) {
        Some(out)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Manifest;

    fn perms(toml: &str) -> Permissions {
        Manifest::parse(toml).unwrap().permissions
    }

    #[test]
    fn grants_only_approved_capabilities() {
        let p = perms(
            r#"
            [plugin]
            name="p"
            version="0.1.0"
            entry="p.wasm"
            [permissions]
            buffer="write"
            network=true
            "#,
        );
        let root = Path::new("/proj");
        // Approve buffer, deny network.
        let g = GrantedCaps::resolve(&p, root, |cap| !matches!(cap, Capability::Network));
        assert!(g.can_write_buffer());
        assert!(!g.network());
    }

    #[test]
    fn fs_whitelist_resolves_under_root() {
        let p = perms(
            r#"
            [plugin]
            name="p"
            version="0.1.0"
            entry="p.wasm"
            [permissions]
            filesystem = ["./.git", "src"]
            "#,
        );
        let root = Path::new("/proj");
        let g = GrantedCaps::resolve(&p, root, |_| true);
        assert!(g.fs_allows(Path::new("/proj/.git/HEAD")));
        assert!(g.fs_allows(Path::new("/proj/src/main.rs")));
        assert!(!g.fs_allows(Path::new("/etc/passwd")));
    }

    #[test]
    fn rejects_parent_dir_escape() {
        assert_eq!(resolve_within(Path::new("/proj"), "../secrets"), None);
        assert_eq!(resolve_within(Path::new("/proj"), "src/../../etc"), None);
        assert!(resolve_within(Path::new("/proj"), "src/./mod.rs").is_some());
    }

    #[test]
    fn shell_is_never_a_capability() {
        // The Capability enum has no Shell variant — v0 cannot grant it by design.
        let p = perms(
            r#"
            [plugin]
            name="p"
            version="0.1.0"
            entry="p.wasm"
            [permissions]
            shell = true
            "#,
        );
        let g = GrantedCaps::resolve(&p, Path::new("/proj"), |_| true);
        // Nothing shell-related is grantable; the granted set is empty of fs/net.
        assert!(g.fs_roots().is_empty());
        assert!(!g.network());
    }
}
