# Salvage: onda-git diff/blame logic

This directory preserves the git **diff** and **blame** computation logic from the
former `onda-git` core crate, which was removed from the workspace because built-in
git integration is **not** part of the editor core per the updated plans.

Git features return in **Phase 3** as a WASM reference plugin
(`git-blame-inline`, see `docs/plan/PHASE3_PLAN.md` T20.1). These files are kept as
a reference for that re-implementation — they are **not compiled** and **not part of
the build**.

## Contents

- `diff.rs` — per-line gutter-sign diff of buffer content vs `HEAD` (`LineSign`
  added/modified/deleted), plus `head_blob_bytes`.
- `blame.rs` — `git blame` per line, unified-diff hunks, and hunk staging/reset.

Both were implemented on top of the `git2` (libgit2) bindings. The Phase 3 plugin
will instead reach git through the host `vcs` interface (decided in the W17 WIT
review), not by linking libgit2 into the editor.

The full original crate (`status.rs`, `worker.rs`, `lib.rs`, the channel-based
editor worker, and tests) remains available in git history before the removal commit.
