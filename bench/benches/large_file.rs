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

criterion_group!(benches, bench_rope_char_to_line, bench_line_to_char);
criterion_main!(benches);
