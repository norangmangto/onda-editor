use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Modifier bitmask — a simplified wrapper around crossterm modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyMod(u8);

impl KeyMod {
    pub const NONE: Self = Self(0);
    pub const CTRL: Self = Self(0b0001);
    pub const ALT: Self = Self(0b0010);
    pub const SHIFT: Self = Self(0b0100);

    pub fn contains(self, other: KeyMod) -> bool {
        self.0 & other.0 != 0
    }
}

impl From<KeyModifiers> for KeyMod {
    fn from(mods: KeyModifiers) -> Self {
        let mut m = 0u8;
        if mods.contains(KeyModifiers::CONTROL) {
            m |= Self::CTRL.0;
        }
        if mods.contains(KeyModifiers::ALT) {
            m |= Self::ALT.0;
        }
        if mods.contains(KeyModifiers::SHIFT) {
            m |= Self::SHIFT.0;
        }
        Self(m)
    }
}

/// Normalised key representation used in keymaps.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Key {
    /// A printable character (with optional modifier).
    Char(char, KeyMod),
    /// A special key.
    Esc,
    Enter,
    Backspace,
    Delete,
    Tab,
    /// Shift-Tab (back-tab).
    BackTab,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    F(u8),
}

impl Key {
    /// Parse a crossterm [`KeyEvent`] into a [`Key`].
    pub fn from_event(ev: &KeyEvent) -> Self {
        let mods = KeyMod::from(ev.modifiers);
        match ev.code {
            KeyCode::Char(c) => Key::Char(c, mods),
            KeyCode::Esc => Key::Esc,
            KeyCode::Enter => Key::Enter,
            KeyCode::Backspace => Key::Backspace,
            KeyCode::Delete => Key::Delete,
            KeyCode::Tab => Key::Tab,
            KeyCode::BackTab => Key::BackTab,
            KeyCode::Left => Key::Left,
            KeyCode::Right => Key::Right,
            KeyCode::Up => Key::Up,
            KeyCode::Down => Key::Down,
            KeyCode::Home => Key::Home,
            KeyCode::End => Key::End,
            KeyCode::PageUp => Key::PageUp,
            KeyCode::PageDown => Key::PageDown,
            KeyCode::F(n) => Key::F(n),
            _ => Key::Char('\0', mods),
        }
    }

    /// Convenience: `Ctrl-<char>`.
    pub fn ctrl(c: char) -> Self {
        Key::Char(c, KeyMod::CTRL)
    }

    pub fn char(c: char) -> Self {
        Key::Char(c, KeyMod::NONE)
    }
}

impl std::fmt::Display for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Key::Char(c, m) if m.contains(KeyMod::CTRL) => write!(f, "<C-{c}>"),
            Key::Char(c, m) if m.contains(KeyMod::ALT) => write!(f, "<A-{c}>"),
            Key::Char(c, _) => write!(f, "{c}"),
            Key::Esc => write!(f, "<Esc>"),
            Key::Enter => write!(f, "<CR>"),
            Key::Backspace => write!(f, "<BS>"),
            Key::Delete => write!(f, "<Del>"),
            Key::Tab => write!(f, "<Tab>"),
            Key::BackTab => write!(f, "<S-Tab>"),
            Key::Left => write!(f, "<Left>"),
            Key::Right => write!(f, "<Right>"),
            Key::Up => write!(f, "<Up>"),
            Key::Down => write!(f, "<Down>"),
            Key::Home => write!(f, "<Home>"),
            Key::End => write!(f, "<End>"),
            Key::PageUp => write!(f, "<PageUp>"),
            Key::PageDown => write!(f, "<PageDown>"),
            Key::F(n) => write!(f, "<F{n}>"),
        }
    }
}
