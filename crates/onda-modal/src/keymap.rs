use std::collections::HashMap;

use crate::{key::Key, mode::Mode, motion::Motion, operator::Operator};

/// All actions that can be triggered by a keymap entry.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    // Mode transitions
    EnterInsert,
    EnterInsertLineEnd,
    EnterInsertLineStart,
    EnterInsertAfter,
    EnterInsertNewLineBelow,
    EnterInsertNewLineAbove,
    EnterNormal,
    EnterVisual,
    EnterVisualLine,
    EnterCommand,

    // Motion (move cursor, no edit)
    Move(Motion),

    // Operator + motion composed (e.g. dw, c3j)
    ApplyOperatorMotion(Operator, Motion),

    // Pending operator (waits for a motion key)
    PendingOperator(Operator),

    // Operator applied to a whole line (dd / yy / cc)
    OperatorLine(Operator),

    // Operator applied to the current visual selection
    OperatorSelection(Operator),

    // Immediate edits
    DeleteChar,
    /// Carries the replacement char encoded as `usize` (char as u32 cast)
    ReplaceChar(char),
    ChangeToEnd,
    DeleteToEnd,
    PasteAfter,
    PasteBefore,
    JoinLine,

    // Undo/redo
    Undo,
    Redo,

    // Visual mode
    SwapAnchorHead,

    // Command-line operations (dispatched to app)
    WriteFile,
    Quit,
    QuitForce,
    WriteQuit,
    EditFile(String),
    NextBuffer,
    PrevBuffer,
}

// ── Keymap trie ───────────────────────────────────────────────────────────────

enum KeymapNode {
    Leaf(Action),
    Node(HashMap<Key, KeymapNode>),
}

pub struct Keymap {
    normal: HashMap<Key, KeymapNode>,
    visual: HashMap<Key, KeymapNode>,
}

impl Keymap {
    fn build_normal() -> HashMap<Key, KeymapNode> {
        let mut m: HashMap<Key, KeymapNode> = HashMap::new();

        macro_rules! leaf {
            ($k:expr, $a:expr) => {
                m.insert($k, KeymapNode::Leaf($a))
            };
        }

        // Mode transitions
        leaf!(Key::char('i'), Action::EnterInsert);
        leaf!(Key::char('I'), Action::EnterInsertLineStart);
        leaf!(Key::char('a'), Action::EnterInsertAfter);
        leaf!(Key::char('A'), Action::EnterInsertLineEnd);
        leaf!(Key::char('o'), Action::EnterInsertNewLineBelow);
        leaf!(Key::char('O'), Action::EnterInsertNewLineAbove);
        leaf!(Key::char('v'), Action::EnterVisual);
        leaf!(Key::char('V'), Action::EnterVisualLine);
        leaf!(Key::char(':'), Action::EnterCommand);

        // Motions
        leaf!(Key::char('h'), Action::Move(Motion::Left));
        leaf!(Key::Left, Action::Move(Motion::Left));
        leaf!(Key::char('l'), Action::Move(Motion::Right));
        leaf!(Key::Right, Action::Move(Motion::Right));
        leaf!(Key::char('j'), Action::Move(Motion::Down));
        leaf!(Key::Down, Action::Move(Motion::Down));
        leaf!(Key::char('k'), Action::Move(Motion::Up));
        leaf!(Key::Up, Action::Move(Motion::Up));
        leaf!(Key::char('w'), Action::Move(Motion::WordForward));
        leaf!(Key::char('b'), Action::Move(Motion::WordBackward));
        leaf!(Key::char('e'), Action::Move(Motion::WordEnd));
        leaf!(Key::char('W'), Action::Move(Motion::BigWordForward));
        leaf!(Key::char('B'), Action::Move(Motion::BigWordBackward));
        leaf!(Key::char('E'), Action::Move(Motion::BigWordEnd));
        leaf!(Key::char('0'), Action::Move(Motion::LineStart));
        leaf!(Key::char('^'), Action::Move(Motion::LineFirstNonBlank));
        leaf!(Key::char('$'), Action::Move(Motion::LineEnd));
        leaf!(Key::char('{'), Action::Move(Motion::ParagraphBackward));
        leaf!(Key::char('}'), Action::Move(Motion::ParagraphForward));
        leaf!(Key::ctrl('d'), Action::Move(Motion::HalfPageDown));
        leaf!(Key::ctrl('u'), Action::Move(Motion::HalfPageUp));
        leaf!(Key::PageDown, Action::Move(Motion::HalfPageDown));
        leaf!(Key::PageUp, Action::Move(Motion::HalfPageUp));
        leaf!(Key::char('G'), Action::Move(Motion::DocumentEnd));

        // 'g' prefix → 'gg'
        {
            let mut g: HashMap<Key, KeymapNode> = HashMap::new();
            g.insert(Key::char('g'), KeymapNode::Leaf(Action::Move(Motion::DocumentStart)));
            m.insert(Key::char('g'), KeymapNode::Node(g));
        }

        // Operator prefixes: d/c/y trigger PendingOperator.
        // Doubling (dd/cc/yy) is handled in KeymapState::process via the pending_operator path.
        leaf!(Key::char('d'), Action::PendingOperator(Operator::Delete));
        leaf!(Key::char('c'), Action::PendingOperator(Operator::Change));
        leaf!(Key::char('y'), Action::PendingOperator(Operator::Yank));

        // Immediate edits
        leaf!(Key::char('x'), Action::DeleteChar);
        leaf!(Key::Delete, Action::DeleteChar);
        leaf!(Key::char('D'), Action::DeleteToEnd);
        leaf!(Key::char('C'), Action::ChangeToEnd);
        leaf!(Key::char('p'), Action::PasteAfter);
        leaf!(Key::char('P'), Action::PasteBefore);
        leaf!(Key::char('J'), Action::JoinLine);

        // Undo/redo
        leaf!(Key::char('u'), Action::Undo);
        leaf!(Key::ctrl('r'), Action::Redo);

        m
    }

    fn build_visual() -> HashMap<Key, KeymapNode> {
        let mut m = Self::build_normal();
        // In visual mode operators apply to selection
        for (ch, op) in [('d', Operator::Delete), ('c', Operator::Change), ('y', Operator::Yank)] {
            m.insert(Key::char(ch), KeymapNode::Leaf(Action::OperatorSelection(op)));
        }
        m.insert(Key::char('o'), KeymapNode::Leaf(Action::SwapAnchorHead));
        m.insert(Key::Esc, KeymapNode::Leaf(Action::EnterNormal));
        m
    }

    pub fn new() -> Self {
        Self { normal: Self::build_normal(), visual: Self::build_visual() }
    }

    fn root(&self, mode: Mode) -> Option<&HashMap<Key, KeymapNode>> {
        match mode {
            Mode::Normal => Some(&self.normal),
            Mode::Visual | Mode::VisualLine => Some(&self.visual),
            Mode::Insert | Mode::Command => None,
        }
    }
}

impl Default for Keymap {
    fn default() -> Self {
        Self::new()
    }
}

// ── KeymapState ───────────────────────────────────────────────────────────────

/// Tracks in-progress multi-key sequence state.
#[derive(Debug, Default)]
pub struct KeymapState {
    /// Accumulated prefix keys (for trie navigation).
    pending_keys: Vec<Key>,
    /// Accumulated count digits.
    pub count: Option<usize>,
    /// Pending operator waiting for a motion.
    pub pending_operator: Option<Operator>,
    /// Pending find-char kind (f/t/F/T), waiting for the target char.
    pending_find: Option<FindKind>,
    /// Pending 'r' replace, waiting for the replacement char.
    pending_replace: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindKind {
    Find,
    Till,
    FindBack,
    TillBack,
}

/// Result of processing a single key event.
#[derive(Debug)]
pub enum PendingResult {
    Action(Action, usize),
    NeedMore,
    NoMatch,
}

impl KeymapState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&mut self) {
        self.pending_keys.clear();
        self.count = None;
        self.pending_operator = None;
        self.pending_find = None;
        self.pending_replace = false;
    }

    /// Process one key in the given mode. Returns what should happen.
    pub fn process(&mut self, key: &Key, mode: Mode, keymap: &Keymap) -> PendingResult {
        // Esc: cancel everything and return to normal
        if *key == Key::Esc && mode != Mode::Insert {
            let was_pending = self.pending_operator.is_some() || !self.pending_keys.is_empty();
            self.reset();
            if was_pending {
                return PendingResult::NeedMore; // just clear
            }
            return PendingResult::Action(Action::EnterNormal, 1);
        }

        // In insert/command mode we don't use the trie
        if matches!(mode, Mode::Insert | Mode::Command) {
            return PendingResult::NoMatch;
        }

        // Handle pending 'r' (replace char)
        if self.pending_replace {
            self.pending_replace = false;
            let count = self.count.unwrap_or(1);
            self.count = None;
            if let Key::Char(c, _) = key {
                return PendingResult::Action(Action::ReplaceChar(*c), count);
            }
            return PendingResult::NoMatch;
        }

        // Handle pending f/t/F/T
        if let Some(find_kind) = self.pending_find.take() {
            let count = self.count.unwrap_or(1);
            if let Key::Char(c, _) = key {
                let motion = match find_kind {
                    FindKind::Find => Motion::FindChar(*c),
                    FindKind::Till => Motion::TillChar(*c),
                    FindKind::FindBack => Motion::FindCharBack(*c),
                    FindKind::TillBack => Motion::TillCharBack(*c),
                };
                if let Some(op) = self.pending_operator.take() {
                    self.count = None;
                    return PendingResult::Action(Action::ApplyOperatorMotion(op, motion), count);
                }
                self.count = None;
                return PendingResult::Action(Action::Move(motion), count);
            }
            self.reset();
            return PendingResult::NoMatch;
        }

        // Count digits — '0' only counts if a digit was already seen
        if let Key::Char(c, _) = key {
            if c.is_ascii_digit() && (*c != '0' || self.count.is_some()) {
                let d = (*c as u8 - b'0') as usize;
                self.count = Some(self.count.unwrap_or(0) * 10 + d);
                return PendingResult::NeedMore;
            }
        }

        let count = self.count.unwrap_or(1);

        // Handle pending operator waiting for motion
        if let Some(op) = self.pending_operator {
            // f/t/F/T — need another char
            match key {
                Key::Char('f', _) => {
                    self.pending_find = Some(FindKind::Find);
                    return PendingResult::NeedMore;
                }
                Key::Char('t', _) => {
                    self.pending_find = Some(FindKind::Till);
                    return PendingResult::NeedMore;
                }
                Key::Char('F', _) => {
                    self.pending_find = Some(FindKind::FindBack);
                    return PendingResult::NeedMore;
                }
                Key::Char('T', _) => {
                    self.pending_find = Some(FindKind::TillBack);
                    return PendingResult::NeedMore;
                }
                _ => {}
            }

            // Operator doubling (dd, cc, yy)
            let is_double = match (op, key) {
                (Operator::Delete, Key::Char('d', _)) => true,
                (Operator::Change, Key::Char('c', _)) => true,
                (Operator::Yank, Key::Char('y', _)) => true,
                _ => false,
            };
            if is_double {
                self.pending_operator = None;
                self.count = None;
                return PendingResult::Action(Action::OperatorLine(op), count);
            }

            // Map key to motion
            if let Some(motion) = key_to_motion(key) {
                self.pending_operator = None;
                self.count = None;
                return PendingResult::Action(Action::ApplyOperatorMotion(op, motion), count);
            }

            self.reset();
            return PendingResult::NoMatch;
        }

        // f/t/F/T in motion context
        match key {
            Key::Char('f', _) => {
                self.pending_find = Some(FindKind::Find);
                return PendingResult::NeedMore;
            }
            Key::Char('t', _) => {
                self.pending_find = Some(FindKind::Till);
                return PendingResult::NeedMore;
            }
            Key::Char('F', _) => {
                self.pending_find = Some(FindKind::FindBack);
                return PendingResult::NeedMore;
            }
            Key::Char('T', _) => {
                self.pending_find = Some(FindKind::TillBack);
                return PendingResult::NeedMore;
            }
            Key::Char('r', _) => {
                self.pending_replace = true;
                return PendingResult::NeedMore;
            }
            _ => {}
        }

        // Trie lookup
        self.pending_keys.push(key.clone());
        let root = match keymap.root(mode) {
            Some(r) => r,
            None => {
                self.reset();
                return PendingResult::NoMatch;
            }
        };

        let mut current = root;
        let mut last_action: Option<&Action> = None;

        for k in &self.pending_keys {
            match current.get(k) {
                Some(KeymapNode::Leaf(a)) => {
                    last_action = Some(a);
                    break;
                }
                Some(KeymapNode::Node(sub)) => {
                    current = sub;
                }
                None => {
                    self.reset();
                    return PendingResult::NoMatch;
                }
            }
        }

        if let Some(action) = last_action {
            let action = action.clone();
            self.pending_keys.clear();
            self.count = None;

            // Operator prefix: set pending and wait for motion
            if let Action::PendingOperator(op) = action {
                self.pending_operator = Some(op);
                return PendingResult::NeedMore;
            }

            return PendingResult::Action(action, count);
        }

        // Still in a node — more keys expected
        PendingResult::NeedMore
    }

    pub fn has_pending(&self) -> bool {
        self.pending_operator.is_some()
            || !self.pending_keys.is_empty()
            || self.pending_find.is_some()
            || self.pending_replace
    }
}

fn key_to_motion(key: &Key) -> Option<Motion> {
    match key {
        Key::Char('h', _) | Key::Left => Some(Motion::Left),
        Key::Char('l', _) | Key::Right => Some(Motion::Right),
        Key::Char('j', _) | Key::Down => Some(Motion::Down),
        Key::Char('k', _) | Key::Up => Some(Motion::Up),
        Key::Char('w', _) => Some(Motion::WordForward),
        Key::Char('b', _) => Some(Motion::WordBackward),
        Key::Char('e', _) => Some(Motion::WordEnd),
        Key::Char('W', _) => Some(Motion::BigWordForward),
        Key::Char('B', _) => Some(Motion::BigWordBackward),
        Key::Char('E', _) => Some(Motion::BigWordEnd),
        Key::Char('0', _) => Some(Motion::LineStart),
        Key::Char('^', _) => Some(Motion::LineFirstNonBlank),
        Key::Char('$', _) => Some(Motion::LineEnd),
        Key::Char('{', _) => Some(Motion::ParagraphBackward),
        Key::Char('}', _) => Some(Motion::ParagraphForward),
        Key::Char('G', _) => Some(Motion::DocumentEnd),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> (KeymapState, Keymap) {
        (KeymapState::new(), Keymap::new())
    }

    #[test]
    fn single_key_action() {
        let (mut st, km) = state();
        match st.process(&Key::char('h'), Mode::Normal, &km) {
            PendingResult::Action(Action::Move(Motion::Left), 1) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn count_then_motion() {
        let (mut st, km) = state();
        assert!(matches!(st.process(&Key::char('3'), Mode::Normal, &km), PendingResult::NeedMore));
        assert!(matches!(st.process(&Key::char('w'), Mode::Normal, &km),
            PendingResult::Action(Action::Move(Motion::WordForward), 3)));
    }

    #[test]
    fn gg_sequence() {
        let (mut st, km) = state();
        assert!(matches!(st.process(&Key::char('g'), Mode::Normal, &km), PendingResult::NeedMore));
        assert!(matches!(st.process(&Key::char('g'), Mode::Normal, &km),
            PendingResult::Action(Action::Move(Motion::DocumentStart), 1)));
    }

    #[test]
    fn dd_linewise_delete() {
        let (mut st, km) = state();
        assert!(matches!(st.process(&Key::char('d'), Mode::Normal, &km), PendingResult::NeedMore));
        assert!(matches!(st.process(&Key::char('d'), Mode::Normal, &km),
            PendingResult::Action(Action::OperatorLine(Operator::Delete), 1)));
    }

    #[test]
    fn dw_composed() {
        let (mut st, km) = state();
        assert!(matches!(st.process(&Key::char('d'), Mode::Normal, &km), PendingResult::NeedMore));
        assert!(matches!(st.process(&Key::char('w'), Mode::Normal, &km),
            PendingResult::Action(Action::ApplyOperatorMotion(Operator::Delete, Motion::WordForward), 1)));
    }

    #[test]
    fn esc_cancels_pending() {
        let (mut st, km) = state();
        st.process(&Key::char('d'), Mode::Normal, &km);
        st.process(&Key::Esc, Mode::Normal, &km);
        assert!(!st.has_pending());
    }

    #[test]
    fn find_char_sequence() {
        let (mut st, km) = state();
        assert!(matches!(st.process(&Key::char('f'), Mode::Normal, &km), PendingResult::NeedMore));
        assert!(matches!(st.process(&Key::char('x'), Mode::Normal, &km),
            PendingResult::Action(Action::Move(Motion::FindChar('x')), 1)));
    }

    #[test]
    fn replace_char_sequence() {
        let (mut st, km) = state();
        assert!(matches!(st.process(&Key::char('r'), Mode::Normal, &km), PendingResult::NeedMore));
        assert!(matches!(st.process(&Key::char('z'), Mode::Normal, &km),
            PendingResult::Action(Action::ReplaceChar('z'), 1)));
    }
}
