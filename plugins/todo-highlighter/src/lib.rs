//! todo-highlighter — reference onda plugin (W20).
//!
//! On buffer open/change, scans buffer lines for TODO/FIXME/HACK and emits a
//! single namespaced decoration batch (highlights + gutter signs). Validates:
//! event flow, snapshot buffer reads, the batch decoration API.

wit_bindgen::generate!({
    world: "plugin",
    path: "../../wit/onda",
});

use exports::onda::plugin::guest::{Event, Guest};
use onda::plugin::buffer;
use onda::plugin::decorations::{self, Batch, Highlight, Sign};
use onda::plugin::log;
use onda::plugin::types::{BufferId, HostError, Range, Style};

const NS: &str = "todo-highlighter";
const KEYWORDS: &[&str] = &["TODO", "FIXME", "HACK"];

struct Plugin;

impl Guest for Plugin {
    fn init() {
        log::debug("todo-highlighter activated");
    }

    fn handle_event(ev: Event) {
        let buf = match ev {
            Event::BufferOpen(b) | Event::BufferChange(b) => b.buf,
            _ => return,
        };
        if let Err(e) = scan(buf) {
            log::debug(&format!("todo-highlighter: {e:?}"));
        }
    }

    fn run_command(_id: u64, _args: Vec<String>) {}
    fn run_keymap(_id: u64) {}
}

fn scan(buf: BufferId) -> Result<(), HostError> {
    let line_count = buffer::line_count(buf)?;
    let lines = buffer::lines(buf, 0, line_count)?;

    let mut highlights = Vec::new();
    let mut signs = Vec::new();

    // Char offset accumulator (lines are returned without trailing newlines).
    let mut offset: u32 = 0;
    for (lineno, line) in lines.iter().enumerate() {
        if let Some(col) = first_keyword(line) {
            let start = offset + col;
            let end = start + keyword_len(line, col);
            highlights.push(Highlight {
                range: Range {
                    anchor: start,
                    head: end,
                },
                style: warn_style(),
            });
            signs.push(Sign {
                line: lineno as u32,
                text: "▶".to_string(),
                style: warn_style(),
            });
        }
        // +1 for the newline separating lines in the buffer.
        offset += line.chars().count() as u32 + 1;
    }

    let batch = Batch {
        namespace: NS.to_string(),
        virt_texts: Vec::new(),
        signs,
        highlights,
    };
    decorations::set(buf, &batch)
}

fn first_keyword(line: &str) -> Option<u32> {
    KEYWORDS.iter().filter_map(|kw| find_char_col(line, kw)).min()
}

fn keyword_len(line: &str, col: u32) -> u32 {
    let rest: String = line.chars().skip(col as usize).collect();
    KEYWORDS
        .iter()
        .find(|kw| rest.starts_with(**kw))
        .map(|kw| kw.chars().count() as u32)
        .unwrap_or(0)
}

/// Find a substring and return its column as a char index (not byte index).
fn find_char_col(haystack: &str, needle: &str) -> Option<u32> {
    let byte = haystack.find(needle)?;
    Some(haystack[..byte].chars().count() as u32)
}

fn warn_style() -> Style {
    Style {
        fg: Some("#e5c07b".to_string()),
        bg: None,
        bold: true,
        italic: false,
        underline: false,
    }
}

export!(Plugin);
