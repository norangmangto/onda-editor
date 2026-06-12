## Task ID

<!-- e.g. T1.2 — ChangeSet composition -->

## What changed

<!-- One paragraph summary -->

## Bench results

<!-- Required if this PR touches onda-core, onda-modal, onda-render, or the event loop.
     Run: cargo xtask bench --check
     Paste the summary table or write "N/A — no hot path touched" with justification. -->

```
cargo xtask bench --check
```

## New dependencies

<!-- List any new crates added to Cargo.toml. For each:
     - Why this library instead of writing it ourselves?
     - Binary size impact?
     - Compile time impact?
     - Maintenance status?
     If none, write "None". -->

## Acceptance criteria checklist

<!-- Copy the criteria from docs/PHASE0_PLAN.md for the task ID above and tick them. -->

- [ ] ...

## Tests added / updated

- [ ] `cargo test --workspace` passes
- [ ] New behaviour has a regression test
