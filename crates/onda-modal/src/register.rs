/// The shape of the yanked/deleted text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RegisterKind {
    #[default]
    Charwise,
    Linewise,
    Blockwise,
}

/// A single register entry.
#[derive(Debug, Clone, Default)]
pub struct Register {
    pub text: String,
    pub kind: RegisterKind,
}

impl Register {
    pub fn new(text: String, kind: RegisterKind) -> Self {
        Self { text, kind }
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
}

/// The full register bank: 26 named (a-z), 10 numbered (0-9), and an unnamed register.
#[derive(Debug)]
pub struct RegisterBank {
    /// Named registers a-z (index 0-25).
    named: [Register; 26],
    /// Numbered registers 0-9 (index 0-9).
    numbered: [Register; 10],
    /// The unnamed (`"`) register.
    unnamed: Register,
}

impl Default for RegisterBank {
    fn default() -> Self {
        Self::new()
    }
}

impl RegisterBank {
    pub fn new() -> Self {
        Self {
            named: std::array::from_fn(|_| Register::default()),
            numbered: std::array::from_fn(|_| Register::default()),
            unnamed: Register::default(),
        }
    }

    /// Retrieve a register by its sigil character.
    ///
    /// | Character  | Register            |
    /// |------------|---------------------|
    /// | `a`–`z`    | named[0..25]        |
    /// | `A`–`Z`    | named[0..25] (same slot, uppercase = append) |
    /// | `0`–`9`    | numbered[0..9]      |
    /// | `"`        | unnamed             |
    /// | `_`        | black-hole (None)   |
    pub fn get(&self, name: char) -> Option<&Register> {
        match name {
            'a'..='z' => Some(&self.named[name as usize - 'a' as usize]),
            'A'..='Z' => Some(&self.named[name as usize - 'A' as usize]),
            '0'..='9' => Some(&self.numbered[name as usize - '0' as usize]),
            '"' => Some(&self.unnamed),
            '_' => None,
            _ => None,
        }
    }

    /// Write to a register.
    ///
    /// Uppercase names append text to the existing content of the corresponding
    /// lowercase slot; all other names overwrite.
    pub fn set(&mut self, name: char, reg: Register) {
        match name {
            'a'..='z' => {
                self.named[name as usize - 'a' as usize] = reg;
            }
            'A'..='Z' => {
                let slot = &mut self.named[name as usize - 'A' as usize];
                slot.text.push_str(&reg.text);
                slot.kind = reg.kind;
            }
            '0'..='9' => {
                self.numbered[name as usize - '0' as usize] = reg;
            }
            '"' => {
                self.unnamed = reg;
            }
            // black-hole and unknown: discard
            _ => {}
        }
    }

    /// Record a deletion: shift numbered[1..8] → [2..9], store in numbered[1] and unnamed.
    pub fn push_delete(&mut self, text: String, kind: RegisterKind) {
        // Shift 8 → 9, 7 → 8, …, 1 → 2
        for i in (2..=9usize).rev() {
            let prev = std::mem::take(&mut self.numbered[i - 1]);
            self.numbered[i] = prev;
        }
        let reg = Register::new(text, kind);
        self.numbered[1] = reg.clone();
        self.unnamed = reg;
    }

    /// Record a yank: store in numbered[0] and unnamed.
    pub fn push_yank(&mut self, text: String, kind: RegisterKind) {
        let reg = Register::new(text, kind);
        self.numbered[0] = reg.clone();
        self.unnamed = reg;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_named() {
        let mut bank = RegisterBank::new();
        bank.set('a', Register::new("hello".into(), RegisterKind::Charwise));
        assert_eq!(bank.get('a').unwrap().text, "hello");
    }

    #[test]
    fn uppercase_appends() {
        let mut bank = RegisterBank::new();
        bank.set('a', Register::new("foo".into(), RegisterKind::Charwise));
        bank.set('A', Register::new("bar".into(), RegisterKind::Charwise));
        assert_eq!(bank.get('a').unwrap().text, "foobar");
    }

    #[test]
    fn black_hole_returns_none() {
        let bank = RegisterBank::new();
        assert!(bank.get('_').is_none());
    }

    #[test]
    fn push_delete_shifts() {
        let mut bank = RegisterBank::new();
        bank.set('1', Register::new("first".into(), RegisterKind::Charwise));
        bank.push_delete("second".into(), RegisterKind::Linewise);
        assert_eq!(bank.get('1').unwrap().text, "second");
        assert_eq!(bank.get('2').unwrap().text, "first");
        assert_eq!(bank.get('"').unwrap().text, "second");
    }

    #[test]
    fn push_yank_stores_in_zero_and_unnamed() {
        let mut bank = RegisterBank::new();
        bank.push_yank("yanked".into(), RegisterKind::Charwise);
        assert_eq!(bank.get('0').unwrap().text, "yanked");
        assert_eq!(bank.get('"').unwrap().text, "yanked");
    }
}
