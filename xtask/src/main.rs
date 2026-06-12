use std::{
    collections::HashMap,
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
    bench --check   Check benchmarks against bench/baseline.json (exit 1 on >5% regression)
    bench-compare   Compare onda vs nvim/helix, write BENCH_REPORT.md
    gen-fixtures    Generate synthetic test fixtures (bench/fixtures/)
    help            Print this help
"#
    );
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn run_cmd(program: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(program)
        .args(args)
        .current_dir(workspace_root())
        .status()
        .with_context(|| format!("failed to run {program}"))?;
    if !status.success() {
        bail!("{program} {} failed with exit code {:?}", args.join(" "), status.code());
    }
    Ok(())
}

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
        &["clippy", "--workspace", "--all-targets", "--", "-D", "warnings"],
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

    let results = vec![startup, large_file];

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

fn summarize(name: &str, times: &mut Vec<f64>) -> BenchResult {
    if times.is_empty() {
        return BenchResult {
            name: name.to_string(),
            median_ms: 0.0,
            mean_ms: 0.0,
            min_ms: 0.0,
            max_ms: 0.0,
            runs: 0,
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
    }
}

fn check_regression(results: &[BenchResult], root: &Path) -> Result<()> {
    let baseline_path = root.join("bench/baseline.json");
    if !baseline_path.exists() {
        println!("No baseline.json found; skipping regression check.");
        return Ok(());
    }

    let baseline: BenchReport =
        serde_json::from_str(&std::fs::read_to_string(&baseline_path)?)?;

    let mut failed = false;
    let threshold = 1.05; // 5% regression

    for result in results {
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
        bail!("Benchmark regression detected (threshold: 5%)");
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

    println!("==> Fixtures generated in {}", fixtures.display());
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

fn git_sha() -> Option<String> {
    Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}
