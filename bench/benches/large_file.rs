use std::path::Path;

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use onda_core::{Document, Transaction};

fn bench_rope_char_to_line(c: &mut Criterion) {
    // Build a large document
    let text: String = (0..100_000)
        .map(|i| format!("Line {i:08}: content here\n"))
        .collect();
    let mut doc = Document::new_empty();
    let cs = onda_core::transaction::ChangeSetBuilder::new(0)
        .insert(text.clone())
        .build();
    doc.apply(&Transaction::new(cs)).unwrap();

    let len = doc.len_chars();

    c.bench_function("char_to_line_large_doc", |b| {
        b.iter(|| {
            let pos = len / 2;
            black_box(doc.char_to_line(black_box(pos)))
        })
    });
}

fn bench_line_to_char(c: &mut Criterion) {
    let text: String = (0..100_000)
        .map(|i| format!("Line {i:08}: content here\n"))
        .collect();
    let mut doc = Document::new_empty();
    let cs = onda_core::transaction::ChangeSetBuilder::new(0)
        .insert(text)
        .build();
    doc.apply(&Transaction::new(cs)).unwrap();

    let mid_line = doc.len_lines() / 2;

    c.bench_function("line_to_char_large_doc", |b| {
        b.iter(|| black_box(doc.line_to_char(black_box(mid_line))))
    });
}

/// Simulate the soft-wrap render path by loading prose.md and reading 80 chars
/// from line 0. This exercises Document::open, rope line access, and the kind
/// of bounded slice extraction a compositor performs when measuring a visual row.
fn bench_soft_wrap_render(c: &mut Criterion) {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("bench/fixtures/prose.md");

    // If the fixture is missing (e.g. on a fresh checkout without gen-fixtures),
    // fall back to an in-memory document so the benchmark still compiles and runs.
    let doc = if fixture.exists() {
        Document::open(&fixture).expect("failed to open prose.md fixture")
    } else {
        let fallback: String = std::iter::repeat(
            "The quick brown fox jumps over the lazy dog and the performance budget holds firm. ",
        )
        .take(200)
        .flat_map(|s| s.chars().chain(std::iter::once('\n')))
        .collect();
        let mut d = Document::new_empty();
        let cs = onda_core::transaction::ChangeSetBuilder::new(0)
            .insert(fallback)
            .build();
        d.apply(&Transaction::new(cs)).unwrap();
        d
    };

    c.bench_function("soft_wrap_render_line0_80chars", |b| {
        b.iter(|| {
            // Simulate what a compositor does: find the char range for line 0,
            // then read up to 80 chars — the typical terminal column width.
            let line_start = doc.line_to_char(black_box(0));
            let line_end = doc.line_to_char(black_box(1));
            let available = (line_end - line_start).min(80);
            let slice_end = line_start + available;
            black_box(doc.rope().slice(line_start..slice_end).to_string())
        })
    });
}

criterion_group!(
    benches,
    bench_rope_char_to_line,
    bench_line_to_char,
    bench_soft_wrap_render
);
criterion_main!(benches);
