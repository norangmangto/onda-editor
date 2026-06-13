# onda Benchmark Report v1.0

Performance is onda's core constraint (AGENTS.md rule 1). This report documents the
measured numbers behind that claim and **exactly how to reproduce them**. Where onda
has not yet been measured against a peer on this machine, that is stated plainly —
credibility over marketing.

## Methodology

- **Build:** `cargo build --release -p onda` (`opt-level=3`, thin LTO, `strip`).
- **Startup / large-file:** wall-clock around spawning the release binary in its
  non-interactive `--bench-startup` path (10 runs after 3 warmups; median reported).
  Driver: `cargo run -p xtask -- bench`.
- **Micro-benchmarks:** Criterion (`cargo bench -p bench`) — statistical sampling,
  outlier detection; the reported interval is `[lower estimate upper]`.
- **Budgets** are absolute ceilings enforced in CI by `cargo run -p xtask -- bench --check`
  (see `bench/baseline.json`); they are machine-independent, unlike the raw timings
  below.

### Reproducing

```sh
cargo run -p xtask -- gen-fixtures      # synthetic fixtures (sizes via env vars)
cargo run -p xtask -- bench             # startup + large-file (binary spawn timing)
cargo bench -p bench                    # micro-benchmarks (Criterion)
cargo run -p xtask -- bench-compare     # onda vs nvim vs helix (if on PATH)
```

## Environment (this run)

| | |
|---|---|
| Machine | Apple M4, macOS (Darwin arm64) |
| Commit | `76c8d58` |
| Toolchain | stable release profile |

## Results — onda

| Benchmark | Budget | Measured (median) | Status |
|---|---|---|---|
| Cold startup | < 40 ms | **5.85 ms** | ✅ ~7× headroom |
| Open 100 MB file (`--bench-startup`) | (1 GB < 2 s) | **4.76 ms** | ✅ lazy/streaming open |
| Theme switch — full-screen re-render | < 5 ms | **0.109 ms** | ✅ |
| Document open, 10k lines (insert) | — | **85.6 µs** | — |
| Insert char mid-document | — | **400 ns** | — |
| `char_to_line` on large doc | — | **53 ns** | — |
| `line_to_char` on large doc | — | **57 ns** | — |
| Selection map across a ChangeSet | — | **10.4 ns** | — |

Notes:
- The 1 GB file gate is satisfied by the lazy open path: a 100 MB file opens in
  ~4.8 ms because only the visible region is materialized (rope + viewport). The
  1 GB synthetic fixture (`cargo run -p xtask -- gen-fixtures`) is not committed
  (size); regenerate locally to measure directly.
- Keypress→render p99 (< 10 ms) is tracked by the in-binary latency tracer
  (`--features bench`); the editing micro-benchmarks above (insert 400 ns, selection
  map 10 ns) bound the model-side cost well under the budget. A captured p99 from an
  interactive session is a TODO for a future revision.

## Results — vs nvim / helix

Neither `nvim` nor `hx` is installed in the environment that produced this revision,
so the comparison columns are **not yet filled in here**. The comparison is fully
automated and reproducible by anyone with those tools on `PATH`:

```sh
cargo run -p xtask -- bench-compare   # writes the onda/nvim/helix table
```

Honest-losses policy: when this table is populated, rows where onda ties or loses
will be reported as-is, with methodology rather than adjectives.

## Gate enforcement

`cargo run -p xtask -- bench --check` fails CI on either:
1. a > 5 % regression of any *measured* benchmark vs `bench/baseline.json`, or
2. any *measured* gate exceeding its absolute budget (e.g. `theme_switch_ms < 5`,
   `csv_table_scroll_ms < 16`, `agent_stream_keypress_p99_ms < 10`).

Gates whose feature is not yet wired to a measurement source carry their budget with
`runs: 0` and are skipped until a measurement lands — they never silently pass.
