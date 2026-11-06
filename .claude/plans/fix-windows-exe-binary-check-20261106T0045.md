# fix-windows-exe-binary-check — download mode checks `bin/initdb` without `.exe` on Windows

Kickoff: P15 G (F-P14k-02, from InSync PR #202 "Known limits"). Base: `origin/fix/fsync-on` @ b80fc01. PR against `fix/fsync-on`.
Follow-up: InSync PR `chore-bump-pg-embed-pin` pinning the new fork sha.

## Problem

In download mode `PgAccess::new` builds `cache_dir/bin/initdb` (and `bin/pg_ctl`) with no exe suffix.
`pg_executables_cached()` opens `init_db_exe` as a file; on Windows the unpacked file is `initdb.exe`,
so the open fails, `acquisition_needed()` returns true, and an unbundled Windows dev build re-downloads
and re-unpacks PostgreSQL on every start. Local mode (`local_binaries_dir`) already appends
`std::env::consts::EXE_SUFFIX` and `validate_local_binaries_dir` does the same — three places, two rules.

## Approach

One helper, `PgAccess::executable_path(root, name)` → `root/bin/<name><EXE_SUFFIX>`, joined per
component, backed by `executable_path_with_suffix(root, name, suffix)` so both suffix cases are unit-testable
on any host. Used by:
- `new` for `pg_ctl_exe` and `init_db_exe`. In local mode `cache_dir` already *is* the install dir, so the
  local/download `match` collapses to two helper calls on `cache_dir`.
- `validate_local_binaries_dir` for the per-executable path (label stays `bin/<file>`).
- test helpers `access_for` / `fake_installation` so tests mirror production paths.

Why safe on macOS/Linux: suffix is `""` and `push("bin/initdb")` == `join("bin").join("initdb")` on Unix,
so paths are byte-identical. On Windows download mode the spawn path becomes `cache\bin\initdb.exe`
instead of `cache\bin/initdb` — the exact form local mode already ships with (InSync installs).

## Locked decisions

- LD-P15-G-001: Single helper for both modes + validation; no other behaviour change (DON'T widen).
- LD-P15-G-002: Suffix injected via `executable_path_with_suffix` for tests; production always uses `EXE_SUFFIX`.
- LD-P15-G-003: The Windows bug cannot be executed on macOS; proof = suffix-parameterised unit tests,
  a vertical test that `pg_executables_cached()` sees a fake installation built with the real suffix,
  and `cargo check --target x86_64-pc-windows-msvc --all-targets` as an extra (non-gating if the target's
  deps can't build) compile check.
- LD-P15-G-004: Never run `tests/` integration tests or anything calling `purge()` (real ~/Library/Caches/pg-embed).
  Download-mode `PgAccess::new` is not called in tests (it creates the real cache dir); tests use `access_for`.
- LD-P15-G-REVIEWBAR: BLOCKER only — Windows still re-downloads with binaries present, macOS/Linux
  behaviour changes, or the InSync pin breaks a build. Pre-existing upstream limits are NITs. Converge.

## Checklist

### Fork PR (Zhanjii/pg-embed)
- [x] 1. Tests first: `executable_path_with_suffix` with `""` → `root/bin/initdb`, with `".exe"` → `root/bin/initdb.exe`;
      `pg_executables_cached()` true for `access_for(fake_installation(&[]))` and false with `initdb` omitted. Sensitive: no
- [x] 2. Add `executable_path` / `executable_path_with_suffix` to `PgAccess`; use in `new` (drop the mode `match`,
      keep the "why .exe" comment, now explaining both modes), `validate_local_binaries_dir`, `access_for`, `fake_installation`. Sensitive: no
- [x] 3. Gate: `bash .claude/validate.sh` (fmt, build, lib tests, clippy vs baseline) + `cargo check --target x86_64-pc-windows-msvc --all-targets`. Sensitive: no
      Result: validate.sh passed (48/48 lib tests, no new clippy). Windows check did not complete on this Mac: the
      Homebrew `cargo`/`rustc` on PATH lacks the Windows std, and with rustup's toolchain forced first, `ring` and
      `bzip2-sys` build scripts need an MSVC C toolchain. Non-gating per LD-P15-G-003; no crate code was reached.
- [x] 4. CHANGELOG entry if the file tracks fork changes (check first). Sensitive: no — added under Unreleased → Fix.
- [ ] 5. /challenge-loop → /create-pr against `fix/fsync-on` → merge on local gate (no fork CI; merge-on-green granted). Record merge sha.

### InSync PR (Zhanjii/claude-code-hook-dashboard)
- [ ] 6. Worktree `/Volumes/Work/work/claude-code-hook-dashboard-chore-bump-pg-embed-pin`, branch `chore-bump-pg-embed-pin` from origin/main; brief plan file there. Sensitive: no
- [ ] 7. `insync-server/Cargo.toml`: `rev` → new sha; comment block names the new merge/PR and adds (g) Windows download-mode `.exe` check; `cargo update -p pg-embed` for Cargo.lock only. Sensitive: no
- [ ] 8. InSync `validate.sh` green → /challenge-loop → /create-pr → CI green in foreground → merge-on-green.
- [ ] 9. /report-back to `main:0.0` with new fork sha + InSync PR number.

## Out of scope
Any other upstream limit listed in fork PR #2 (half-unpacked tree, version check, zip magic bytes, etc.).
