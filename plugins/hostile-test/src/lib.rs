//! hostile-test — a deliberately misbehaving plugin used to prove containment
//! (W18 T18.2). On `init` it busy-loops forever; the host's epoch deadline must
//! trap it within the handler budget, leaving the editor unaffected.

wit_bindgen::generate!({
    world: "plugin",
    path: "../../wit/onda",
});

use exports::onda::plugin::guest::{Event, Guest};

struct Plugin;

impl Guest for Plugin {
    fn init() {
        // Spin forever — the host must interrupt this via the epoch budget.
        let mut x: u64 = 0;
        loop {
            x = x.wrapping_add(1);
            std::hint::black_box(x);
        }
    }

    fn handle_event(_ev: Event) {}
    fn run_command(_id: u64, _args: Vec<String>) {}
    fn run_keymap(_id: u64) {}
}

export!(Plugin);
