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
    by_filename: HashMap<String, usize>,
    by_shebang: HashMap<String, usize>,
}

impl LanguageRegistry {
    /// Build the registry with built-in languages.
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
            &[],
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
            // jsonl: one JSON value per line; reuse the JSON grammar
            &["json", "jsonc", "jsonl"],
            &[],
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
            &[],
        );

        reg.add(
            LanguageConfig {
                name: "yaml".into(),
                grammar_name: "yaml".into(),
                comment_token: "#".into(),
                indent_unit: "  ".into(),
                indent_width: 2,
            },
            &["yaml", "yml"],
            &[],
            &[],
        );

        reg.add(
            LanguageConfig {
                name: "markdown".into(),
                grammar_name: "markdown".into(),
                comment_token: "".into(),
                indent_unit: "  ".into(),
                indent_width: 2,
            },
            &["md", "markdown"],
            &[],
            &[],
        );

        reg.add(
            LanguageConfig {
                name: "hcl".into(),
                grammar_name: "hcl".into(),
                comment_token: "#".into(),
                indent_unit: "  ".into(),
                indent_width: 2,
            },
            // .tf and .tfvars are Terraform HCL; .hcl is generic HCL
            &["hcl", "tf", "tfvars"],
            &[],
            &[],
        );

        // CSV: plain-text columnar data; tree-sitter-free column tinting (also `:table`).
        reg.add(
            LanguageConfig {
                name: "csv".into(),
                grammar_name: "csv".into(),
                comment_token: "".into(),
                indent_unit: "".into(),
                indent_width: 0,
            },
            &["csv", "tsv"],
            &[],
            &[],
        );

        // Shell (bash grammar covers sh/bash/zsh).
        reg.add(
            LanguageConfig {
                name: "bash".into(),
                grammar_name: "bash".into(),
                comment_token: "#".into(),
                indent_unit: "  ".into(),
                indent_width: 2,
            },
            &["sh", "bash", "zsh"],
            &[],
            &["sh", "bash", "zsh", "dash", "ksh"],
        );

        // Makefile — extensionless; detected by exact file name (tabs are significant).
        reg.add(
            LanguageConfig {
                name: "make".into(),
                grammar_name: "make".into(),
                comment_token: "#".into(),
                indent_unit: "\t".into(),
                indent_width: 4,
            },
            &["mk"],
            &["Makefile", "makefile", "GNUmakefile"],
            &[],
        );

        reg.add(
            LanguageConfig {
                name: "go".into(),
                grammar_name: "go".into(),
                comment_token: "//".into(),
                indent_unit: "\t".into(),
                indent_width: 4,
            },
            &["go"],
            &[],
            &[],
        );

        reg.add(
            LanguageConfig {
                name: "javascript".into(),
                grammar_name: "javascript".into(),
                comment_token: "//".into(),
                indent_unit: "  ".into(),
                indent_width: 2,
            },
            &["js", "mjs", "cjs", "jsx"],
            &[],
            &["node"],
        );

        reg.add(
            LanguageConfig {
                name: "typescript".into(),
                grammar_name: "typescript".into(),
                comment_token: "//".into(),
                indent_unit: "  ".into(),
                indent_width: 2,
            },
            &["ts", "tsx"],
            &[],
            &[],
        );

        reg.add(
            LanguageConfig {
                name: "html".into(),
                grammar_name: "html".into(),
                comment_token: "".into(),
                indent_unit: "  ".into(),
                indent_width: 2,
            },
            &["html", "htm"],
            &[],
            &[],
        );

        reg.add(
            LanguageConfig {
                name: "css".into(),
                grammar_name: "css".into(),
                comment_token: "".into(),
                indent_unit: "  ".into(),
                indent_width: 2,
            },
            &["css"],
            &[],
            &[],
        );

        reg
    }

    fn add(
        &mut self,
        config: LanguageConfig,
        extensions: &[&str],
        filenames: &[&str],
        shebangs: &[&str],
    ) {
        let idx = self.configs.len();
        for ext in extensions {
            self.by_extension.insert((*ext).to_owned(), idx);
        }
        for name in filenames {
            self.by_filename.insert((*name).to_owned(), idx);
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

    /// Look up a language config by exact file name (e.g. `Makefile`, `Dockerfile`).
    pub fn by_filename(&self, filename: &str) -> Option<&LanguageConfig> {
        self.by_filename.get(filename).map(|&i| &self.configs[i])
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
        // 0. Exact file name (e.g. Makefile, Dockerfile — extensionless).
        let basename = path.rsplit(['/', '\\']).next().unwrap_or(path);
        if let Some(cfg) = self.by_filename(basename) {
            return Some(cfg);
        }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_has_builtin_languages() {
        let r = LanguageRegistry::new();
        for (name, ext) in [
            ("rust", "rs"),
            ("python", "py"),
            ("json", "json"),
            ("json", "jsonl"),
            ("toml", "toml"),
            ("yaml", "yaml"),
            ("yaml", "yml"),
            ("markdown", "md"),
            ("hcl", "tf"),
            ("hcl", "tfvars"),
            ("hcl", "hcl"),
            ("csv", "csv"),
            ("csv", "tsv"),
        ] {
            assert_eq!(
                r.by_extension(ext).map(|c| c.name.as_str()),
                Some(name),
                "extension .{ext} should map to {name}"
            );
        }
    }

    #[test]
    fn detect_by_extension() {
        let r = LanguageRegistry::new();
        assert_eq!(
            r.detect("src/main.rs", None).map(|c| c.name.as_str()),
            Some("rust")
        );
        assert_eq!(
            r.detect("a/b/config.toml", None).map(|c| c.name.as_str()),
            Some("toml")
        );
        assert_eq!(
            r.detect("data.jsonc", None).map(|c| c.name.as_str()),
            Some("json")
        );
        assert_eq!(
            r.detect("events.jsonl", None).map(|c| c.name.as_str()),
            Some("json")
        );
        assert_eq!(
            r.detect("ci.yml", None).map(|c| c.name.as_str()),
            Some("yaml")
        );
        assert_eq!(
            r.detect("main.tf", None).map(|c| c.name.as_str()),
            Some("hcl")
        );
        assert_eq!(
            r.detect("README.md", None).map(|c| c.name.as_str()),
            Some("markdown")
        );
        assert_eq!(
            r.detect("data.csv", None).map(|c| c.name.as_str()),
            Some("csv")
        );
    }

    #[test]
    fn detect_unknown_extension_is_none() {
        let r = LanguageRegistry::new();
        assert!(r.detect("notes.xyz", None).is_none());
        assert!(r.detect("LICENSE", None).is_none());
    }

    #[test]
    fn detect_by_exact_filename() {
        let r = LanguageRegistry::new();
        for path in ["Makefile", "src/Makefile", "/a/b/GNUmakefile", "makefile"] {
            assert_eq!(
                r.detect(path, None).map(|c| c.name.as_str()),
                Some("make"),
                "{path} should detect as make"
            );
        }
        // .mk extension also maps to make.
        assert_eq!(
            r.detect("rules.mk", None).map(|c| c.name.as_str()),
            Some("make")
        );
    }

    #[test]
    fn detect_new_languages_by_extension() {
        let r = LanguageRegistry::new();
        for (path, name) in [
            ("build.sh", "bash"),
            ("main.go", "go"),
            ("app.js", "javascript"),
            ("app.tsx", "typescript"),
            ("index.html", "html"),
            ("style.css", "css"),
        ] {
            assert_eq!(
                r.detect(path, None).map(|c| c.name.as_str()),
                Some(name),
                "{path} should detect as {name}"
            );
        }
    }

    #[test]
    fn detect_shell_by_shebang() {
        let r = LanguageRegistry::new();
        let got = r.detect("configure", Some("#!/bin/bash"));
        assert_eq!(got.map(|c| c.name.as_str()), Some("bash"));
        let got = r.detect("run", Some("#!/usr/bin/env zsh"));
        assert_eq!(got.map(|c| c.name.as_str()), Some("bash"));
    }

    #[test]
    fn detect_by_shebang_when_no_extension() {
        let r = LanguageRegistry::new();
        let got = r.detect("script", Some("#!/usr/bin/env python3"));
        assert_eq!(got.map(|c| c.name.as_str()), Some("python"));
        let got = r.detect("script", Some("#!/usr/bin/python"));
        assert_eq!(got.map(|c| c.name.as_str()), Some("python"));
    }

    #[test]
    fn extension_takes_priority_over_shebang() {
        let r = LanguageRegistry::new();
        // .rs extension wins even though the shebang says python.
        let got = r.detect("weird.rs", Some("#!/usr/bin/env python3"));
        assert_eq!(got.map(|c| c.name.as_str()), Some("rust"));
    }

    #[test]
    fn parse_shebang_variants() {
        assert_eq!(parse_shebang("#!/usr/bin/env python3"), Some("python3"));
        assert_eq!(parse_shebang("#!/usr/bin/python"), Some("python"));
        assert_eq!(parse_shebang("#!/bin/bash"), Some("bash"));
        assert_eq!(parse_shebang("not a shebang"), None);
        assert_eq!(parse_shebang("#!"), None);
    }

    #[test]
    fn language_config_carries_comment_and_indent() {
        let r = LanguageRegistry::new();
        let rust = r.by_name("rust").unwrap();
        assert_eq!(rust.comment_token, "//");
        assert_eq!(rust.indent_width, 4);
        let json = r.by_name("json").unwrap();
        assert_eq!(json.indent_width, 2);
    }
}
