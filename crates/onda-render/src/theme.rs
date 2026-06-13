//! Theme system (onda T18.1).
//!
//! A `Theme` maps scope names (`ui.text`, `ui.statusline.insert`, `syntax.keyword`,
//! `diff.add`, …) to [`Style`]s. Themes load from TOML and ship as three built-ins
//! (`onda-dark`, `onda-light`, `onda-contrast`). The renderer reads styles through the
//! typed accessor methods, each of which carries a sensible dark default so an
//! incomplete theme never leaves a surface unstyled. Plugins can override any group
//! via [`Theme::set`].

use std::collections::HashMap;

use serde::Deserialize;
use thiserror::Error;

use crate::grid::{Attribute, Color, Style};

/// Errors from theme parsing.
#[derive(Debug, Error)]
pub enum ThemeError {
    #[error("theme parse error: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("invalid color: {0}")]
    Color(String),
}

/// A resolved theme: scope name → style.
#[derive(Debug, Clone, Default)]
pub struct Theme {
    name: String,
    styles: HashMap<String, Style>,
}

/// Serde view of a single style entry in a theme TOML file.
#[derive(Debug, Deserialize)]
struct StyleSpec {
    fg: Option<String>,
    bg: Option<String>,
    #[serde(default)]
    bold: bool,
    #[serde(default)]
    italic: bool,
    #[serde(default)]
    underline: bool,
}

/// Built-in theme names, in display order.
pub const BUILTIN_THEMES: &[&str] = &["onda-dark", "onda-light", "onda-contrast"];

impl Theme {
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Look up a scope, falling back to `default` when the theme doesn't define it.
    pub fn get(&self, scope: &str, default: Style) -> Style {
        self.styles.get(scope).copied().unwrap_or(default)
    }

    /// Define or override a highlight group (used by the Lua `onda.highlight.set` API).
    pub fn set(&mut self, scope: impl Into<String>, style: Style) {
        self.styles.insert(scope.into(), style);
    }

    /// Define/override a highlight group from string color specs (the form the Lua
    /// `onda.highlight.set` API and theme TOML use). Colors are `#rrggbb` or names.
    pub fn set_parsed(
        &mut self,
        group: &str,
        fg: Option<&str>,
        bg: Option<&str>,
        bold: bool,
        italic: bool,
        underline: bool,
    ) -> Result<(), ThemeError> {
        let mut attrs = Attribute::empty();
        if bold {
            attrs |= Attribute::BOLD;
        }
        if italic {
            attrs |= Attribute::ITALIC;
        }
        if underline {
            attrs |= Attribute::UNDERLINE;
        }
        let fg = match fg {
            Some(s) => parse_color(s)?,
            None => Color::Reset,
        };
        let bg = match bg {
            Some(s) => parse_color(s)?,
            None => Color::Reset,
        };
        self.styles
            .insert(group.to_string(), Style { fg, bg, attrs });
        Ok(())
    }

    /// Parse a theme from TOML text. Keys are scope names; values are style tables:
    /// `"ui.text" = { fg = "#c0c0c0", bg = "#101010", bold = true }`.
    pub fn from_toml(name: &str, text: &str) -> Result<Theme, ThemeError> {
        let specs: HashMap<String, StyleSpec> = toml::from_str(text)?;
        let mut styles = HashMap::with_capacity(specs.len());
        for (scope, spec) in specs {
            let mut attrs = Attribute::empty();
            if spec.bold {
                attrs |= Attribute::BOLD;
            }
            if spec.italic {
                attrs |= Attribute::ITALIC;
            }
            if spec.underline {
                attrs |= Attribute::UNDERLINE;
            }
            let fg = match spec.fg {
                Some(s) => parse_color(&s)?,
                None => Color::Reset,
            };
            let bg = match spec.bg {
                Some(s) => parse_color(&s)?,
                None => Color::Reset,
            };
            styles.insert(scope, Style { fg, bg, attrs });
        }
        Ok(Theme {
            name: name.to_string(),
            styles,
        })
    }

    /// Return a built-in theme by name, or `None` if unknown.
    pub fn builtin(name: &str) -> Option<Theme> {
        let toml = match name {
            "onda-dark" => include_str!("../../../runtime/themes/onda-dark.toml"),
            "onda-light" => include_str!("../../../runtime/themes/onda-light.toml"),
            "onda-contrast" => include_str!("../../../runtime/themes/onda-contrast.toml"),
            _ => return None,
        };
        // INVARIANT: built-in theme TOML files are validated by the theme_builtins test.
        Theme::from_toml(name, toml).ok()
    }

    /// The default theme (`onda-dark`), used before any config is applied.
    pub fn default_dark() -> Theme {
        Theme::builtin("onda-dark").unwrap_or_else(|| Theme {
            name: "onda-dark".into(),
            styles: HashMap::new(),
        })
    }

    // ── Typed accessors (each with its dark default) ──────────────────────────

    pub fn text(&self) -> Style {
        self.get("ui.text", Style::RESET)
    }
    pub fn cursor_normal(&self) -> Style {
        self.get("ui.cursor", sty(Color::Black, Color::White))
    }
    pub fn cursor_insert(&self) -> Style {
        self.get("ui.cursor.insert", sty(Color::Black, Color::LightCyan))
    }
    pub fn selection(&self) -> Style {
        self.get("ui.selection", sty(Color::Black, Color::LightBlue))
    }
    pub fn line_nr(&self) -> Style {
        self.get("ui.linenr", fgsty(Color::DarkGray))
    }
    pub fn line_nr_current(&self) -> Style {
        self.get("ui.linenr.current", fgsty(Color::Yellow))
    }
    pub fn status_bg(&self) -> Style {
        self.get("ui.statusline", sty(Color::White, Color::DarkGray))
    }
    pub fn status_normal(&self) -> Style {
        self.get("ui.statusline.normal", sty(Color::Black, Color::Green))
    }
    pub fn status_insert(&self) -> Style {
        self.get("ui.statusline.insert", sty(Color::Black, Color::LightCyan))
    }
    pub fn status_visual(&self) -> Style {
        self.get("ui.statusline.visual", sty(Color::Black, Color::Yellow))
    }
    pub fn status_terminal(&self) -> Style {
        self.get(
            "ui.statusline.terminal",
            sty(Color::Black, Color::LightBlue),
        )
    }
    pub fn msg_error(&self) -> Style {
        self.get("ui.message.error", fgsty(Color::LightRed))
    }
    pub fn msg_info(&self) -> Style {
        self.get("ui.message.info", Style::RESET)
    }
    pub fn menu(&self) -> Style {
        self.get("ui.menu", sty(Color::White, Color::DarkGray))
    }
    pub fn menu_selected(&self) -> Style {
        self.get("ui.menu.selected", sty(Color::Black, Color::LightCyan))
    }
    pub fn float_bg(&self) -> Style {
        self.get("ui.float", sty(Color::White, Color::DarkGray))
    }
    pub fn float_border(&self) -> Style {
        self.get("ui.float.border", sty(Color::LightCyan, Color::DarkGray))
    }
    pub fn diag_error(&self) -> Style {
        self.get("diagnostic.error", underline(Color::LightRed))
    }
    pub fn diag_warning(&self) -> Style {
        self.get("diagnostic.warning", underline(Color::Yellow))
    }
    pub fn diag_info(&self) -> Style {
        self.get("diagnostic.info", underline(Color::LightCyan))
    }
    pub fn gutter_error(&self) -> Style {
        self.get("gutter.error", fgsty(Color::LightRed))
    }
    pub fn gutter_warning(&self) -> Style {
        self.get("gutter.warning", fgsty(Color::Yellow))
    }
    pub fn diff_add(&self) -> Style {
        self.get("diff.add", fgsty(Color::Green))
    }
    pub fn diff_delete(&self) -> Style {
        self.get("diff.delete", fgsty(Color::Red))
    }
    pub fn diff_change(&self) -> Style {
        self.get("diff.change", fgsty(Color::Yellow))
    }
}

fn sty(fg: Color, bg: Color) -> Style {
    Style {
        fg,
        bg,
        attrs: Attribute::empty(),
    }
}
fn fgsty(fg: Color) -> Style {
    sty(fg, Color::Reset)
}
fn underline(fg: Color) -> Style {
    Style {
        fg,
        bg: Color::Reset,
        attrs: Attribute::UNDERLINE,
    }
}

/// Parse a color string: `#rrggbb`, `reset`, or a basic ANSI name.
fn parse_color(s: &str) -> Result<Color, ThemeError> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix('#') {
        if hex.len() != 6 {
            return Err(ThemeError::Color(s.to_string()));
        }
        let r = u8::from_str_radix(&hex[0..2], 16).map_err(|_| ThemeError::Color(s.to_string()))?;
        let g = u8::from_str_radix(&hex[2..4], 16).map_err(|_| ThemeError::Color(s.to_string()))?;
        let b = u8::from_str_radix(&hex[4..6], 16).map_err(|_| ThemeError::Color(s.to_string()))?;
        return Ok(Color::Rgb(r, g, b));
    }
    Ok(match s.to_ascii_lowercase().as_str() {
        "reset" => Color::Reset,
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "white" => Color::White,
        "darkgray" | "darkgrey" => Color::DarkGray,
        "gray" | "grey" => Color::Gray,
        "lightred" => Color::LightRed,
        "lightgreen" => Color::LightGreen,
        "lightyellow" => Color::LightYellow,
        "lightblue" => Color::LightBlue,
        "lightmagenta" => Color::LightMagenta,
        "lightcyan" => Color::LightCyan,
        other => return Err(ThemeError::Color(other.to_string())),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hex_color() {
        assert_eq!(parse_color("#ff8800").unwrap(), Color::Rgb(255, 136, 0));
        assert_eq!(parse_color("lightcyan").unwrap(), Color::LightCyan);
        assert!(parse_color("#xyz").is_err());
        assert!(parse_color("notacolor").is_err());
    }

    #[test]
    fn from_toml_basic() {
        let toml = r##"
"ui.text" = { fg = "#c0c0c0", bg = "#101010" }
"syntax.keyword" = { fg = "#ff79c6", bold = true }
"##;
        let theme = Theme::from_toml("test", toml).unwrap();
        assert_eq!(theme.text().fg, Color::Rgb(0xc0, 0xc0, 0xc0));
        assert_eq!(theme.text().bg, Color::Rgb(0x10, 0x10, 0x10));
        let kw = theme.get("syntax.keyword", Style::RESET);
        assert!(kw.attrs.contains(Attribute::BOLD));
    }

    #[test]
    fn missing_scope_uses_default() {
        let theme = Theme::from_toml("empty", "").unwrap();
        // Default cursor is black-on-white.
        assert_eq!(theme.cursor_normal().bg, Color::White);
    }

    #[test]
    fn set_overrides_group() {
        let mut theme = Theme::default();
        theme.set("ui.text", sty(Color::Red, Color::Blue));
        assert_eq!(theme.text().fg, Color::Red);
    }

    #[test]
    fn theme_builtins_all_parse() {
        for name in BUILTIN_THEMES {
            let theme = Theme::builtin(name)
                .unwrap_or_else(|| panic!("builtin theme {name} failed to parse"));
            assert_eq!(theme.name(), *name);
            // Sanity: each built-in defines the core surfaces.
            assert!(theme.styles.contains_key("ui.statusline"));
        }
    }

    #[test]
    fn unknown_builtin_is_none() {
        assert!(Theme::builtin("no-such-theme").is_none());
    }
}
