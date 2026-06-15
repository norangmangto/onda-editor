//! W18 integration tests against real WASM components built from `plugins/`
//! (committed under tests/fixtures/). Proves the host API works end-to-end and
//! that the epoch budget contains a runaway plugin.

use onda_plugin::{BufferSnapshot, GrantedCaps, Manifest, PluginApiCall, PluginEngine};

const TODO_WASM: &[u8] = include_bytes!("fixtures/todo_highlighter.wasm");
const HOSTILE_WASM: &[u8] = include_bytes!("fixtures/hostile_test.wasm");
const BLAME_WASM: &[u8] = include_bytes!("fixtures/git_blame_inline.wasm");

#[test]
fn todo_highlighter_produces_decorations() {
    let engine = PluginEngine::new().expect("engine");
    let snap = BufferSnapshot::new("fn main() {}\n// TODO: fix this\nlet ok = 1;\n");

    // todo-highlighter only reads buffers; default (no) caps is sufficient
    // because reads are answered from the snapshot, not gated by caps.
    let mut inst = engine
        .instantiate(
            TODO_WASM,
            GrantedCaps::default(),
            ".".into(),
            vec![(0, snap)],
        )
        .expect("instantiate + init");

    inst.fire_buffer_open(0, "test.rs").expect("buffer-open");
    let calls = inst.drain_calls();

    let deco = calls.iter().find_map(|c| match c {
        PluginApiCall::SetDecorations { batch, .. } => Some(batch),
        _ => None,
    });
    let batch = deco.expect("plugin should emit a decoration batch");
    assert_eq!(batch.namespace, "todo-highlighter");
    assert_eq!(
        batch.highlights.len(),
        1,
        "exactly one TODO line should be highlighted"
    );
    assert_eq!(
        batch.signs.len(),
        1,
        "the TODO line should get a gutter sign"
    );
}

#[test]
fn hostile_busy_loop_is_contained_by_epoch_budget() {
    let engine = PluginEngine::new().expect("engine");
    // init() busy-loops forever; the epoch watchdog must trap it. If the budget
    // were not enforced, this call would hang and the test would time out.
    let res = engine.instantiate(HOSTILE_WASM, GrantedCaps::default(), ".".into(), vec![]);
    assert!(
        res.is_err(),
        "a busy-looping plugin must be trapped, not run unbounded"
    );
}

#[test]
fn git_blame_reads_head_through_granted_fs_capability() {
    let proj = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(proj.path().join(".git")).unwrap();
    std::fs::write(proj.path().join(".git/HEAD"), "ref: refs/heads/feature-x\n").unwrap();

    // Grant the fs capability the plugin's manifest requests (filesystem=[".git"]).
    let manifest = Manifest::parse(
        r#"
        [plugin]
        name = "git-blame-inline"
        version = "0.1.0"
        entry = "git_blame_inline.wasm"
        [permissions]
        buffer = "read"
        filesystem = [".git"]
        "#,
    )
    .unwrap();
    let caps = GrantedCaps::resolve(&manifest.permissions, proj.path(), |_| true);

    let engine = PluginEngine::new().expect("engine");
    let mut inst = engine
        .instantiate(
            BLAME_WASM,
            caps,
            proj.path().to_path_buf(),
            vec![(0, BufferSnapshot::new("fn main() {}\n"))],
        )
        .expect("instantiate with fs cap");

    inst.fire_cursor_hold(0, 3).expect("cursor-hold");
    let calls = inst.drain_calls();
    let virt = calls.iter().find_map(|c| match c {
        PluginApiCall::SetDecorations { batch, .. } => batch.virt_texts.first(),
        _ => None,
    });
    let (_, text, _) = virt.expect("a virtual-text decoration");
    assert!(
        text.contains("feature-x"),
        "blame should read the branch from .git/HEAD, got {text:?}"
    );
}

#[test]
fn ungranted_capability_fails_to_link() {
    // git-blame imports onda:plugin/fs. With no fs capability granted, the host
    // never adds fs to the linker, so the component fails to instantiate —
    // link-time enforcement of the permission model (T17.3).
    let engine = PluginEngine::new().expect("engine");
    let res = engine.instantiate(BLAME_WASM, GrantedCaps::default(), ".".into(), vec![]);
    assert!(
        res.is_err(),
        "a plugin importing an ungranted capability must fail to link"
    );
}

#[test]
fn instantiating_twice_is_independent() {
    let engine = PluginEngine::new().expect("engine");
    let snap = BufferSnapshot::new("// FIXME later\n");
    let mut a = engine
        .instantiate(
            TODO_WASM,
            GrantedCaps::default(),
            ".".into(),
            vec![(0, snap.clone())],
        )
        .expect("a");
    let mut b = engine
        .instantiate(
            TODO_WASM,
            GrantedCaps::default(),
            ".".into(),
            vec![(0, snap)],
        )
        .expect("b");
    a.fire_buffer_change(0, "x.rs").expect("a change");
    b.fire_buffer_change(0, "x.rs").expect("b change");
    assert!(!a.drain_calls().is_empty());
    assert!(!b.drain_calls().is_empty());
}

/// Sum of highlight ranges across all decoration batches in `calls`.
fn highlight_count(calls: &[PluginApiCall]) -> usize {
    calls
        .iter()
        .filter_map(|c| match c {
            PluginApiCall::SetDecorations { batch, .. } => Some(batch.highlights.len()),
            _ => None,
        })
        .sum()
}

#[test]
fn todo_highlighter_ignores_buffer_without_markers() {
    let engine = PluginEngine::new().expect("engine");
    let snap = BufferSnapshot::new("fn main() {}\nlet ok = 1;\n");
    let mut inst = engine
        .instantiate(
            TODO_WASM,
            GrantedCaps::default(),
            ".".into(),
            vec![(0, snap)],
        )
        .expect("instantiate");
    inst.fire_buffer_open(0, "clean.rs").expect("buffer-open");
    assert_eq!(highlight_count(&inst.drain_calls()), 0);
}

#[test]
fn todo_highlighter_marks_multiple_lines() {
    let engine = PluginEngine::new().expect("engine");
    let snap = BufferSnapshot::new("// TODO: a\nok\n// FIXME: b\n// HACK: c\n");
    let mut inst = engine
        .instantiate(
            TODO_WASM,
            GrantedCaps::default(),
            ".".into(),
            vec![(0, snap)],
        )
        .expect("instantiate");
    inst.fire_buffer_open(0, "many.rs").expect("buffer-open");
    assert_eq!(highlight_count(&inst.drain_calls()), 3);
}

#[test]
fn set_buffer_snapshot_updates_decorations() {
    let engine = PluginEngine::new().expect("engine");
    let mut inst = engine
        .instantiate(
            TODO_WASM,
            GrantedCaps::default(),
            ".".into(),
            vec![(0, BufferSnapshot::new("no markers here\n"))],
        )
        .expect("instantiate");
    inst.fire_buffer_open(0, "f.rs").expect("open");
    assert_eq!(highlight_count(&inst.drain_calls()), 0);

    // Update the snapshot to contain a TODO and re-fire.
    inst.set_buffer_snapshot(0, BufferSnapshot::new("// TODO: now\n"));
    inst.fire_buffer_change(0, "f.rs").expect("change");
    assert_eq!(highlight_count(&inst.drain_calls()), 1);
}
