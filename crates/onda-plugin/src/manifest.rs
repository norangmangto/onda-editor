//! `onda-plugin.toml` manifest schema (DESIGN §5.5, PHASE3 T17.3).
//!
//! The manifest is the plugin's declaration of identity, entry point, requested
//! permissions, and lazy-activation events. Capability interfaces (`fs`, `http`)
//! are wired into the WASM instance **only** if the manifest declares them and
//! the user approves — absence of a declaration is enforcement, not just policy.

use serde::Deserialize;
use thiserror::Error;

/// API version this host implements (mirrors `wit/onda/world.wit`).
pub const HOST_API_VERSION: ApiVersion = ApiVersion { major: 0, minor: 1 };

#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("manifest parse error: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("invalid manifest: {0}")]
    Invalid(String),
    #[error("plugin requires API {required}, host provides {host} (incompatible major version)")]
    ApiIncompatible {
        required: ApiVersion,
        host: ApiVersion,
    },
}

/// A `major.minor` API version. Same `major` = compatible (v0: any 0.x links,
/// but a plugin asking for a higher minor than the host fails).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub struct ApiVersion {
    pub major: u32,
    pub minor: u32,
}

impl std::fmt::Display for ApiVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

impl TryFrom<String> for ApiVersion {
    type Error = String;
    fn try_from(s: String) -> Result<Self, String> {
        let (maj, min) = s
            .split_once('.')
            .ok_or_else(|| format!("expected `major.minor`, got `{s}`"))?;
        Ok(ApiVersion {
            major: maj.parse().map_err(|_| format!("bad major in `{s}`"))?,
            minor: min.parse().map_err(|_| format!("bad minor in `{s}`"))?,
        })
    }
}

impl ApiVersion {
    /// Whether `self` (the host) can satisfy a plugin requiring `required`.
    pub fn satisfies(self, required: ApiVersion) -> bool {
        self.major == required.major && self.minor >= required.minor
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub plugin: PluginMeta,
    #[serde(default)]
    pub permissions: Permissions,
    #[serde(default)]
    pub activation: Activation,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PluginMeta {
    pub name: String,
    pub version: String,
    /// Relative path to the compiled WASM component (e.g. `plugin.wasm`).
    pub entry: String,
    /// Minimum host API version required. Defaults to the host version.
    #[serde(rename = "min-api-version", default = "default_api")]
    pub min_api_version: ApiVersion,
}

fn default_api() -> ApiVersion {
    HOST_API_VERSION
}

/// Buffer access level requested by the plugin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BufferAccess {
    #[default]
    None,
    Read,
    Write,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Permissions {
    #[serde(default)]
    pub buffer: BufferAccess,
    /// Filesystem path whitelist (resolved against the project root). Empty = no fs.
    #[serde(default)]
    pub filesystem: Vec<String>,
    #[serde(default)]
    pub network: bool,
    /// Shell execution. v0: always denied regardless of this flag (DESIGN §5.5).
    #[serde(default)]
    pub shell: bool,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Activation {
    /// Events that lazily activate the plugin (protects startup, T18.1).
    /// e.g. "buffer-open", "cursor-hold", "buffer-save", "buffer-change",
    /// "mode-change", or "command:<name>".
    #[serde(default)]
    pub events: Vec<String>,
}

impl Manifest {
    /// Parse and validate a manifest against the host API version.
    pub fn parse(toml_src: &str) -> Result<Self, ManifestError> {
        let m: Manifest = toml::from_str(toml_src)?;
        if m.plugin.name.trim().is_empty() {
            return Err(ManifestError::Invalid("plugin.name is empty".into()));
        }
        if m.plugin.entry.trim().is_empty() {
            return Err(ManifestError::Invalid("plugin.entry is empty".into()));
        }
        if !HOST_API_VERSION.satisfies(m.plugin.min_api_version) {
            return Err(ManifestError::ApiIncompatible {
                required: m.plugin.min_api_version,
                host: HOST_API_VERSION,
            });
        }
        Ok(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
        [plugin]
        name = "git-blame-inline"
        version = "0.1.0"
        entry = "plugin.wasm"

        [permissions]
        buffer = "read"
        filesystem = ["./.git"]
        network = false
        shell = false

        [activation]
        events = ["buffer-open", "cursor-hold"]
    "#;

    #[test]
    fn parses_sample_manifest() {
        let m = Manifest::parse(SAMPLE).expect("valid manifest");
        assert_eq!(m.plugin.name, "git-blame-inline");
        assert_eq!(m.plugin.entry, "plugin.wasm");
        assert_eq!(m.permissions.buffer, BufferAccess::Read);
        assert_eq!(m.permissions.filesystem, vec!["./.git".to_string()]);
        assert!(!m.permissions.network);
        assert_eq!(m.activation.events.len(), 2);
    }

    #[test]
    fn defaults_are_least_privilege() {
        let m = Manifest::parse(
            r#"
            [plugin]
            name = "minimal"
            version = "0.0.1"
            entry = "p.wasm"
            "#,
        )
        .expect("minimal manifest");
        assert_eq!(m.permissions.buffer, BufferAccess::None);
        assert!(m.permissions.filesystem.is_empty());
        assert!(!m.permissions.network);
        assert!(!m.permissions.shell);
        assert!(m.activation.events.is_empty());
    }

    #[test]
    fn rejects_empty_name() {
        let err = Manifest::parse(
            r#"
            [plugin]
            name = ""
            version = "0.0.1"
            entry = "p.wasm"
            "#,
        );
        assert!(err.is_err());
    }

    #[test]
    fn rejects_future_api_version() {
        let err = Manifest::parse(
            r#"
            [plugin]
            name = "future"
            version = "0.0.1"
            entry = "p.wasm"
            min-api-version = "0.99"
            "#,
        );
        assert!(matches!(err, Err(ManifestError::ApiIncompatible { .. })));
    }

    #[test]
    fn rejects_different_major() {
        assert!(!HOST_API_VERSION.satisfies(ApiVersion { major: 1, minor: 0 }));
    }

    #[test]
    fn api_version_roundtrip() {
        let v = ApiVersion::try_from("0.1".to_string()).unwrap();
        assert_eq!(v, ApiVersion { major: 0, minor: 1 });
        assert_eq!(v.to_string(), "0.1");
    }
}
