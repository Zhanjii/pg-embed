---
pre_challenge: true
method: challenge-loop
branch: fix-unpack-error-propagation
diff_hash: f8a5e8b9a947d0a432fb170f2b23e399ab2a23b7a50a22862e3b81632844a255
validation: passed
subdir_audit: passed
timestamp: 2026-11-06T15:23:09Z
iterations: 3
converged: true
---

## [CHALLENGE-LOOP] Summary

**Iterations:** 3
**Converged:** Yes
**Reviewer model:** Opus (pinned) for iterations 1–3; no Sonnet rounds (code changed)
**Total findings:** 17 (0 BLOCKERs, 3 WARNINGs, 0 CONVENTIONs, 14 NITs)
**Fixed:** 2 | **Deferred:** 1 | **Asked (awaiting user):** 0

Review bar: LD-P15-Q2-REVIEWBAR. BLOCKER only: a corrupt archive still yields a trusted tree, a
good archive now fails, macOS/Linux/Windows paths change, or the InSync pin breaks a build.
Reviewers diffed against the PR base `origin/fix/fsync-on`.

### Per-Iteration Breakdown

#### Iteration 1
`challenge-round=1 reviewer_model=opus docs_only=false`
**New findings:** 0 BLOCKERs, 1 WARNING, 0 CONVENTIONs, 4 NITs
- [WARNING] src/pg_unpack.rs:111 — Propagating tar errors could make a good archive fail on a
  platform-specific entry (Windows symlinks need Developer Mode) --> DEFERRED: not an issue,
  verified. Read-only listing of the real 15.1.0 archives: windows-amd64 has 1425 files + 65 dirs
  and no symlinks or hard links. linux-amd64 has 14 symlinks and darwin-amd64 has 18; both are
  native on Unix. A temporary, reverted probe ran the real darwin-amd64 archive through the
  branch's `unpack_postgres` in a scratch dir: `Ok`, with `bin/initdb` and the dylib symlinks present.
- [NIT] src/pg_unpack.rs:111 — tar 0.4 doesn't overwrite existing hard links on retry (no archive has any)
- [NIT] src/postgres.rs:194 — initdb is invalidated on any unpack failure, including pre-tar ones (self-healing re-download)
- [NIT] src/pg_unpack.rs:121 — no test for the case where both extraction and cleanup fail
- [NIT] src/pg_unpack.rs:161 — the fixture's inner file name says darwin on every platform

#### Iteration 2
`challenge-round=2 reviewer_model=opus docs_only=false`
**New findings:** 0 BLOCKERs, 2 WARNINGs, 0 CONVENTIONs, 4 NITs
- [WARNING] src/postgres.rs:191 / src/pg_unpack.rs:114 — A cancellation (a timeout around
  setup()) could drop the future at the async `.tar` removal, after a failed extraction but
  before the initdb invalidation, leaving a trusted partial tree --> FIXED (a1dc7e1): the `.tar`
  removal and the initdb removal are now synchronous, so no await sits between the failed
  extraction and the invalidation.
- [WARNING] src/postgres.rs:191 — A `PgCleanUpFailure` after a successful extraction deleted a
  good initdb, which could mean a re-download on every start if a Windows AV holds the `.tar` -->
  FIXED (a1dc7e1): invalidation is skipped for `PgCleanUpFailure` (the tree is untouched or complete).
- [NIT] plan Known limits — a process killed mid-extraction still leaves a trusted tree (added to the plan's known limits in a1dc7e1)
- [NIT] src/postgres.rs:191 — cross-process race on a shared cache dir (pre-existing upstream)
- [NIT] src/pg_unpack.rs:121 — the `match` could be an `if let`
- [NIT] src/pg_unpack.rs:157 — name the 512-byte tar block size

#### Iteration 3
`challenge-round=3 reviewer_model=opus docs_only=false`
**New findings:** 0 BLOCKERs, 0 WARNINGs, 0 CONVENTIONs, 4 NITs
**Duplicates of prior findings (confirmed resolved):** 2 (cancellation safety and the PgCleanUpFailure skip, both cited as strengths)
- [NIT] src/postgres.rs:200 — the `PgCleanUpFailure` skip relies on an unpack_postgres invariant; document it
- [NIT] src/postgres.rs:200 — the `PgCleanUpFailure` branch is untested (needs a filesystem fault)
- [NIT] src/pg_unpack.rs:96 — the `unpack_postgres` doc comment could describe the new contract
- [NIT] src/postgres.rs:194 — a direct `acquire_postgres()` call over a complete cache whose download is bad drops initdb (re-download only)
**Converged** — no new actionable findings.

### Final Ledger

| # | Iter | Category | File:Line | Description | Status | Resolution |
|---|------|----------|-----------|-------------|--------|------------|
| 1 | 1 | WARNING | src/pg_unpack.rs:111 | Windows symlink entries could make a good archive fail | DEFERRED | Verified: Windows 15.1.0 archive has no links; real darwin archive unpacks Ok |
| 2 | 2 | WARNING | src/postgres.rs:191 | Cancellation between failed extraction and invalidation | FIXED | a1dc7e1 |
| 3 | 2 | WARNING | src/postgres.rs:191 | Cleanup failure after good extraction deletes good initdb | FIXED | a1dc7e1 |

### Outstanding questions (ASKED, still unresolved when the run ended)
- None.

### NITs (non-blocking, across all iterations)
- [NIT] src/pg_unpack.rs:111 — tar crate doesn't overwrite hard links on retry (iteration 1)
- [NIT] src/postgres.rs:194 — invalidation on pre-tar unpack failures is a harmless re-download (iterations 1, 3)
- [NIT] src/pg_unpack.rs:121 — no test where both extraction and cleanup fail (iteration 1)
- [NIT] src/pg_unpack.rs:161 — fixture inner file named darwin on all platforms (iteration 1)
- [NIT] plan — process killed mid-extraction leaves a trusted tree; now stated in Known limits (iteration 2)
- [NIT] src/postgres.rs:191 — cross-process race on a shared cache dir, pre-existing upstream (iteration 2)
- [NIT] src/pg_unpack.rs:121 — `match` readability (iteration 2)
- [NIT] src/pg_unpack.rs:157 — magic 512 (iteration 2)
- [NIT] src/postgres.rs:200 — document the PgCleanUpFailure invariant at the call site (iteration 3)
- [NIT] src/postgres.rs:200 — PgCleanUpFailure branch untested (iteration 3)
- [NIT] src/pg_unpack.rs:96 — unpack_postgres doc comment (iteration 3)

### Strengths (across all iterations)
- The extraction error takes priority over a cleanup error, and the cleanup error is logged at warn rather than swallowed (iterations 1–3)
- Invalidation deletes only the presence marker `init_db_exe`, so no recursive delete; it is download mode only (iterations 1, 3)
- The `write_pg_zip` flush fixes a real tokio File race, with the stress evidence recorded in LD-P15-Q2-006 (iterations 1–3)
- Tests came first, and a vertical test runs serve_once → write → unpack → invalidation → status reset → acquisition_needed; fixtures are in memory and the dev-deps are already in the tree (iterations 1–3)
- Cancellation safety: no await between a failed extraction and the invalidation (iteration 3)
