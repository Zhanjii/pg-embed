# fix-unpack-error-propagation — stop trusting a half-unpacked PostgreSQL tree (F-P15G-01)

P15 Q2. Base and PR target: `fix/fsync-on` (InSync's pinned line). Predecessor: pg-embed#3 (9eca3e3a).
Follow-on: InSync PR `chore-bump-pg-embed-pin-unpack` (its own plan file in that repo).

## Problem

`pg_unpack::unpack_postgres` runs `let _ = decompress_tar(&tar_file_path, &cache_dir);`. A tar
that fails mid-extraction (truncated download that still passed zip/xz, disk full, I/O error)
returns `Ok(())`. `acquire_postgres_steps` then marks the acquisition `Finished`. A partial tree
that already contains `bin/initdb[.exe]` passes `pg_executables_cached()` on every later start,
so pg-embed never repairs it. pg-embed#3 made Windows trust its cache too, so every platform is
now affected.

## Design

1. **Propagate.** `unpack_postgres` keeps the `decompress_tar` result. The intermediate `.tar` is
   still removed whatever the result, so a failure doesn't leave ~100 MB behind. Then it returns
   the tar error, which is already typed `UnpackFailure` (or `ReadFileError` if the tar can't be
   opened). If both the extraction and the `.tar` cleanup fail, the extraction error wins and the
   cleanup error is logged at warn.
2. **Invalidate the partial tree.** In `PgEmbed::acquire_postgres_steps`, if unpacking fails,
   delete `pg_access.init_db_exe`. `pg_executables_cached()` checks for that file, so with it
   gone the next start sees "not cached" and acquires again. `NotFound` is fine (extraction
   failed before reaching initdb). Any other removal error is logged at warn, and the unpack
   error is still returned.
3. **Status reset.** No new code. `acquire_postgres` already calls `mark_acquisition_failed` on
   any `Err` from `acquire_postgres_steps`, so peers waiting on the status stop waiting and
   retry, exactly as after a failed download (pg-embed#2).

## Locked decisions

- LD-P15-Q2-001: Invalidate by deleting the presence marker (`init_db_exe`), not with
  `remove_dir_all(cache_dir)`. The marker is exactly what `pg_executables_cached()` keys on, so
  deleting it alone is enough to force re-acquisition. A recursive delete in a path-taking
  function is a bigger hazard than a stale `lib/` that the next unpack overwrites. Download mode
  only: `acquire_postgres` refuses local mode before any step runs, so a user's own install is
  never touched.
- LD-P15-Q2-002: Invalidation lives in `postgres.rs` (`acquire_postgres_steps`), not in
  `pg_unpack.rs`. Only `PgAccess` knows the platform-suffixed path (`executable_path`, pg#3), so
  this avoids duplicating the path logic. `unpack_postgres`'s public signature is unchanged.
- LD-P15-Q2-003: No staging-dir-then-rename extraction. The tar unpacks several top-level dirs
  (`bin`, `lib`, `share`) into the cache root, so no single rename is atomic, and that would
  widen beyond error handling.
- LD-P15-Q2-004: The `.txz` / `.zip` leftovers from an earlier failing step (`unzip_txz` /
  `decompress_xz` errors) are left as upstream had them. That is out of scope and a NIT.
- LD-P15-Q2-005: Test fixtures are built in-test with `tar` 0.4 + `xz2` 0.1 as **dev-dependencies**
  (the versions `archiver-rs` 0.5 already pulls in, so no new crates resolve) plus the existing
  `zip` 0.5 dependency. No binary fixtures are committed.
- LD-P15-Q2-REVIEWBAR: BLOCKER only: a corrupt archive still yields a trusted tree, a good
  archive now fails, macOS/Linux/Windows paths change, or the InSync pin breaks a build.
  Pre-existing upstream limits are NITs. Converge.

## Known limits (stated, not fixed)

- A tar truncated exactly at an entry boundary, with no end-of-archive blocks, reads as a clean
  end of archive in the `tar` crate. Reaching that state needs an xz stream that decodes
  cleanly, and xz's integrity check plus the zip CRC make that implausible for a corrupted
  download.
- The integration tests in `tests/` are not run: they call `purge()`, which deletes the real
  `~/Library/Caches/pg-embed`.

## Checklist

### Fork PR (Zhanjii/pg-embed → `fix/fsync-on`)
- [ ] 1. Tests first (they must fail before the fix). A shared test helper builds a zip ⊃ `.txz`
      ⊃ tar whose first entry is `bin/initdb<EXE_SUFFIX>` (complete) and whose second entry is
      cut off mid-data. Sensitive: no
  - `pg_unpack` unit test: `unpack_postgres` on that zip returns `Err` with
    `UnpackFailure`, and the `.tar` intermediate is gone.
  - `pg_unpack` unit test: a well-formed archive still unpacks `Ok` with `bin/initdb` present and
    the `.txz` / `.tar` intermediates removed (good-archive regression guard).
  - `postgres.rs` vertical test (struct-literal `PgEmbed`, `access_for(tempdir)`, `serve_once("200 OK", zip)`):
    `acquire_postgres()` errs with `UnpackFailure`. Afterwards `init_db_exe` does not exist,
    `pg_executables_cached()` is false, the status is `Undefined`, and `acquisition_needed()`
    is true.
- [ ] 2. `pg_unpack.rs`: propagate the `decompress_tar` result after the `.tar` cleanup (Design 1). Sensitive: no
- [ ] 3. `postgres.rs`: delete `init_db_exe` when unpacking fails, then return the error (Design 2). Sensitive: no
- [ ] 4. `Cargo.toml` dev-deps `tar = "0.4"`, `xz2 = "0.1"`; CHANGELOG Unreleased → Fix entry. Sensitive: no
- [ ] 5. Gate: `bash .claude/validate.sh` (fmt, build, lib tests, no new clippy vs baseline). Never run `tests/` or `purge()`. Sensitive: no
- [ ] 6. `/challenge-loop` with the review bar above; PR against `fix/fsync-on`; merge on the local gate (no fork CI). Record the merge sha. Sensitive: no

### InSync pin PR (separate worktree of /Volumes/Work/work/claude-code-hook-dashboard, branch `chore-bump-pg-embed-pin-unpack`)
- [ ] 7. Plan file in that repo; `insync-server/Cargo.toml` `rev` → the new fork sha. The comment
      block names the fork PR and adds item **(h)** ("a failed unpack is reported and the partial
      tree is invalidated so the next start re-downloads"), and the revert line becomes "(a)-(h)".
      `Cargo.lock`: edit only the pg-embed `source` line, as in #206 (the new dev-deps don't
      affect consumers). Sensitive: no
- [ ] 8. InSync `validate.sh`, `/challenge-loop`, PR, CI green in the foreground, merge-on-green. Do not edit InSync `plans/*.md`. Sensitive: no
- [ ] 9. `/report-back` to `main:0.0` with the fork sha and the InSync PR number. Sensitive: no
