/// Editor modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Mode {
    #[default]
    Normal,
    Insert,
    /// Character-wise visual selection.
    Visual,
    /// Line-wise visual selection.
    VisualLine,
    /// Block-wise visual selection (Ctrl-v).
    VisualBlock,
    /// Command-line (`:`) mode.
    Command,
}

impl Mode {
    pub fn is_visual(self) -> bool {
        matches!(self, Mode::Visual | Mode::VisualLine | Mode::VisualBlock)
    }

    pub fn is_insert(self) -> bool {
        self == Mode::Insert
    }

    pub fn is_normal(self) -> bool {
        self == Mode::Normal
    }
}
