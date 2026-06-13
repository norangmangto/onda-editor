//! Permission gate model & persistence (T24.3).
//!
//! Every agent tool request (write a file, run a command) is gated. The user can
//! allow/deny once, or allow/deny **always** — in which case a scope-pattern rule is
//! persisted per `agent + tool + scope` (e.g. "writes under `src/`: always"). Rules
//! are matched on subsequent requests so the user isn't re-prompted.
//!
//! Safety properties enforced by construction:
//! - Shell execution always prompts unless an explicit always-rule exists (there is
//!   no API to create a blanket allow-all rule).
//! - "Once" decisions never persist.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::protocol::{PermissionOptionKind, ToolKind};

/// What a tool request targets (used to derive a rule scope).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A file write/edit/delete at `path`.
    Path(PathBuf),
    /// A shell command (full command line; the first token is the program).
    Command(String),
}

/// A persisted allow/deny decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Allow,
    Deny,
}

/// The scope a rule applies to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Scope {
    /// Matches any path equal to or under `prefix` (component-wise).
    PathPrefix { prefix: String },
    /// Matches commands whose program (first token) equals `program`.
    Command { program: String },
}

impl Scope {
    fn matches(&self, target: &Target) -> bool {
        match (self, target) {
            (Scope::PathPrefix { prefix }, Target::Path(p)) => path_under(p, prefix),
            (Scope::Command { program }, Target::Command(cmd)) => {
                first_token(cmd) == program.as_str()
            }
            _ => false,
        }
    }
}

/// A persisted permission rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    pub agent: String,
    pub tool: ToolKind,
    pub scope: Scope,
    pub decision: Decision,
}

/// Store of persisted permission rules.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct PermissionStore {
    #[serde(default)]
    rules: Vec<Rule>,
}

impl PermissionStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Return a persisted decision for `(agent, tool, target)`, or `None` if the user
    /// must be prompted. The most recently added matching rule wins.
    pub fn check(&self, agent: &str, tool: ToolKind, target: &Target) -> Option<Decision> {
        self.rules
            .iter()
            .rev()
            .find(|r| r.agent == agent && r.tool == tool && r.scope.matches(target))
            .map(|r| r.decision)
    }

    /// Apply a user's choice. Returns the effective decision; persists a rule only for
    /// the "always" variants. Returns `None` for cancellation-like inputs (none here).
    pub fn apply_choice(
        &mut self,
        agent: &str,
        tool: ToolKind,
        target: &Target,
        choice: PermissionOptionKind,
    ) -> Decision {
        let (decision, persist) = match choice {
            PermissionOptionKind::AllowOnce => (Decision::Allow, false),
            PermissionOptionKind::AllowAlways => (Decision::Allow, true),
            PermissionOptionKind::RejectOnce => (Decision::Deny, false),
            PermissionOptionKind::RejectAlways => (Decision::Deny, true),
        };
        if persist {
            let scope = derive_scope(target);
            self.add_rule(Rule {
                agent: agent.to_string(),
                tool,
                scope,
                decision,
            });
        }
        decision
    }

    /// Add a rule, replacing any existing rule with the same agent+tool+scope.
    pub fn add_rule(&mut self, rule: Rule) {
        self.rules
            .retain(|r| !(r.agent == rule.agent && r.tool == rule.tool && r.scope == rule.scope));
        self.rules.push(rule);
    }

    /// Remove the rule at `index`; takes effect immediately for future `check`s.
    pub fn revoke(&mut self, index: usize) -> Option<Rule> {
        if index < self.rules.len() {
            Some(self.rules.remove(index))
        } else {
            None
        }
    }

    /// Load rules from a JSON state file (missing file → empty store).
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Persist rules to a JSON state file (creating parent dirs).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into());
        std::fs::write(path, json)
    }
}

/// Derive the rule scope from a target: a file's parent directory (so an "always"
/// decision covers that directory), or a command's program name.
fn derive_scope(target: &Target) -> Scope {
    match target {
        Target::Path(p) => {
            let prefix = p
                .parent()
                .map(|d| d.to_string_lossy().into_owned())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| ".".to_string());
            Scope::PathPrefix { prefix }
        }
        Target::Command(cmd) => Scope::Command {
            program: first_token(cmd).to_string(),
        },
    }
}

/// True if `path` equals `prefix` or sits under it (component-wise, not substring).
fn path_under(path: &Path, prefix: &str) -> bool {
    if prefix == "." {
        // Parent-of-a-bare-filename scope: matches files with no directory.
        return path
            .parent()
            .map(|p| p.as_os_str().is_empty())
            .unwrap_or(true);
    }
    let prefix_path = Path::new(prefix);
    path == prefix_path || path.starts_with(prefix_path)
}

fn first_token(cmd: &str) -> &str {
    cmd.split_whitespace().next().unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(p: &str) -> Target {
        Target::Path(PathBuf::from(p))
    }

    #[test]
    fn unknown_request_must_prompt() {
        let store = PermissionStore::new();
        assert_eq!(
            store.check("claude", ToolKind::Edit, &path("src/main.rs")),
            None
        );
    }

    #[test]
    fn allow_always_covers_directory() {
        let mut store = PermissionStore::new();
        store.apply_choice(
            "claude",
            ToolKind::Edit,
            &path("src/main.rs"),
            PermissionOptionKind::AllowAlways,
        );
        // Same dir and subdirs are now allowed without prompting.
        assert_eq!(
            store.check("claude", ToolKind::Edit, &path("src/main.rs")),
            Some(Decision::Allow)
        );
        assert_eq!(
            store.check("claude", ToolKind::Edit, &path("src/sub/x.rs")),
            Some(Decision::Allow)
        );
        // A different directory still prompts.
        assert_eq!(
            store.check("claude", ToolKind::Edit, &path("lib/x.rs")),
            None
        );
    }

    #[test]
    fn allow_once_does_not_persist() {
        let mut store = PermissionStore::new();
        let d = store.apply_choice(
            "claude",
            ToolKind::Edit,
            &path("src/main.rs"),
            PermissionOptionKind::AllowOnce,
        );
        assert_eq!(d, Decision::Allow);
        assert!(store.is_empty());
        assert_eq!(
            store.check("claude", ToolKind::Edit, &path("src/main.rs")),
            None
        );
    }

    #[test]
    fn deny_always_persists_deny() {
        let mut store = PermissionStore::new();
        store.apply_choice(
            "claude",
            ToolKind::Execute,
            &Target::Command("rm -rf /".into()),
            PermissionOptionKind::RejectAlways,
        );
        assert_eq!(
            store.check(
                "claude",
                ToolKind::Execute,
                &Target::Command("rm -rf /tmp".into())
            ),
            Some(Decision::Deny)
        );
    }

    #[test]
    fn execute_prompts_without_explicit_rule() {
        // Safety: no blanket allow; a write rule never covers execute.
        let mut store = PermissionStore::new();
        store.apply_choice(
            "claude",
            ToolKind::Edit,
            &path("src/x.rs"),
            PermissionOptionKind::AllowAlways,
        );
        assert_eq!(
            store.check(
                "claude",
                ToolKind::Execute,
                &Target::Command("cargo test".into())
            ),
            None
        );
    }

    #[test]
    fn command_scope_matches_program_only() {
        let mut store = PermissionStore::new();
        store.apply_choice(
            "claude",
            ToolKind::Execute,
            &Target::Command("cargo test".into()),
            PermissionOptionKind::AllowAlways,
        );
        // Same program, different args → allowed.
        assert_eq!(
            store.check(
                "claude",
                ToolKind::Execute,
                &Target::Command("cargo build".into())
            ),
            Some(Decision::Allow)
        );
        // Different program → prompt.
        assert_eq!(
            store.check(
                "claude",
                ToolKind::Execute,
                &Target::Command("rm file".into())
            ),
            None
        );
    }

    #[test]
    fn agent_scoped() {
        let mut store = PermissionStore::new();
        store.apply_choice(
            "claude",
            ToolKind::Edit,
            &path("src/x.rs"),
            PermissionOptionKind::AllowAlways,
        );
        // A different agent does not inherit the rule.
        assert_eq!(
            store.check("other", ToolKind::Edit, &path("src/x.rs")),
            None
        );
    }

    #[test]
    fn revoke_takes_effect_immediately() {
        let mut store = PermissionStore::new();
        store.apply_choice(
            "claude",
            ToolKind::Edit,
            &path("src/x.rs"),
            PermissionOptionKind::AllowAlways,
        );
        assert_eq!(store.rules().len(), 1);
        store.revoke(0);
        assert_eq!(
            store.check("claude", ToolKind::Edit, &path("src/x.rs")),
            None
        );
    }

    #[test]
    fn persistence_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("perms.json");
        let mut store = PermissionStore::new();
        store.apply_choice(
            "claude",
            ToolKind::Edit,
            &path("src/main.rs"),
            PermissionOptionKind::AllowAlways,
        );
        store.save(&file).unwrap();

        let loaded = PermissionStore::load(&file);
        assert_eq!(
            loaded.check("claude", ToolKind::Edit, &path("src/lib.rs")),
            Some(Decision::Allow)
        );
    }

    #[test]
    fn newest_rule_wins() {
        let mut store = PermissionStore::new();
        // Allow then deny the same scope → deny (most recent) replaces.
        store.apply_choice(
            "claude",
            ToolKind::Edit,
            &path("src/x.rs"),
            PermissionOptionKind::AllowAlways,
        );
        store.apply_choice(
            "claude",
            ToolKind::Edit,
            &path("src/x.rs"),
            PermissionOptionKind::RejectAlways,
        );
        assert_eq!(
            store.check("claude", ToolKind::Edit, &path("src/x.rs")),
            Some(Decision::Deny)
        );
        assert_eq!(
            store.rules().len(),
            1,
            "same scope replaces, not duplicates"
        );
    }
}
