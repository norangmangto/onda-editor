use std::collections::HashMap;

use crate::key::Key;

/// Records keystrokes for macro playback (`q` / `@`) and dot-repeat (`.`).
#[derive(Debug, Default)]
pub struct MacroRecorder {
    /// Set to `Some(register_char)` while a macro is being recorded.
    recording: Option<char>,
    /// Keys accumulated since `start_recording` was called.
    current: Vec<Key>,
    /// Completed macro sequences keyed by register character.
    macros: HashMap<char, Vec<Key>>,
    /// The key sequence of the last change, for dot-repeat.
    last_change: Option<Vec<Key>>,
    /// Keys being accumulated for the current change operation.
    change_keys: Vec<Key>,
}

impl MacroRecorder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start or stop macro recording on `reg`.
    ///
    /// - If not currently recording: begin recording into `reg`, return `true`.
    /// - If currently recording (any register): stop recording, store the macro,
    ///   return `false`.
    pub fn start_recording(&mut self, reg: char) -> bool {
        if self.recording.is_some() {
            // Stop recording.
            self.stop_recording();
            false
        } else {
            self.recording = Some(reg);
            self.current.clear();
            true
        }
    }

    /// Explicitly stop recording and persist the current buffer.
    ///
    /// Returns the register character the macro was stored in, or `None` if
    /// no recording was in progress.
    pub fn stop_recording(&mut self) -> Option<char> {
        let reg = self.recording.take()?;
        let keys = std::mem::take(&mut self.current);
        self.macros.insert(reg, keys);
        Some(reg)
    }

    /// Return the register currently being recorded into, if any.
    pub fn is_recording(&self) -> Option<char> {
        self.recording
    }

    /// Feed a key into the active recording (no-op if not recording).
    pub fn record_key(&mut self, key: &Key) {
        if self.recording.is_some() {
            self.current.push(key.clone());
        }
    }

    /// Drop the most recently recorded key.
    ///
    /// `record_key` runs unconditionally on every keystroke, before the editor
    /// knows whether that keystroke is the bare `q` that *stops* the recording
    /// (vim: `q{c}...q` — the terminating `q` is not part of the macro body).
    /// Call this right before [`stop_recording`](Self::stop_recording) to strip
    /// that trailing key back out.
    pub fn discard_last_recorded_key(&mut self) {
        if self.recording.is_some() {
            self.current.pop();
        }
    }

    /// Retrieve the stored macro for `reg`, or `None` if it doesn't exist.
    pub fn get_macro(&self, reg: char) -> Option<&[Key]> {
        self.macros.get(&reg).map(Vec::as_slice)
    }

    /// Append a key to the in-progress command sequence (dot-repeat tracking).
    /// The editor feeds every editing key here, then either commits the sequence
    /// (when the command changed the buffer) or clears it (when it didn't).
    pub fn dot_feed(&mut self, key: &Key) {
        self.change_keys.push(key.clone());
    }

    /// Commit the accumulated sequence as the last change (replayed on `.`).
    pub fn dot_commit(&mut self) {
        if !self.change_keys.is_empty() {
            self.last_change = Some(std::mem::take(&mut self.change_keys));
        }
    }

    /// Discard the in-progress sequence without recording it (e.g. a pure motion).
    pub fn dot_clear(&mut self) {
        self.change_keys.clear();
    }

    /// Return the key sequence for dot-repeat, or `None` if no change has been recorded.
    pub fn dot_repeat(&self) -> Option<&[Key]> {
        self.last_change.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::key::KeyMod;

    fn k(c: char) -> Key {
        Key::Char(c, KeyMod::NONE)
    }

    #[test]
    fn record_and_retrieve_macro() {
        let mut rec = MacroRecorder::new();
        assert!(rec.start_recording('q'));
        rec.record_key(&k('d'));
        rec.record_key(&k('w'));
        let stopped = rec.stop_recording();
        assert_eq!(stopped, Some('q'));
        assert_eq!(rec.get_macro('q'), Some([k('d'), k('w')].as_slice()));
    }

    #[test]
    fn start_while_recording_stops() {
        let mut rec = MacroRecorder::new();
        rec.start_recording('a');
        rec.record_key(&k('x'));
        // calling start_recording again stops
        let started = rec.start_recording('a');
        assert!(!started);
        assert!(rec.is_recording().is_none());
        assert_eq!(rec.get_macro('a'), Some([k('x')].as_slice()));
    }

    #[test]
    fn dot_repeat_tracks_change() {
        let mut rec = MacroRecorder::new();
        rec.dot_feed(&k('c'));
        rec.dot_feed(&k('w'));
        rec.dot_commit();
        assert_eq!(rec.dot_repeat(), Some([k('c'), k('w')].as_slice()));
    }

    #[test]
    fn dot_clear_discards_in_progress_sequence() {
        let mut rec = MacroRecorder::new();
        rec.dot_feed(&k('d'));
        rec.dot_feed(&k('w'));
        rec.dot_commit(); // last_change = dw
        rec.dot_feed(&k('w')); // a motion
        rec.dot_clear(); // discarded — last_change unchanged
        assert_eq!(rec.dot_repeat(), Some([k('d'), k('w')].as_slice()));
    }

    #[test]
    fn dot_repeat_none_before_any_change() {
        let rec = MacroRecorder::new();
        assert!(rec.dot_repeat().is_none());
    }
}
