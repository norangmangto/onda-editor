//! git-blame-inline — reference onda plugin (W20).
//!
//! On cursor-hold, reads `.git/HEAD` through the granted `fs` capability and
//! shows the current branch as virtual text at the cursor. Validates: the fs
//! permission path (link-time + call-time), cursor-hold events, virtual text.
//!
//! NOTE: real per-line blame needs a host `vcs` interface (flagged as a W17
//! design decision); v0 demonstrates the capability + decoration plumbing with
//! the branch readout. Full blame is deferred to the vcs-interface work.

wit_bindgen::generate!({
    world: "plugin",
    path: "../../wit/onda",
});

use exports::onda::plugin::guest::{Event, Guest};
use onda::plugin::decorations::{self, Batch, VirtText};
use onda::plugin::types::Style;
use onda::plugin::{fs, log};

const NS: &str = "git-blame-inline";

struct Plugin;

impl Guest for Plugin {
    fn init() {
        log::debug("git-blame-inline activated");
    }

    fn handle_event(ev: Event) {
        let (buf, pos) = match ev {
            Event::CursorHold(c) => (c.buf, c.pos),
            _ => return,
        };
        let label = match branch() {
            Some(b) => format!("  on {b}"),
            None => return,
        };
        let batch = Batch {
            namespace: NS.to_string(),
            virt_texts: vec![VirtText {
                at: pos,
                text: label,
                style: dim(),
            }],
            signs: Vec::new(),
            highlights: Vec::new(),
        };
        let _ = decorations::set(buf, &batch);
    }

    fn run_command(_id: u64, _args: Vec<String>) {}
    fn run_keymap(_id: u64) {}
}

/// Read `.git/HEAD` via the fs capability and extract the branch name.
fn branch() -> Option<String> {
    let bytes = fs::read(".git/HEAD").ok()?;
    let head = String::from_utf8(bytes).ok()?;
    // "ref: refs/heads/<branch>\n" → "<branch>"; detached → short sha.
    let head = head.trim();
    if let Some(rest) = head.strip_prefix("ref: refs/heads/") {
        Some(rest.to_string())
    } else {
        Some(head.chars().take(8).collect())
    }
}

fn dim() -> Style {
    Style {
        fg: Some("#5c6370".to_string()),
        bg: None,
        bold: false,
        italic: true,
        underline: false,
    }
}

export!(Plugin);
