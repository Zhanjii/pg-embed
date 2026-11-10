---
pre_challenge: true
method: challenge-loop
branch: fix-unpack-staging-dir
diff_hash: c12aa4d48d4141e7c0c1522ee16ec65b3d72dcda98929b26f24ea409857c6b0e
validation: passed
subdir_audit: passed
timestamp: 2026-11-10T21:25:00Z
iterations: 4
converged: true
---

## [CHALLENGE-LOOP] Summary

**Iterations:** 4
**Converged:** Yes
**Reviewer model:** Opus (pinned) for 1–4
**Total findings:** 6 (0 BLOCKERs, 5 WARNINGs, 1 CONVENTION, 14 NITs)
**Fixed:** 6 | **Deferred:** 0 | **Asked (awaiting user):** 0

Review bar (LD-P15-B3-REVIEWBAR): BLOCKER only: a killed or failed extraction still yields a
trusted tree, a good archive now fails, an existing complete cache is re-extracted on every
start, or the InSync pin breaks a build. Upstream limits are NITs.

### Per-Iteration Breakdown

#### Iteration 1
`challenge-round=1 reviewer_model=opus docs_only=false`
**New findings:** 0 BLOCKERs, 2 WARNINGs, 0 CONVENTIONs, 5 NITs
- [WARNING] src/pg_unpack.rs:190 — One fixed `.unpack-staging` path shared by every run: an overlapping in-process run's cleanup deleted a peer's half-written tree, which was then moved into place and trusted --> FIXED (6f0f960): per-run `.unpack-staging-<pid>-<seq>`, cleanup of other-pid leftovers only, process-wide lock on the move phase that removes the old `bin` first
- [WARNING] src/postgres.rs:189 — Leftover cleanup ran after the zip write, so a disk too full for the zip never freed it --> FIXED (6f0f960): cleanup at the start of `acquire_postgres_steps`; a failed run removes its own staging dir
- [NIT] src/pg_unpack.rs:116 — Windows directory symlink/junction at a destination needs `remove_dir`
- [NIT] plan Known limits — no fsync; power loss not covered (added to Known limits)
- [NIT] src/pg_unpack.rs:103 — `unpack_failure` lacked a doc comment (fixed in 6f0f960)
- [NIT] src/pg_unpack.rs:350 — killed-move test fixture did not model a real killed move (fixed in 6f0f960)
- [NIT] src/pg_access.rs:803 — the leftover-staging cached test is a regression guard only

#### Iteration 2
`challenge-round=2 reviewer_model=opus docs_only=false`
**New findings:** 0 BLOCKERs, 2 WARNINGs, 1 CONVENTION, 2 NITs
- [WARNING] src/pg_unpack.rs:249 — `remove_dir_all(cache_dir/bin)` is not atomic; a kill partway could leave `initdb` next to deleted binaries --> FIXED (cc584df): every replaced destination, `bin` first, is renamed into the run's own `.replaced-entries` and deleted there
- [WARNING] src/pg_unpack.rs:68 — cross-process overlap: the leftover cleanup can delete a live process's staging dir mid-extraction --> FIXED (cc584df): consequence stated in the doc comment and the plan's Known limits (cross-process overlap unsupported, as upstream: shared zip/.txz/.tar paths)
- [CONVENTION] plan Design 1-5 and checklist item 1 stale after iteration 1 --> FIXED (cc584df)
- [NIT] src/postgres.rs:208 — stale comment about keeping a failed extraction (fixed in cc584df)
- [NIT] src/pg_unpack.rs:24 — `AtomicU64` → `AtomicUsize` for 32-bit targets (fixed in cc584df)
- (also noted: a peer's running postgres on Windows during an in-process overlap — added to Known limits)

#### Iteration 3
`challenge-round=3 reviewer_model=opus docs_only=false`
**New findings:** 0 BLOCKERs, 1 WARNING, 0 CONVENTIONs, 4 NITs
- [WARNING] src/pg_unpack.rs:385 — no test for a move-phase failure after the old `bin` left --> FIXED (0f170aa): `failed_move_after_the_old_bin_left_leaves_no_initdb` (read-only prior `lib`, Unix)
- [NIT] pid reuse could extract into a killed process's same-name leftover (fixed in 0f170aa: `prepare_own_staging_dir`)
- [NIT] leftover cleanup stopped at the first undeletable entry (fixed in 0f170aa: warn and continue)
- [NIT] `.replaced-entries` deleted before the final `bin` rename (fixed in 0f170aa: deleted after, warn-only, lock released first)
- [NIT] src/postgres.rs:212 — a failure after the final `bin` rename invalidated a complete tree (fixed in 0f170aa: post-rename cleanup is warn-only)

#### Iteration 4
`challenge-round=4 reviewer_model=opus docs_only=false`
**New findings:** 0 BLOCKERs, 0 WARNINGs, 0 CONVENTIONs, 5 NITs
**Converged** — no new actionable findings.

### Final Ledger

| # | Iter | Category | File:Line | Description | Status | Resolution |
|---|------|----------|-----------|-------------|--------|------------|
| 1 | 1 | WARNING | src/pg_unpack.rs:190 | Shared staging path lets an overlapping run's cleanup produce a trusted partial tree | FIXED | 6f0f960 |
| 2 | 1 | WARNING | src/postgres.rs:189 | Leftover cleanup after the zip write | FIXED | 6f0f960 |
| 3 | 2 | WARNING | src/pg_unpack.rs:249 | Non-atomic removal of the old `bin` | FIXED | cc584df |
| 4 | 2 | WARNING | src/pg_unpack.rs:68 | Cross-process overlap consequence undocumented | FIXED | cc584df |
| 5 | 2 | CONVENTION | plan Design | Plan Design stale | FIXED | cc584df |
| 6 | 3 | WARNING | src/pg_unpack.rs:385 | No move-phase failure test | FIXED | 0f170aa |

### NITs (non-blocking, across all iterations)
- [NIT] src/pg_unpack.rs:116 — Windows directory symlink/junction destinations need `remove_dir` (iteration 1; destinations are pg-embed's own archive entries)
- [NIT] src/pg_access.rs:803 — leftover-staging cached test is a regression guard only (iteration 1)
- [NIT] src/pg_unpack.rs:276 — one comment line is 127 characters (iteration 4)
- [NIT] src/pg_unpack.rs:262 — an archive with no top-level `bin` would replace `lib`/`share` next to an old `bin`; unreachable with real PostgreSQL archives (iteration 4)
- [NIT] plan Known limits — a failed replacement of a complete cache costs a re-download rather than a rollback (iteration 4)
- [NIT] src/postgres.rs:192 — synchronous leftover `remove_dir_all` on the executor thread, after a kill only (iteration 4)
- [NIT] src/pg_unpack.rs:612 — the read-only-dir test returns early under root without a visible skip notice (iteration 4)

### Strengths (across all iterations)
- `initdb` is the only presence marker and the new `bin` is renamed in last, so no kill point leaves a trusted `initdb` next to an incomplete tree; the layout and the presence check are unchanged, so complete caches are never re-extracted (iterations 1-4)
- Error precedence is careful: the first error wins, cleanup failures are logged; `PgCleanUpFailure` still means "untouched or complete" (iterations 1-4)
- Leftover cleanup is narrow (other-pid exact-prefix names only, symlinks never followed) and runs before the download (iterations 2-4)
- Vertical tests through `acquire_postgres` with a local HTTP server, plus unit tests for every kill point and the real PG 15.1 archive check recorded in the plan (iterations 1-4)
