// Theme-switch latency bench (T18.1 gate: full re-render < 5ms).
//
// Measures the cost of loading a built-in theme and repainting a full 80x40 screen
// of document content with it — the work `:theme <name>` triggers (the compositor
// then diffs, but the worst case is a full grid paint).

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use onda_core::{Document, Selection, Transaction};
use onda_render::{theme::Theme, DocumentView, Grid, ModeIndicator, Viewport};

fn build_doc(lines: usize) -> Document {
    let text: String = (0..lines)
        .map(|i| format!("fn item_{i}() -> u32 {{ let x = {i}; x + 1 }}\n"))
        .collect();
    let mut doc = Document::new_empty();
    let cs = onda_core::transaction::ChangeSetBuilder::new(0)
        .insert(text)
        .build();
    doc.apply(&Transaction::new(cs)).unwrap();
    doc
}

fn bench_theme_switch(c: &mut Criterion) {
    let doc = build_doc(2000);
    let sel = Selection::point(0);
    let viewport = Viewport::new();
    let mut grid = Grid::new(80, 40);

    c.bench_function("theme_switch_full_render", |b| {
        b.iter(|| {
            // Load the theme (as `:theme` does) and repaint the whole viewport.
            let theme = Theme::builtin(black_box("onda-light")).unwrap();
            DocumentView::render_with_highlights(
                &mut grid,
                &doc,
                &sel,
                &viewport,
                ModeIndicator::Normal,
                0,
                40,
                None,
                &[],
                &theme,
            );
            black_box(grid.width())
        })
    });
}

criterion_group!(benches, bench_theme_switch);
criterion_main!(benches);
