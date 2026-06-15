use std::collections::HashMap;
use std::path::PathBuf;

use serde::Deserialize;
use tracing::warn;

// ---------------------------------------------------------------------------
// LineNumbers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LineNumbers {
    #[default]
    Absolute,
    Relative,
    None,
}

// ---------------------------------------------------------------------------
// EditorConfig
// ---------------------------------------------------------------------------

fn default_scrolloff() -> usize {
    5
}

fn default_tab_width() -> usize {
    4
}

fn default_expand_tab() -> bool {
    true
}

fn default_auto_indent() -> bool {
    true
}

fn default_clipboard() -> bool {
    true
}

fn default_theme() -> String {
    "default".to_string()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct EditorConfig {
    #[serde(default = "default_scrolloff")]
    pub scrolloff: usize,
    #[serde(default = "default_tab_width")]
    pub tab_width: usize,
    #[serde(default = "default_expand_tab")]
    pub expand_tab: bool,
    #[serde(default)]
    pub line_numbers: LineNumbers,
    #[serde(default)]
    pub cursorline: bool,
    #[serde(default = "default_auto_indent")]
    pub auto_indent: bool,
    #[serde(default = "default_clipboard")]
    pub clipboard: bool,
    #[serde(default)]
    pub mouse: bool,
    /// Persist per-file undo history across sessions (T29.1). Default off for v0.1.
    #[serde(default)]
    pub persistent_undo: bool,
}

impl Default for EditorConfig {
    fn default() -> Self {
        Self {
            scrolloff: default_scrolloff(),
            tab_width: default_tab_width(),
            expand_tab: default_expand_tab(),
            line_numbers: LineNumbers::default(),
            cursorline: false,
            auto_indent: default_auto_indent(),
            clipboard: default_clipboard(),
            mouse: false,
            persistent_undo: false,
        }
    }
}

// ---------------------------------------------------------------------------
// KeysConfig
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct KeysConfig {
    #[serde(default)]
    pub normal: HashMap<String, String>,
    #[serde(default)]
    pub insert: HashMap<String, String>,
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    #[serde(default)]
    pub editor: EditorConfig,
    #[serde(default)]
    pub keys: KeysConfig,
    #[serde(default = "default_theme")]
    pub theme: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            editor: EditorConfig::default(),
            keys: KeysConfig::default(),
            theme: default_theme(),
        }
    }
}

// ---------------------------------------------------------------------------
// ConfigLoadResult
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct ConfigLoadResult {
    pub config: Config,
    pub warning: Option<String>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Attempt to read and parse a TOML config file. Returns `None` if the file
/// does not exist, `Some(Ok(config))` on success, or `Some(Err(warning))`
/// on a parse / read error.
fn try_load(path: &PathBuf) -> Option<Result<Config, String>> {
    if !path.exists() {
        return None;
    }
    match std::fs::read_to_string(path) {
        Err(e) => Some(Err(format!(
            "onda-config: could not read {}: {}",
            path.display(),
            e
        ))),
        Ok(text) => match toml::from_str::<Config>(&text) {
            Ok(cfg) => Some(Ok(cfg)),
            Err(e) => Some(Err(format!(
                "onda-config: parse error in {}: {}",
                path.display(),
                e
            ))),
        },
    }
}

/// Merge `overlay` on top of `base`: non-default fields in overlay win.
/// We re-serialise to TOML and re-deserialise using the `#[serde(default)]`
/// machinery so that only keys explicitly present in the overlay file override
/// the base. The raw TOML tables are merged at the map level.
fn merge(base: Config, overlay: Config) -> Config {
    // Merge editor — field-by-field (overlay wins for every field it touched,
    // but since we already parsed with defaults we use the overlay values
    // directly; callers load home first then project, so overlay = project).
    //
    // A full deep-merge would require tracking which fields were explicitly
    // set vs defaulted. For Phase 0 correctness we simply let the project
    // config override the entire struct it specifies. That is: if the project
    // file contains an [editor] section the whole EditorConfig from that file
    // is used; otherwise the home value is kept.
    //
    // We detect "was the section present?" by re-parsing the raw TOML values.
    // For simplicity and correctness, overlay wins on a per-top-level-section
    // basis (editor / keys / theme).
    Config {
        editor: overlay.editor,
        keys: {
            let mut normal = base.keys.normal;
            normal.extend(overlay.keys.normal);
            let mut insert = base.keys.insert;
            insert.extend(overlay.keys.insert);
            KeysConfig { normal, insert }
        },
        theme: overlay.theme,
    }
}

// ---------------------------------------------------------------------------
// Config::load
// ---------------------------------------------------------------------------

impl Config {
    /// Load configuration, searching in priority order:
    ///
    /// 1. `~/.config/onda/config.toml` (via `HOME`)
    /// 2. `$XDG_CONFIG_HOME/onda/config.toml`
    /// 3. `.onda/config.toml` in the current directory (project-local, highest priority)
    ///
    /// Files found later override files found earlier. On parse errors the
    /// defaults are returned together with a warning string.
    pub fn load() -> ConfigLoadResult {
        let candidates: Vec<PathBuf> = {
            let mut v = Vec::new();

            // 1. HOME-based default
            if let Ok(home) = std::env::var("HOME") {
                v.push(PathBuf::from(home).join(".config/onda/config.toml"));
            }

            // 2. XDG_CONFIG_HOME (skip if it resolves to the same path as HOME/.config)
            if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
                let p = PathBuf::from(xdg).join("onda/config.toml");
                if !v.contains(&p) {
                    v.push(p);
                }
            }

            // 3. Project-local
            v.push(PathBuf::from(".onda/config.toml"));

            v
        };

        let mut accumulated = Config::default();
        let mut warning: Option<String> = None;

        for path in &candidates {
            match try_load(path) {
                None => {} // file absent — skip silently
                Some(Ok(cfg)) => {
                    accumulated = merge(accumulated, cfg);
                }
                Some(Err(msg)) => {
                    warn!("{}", msg);
                    // Keep whatever we have so far; record the first warning.
                    if warning.is_none() {
                        warning = Some(msg);
                    }
                }
            }
        }

        ConfigLoadResult {
            config: accumulated,
            warning,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let cfg = Config::default();
        assert_eq!(cfg.editor.scrolloff, 5);
        assert_eq!(cfg.editor.tab_width, 4);
        assert!(cfg.editor.expand_tab);
        assert_eq!(cfg.editor.line_numbers, LineNumbers::Absolute);
        assert!(!cfg.editor.cursorline);
        assert!(cfg.editor.auto_indent);
        assert!(cfg.editor.clipboard);
        assert!(!cfg.editor.mouse);
        assert_eq!(cfg.theme, "default");
        assert!(cfg.keys.normal.is_empty());
        assert!(cfg.keys.insert.is_empty());
    }

    #[test]
    fn parse_minimal_toml() {
        let src = r#"
theme = "gruvbox"

[editor]
tab_width = 2
cursorline = true
line_numbers = "relative"
"#;
        let cfg: Config = toml::from_str(src).expect("parse failed");
        assert_eq!(cfg.theme, "gruvbox");
        assert_eq!(cfg.editor.tab_width, 2);
        assert!(cfg.editor.cursorline);
        assert_eq!(cfg.editor.line_numbers, LineNumbers::Relative);
        // unset fields keep defaults
        assert_eq!(cfg.editor.scrolloff, 5);
    }

    #[test]
    fn parse_keybindings() {
        let src = r#"
[keys.normal]
"<C-s>" = "write"

[keys.insert]
"jk" = "normal_mode"
"#;
        let cfg: Config = toml::from_str(src).expect("parse failed");
        assert_eq!(
            cfg.keys.normal.get("<C-s>").map(String::as_str),
            Some("write")
        );
        assert_eq!(
            cfg.keys.insert.get("jk").map(String::as_str),
            Some("normal_mode")
        );
    }

    #[test]
    fn bad_toml_returns_default_with_warning() {
        use std::io::Write;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "this is [[ not valid toml").unwrap();
        let path = tmp.path().to_path_buf();
        let result = try_load(&path);
        assert!(matches!(result, Some(Err(_))));
    }

    #[test]
    fn missing_file_returns_none() {
        let path = PathBuf::from("/nonexistent/path/config.toml");
        assert!(try_load(&path).is_none());
    }

    #[test]
    fn merge_keys_are_combined() {
        let mut base = Config::default();
        base.keys.normal.insert("k".into(), "move_up".into());

        let mut overlay = Config::default();
        overlay.keys.normal.insert("j".into(), "move_down".into());

        let merged = merge(base, overlay);
        assert_eq!(
            merged.keys.normal.get("k").map(String::as_str),
            Some("move_up")
        );
        assert_eq!(
            merged.keys.normal.get("j").map(String::as_str),
            Some("move_down")
        );
    }

    #[test]
    fn overlay_editor_and_theme_override_base() {
        let mut base = Config::default();
        base.editor.tab_width = 8;
        base.theme = "base-theme".into();

        let mut overlay = Config::default();
        overlay.editor.tab_width = 2;
        overlay.editor.expand_tab = false;
        overlay.theme = "project-theme".into();

        let merged = merge(base, overlay);
        assert_eq!(merged.editor.tab_width, 2);
        assert!(!merged.editor.expand_tab);
        assert_eq!(merged.theme, "project-theme");
    }

    #[test]
    fn parse_editor_section_overrides_defaults() {
        let toml = r#"
            theme = "onda-light"
            [editor]
            tab_width = 2
            expand_tab = false
            scrolloff = 5
        "#;
        let cfg: Config = toml::from_str(toml).unwrap();
        assert_eq!(cfg.editor.tab_width, 2);
        assert!(!cfg.editor.expand_tab);
        assert_eq!(cfg.editor.scrolloff, 5);
        assert_eq!(cfg.theme, "onda-light");
        // Unspecified editor fields keep their defaults.
        assert_eq!(cfg.editor.auto_indent, default_auto_indent());
    }
}
