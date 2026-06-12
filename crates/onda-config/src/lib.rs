/// Editor configuration (Phase 0 stub — all values are hardcoded defaults).
///
/// Phase 1 will load this from a TOML file.
#[derive(Debug, Clone)]
pub struct Config {
    pub scrolloff: usize,
    pub tab_width: usize,
    pub show_line_numbers: bool,
    pub relative_line_numbers: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            scrolloff: 5,
            tab_width: 4,
            show_line_numbers: true,
            relative_line_numbers: false,
        }
    }
}

impl Config {
    pub fn load() -> Self {
        Self::default()
    }
}
