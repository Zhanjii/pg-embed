# fix-unpack-staging-dir — a killed extraction never leaves a trusted tree (F-P15Q2-01)

P15 Wave B3. Base and PR target: `fix/fsync-on` (InSync's pinned line). Predecessor: pg-embed#4
(`5eb38785`, F-P15G-01), pinned by InSync #218. Follow-on: InSync PR
`chore-bump-pg-embed-pin-staging` (its own plan file in that repo).

## Problem

`unpack_postgres` extracts the tar straight into `cache_dir`, the same folder
`pg_executables_cached()` checks (`cache_dir/bin/initdb[.exe]`). pg-embed#4 invalidates the tree
when extraction *fails*, but a process *killed* mid-extraction (quit or crash on first start) runs
no error path. `bin/initdb` is usually the first entry, so it is already on disk and every later
start trusts the partial tree (LD-P15-Q2-003, Known limits of pg-embed#4).

## Design

1. **Extract into a staging dir.** `unpack_postgres` extracts the tar into
   `cache_dir/.unpack-staging` (constant `UNPACK_STAGING_DIR_NAME`), not into `cache_dir`. It sits
   inside `cache_dir`, so it is on the same volume and `rename` is a metadata move on
   macOS/Linux/Windows. `pg_executables_cached()` never looks inside it.
2. **Clean a leftover first.** Before extracting, `remove_dir_all` the staging dir (`NotFound` is
   fine). A leftover from a killed run is therefore ignored by the presence check and cleaned by
   the next acquire.
3. **Move into place, `bin` last.** After a successful extraction, every top-level entry of the
   staging dir is renamed into `cache_dir`. An existing destination entry (left by a run killed
   during this move phase, or by a pre-fix partial tree) is removed first: `remove_dir_all` for a
   dir, `remove_file` otherwise, since a dir rename onto an existing dir fails on Windows and onto
   a non-empty one on Unix. `bin` is moved after every other entry. `initdb` is the only presence
   marker, so it appears in `cache_dir` only once everything else is in place. A kill at any point
   before the `bin` rename leaves no `cache_dir/bin/initdb`, and the next start re-acquires. Then
   the now-empty staging dir is removed.
4. **Synchronous.** Cleanup, extraction and moves are all `std::fs`, with no await in between
   (the same cancellation reasoning as pg-embed#4).
5. **Errors.** A failed staging cleanup or a failed move is `UnpackFailure` with the path in
   `message`. A failed extraction keeps pg-embed#4's behaviour: the `.tar` is removed, the
   extraction error wins, and the staging dir is left for the next acquire to clean. The
   `init_db_exe` invalidation in `acquire_postgres_steps` stays as it is (now normally `NotFound`,
   kept as defence in depth).

## Locked decisions

- LD-P15-B3-001: Staging sits inside `cache_dir` (`cache_dir/.unpack-staging`), not beside it
  (`<version>.staging`). It is on the same volume, inside the folder pg-embed already owns, and
  no new path appears in the parent `pg-embed/<os>/<arch>/` tree that `purge()` and InSync's
  macOS uninstall treat as pg-embed's.
- LD-P15-B3-002: The on-disk layout and the presence check are unchanged
  (`cache_dir/bin/initdb[.exe]`, no completion marker file). An existing complete cache stays
  trusted with no re-extraction. This supersedes LD-P15-Q2-003's "no single rename is atomic"
  objection: atomicity is only needed for the presence marker, and moving `bin` last gives that.
- LD-P15-B3-003: Recursive deletes are confined to `cache_dir/.unpack-staging` and to the
  top-level names the archive itself provides inside `cache_dir`. Download mode only:
  `acquire_postgres` refuses local mode before any step runs, so a user's own install is never
  touched. Never `remove_dir_all(cache_dir)`.
- LD-P15-B3-004: The entry order is the archive's, except `bin` goes last. No hard-coded list of
  `lib`/`share`/`include`, so a release with an extra top-level dir still works.
- LD-P15-B3-005: `unpack_postgres`'s public signature is unchanged; the staging logic is
  internal to `pg_unpack.rs`.
- LD-P15-B3-REVIEWBAR: BLOCKER only: a killed or failed extraction still yields a trusted tree,
  a good archive now fails, an existing complete cache is re-extracted on every start, or the
  InSync pin breaks a build. Upstream limits are NITs. Converge.

## Known limits (stated, not fixed)

- A partial tree left by a pre-fix version (killed mid-extraction before this change) still has
  `bin/initdb` and stays trusted. Detecting it would need a completion marker, which would also
  re-extract every complete cache once; out of scope (LD-P15-B3-002). InSync release builds use
  the bundle (local mode) and never download, so only dev/headless download installs are affected.
- A kill after the `bin` rename but before the empty staging dir is removed leaves an empty
  `.unpack-staging` until the next acquire, which never comes for a complete cache. Harmless.
- On Windows, antivirus can briefly lock a freshly extracted file and make a rename fail. That
  surfaces as `UnpackFailure`, the status resets, and the next start retries.
- The `tests/` integration tests are not run: they call `purge()`, which deletes the real
  `~/Library/Caches/pg-embed`.

## Checklist

### Fork PR (Zhanjii/pg-embed → `fix/fsync-on`)

- [x] 1. Tests first; they must fail before the fix. Sensitive: no
  - `pg_unpack` unit test: the corrupt archive still returns `UnpackFailure` and leaves no
    `.tar`/`.txz`, and now leaves **no** `cache_dir/bin/initdb` (the existing test's premise
    flips: the partial tree is in staging).
  - `pg_unpack` unit test: the good archive unpacks to `cache_dir/bin/initdb` + `cache_dir/lib/libfake`
    (full length) and leaves no `.unpack-staging`, `.tar` or `.txz`.
  - `pg_unpack` unit test (killed during extraction): pre-seed `cache_dir/.unpack-staging/bin/initdb`
    plus a stale file, then `unpack_postgres` a good archive: `Ok`, the stale file is gone and the
    tree is complete.
  - `pg_unpack` unit test (killed during the move phase): pre-seed `cache_dir/lib/` holding a stale
    file plus a leftover staging dir, with no `cache_dir/bin`. A good archive unpacks `Ok` and
    `lib` is replaced (stale file gone, `libfake` full length).
  - `postgres.rs` vertical test: a cache holding only a leftover `.unpack-staging/bin/initdb<EXE_SUFFIX>`
    gives `pg_executables_cached()` false and `acquisition_needed()` true. Then `acquire_postgres()`
    with `serve_once("200 OK", good zip)` is `Ok`, `init_db_exe` exists, the staging dir is gone,
    and the status is `Finished`.
  - `pg_access` test: a complete `cache_dir/bin/initdb` next to a leftover staging dir still
    reads as cached (no re-extraction).
- [x] 2. `pg_unpack.rs`: staging cleanup, extraction into staging, then the bin-last move and
      staging removal (Design 1-5). Sensitive: yes (crash ordering)
      Note: if the `.tar` removal fails after a good extraction, the tree is still moved into
      place and `PgCleanUpFailure` is returned, so that error keeps meaning "cache dir untouched
      or complete" (the caller's skip-invalidation stays correct). If a move also fails, the
      move's `UnpackFailure` wins and the cleanup error is logged at warn.
- [x] 3. `postgres.rs`: comment touch-up only where it describes the partial tree; no logic
      change. Sensitive: no
- [x] 4. CHANGELOG Unreleased → Fix entry. Sensitive: no
- [x] 5. Gate: `bash .claude/validate.sh` (55/55 lib tests, 5 new) (fmt, build, lib tests, no new clippy vs baseline).
      Never run `tests/` or `purge()`. Sensitive: no
- [x] 6. (Done: top level `bin`, `lib`, `share` plus the zip, no staging dir, 18 `lib` symlinks
      with 0 broken, `initdb --version` = PostgreSQL 15.1; probe reverted, scratch deleted.)
      Real-archive check, read-only, in a scratch dir: download the PG 15.1.0 darwin-amd64
      zip from Maven into a scratch dir (never read or write `~/Library/Caches/pg-embed`) and run
      it through `unpack_postgres` with a temporary probe (reverted). Expect `Ok`, `bin/initdb`,
      the dylib symlinks resolving, and no `.unpack-staging`. Sensitive: no
- [ ] 7. `/challenge-loop` with the review bar above; PR against `fix/fsync-on`; merge on the
      local gate (no fork CI). Record the merge sha. Sensitive: no

### InSync pin PR (worktree of /Volumes/Work/work/claude-code-hook-dashboard, branch `chore-bump-pg-embed-pin-staging`)

- [ ] 8. Plan file in that repo. `insync-server/Cargo.toml`: set `rev` to the new fork sha; the
      comment names the new fork PR and merge sha; add item **(i)** ("a download-mode unpack
      extracts into a staging dir and moves `bin/` into place last, so a process killed mid-unpack
      leaves no trusted `bin/initdb` (F-P15Q2-01)"); the revert line becomes "(a)-(i)".
      `Cargo.lock`: only the pg-embed `source` line changes. Sensitive: no
- [ ] 9. In that same PR, remove the F-P15Q2-01 line from "### P15 open follow-ups" in
      `plans/2026-10-11-company-readiness-master.md`, with Edit only (never prettier). Sensitive: no
- [ ] 10. InSync `validate.sh`, `/challenge-loop`, PR, CI green watched in the foreground,
      merge-on-green. Sensitive: no
- [ ] 11. `/report-back` to `main:0.0` with the fork sha and the InSync PR number. Sensitive: no
