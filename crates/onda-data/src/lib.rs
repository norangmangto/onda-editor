//! Data-file view engines for onda (Phase 5): CSV/TSV virtual tables and JSONL
//! record views.
//!
//! Both are *view* engines: pure parsing + layout/schema logic with no I/O and no
//! parallel data model — the rope remains the single source of truth, so edits are
//! ordinary text Transactions and the views can be toggled instantly.

pub mod csv;
pub mod jsonl;

pub use csv::{
    cell_index_at, cell_inner, cell_outer, column_cells_inner, column_layout, field_spans,
    next_cell_start, parse_fields, prev_cell_start, sniff, ColumnLayout, Dialect, FieldSpan,
};
pub use jsonl::{
    field_schema, minify, parse_record, pretty, record_diagnostic, round_trip, summary, FieldStat,
    JsonType, RecordDiagnostic,
};
