use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Instant,
};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let task = args.first().map(|s| s.as_str()).unwrap_or("help");

    match task {
        "ci" => ci(),
        "bench" => bench(&args[1..]),
        "bench-compare" => bench_compare(),
        "gen-fixtures" => gen_fixtures(),
        "help" | "--help" | "-h" => {
            print_help();
            Ok(())
        }
        other => {
            eprintln!("Unknown task: {other}");
            print_help();
            std::process::exit(1);
        }
    }
}

fn print_help() {
    println!(
        r#"onda xtask — project automation

USAGE:
    cargo xtask <task>

TASKS:
    ci              Run fmt check, clippy, tests, and deny
    bench           Run benchmarks and print results
    bench --check   Check benchmarks against bench/baseline.json (exit 1 on >5%
                    regression OR on any measured Phase 3 gate exceeding its budget:
                    dap_on_keypress_p99_ms<10, git_blame_render_ms<2, theme_switch_ms<5)
    bench-compare   Compare onda vs nvim/helix, write BENCH_REPORT.md
    gen-fixtures    Generate synthetic test fixtures (bench/fixtures/):
                      large_100mb.txt   — 100 MB line-numbered text
                      large_1gb.txt     — 1 GB line-numbered text
                      rust_100k.rs      — ~100k lines of Rust-like syntax
                      nested.json       — 20-level deeply-nested JSON
                      malformed.toml    — TOML with a syntax error at line 50
                      prose_long.md     — 500 lines of 150+ char prose (soft-wrap bench)
                      wide.csv/narrow.csv — CSV table fixtures (ONDA_CSV_BYTES)
                      records.jsonl     — JSONL record fixture (ONDA_JSONL_BYTES)
                      malformed.csv/.jsonl — quoting/parse edge-case corpus
    help            Print this help
"#
    );
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn run_cmd(program: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(program)
        .args(args)
        .current_dir(workspace_root())
        .status()
        .with_context(|| format!("failed to run {program}"))?;
    if !status.success() {
        bail!(
            "{program} {} failed with exit code {:?}",
            args.join(" "),
            status.code()
        );
    }
    Ok(())
}

#[allow(dead_code)]
fn run_cmd_output(program: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(program)
        .args(args)
        .current_dir(workspace_root())
        .output()
        .with_context(|| format!("failed to run {program}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("{program} failed: {stderr}");
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

// ── CI ────────────────────────────────────────────────────────────────────────

fn ci() -> Result<()> {
    println!("==> fmt check");
    run_cmd("cargo", &["fmt", "--all", "--", "--check"])?;

    println!("==> clippy");
    run_cmd(
        "cargo",
        &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
    )?;

    println!("==> test");
    run_cmd("cargo", &["test", "--workspace"])?;

    // cargo-deny is optional (CI has it, local may not)
    println!("==> deny (best-effort)");
    let _ = run_cmd("cargo", &["deny", "check"]);

    println!("==> CI passed");
    Ok(())
}

// ── Benchmark harness ─────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Clone)]
struct BenchResult {
    name: String,
    median_ms: f64,
    mean_ms: f64,
    min_ms: f64,
    max_ms: f64,
    runs: usize,
    /// Absolute performance budget (ceiling). `Some(x)` means `--check` fails if
    /// `median_ms > x` once the gate has real measurements (`runs > 0`). Gates whose
    /// feature is not yet wired carry a budget but `runs: 0`, so they don't spuriously
    /// fail until a measurement source lands.
    #[serde(default)]
    budget_ms: Option<f64>,
}

/// Phase 3+4 perf gates. Budgets are enforced by `bench --check`. Measurements are
/// wired in as each feature lands (theme switch in T18.1; git blame in T16.1; DAP-on
/// keypress in W15; agent panel-stream + stream-while-editing in W23/W26); until then
/// they report `runs: 0` and the absolute-budget check is skipped for them.
fn extra_gates() -> Vec<BenchResult> {
    let gate = |name: &str, budget_ms: f64| BenchResult {
        name: name.to_string(),
        median_ms: 0.0,
        mean_ms: 0.0,
        min_ms: 0.0,
        max_ms: 0.0,
        runs: 0,
        budget_ms: Some(budget_ms),
    };
    vec![
        // DAP attached: keypress → render p99 must stay under the 10ms input budget.
        gate("dap_on_keypress_p99_ms", 10.0),
        // Git blame annotation render cost for a 500-line file.
        gate("git_blame_render_ms", 2.0),
        // Full-screen re-render on `:theme` switch.
        gate("theme_switch_ms", 5.0),
        // Phase 4 (ACP agent): coalesced agent-panel stream re-render per frame
        // under a 10k tokens/s burst must fit the frame budget.
        gate("panel_stream_frame_ms", 16.0),
        // Phase 4: keypress → render p99 while an agent streams in the panel.
        gate("agent_stream_keypress_p99_ms", 10.0),
        // Phase 5: scroll one frame of CSV table mode on the 1GB fixture.
        gate("csv_table_scroll_ms", 16.0),
        // Phase 5: time to first parsed/visible record opening the 10GB JSONL fixture.
        gate("jsonl_first_record_ms", 500.0),
        // Phase 5: lazy persistent-undo load must not threaten the 40ms startup gate.
        gate("persistent_undo_load_ms", 40.0),
    ]
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct BenchReport {
    results: Vec<BenchResult>,
    git_sha: String,
    timestamp: String,
}

fn bench(args: &[String]) -> Result<()> {
    let check_mode = args.iter().any(|a| a == "--check");
    let root = workspace_root();

    // Build release binary
    println!("==> Building release binary...");
    run_cmd("cargo", &["build", "--release", "-p", "onda"])?;

    let binary = root.join("target/release/onda");
    if !binary.exists() {
        bail!("Release binary not found at {}", binary.display());
    }

    println!("==> Running startup benchmark...");
    let startup = bench_startup(&binary)?;

    println!("==> Running large-file benchmark...");
    let large_file = bench_large_file(&binary, &root)?;

    let mut results = vec![startup, large_file];
    // Phase 3+4 perf gates. Carried through with their budgets so `--check` enforces
    // them; measurements are filled in by the features that own each gate.
    results.extend(extra_gates());

    let report = BenchReport {
        results: results.clone(),
        git_sha: git_sha().unwrap_or_default(),
        timestamp: "".to_string(), // determinism: set externally
    };

    // Print summary
    println!("\n=== Benchmark Results ===");
    for r in &results {
        println!(
            "{}: median={:.2}ms mean={:.2}ms min={:.2}ms max={:.2}ms",
            r.name, r.median_ms, r.mean_ms, r.min_ms, r.max_ms
        );
    }

    if check_mode {
        check_regression(&results, &root)?;
    } else {
        // Save as new baseline? Only when explicitly passing --save-baseline
        if args.iter().any(|a| a == "--save-baseline") {
            let baseline_path = root.join("bench/baseline.json");
            let json = serde_json::to_string_pretty(&report)?;
            std::fs::write(&baseline_path, json)?;
            println!("Saved baseline to {}", baseline_path.display());
        }
    }

    Ok(())
}

fn bench_startup(binary: &Path) -> Result<BenchResult> {
    let warmup = 3usize;
    let runs = 10usize;

    // Warmup
    for _ in 0..warmup {
        Command::new(binary)
            .arg("--bench-startup")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
    }

    let mut times_ms: Vec<f64> = Vec::with_capacity(runs);
    for _ in 0..runs {
        let start = Instant::now();
        let status = Command::new(binary)
            .arg("--bench-startup")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        if status.success() {
            times_ms.push(start.elapsed().as_secs_f64() * 1000.0);
        }
    }

    Ok(summarize("startup_ms", &mut times_ms))
}

fn bench_large_file(binary: &Path, root: &Path) -> Result<BenchResult> {
    let fixture_path = root.join("bench/fixtures/large_100mb.txt");
    if !fixture_path.exists() {
        println!("  (skipping large-file bench: fixture not found, run gen-fixtures first)");
        return Ok(BenchResult {
            name: "large_file_open_ms".into(),
            median_ms: 0.0,
            mean_ms: 0.0,
            min_ms: 0.0,
            max_ms: 0.0,
            runs: 0,
            budget_ms: Some(2000.0),
        });
    }

    let runs = 5usize;
    let mut times_ms: Vec<f64> = Vec::with_capacity(runs);

    for _ in 0..runs {
        let start = Instant::now();
        let status = Command::new(binary)
            .args(["--bench-startup", fixture_path.to_str().unwrap()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        if status.success() {
            times_ms.push(start.elapsed().as_secs_f64() * 1000.0);
        }
    }

    Ok(summarize("large_file_open_ms", &mut times_ms))
}

fn summarize(name: &str, times: &mut [f64]) -> BenchResult {
    summarize_with_budget(name, times, None)
}

fn summarize_with_budget(name: &str, times: &mut [f64], budget_ms: Option<f64>) -> BenchResult {
    if times.is_empty() {
        return BenchResult {
            name: name.to_string(),
            median_ms: 0.0,
            mean_ms: 0.0,
            min_ms: 0.0,
            max_ms: 0.0,
            runs: 0,
            budget_ms,
        };
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = times.len();
    let median = times[n / 2];
    let mean = times.iter().sum::<f64>() / n as f64;
    let min = times[0];
    let max = times[n - 1];
    BenchResult {
        name: name.to_string(),
        median_ms: median,
        mean_ms: mean,
        min_ms: min,
        max_ms: max,
        runs: n,
        budget_ms,
    }
}

fn check_regression(results: &[BenchResult], root: &Path) -> Result<()> {
    let baseline_path = root.join("bench/baseline.json");
    if !baseline_path.exists() {
        println!("No baseline.json found; skipping regression check.");
        return Ok(());
    }

    let baseline: BenchReport = serde_json::from_str(&std::fs::read_to_string(&baseline_path)?)?;

    let mut failed = false;
    let threshold = 1.05; // 5% regression

    for result in results {
        // Absolute budget ceiling (T15.0): independent of the baseline, a measured gate
        // that blows its budget always fails. Unmeasured gates (runs == 0) are skipped.
        if let Some(budget) = result.budget_ms {
            if result.runs > 0 && result.median_ms > budget {
                eprintln!(
                    "BUDGET EXCEEDED: {} median {:.2}ms > budget {:.2}ms",
                    result.name, result.median_ms, budget
                );
                failed = true;
            }
        }

        if let Some(base) = baseline.results.iter().find(|r| r.name == result.name) {
            if base.median_ms > 0.0 {
                let ratio = result.median_ms / base.median_ms;
                if ratio > threshold {
                    eprintln!(
                        "REGRESSION: {} median {:.2}ms vs baseline {:.2}ms ({:.1}% slower)",
                        result.name,
                        result.median_ms,
                        base.median_ms,
                        (ratio - 1.0) * 100.0
                    );
                    failed = true;
                } else {
                    println!(
                        "OK: {} {:.2}ms (baseline {:.2}ms, delta {:.1}%)",
                        result.name,
                        result.median_ms,
                        base.median_ms,
                        (ratio - 1.0) * 100.0
                    );
                }
            }
        }
    }

    if failed {
        bail!("Benchmark regression / budget violation detected (threshold: 5%)");
    }
    Ok(())
}

// ── bench-compare ─────────────────────────────────────────────────────────────

fn bench_compare() -> Result<()> {
    let root = workspace_root();

    println!("==> Building onda release binary...");
    run_cmd("cargo", &["build", "--release", "-p", "onda"])?;
    let onda_bin = root.join("target/release/onda");

    let mut rows: Vec<[String; 4]> = vec![[
        "Benchmark".into(),
        "onda".into(),
        "nvim".into(),
        "helix".into(),
    ]];

    // Startup benchmarks
    let onda_startup = bench_startup(&onda_bin)?;
    let nvim_startup = measure_startup("nvim", &["--headless", "+quit"]);
    let helix_startup = measure_startup("hx", &["--version"]);

    rows.push([
        "startup (median ms)".into(),
        format!("{:.1}", onda_startup.median_ms),
        nvim_startup
            .map(|t| format!("{t:.1}"))
            .unwrap_or_else(|| "n/a".into()),
        helix_startup
            .map(|t| format!("{t:.1}"))
            .unwrap_or_else(|| "n/a".into()),
    ]);

    // Write BENCH_REPORT.md
    let report = generate_markdown_table(&rows);
    let report_path = root.join("BENCH_REPORT.md");
    std::fs::write(
        &report_path,
        format!(
            "# onda Benchmark Report\n\nGenerated on commit `{}`.\n\n{}\n",
            git_sha().unwrap_or_default(),
            report
        ),
    )?;
    println!("Wrote {}", report_path.display());
    Ok(())
}

fn measure_startup(program: &str, args: &[&str]) -> Option<f64> {
    let runs = 5;
    let mut times = Vec::with_capacity(runs);
    for _ in 0..runs {
        let start = Instant::now();
        let ok = Command::new(program)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            times.push(start.elapsed().as_secs_f64() * 1000.0);
        }
    }
    if times.is_empty() {
        return None;
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    Some(times[times.len() / 2])
}

fn generate_markdown_table(rows: &[[String; 4]]) -> String {
    let mut out = String::new();
    for (i, row) in rows.iter().enumerate() {
        out.push('|');
        for cell in row {
            out.push_str(&format!(" {} |", cell));
        }
        out.push('\n');
        if i == 0 {
            out.push_str("|---|---|---|---|\n");
        }
    }
    out
}

// ── gen-fixtures ──────────────────────────────────────────────────────────────

fn gen_fixtures() -> Result<()> {
    let root = workspace_root();
    let fixtures = root.join("bench/fixtures");
    std::fs::create_dir_all(&fixtures)?;

    println!("==> Generating 100MB text fixture...");
    gen_text_fixture(&fixtures.join("large_100mb.txt"), 100 * 1024 * 1024)?;

    println!("==> Generating 1GB text fixture...");
    gen_text_fixture(&fixtures.join("large_1gb.txt"), 1024 * 1024 * 1024)?;

    println!("==> Generating 100k-line Rust fixture...");
    gen_rust_fixture(&fixtures.join("rust_100k.rs"))?;

    println!("==> Generating deeply-nested JSON fixture...");
    gen_nested_json_fixture(&fixtures.join("nested.json"))?;

    println!("==> Generating malformed TOML fixture...");
    gen_malformed_toml_fixture(&fixtures.join("malformed.toml"))?;

    println!("==> Generating long-line prose fixture...");
    gen_prose_long_line_fixture(&fixtures.join("prose_long.md"))?;

    // Phase 5 data-view fixtures. The huge variants stream to disk; size is taken
    // from env so CI can pick a representative size without filling the runner
    // (ONDA_CSV_BYTES / ONDA_JSONL_BYTES, defaulting to ~64MB).
    let csv_bytes: usize = env_bytes("ONDA_CSV_BYTES", 64 * 1024 * 1024);
    let jsonl_bytes: usize = env_bytes("ONDA_JSONL_BYTES", 64 * 1024 * 1024);

    println!(
        "==> Generating wide CSV fixture (~{} MB)...",
        csv_bytes >> 20
    );
    gen_csv_fixture(&fixtures.join("wide.csv"), csv_bytes, 240)?;
    println!(
        "==> Generating narrow CSV fixture (~{} MB)...",
        csv_bytes >> 20
    );
    gen_csv_fixture(&fixtures.join("narrow.csv"), csv_bytes, 4)?;
    println!("==> Generating malformed CSV fixture...");
    gen_malformed_csv_fixture(&fixtures.join("malformed.csv"))?;

    println!(
        "==> Generating JSONL fixture (~{} MB, streamed)...",
        jsonl_bytes >> 20
    );
    gen_jsonl_fixture(&fixtures.join("records.jsonl"), jsonl_bytes)?;
    println!("==> Generating malformed JSONL fixture...");
    gen_malformed_jsonl_fixture(&fixtures.join("malformed.jsonl"))?;

    println!("==> Fixtures generated in {}", fixtures.display());
    Ok(())
}

/// Read a byte-size from an env var, falling back to `default`.
fn env_bytes(var: &str, default: usize) -> usize {
    std::env::var(var)
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(default)
}

/// Generate a CSV with `columns` columns up to `target_bytes`, streamed.
fn gen_csv_fixture(path: &Path, target_bytes: usize, columns: usize) -> Result<()> {
    if path.exists() {
        println!("  {} already exists, skipping", path.display());
        return Ok(());
    }
    let mut file = io::BufWriter::new(std::fs::File::create(path)?);
    // Header row.
    let header: Vec<String> = (0..columns).map(|c| format!("col_{c}")).collect();
    writeln!(file, "{}", header.join(","))?;
    let mut written = header.join(",").len() + 1;
    let mut row = 0usize;
    while written < target_bytes {
        let mut line = String::with_capacity(columns * 8);
        for c in 0..columns {
            if c > 0 {
                line.push(',');
            }
            // Mix numbers, text, and a quoted field with an embedded comma + unicode.
            match c % 4 {
                0 => line.push_str(&format!("{}", row * 31 + c)),
                1 => line.push_str(&format!("item-{row}-{c}")),
                2 => line.push_str("\"a,b\""),
                _ => line.push_str("李雷"),
            }
        }
        line.push('\n');
        written += line.len();
        file.write_all(line.as_bytes())?;
        row += 1;
    }
    file.flush()?;
    Ok(())
}

/// A small CSV with quoting/raggedness edge cases for the sniffer/parser corpus.
fn gen_malformed_csv_fixture(path: &Path) -> Result<()> {
    if path.exists() {
        println!("  {} already exists, skipping", path.display());
        return Ok(());
    }
    let content = "\u{feff}id,name,note\r\n\
        1,Alice,\"hello, world\"\r\n\
        2,Bob\r\n\
        3,Cara,\"unterminated quote\r\n\
        4,Dan,ok,extra,columns\r\n\
        5,\"esc \"\"quote\"\"\",fine\r\n";
    std::fs::write(path, content)?;
    Ok(())
}

/// Generate a JSONL file of heterogeneous records up to `target_bytes`, streamed.
fn gen_jsonl_fixture(path: &Path, target_bytes: usize) -> Result<()> {
    if path.exists() {
        println!("  {} already exists, skipping", path.display());
        return Ok(());
    }
    let mut file = io::BufWriter::new(std::fs::File::create(path)?);
    let mut written = 0usize;
    let mut i = 0usize;
    while written < target_bytes {
        // Vary the schema so the :fields overlay has something to summarize.
        let line = match i % 3 {
            0 => format!(
                r#"{{"id":{i},"name":"user-{i}","active":{},"tags":["a","b"]}}"#,
                i % 2 == 0
            ),
            1 => format!(
                r#"{{"id":"x{i}","name":"user-{i}","score":{}}}"#,
                i as f64 * 1.5
            ),
            _ => format!(r#"{{"id":{i},"nested":{{"k":{i},"v":"日本語"}}}}"#),
        };
        written += line.len() + 1;
        writeln!(file, "{line}")?;
        i += 1;
    }
    file.flush()?;
    Ok(())
}

/// A small JSONL with malformed records interleaved with valid ones.
fn gen_malformed_jsonl_fixture(path: &Path) -> Result<()> {
    if path.exists() {
        println!("  {} already exists, skipping", path.display());
        return Ok(());
    }
    let content = "{\"id\":1,\"ok\":true}\n\
        {not valid json}\n\
        {\"id\":3}\n\
        \n\
        {\"id\":4,\"trailing\":}\n\
        {\"id\":5,\"unicode\":\"日本語\"}\n";
    std::fs::write(path, content)?;
    Ok(())
}

/// Generate ~100k lines of Rust-like syntax: fn declarations, let bindings,
/// string literals, and line comments.
fn gen_rust_fixture(path: &Path) -> Result<()> {
    if path.exists() {
        println!("  {} already exists, skipping", path.display());
        return Ok(());
    }

    let mut file = io::BufWriter::new(std::fs::File::create(path)?);

    // A small set of patterns rotated to produce varied but valid-ish Rust.
    let target_lines = 100_000usize;
    let fns_per_block = 10usize; // lines per "function block"
    let mut line_no = 0usize;

    // Module header
    writeln!(
        file,
        "//! Auto-generated Rust fixture — {target_lines} lines"
    )?;
    writeln!(file, "#![allow(dead_code, unused_variables)]")?;
    line_no += 2;

    let mut fn_idx = 0usize;
    while line_no < target_lines {
        writeln!(file)?;
        writeln!(file, "// Function block {fn_idx}")?;
        writeln!(
            file,
            "fn generated_fn_{fn_idx}(x: u64, label: &str) -> String {{"
        )?;
        line_no += 3;

        for i in 0..fns_per_block {
            writeln!(
                file,
                "    let var_{fn_idx}_{i}: u64 = x.wrapping_add({i} as u64);"
            )?;
            writeln!(
                file,
                "    let msg_{fn_idx}_{i} = format!(\"item-{{}}-{{}}\", label, var_{fn_idx}_{i});"
            )?;
            writeln!(file, "    // step {i} of fn {fn_idx}")?;
            line_no += 3;
            if line_no >= target_lines {
                break;
            }
        }

        writeln!(file, "    format!(\"result-{{}}-{fn_idx}\", label)")?;
        writeln!(file, "}}")?;
        line_no += 2;
        fn_idx += 1;
    }

    file.flush()?;
    Ok(())
}

/// Generate a 20-level deeply-nested JSON object.
fn gen_nested_json_fixture(path: &Path) -> Result<()> {
    if path.exists() {
        println!("  {} already exists, skipping", path.display());
        return Ok(());
    }

    let depths = 20usize;

    // Build inside-out: start from the innermost value.
    let mut content = String::from(r#"{"value": "deepest", "index": 0, "tag": "innermost-leaf"}"#);

    for d in 1..=depths {
        content = format!(
            r#"{{"level": {d}, "description": "nesting-depth-{d}", "metadata": {{"created_at": "2026-06-12", "depth": {d}}}, "child": {content}}}"#
        );
    }

    // Pretty-print by inserting newlines at each opening/closing brace.
    // Use serde_json to round-trip for guaranteed validity.
    let parsed: serde_json::Value =
        serde_json::from_str(&content).context("failed to build nested JSON — internal error")?;
    let pretty = serde_json::to_string_pretty(&parsed)?;
    std::fs::write(path, pretty)?;
    Ok(())
}

/// Generate a TOML file that is valid for the first 49 lines and contains a
/// syntax error on line 50.
fn gen_malformed_toml_fixture(path: &Path) -> Result<()> {
    if path.exists() {
        println!("  {} already exists, skipping", path.display());
        return Ok(());
    }

    let mut lines: Vec<String> = Vec::with_capacity(60);

    lines.push("# Auto-generated malformed TOML fixture".into());
    lines.push("# Lines 1-49 are valid; line 50 contains a deliberate syntax error.".into());
    lines.push("".into());
    lines.push("[package]".into());
    lines.push(r#"name = "onda-test-fixture""#.into());
    lines.push(r#"version = "0.1.0""#.into());
    lines.push(r#"edition = "2021""#.into());
    lines.push("".into());
    lines.push("[dependencies]".into());

    // Fill up to line 49 with valid key = value pairs.
    for i in 1..=40usize {
        lines.push(format!("dep_{i} = \"{i}.0.0\""));
    }

    // Line 50: malformed — unclosed inline table / bare key with spaces.
    assert_eq!(lines.len(), 49, "fixture builder miscounted lines");
    lines.push("broken key with spaces = {{ value = 1".into()); // line 50

    // A few more lines after the error so parsers can report the line number.
    lines.push("".into());
    lines.push("# content after the error (should not be reached by strict parsers)".into());
    lines.push(r#"after_error = "still here""#.into());

    std::fs::write(path, lines.join("\n") + "\n")?;
    Ok(())
}

fn gen_text_fixture(path: &Path, target_bytes: usize) -> Result<()> {
    if path.exists() {
        println!("  {} already exists, skipping", path.display());
        return Ok(());
    }

    let mut file = io::BufWriter::new(std::fs::File::create(path)?);
    let line = "The quick brown fox jumps over the lazy dog. \
                Lorem ipsum dolor sit amet, consectetur adipiscing elit.\n";
    let line_bytes = line.len();
    let total_lines = target_bytes / line_bytes + 1;

    for i in 0..total_lines {
        write!(file, "{:08} {}", i, line)?;
        if file.get_ref().metadata()?.len() as usize >= target_bytes {
            break;
        }
    }
    file.flush()?;
    Ok(())
}

/// Generate a Markdown prose fixture with 500 lines each 150+ characters long.
///
/// Used by `bench_soft_wrap_render` and any benchmark that needs realistic
/// long-line content to exercise the compositor's soft-wrap layout path.
/// The key used in the fixtures map is `"prose_long_line"`.
fn gen_prose_long_line_fixture(path: &Path) -> Result<()> {
    if path.exists() {
        println!("  {} already exists, skipping", path.display());
        return Ok(());
    }

    let target_lines = 500usize;
    let min_line_len = 150usize;

    // Representative prose sentences of varying content rotated to fill lines.
    let sentences = [
        "Text editors occupy a peculiar position in the software landscape: simultaneously among the oldest and most performance-critical applications that developers use every day.",
        "The rope data structure represents text as a balanced binary tree of string chunks, enabling O(log n) insertion and deletion without the linear shift cost of contiguous arrays.",
        "Damage tracking in the compositor ensures that only changed cells are transmitted to the terminal, dramatically reducing the volume of escape sequences per frame during typical editing.",
        "The main event loop must never block: file I/O, LSP communication, syntax highlighting, and fuzzy search all run on background tokio worker threads and deliver results through channels.",
        "Tree-sitter's incremental parsing algorithm reuses unchanged subtrees directly, limiting reparse work to the region affected by an edit and completing single-character insertions in under one millisecond.",
        "Multicursor editing treats the selection as a first-class sorted collection of non-overlapping ranges, each participating equally in motions and operators without special-casing any range as primary.",
        "Cold startup latency below 40 milliseconds feels instantaneous to users; the dominant contributors are dynamic linking, configuration parsing, plugin initialization, and grammar compilation.",
        "Criterion.rs provides statistical rigor through adaptive sample collection, Tukey-fence outlier detection, and Welch's t-test for comparing before-and-after measurements with a five-percent regression threshold.",
        "GPU-accelerated rendering moves cell rasterization and compositing to the GPU's massively parallel execution units, enabling sub-millisecond render latency for the compositor's output stage.",
        "Persistent incremental indexing caches project-wide symbol tables and cross-reference graphs, enabling instantaneous responses to queries that would otherwise require full-project scans on every invocation.",
        "The ropey B-tree variant uses leaf nodes of 64 to 512 bytes to improve cache locality for sequential iteration while retaining the logarithmic insertion and deletion guarantees of the classical rope.",
        "Soft-wrap layout caches visual line break positions keyed on document version numbers, so unchanged document lines reuse their cached layout without recomputation on every rendered frame.",
        "The cell grid compositor maintains a shadow grid mirroring the terminal's current state, and emits only the minimal set of escape sequences required to advance from the shadow to the desired grid.",
        "An editor that treats performance as a first-class constraint from the very first commit will naturally arrive at designs that are fast, because slow solutions are eliminated before they can accumulate.",
        "Property-based tests verify that ChangeSet application preserves all rope invariants — character count, line count, and content correctness — across arbitrary sequences of insertions and deletions.",
    ];

    let mut file = io::BufWriter::new(std::fs::File::create(path)?);
    writeln!(
        file,
        "# Long-Line Prose Fixture — {target_lines} lines of {min_line_len}+ characters"
    )?;
    writeln!(file)?;

    let n_sentences = sentences.len();
    let mut lines_written = 2usize; // header + blank

    while lines_written < target_lines {
        let idx = (lines_written - 2) % n_sentences;
        let base = sentences[idx];

        // Pad the line to at least min_line_len by appending a counter suffix.
        let suffix = format!(" [line {:04}, fixture: prose_long_line]", lines_written - 1);
        let mut line = base.to_string();
        if line.len() + suffix.len() < min_line_len {
            // Keep appending words until we exceed the minimum.
            let filler =
                " — performance, correctness, and maintainability are not in conflict here.";
            while line.len() + suffix.len() < min_line_len {
                line.push_str(filler);
            }
        }
        line.push_str(&suffix);

        writeln!(file, "{line}")?;
        lines_written += 1;
    }

    file.flush()?;
    Ok(())
}

fn git_sha() -> Option<String> {
    Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}
