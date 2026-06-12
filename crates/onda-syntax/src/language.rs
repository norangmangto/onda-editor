use std::collections::HashMap;

use thiserror::Error;

/// Errors from language registry operations.
#[derive(Debug, Error)]
pub enum LanguageError {
    #[error("unknown language: {0}")]
    Unknown(String),
}

/// A resolved tree-sitter language with its metadata.
#[derive(Debug, Clone)]
pub struct Language {
    pub name: String,
    pub extensions: Vec<String>,
}

/// Static configuration for a single language.
#[derive(Debug, Clone)]
pub struct LanguageConfig {
    /// Human-readable language name (e.g. "rust", "python").
    pub name: String,
    /// Grammar name used by tree-sitter crate functions.
    pub grammar_name: String,
    /// Line-comment token (e.g. "//", "#").
    pub comment_token: String,
    /// String used for one indent level (e.g. "    " or "\t").
    pub indent_unit: String,
    /// Number of spaces per indent level.
    pub indent_width: u8,
}

/// Registry that maps file extensions and shebangs to language configs.
#[derive(Debug, Default)]
pub struct LanguageRegistry {
    configs: Vec<LanguageConfig>,
    by_extension: HashMap<String, usize>,
    by_shebang: HashMap<String, usize>,
}

impl LanguageRegistry {
    /// Build the registry with the four built-in languages.
    pub fn new() -> Self {
        let mut reg = Self::default();

        reg.add(
            LanguageConfig {
                name: "rust".into(),
                grammar_name: "rust".into(),
                comment_token: "//".into(),
                indent_unit: "    ".into(),
                indent_width: 4,
            },
            &["rs"],
            &[],
        );

        reg.add(
            LanguageConfig {
                name: "python".into(),
                grammar_name: "python".into(),
                comment_token: "#".into(),
                indent_unit: "    ".into(),
                indent_width: 4,
            },
            &["py", "pyw"],
            &["python", "python3", "python2"],
        );

        reg.add(
            LanguageConfig {
                name: "json".into(),
                grammar_name: "json".into(),
                comment_token: "".into(),
                indent_unit: "  ".into(),
                indent_width: 2,
            },
            &["json", "jsonc"],
            &[],
        );

        reg.add(
            LanguageConfig {
                name: "toml".into(),
                grammar_name: "toml".into(),
                comment_token: "#".into(),
                indent_unit: "  ".into(),
                indent_width: 2,
            },
            &["toml"],
            &[],
        );

        reg
    }

    fn add(&mut self, config: LanguageConfig, extensions: &[&str], shebangs: &[&str]) {
        let idx = self.configs.len();
        for ext in extensions {
            self.by_extension.insert((*ext).to_owned(), idx);
        }
        for shebang in shebangs {
            self.by_shebang.insert((*shebang).to_owned(), idx);
        }
        self.configs.push(config);
    }

    /// Look up a language config by file extension (without leading dot).
    pub fn by_extension(&self, ext: &str) -> Option<&LanguageConfig> {
        self.by_extension.get(ext).map(|&i| &self.configs[i])
    }

    /// Look up a language config by the shebang interpreter name.
    pub fn by_shebang(&self, interpreter: &str) -> Option<&LanguageConfig> {
        self.by_shebang.get(interpreter).map(|&i| &self.configs[i])
    }

    /// Look up a language config by language name.
    pub fn by_name(&self, name: &str) -> Option<&LanguageConfig> {
        self.configs.iter().find(|c| c.name == name)
    }

    /// Detect the language for a file given its path and (optionally) the first
    /// line of its content.  Tries extension first, then shebang.
    pub fn detect(&self, path: &str, first_line: Option<&str>) -> Option<&LanguageConfig> {
        // 1. Try file extension.
        if let Some(ext) = path.rsplit('.').next() {
            if let Some(cfg) = self.by_extension(ext) {
                return Some(cfg);
            }
        }

        // 2. Try shebang on the first line.
        if let Some(line) = first_line {
            if let Some(interpreter) = parse_shebang(line) {
                if let Some(cfg) = self.by_shebang(interpreter) {
                    return Some(cfg);
                }
            }
        }

        None
    }
}

/// Extract the interpreter name from a shebang line like `#!/usr/bin/env python3`.
fn parse_shebang(line: &str) -> Option<&str> {
    let line = line.strip_prefix("#!")?;
    // Handle `#!/usr/bin/env python3` or `#!/usr/bin/python3`
    let parts: Vec<&str> = line.split_whitespace().collect();
    let exe = parts.last()?;
    // Take the basename of the path.
    exe.rsplit('/').next()
}
