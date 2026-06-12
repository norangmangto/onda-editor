use criterion::{black_box, criterion_group, criterion_main, Criterion};
use onda_core::{Document, Selection, Transaction};

fn bench_document_open(c: &mut Criterion) {
    // Create an in-memory document with 10k lines for the criterion benchmark
    let text: String = (0..10_000).map(|i| format!("Line {:08}: hello world\n", i)).collect();

    c.bench_function("document_open_10k_lines", |b| {
        b.iter(|| {
            let mut doc = Document::new_empty();
            let cs = onda_core::transaction::ChangeSetBuilder::new(0)
                .insert(black_box(text.clone()))
                .build();
            doc.apply(&Transaction::new(cs)).unwrap();
            black_box(doc.len_lines())
        })
    });
}

fn bench_document_apply_insert(c: &mut Criterion) {
    let mut doc = Document::new_empty();
    let text: String = (0..1000).map(|i| format!("Line {i}: hello world\n")).collect();
    let cs =
        onda_core::transaction::ChangeSetBuilder::new(0).insert(text).build();
    doc.apply(&Transaction::new(cs)).unwrap();

    c.bench_function("document_insert_middle", |b| {
        let len = doc.len_chars();
        b.iter(|| {
            let pos = len / 2;
            let cs = onda_core::transaction::ChangeSetBuilder::new(len)
                .retain(pos)
                .insert("X")
                .retain(len - pos)
                .build();
            let mut d = doc.rope().clone();
            cs.apply(&mut d).unwrap();
            black_box(d.len_chars())
        })
    });
}

fn bench_selection_map(c: &mut Criterion) {
    let sel = Selection::point(50_000);
    let cs = onda_core::transaction::ChangeSetBuilder::new(100_000)
        .retain(25_000)
        .insert("hello world")
        .retain(75_000)
        .build();

    c.bench_function("selection_map", |b| {
        b.iter(|| {
            let mapped = sel.map(black_box(&cs));
            black_box(mapped.primary().head)
        })
    });
}

criterion_group!(benches, bench_document_open, bench_document_apply_insert, bench_selection_map);
criterion_main!(benches);
