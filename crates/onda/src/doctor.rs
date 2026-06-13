//! `onda doctor` — environment diagnosis (T30.2).
//!
//! Reports terminal capabilities, bundled grammars, language servers, search and
//! clipboard tools, and config parse status, each with an actionable fix-it hint on
//! failure. The check *logic* is pure (inputs injected) so it's unit-testable; `run`
//! gathers the real environment and prints the report.

use std::path::PathBuf;

/// Outcome of a single check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Warn,
    Fail,
}

impl Status {
    fn glyph(self) -> &'static str {
        match self {
            Status::Ok => "✓",
            Status::Warn => "!",
            Status::Fail => "✗",
        }
    }
}

/// One diagnostic line.
#[derive(Debug, Clone)]
pub struct Check {
    pub name: String,
    pub status: Status,
    pub detail: String,
    pub hint: Option<String>,
}

impl Check {
    fn new(name: &str, status: Status, detail: impl Into<String>) -> Self {
        Self {
            name: name.to_string(),
            status,
            detail: detail.into(),
            hint: None,
        }
    }
    fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

/// Locate `program` on `PATH` (uses the real `PATH` env).
pub fn which(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(program);
        if candidate.is_file() {
            return Some(candidate);
        }
        // Windows-style would also try extensions; onda targets unix for v0.1.
    }
    None
}

/// True-color support from `COLORTERM`/`TERM` (pure; inputs injected for tests).
pub fn check_true_color(colorterm: Option<&str>, term: Option<&str>) -> Check {
    let truecolor = matches!(colorterm, Some(c) if c.contains("truecolor") || c.contains("24bit"));
    if truecolor {
        Check::new(
            "true color",
            Status::Ok,
            "COLORTERM advertises 24-bit color",
        )
    } else if matches!(term, Some(t) if t.contains("256color")) {
        Check::new("true color", Status::Warn, "only 256-color detected")
            .with_hint("set COLORTERM=truecolor in your shell for full theme fidelity")
    } else {
        Check::new(
            "true color",
            Status::Warn,
            format!("undetected (TERM={})", term.unwrap_or("unset")),
        )
        .with_hint("use a true-color terminal and export COLORTERM=truecolor")
    }
}

/// Check a required/optional external program by name.
pub fn check_program(label: &str, program: &str, required: bool, hint: &str) -> Check {
    match which(program) {
        Some(p) => Check::new(label, Status::Ok, p.display().to_string()),
        None => {
            let status = if required { Status::Fail } else { Status::Warn };
            Check::new(label, status, format!("`{program}` not found on PATH")).with_hint(hint)
        }
    }
}

/// Report bundled tree-sitter grammars (statically compiled into onda).
pub fn check_grammars(langs: &[&str]) -> Check {
    Check::new(
        "grammars",
        Status::Ok,
        format!("bundled: {}", langs.join(", ")),
    )
}

/// Turn a config-load warning into a check (None warning = clean parse).
pub fn check_config(warning: Option<&str>) -> Check {
    match warning {
        None => Check::new("config", Status::Ok, "parsed cleanly (or using defaults)"),
        Some(w) => Check::new("config", Status::Warn, w.to_string())
            .with_hint("fix the reported config.toml error; defaults are in use until then"),
    }
}

/// The first available clipboard provider for the platform, or a Fail.
pub fn check_clipboard(providers: &[&str]) -> Check {
    for p in providers {
        if which(p).is_some() {
            return Check::new("clipboard", Status::Ok, format!("using `{p}`"));
        }
    }
    Check::new("clipboard", Status::Warn, "no clipboard provider found")
        .with_hint("install one of: pbcopy (macOS), wl-clipboard or xclip/xsel (Linux)")
}

/// Default clipboard providers to probe, by platform.
fn clipboard_candidates() -> Vec<&'static str> {
    if cfg!(target_os = "macos") {
        vec!["pbcopy"]
    } else {
        vec!["wl-copy", "xclip", "xsel"]
    }
}

/// Assemble all checks from the real environment.
pub fn gather() -> Vec<Check> {
    let colorterm = std::env::var("COLORTERM").ok();
    let term = std::env::var("TERM").ok();

    let mut checks = vec![
        check_true_color(colorterm.as_deref(), term.as_deref()),
        check_grammars(&["rust", "python", "go", "typescript", "c"]),
        check_program(
            "ripgrep",
            "rg",
            false,
            "install ripgrep for fast workspace search",
        ),
        check_program(
            "lsp: rust-analyzer",
            "rust-analyzer",
            false,
            "rustup component add rust-analyzer",
        ),
        check_program(
            "lsp: gopls",
            "gopls",
            false,
            "go install golang.org/x/tools/gopls@latest",
        ),
        check_program(
            "lsp: typescript",
            "typescript-language-server",
            false,
            "npm i -g typescript-language-server typescript",
        ),
        check_program(
            "lsp: clangd",
            "clangd",
            false,
            "install your distro's clangd package",
        ),
        check_clipboard(&clipboard_candidates()),
    ];

    let cfg = onda_config::Config::load();
    checks.push(check_config(cfg.warning.as_deref()));
    checks
}

/// Run `onda doctor`: print the report, return a process exit code (non-zero on Fail).
pub fn run() -> i32 {
    let checks = gather();
    println!("onda doctor — environment report\n");
    let mut failures = 0;
    for c in &checks {
        println!("  {} {:<22} {}", c.status.glyph(), c.name, c.detail);
        if let Some(hint) = &c.hint {
            if c.status != Status::Ok {
                println!("      ↳ {hint}");
            }
        }
        if c.status == Status::Fail {
            failures += 1;
        }
    }
    println!();
    if failures == 0 {
        println!("All required checks passed.");
        0
    } else {
        println!("{failures} required check(s) failed — see hints above.");
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn true_color_detected() {
        let c = check_true_color(Some("truecolor"), Some("xterm-256color"));
        assert_eq!(c.status, Status::Ok);
    }

    #[test]
    fn only_256_color_warns() {
        let c = check_true_color(None, Some("xterm-256color"));
        assert_eq!(c.status, Status::Warn);
        assert!(c.hint.is_some());
    }

    #[test]
    fn no_color_info_warns() {
        let c = check_true_color(None, None);
        assert_eq!(c.status, Status::Warn);
    }

    #[test]
    fn missing_optional_program_warns() {
        let c = check_program("tool", "definitely-not-a-real-binary-xyz", false, "hint");
        assert_eq!(c.status, Status::Warn);
    }

    #[test]
    fn missing_required_program_fails() {
        let c = check_program("tool", "definitely-not-a-real-binary-xyz", true, "hint");
        assert_eq!(c.status, Status::Fail);
    }

    #[test]
    fn present_program_ok() {
        // `cargo` is guaranteed present in the test environment.
        let c = check_program("cargo", "cargo", true, "install rust");
        assert_eq!(c.status, Status::Ok);
    }

    #[test]
    fn config_clean_vs_warning() {
        assert_eq!(check_config(None).status, Status::Ok);
        assert_eq!(
            check_config(Some("bad toml at line 3")).status,
            Status::Warn
        );
    }

    #[test]
    fn grammars_listed() {
        let c = check_grammars(&["rust", "go"]);
        assert_eq!(c.status, Status::Ok);
        assert!(c.detail.contains("rust"));
    }

    #[test]
    fn clipboard_none_warns() {
        let c = check_clipboard(&["definitely-not-a-real-binary-xyz"]);
        assert_eq!(c.status, Status::Warn);
    }

    #[test]
    fn gather_runs_without_panic() {
        // Smoke: gathering from the real environment produces checks.
        assert!(!gather().is_empty());
    }
}
