//! http-client — reference onda plugin (W20).
//!
//! Registers an `:http <url>` command; on invocation it issues a GET through the
//! granted `network` capability and reports the outcome. Validates: command
//! registration, the network permission path (link-time + call-time), and a
//! picker contribution. (The host's HTTP is v0-stubbed; this exercises the
//! permission + command + picker UX end-to-end, which is W20's goal here.)

wit_bindgen::generate!({
    world: "plugin",
    path: "../../wit/onda",
});

use exports::onda::plugin::guest::{Event, Guest};
use onda::plugin::types::NotifyLevel;
use onda::plugin::{commands, http, log, ui};

const CMD_ID: u64 = 1;

struct Plugin;

impl Guest for Plugin {
    fn init() {
        // Register `:http <url>`.
        commands::create("http", CMD_ID, Some("send an HTTP GET"), 1);
        log::debug("http-client activated");
    }

    fn handle_event(_ev: Event) {}

    fn run_command(id: u64, args: Vec<String>) {
        if id != CMD_ID {
            return;
        }
        let Some(url) = args.first() else {
            log::notify("usage: :http <url>", NotifyLevel::Warn);
            return;
        };
        match http::get(url) {
            Ok(resp) => {
                log::notify(&format!("{} → {}", url, resp.status), NotifyLevel::Info);
                // Offer the response headers/lines through a picker.
                let body = String::from_utf8_lossy(&resp.body).into_owned();
                let items: Vec<ui::PickerItem> = body
                    .lines()
                    .take(50)
                    .map(|l| ui::PickerItem {
                        label: l.to_string(),
                        detail: None,
                    })
                    .collect();
                ui::pick("HTTP response", &items, CMD_ID);
            }
            Err(e) => log::notify(&format!("http error: {e:?}"), NotifyLevel::Error),
        }
    }

    fn run_keymap(_id: u64) {}
}

export!(Plugin);
