# Plan: pg-embed fork: download status errors, mirror host, local binaries

Branch: `feature/download-host-and-local-binaries` (worktree
`/Volumes/Work/work/pg-embed-feature-download-host-and-local-binaries`)
Base: `0ffc50b88febfd54e231d1ae7d1ef50017ff77ac`, the rev InSync pins (CREATE_NO_WINDOW + fsync on), NOT `origin/master`.
PR target: `Zhanjii/pg-embed`, base `fix/fsync-on` (it contains 0ffc50b). Never upstream `faokunega/pg-embed`.
Origin: InSync P14 rollout master plan (`claude-code-hook-dashboard/plans/2026-10-31-p14-rollout-master.md`),
"Operator pre-flight" + P14k row, finding A243, LD-P14-009 (InSync bundles PG 15 and points pg-embed at it).

## Goal

1. A 4xx/5xx binary download becomes a typed error naming the URL and status. It is never written to disk or unpacked as a zip.
2. A configurable download host, so builds without the bundle can use a mirror. The current host stays the default.
3. A "use these binaries" path: point pg-embed at an existing PG 15 install directory. pg-embed validates it and uses it instead of downloading.

InSync's current call sites must compile and behave unchanged:
`PgFetchSettings { version: PG_V15, ..Default::default() }` and a full `PgSettings { .. }` struct literal.

## Locked decisions

- **LD-P14kpre-001** Base on 0ffc50b and open the PR against `fix/fsync-on`. The CREATE_NO_WINDOW (`command_executor.rs`, `pg_access.rs::stop_db_command_sync`) and fsync (`pg_commands.rs`) changes stay as they are.
- **LD-P14kpre-002** Clippy gate means **no new warnings**. At 0ffc50b, `cargo clippy --all-targets -- -D warnings` already fails with 48 errors, all in upstream code (`&PathBuf` params, redundant borrows, a manual `ToString` impl, and so on). We do not fix those (operator choice, 2026-11-04). This keeps the diff surgical and rebases onto upstream clean. The PR records the baseline count and shows that the count after the change is the same, with no warnings in new or changed lines. `cargo fmt --check` is clean at baseline and must stay clean.
- **LD-P14kpre-003** Local binaries support **a directory only** (operator choice). There is no local-archive mode. The new field is `PgFetchSettings.local_binaries_dir: Option<PathBuf>`, default `None`. It goes on `PgFetchSettings`, not `PgSettings`: InSync builds `PgFetchSettings` with `..Default::default()`, so a new field there is source-compatible, whereas a new `PgSettings` field would break InSync's full struct literal.
- **LD-P14kpre-004** "Configurable host" is the **existing** `PgFetchSettings.host` field (default `https://repo1.maven.org`). We keep it and make it robust and tested. We do not add a second setting. The URL building moves into a pure `pub fn download_url(&self) -> String`. A trailing `/` on the host is trimmed, so `https://mirror/` does not produce `//maven2`. A mirror must serve the Maven layout `<host>/maven2/io/zonky/test/postgres/embedded-postgres-binaries-<platform>/<version>/embedded-postgres-binaries-<platform>-<version>.jar`. This is documented on the field and in the README.
- **LD-P14kpre-005** No new `PgEmbedErrorType` variants. The enum is exhaustive, and a new variant would break any downstream exhaustive `match`. HTTP status failures use `DownloadFailure` with `message` naming the URL and status. A missing or invalid local binaries dir uses `InvalidPgPackage` with `message` naming the dir and every missing executable.
- **LD-P14kpre-006** Local-dir validation happens in `PgAccess::new`, which `PgEmbed::new` calls, so the error surfaces at construction time, before any download or initdb. The check is that `bin/initdb`, `bin/pg_ctl` and `bin/postgres` (each with `std::env::consts::EXE_SUFFIX`) exist as files. All missing ones are reported in one message. The version is not checked (out of scope, see Not handled).
- **LD-P14kpre-007** In local mode, `PgAccess.pg_ctl_exe` and `init_db_exe` point into `local_binaries_dir/bin`. `acquisition_needed()` then returns `false` through the existing `pg_executables_cached()` check (initdb exists), so `setup()` never downloads. No new branch in `PgEmbed::setup` is needed. The cache directory is still created as before; that is harmless and keeps the diff small. *(Superseded: LD-012 re-validates in `acquisition_needed()` and returns `false` directly; LD-014 makes local mode use no cache directory.)*
- **LD-P14kpre-008** On a non-success status, read the body best-effort and log it at `warn` with the URL and status, truncated to 512 bytes (an HTML error page, never secrets). Then return the error. The URL is logged at `debug` before the request (defensive-logging convention).
- **LD-P14kpre-009** No `/repo-setup` and no CLAUDE.md for the fork. It is an upstream fork we want to stay rebase-clean, and the kickoff scope is the fork change only. The only non-source additions are this plan file and the gate in LD-P14kpre-010.
- **LD-P14kpre-010** The pre-PR gate is a committed `.claude/validate.sh`, which the canonical validation helper runs in place of its default cargo sequence. The default cannot run on this fork: `--all-features` fails at 0ffc50b because the `rt_*` features pull three renamed copies of sqlx; `Cargo.lock` is gitignored, so `--locked` has nothing to lock; and there is no sqlx offline data. The script runs fmt check, a build of all targets and `cargo test --lib`. It fails if clippy fails to run, or on any clippy warning not in `.claude/clippy-baseline.txt` (line numbers stripped, compared as a multiset). That baseline is the upstream warning list, regenerated at the branch head so lines this branch removed cannot quietly return. Integration tests are excluded from the gate: they download from Maven, and `postgres_tokio` purges the user's real pg-embed cache.
- **LD-P14kpre-011** Credentials in `host` (`https://user:token@mirror`) are sent by reqwest as basic auth but redacted from every log line and error message (challenge-loop round 1).
- **LD-P14kpre-012** In local mode `acquisition_needed()` re-validates the directory and never returns `true`, so a bundle that disappears after `PgEmbed::new` gives `InvalidPgPackage` instead of a silent download into the cache (challenge-loop round 1).
- **LD-P14kpre-013** Local-dir validation also requires a `share/` directory (initdb reads `postgres.bki` and timezone data from it). Only the directory is checked, because layouts differ: zonky puts files directly in `share/`, distro builds use `share/postgresql/...`. A path that is not valid UTF-8 is rejected, because upstream `stop_db_command_sync` calls `to_str().unwrap()` from `Drop`. The CHANGELOG notes that a full `PgFetchSettings` struct literal must add the new field (challenge-loop round 2).
- **LD-P14kpre-014** Supersedes the "cache directory is still created" part of LD-007. In local mode `PgAccess.cache_dir` is the local install dir. No cache directory is created and `dirs::cache_dir()` is not needed, so a host without one still works and unit tests leave the real cache alone. `PgEmbed::acquire_postgres` returns `InvalidPgPackage` in local mode, so an explicit call can never write a zip into the install or unpack over it. `host` is documented as a repository base url that may carry a path prefix (Artifactory/Nexus remotes), with `/maven2/...` always appended (challenge-loop round 3).
- **LD-P14kpre-015** Whether local mode is on is decided only by `PgAccess`'s private copy (`uses_local_binaries()`), never by the mutable public `PgEmbed.fetch_settings`. Both `acquire_postgres` and the public `write_pg_zip` refuse in local mode (challenge-loop round 4).
- **LD-P14kpre-016** A relative `local_binaries_dir` is made absolute against the current directory once, in `PgAccess::new`, so later spawns (including the `Drop`-time `pg_ctl stop`) do not depend on the process cwd. It is not canonicalized, because `canonicalize` yields `\\?\` paths on Windows. On a non-success status the body is logged at `debug` and only the url and status at `warn`, because mirror error pages can echo the `Authorization` header. Only userinfo is redacted; tokens in a path or query string are not (challenge-loop round 6).

## Checklist

- [x] 1. **Download URL + status error** (`src/pg_fetch.rs`). Sensitive: no
  - Add `pub fn download_url(&self) -> String` (host trailing-`/` trimmed; same path as today).
  - `fetch_postgres`: `log::debug!` the URL; transport error → `DownloadFailure` with message `"failed to download PostgreSQL binaries from {url}"` + source; non-success status → best-effort body, `log::warn!(url, status, body≤512B)`, return `DownloadFailure` with message `"downloading PostgreSQL binaries from {url} failed with HTTP status {status}"`; body-read failure → `ConversionFailure` with message naming the URL.
  - Doc comment on `host` that explains the Maven-layout requirement for mirrors.
- [x] 2. **`local_binaries_dir` setting** (`src/pg_fetch.rs`). Sensitive: no
  - New `pub local_binaries_dir: Option<PathBuf>` with a doc comment (PG 15 tree containing `bin/`; must match `version`; skips download). `Default` → `None`.
- [x] 3. **Local-dir resolution + validation** (`src/pg_access.rs`). Sensitive: no
  - In `PgAccess::new`, when `local_binaries_dir` is `Some(dir)`: validate (LD-006), then set `pg_ctl_exe = dir/bin/pg_ctl`, `init_db_exe = dir/bin/initdb`. Otherwise unchanged.
  - A small private helper `fn validate_local_binaries_dir(dir: &Path) -> PgResult<()>` so it is unit-testable.
  - *(Updated by challenge-loop: validation also requires `share/` and a UTF-8 path (LD-013); a relative dir is made absolute first (LD-016); local mode uses no cache directory and `cache_dir` is the install dir (LD-014); the local-mode decision comes from `PgAccess` only (LD-015).)*
- [x] 4. **Tests**. Sensitive: no
  - Note: local-mode `pg_ctl_exe`/`init_db_exe` carry `EXE_SUFFIX` (unlike the cache paths) because `pg_executables_cached()` opens `init_db_exe` as a file; without `.exe` a Windows local install would be seen as "not cached" and trigger a download. The cache-path behavior is unchanged.
  - Add dev-dependency `tempfile = "3"`.
  - `pg_fetch` unit tests: default-host URL for a fixed os/arch/`PG_V15`; custom host; host with trailing `/`.
  - `pg_fetch` unit tests with a one-shot local HTTP server (a `tokio::net::TcpListener` on `127.0.0.1:0` writing a canned response, no new deps): 404 → `Err(DownloadFailure)` whose message contains the URL and `404`; 500 likewise; 200 → `Ok` bytes equal to the body. No real network.
  - `pg_access` tests with a `tempfile::TempDir` holding fake `bin/{initdb,pg_ctl,postgres}{EXE_SUFFIX}`: valid dir → `PgAccess::new` OK, exe paths point into the dir, `acquisition_needed()` is `false`; missing `postgres` → `Err(InvalidPgPackage)` naming it; nonexistent dir → `Err(InvalidPgPackage)`.
  - Vertical slice: `PgEmbed::new(settings, PgFetchSettings { local_binaries_dir: Some(fake), ..Default::default() })`, then assert `pg_access.acquisition_needed()` is `false` and the exe paths point into the fake dir. This proves the setting flows API → access layer with no download.
  - Compile check that InSync's exact call shape still builds (a test constructing `PgFetchSettings { version: PG_V15, ..Default::default() }` and a full `PgSettings` literal).
- [x] 5. **Docs**: README section on `host` (mirror layout) and `local_binaries_dir`; CHANGELOG entry under Unreleased. Sensitive: no
- [x] 6. **Validate**: `cargo fmt --check`; `cargo clippy --all-targets -- -D warnings` (per LD-002: same 48 baseline errors, none in new code; also run without `-D` and diff the warning list against the baseline); `cargo test --lib` (new unit tests). Run the existing integration tests (`cargo test`) as they are today; they already download from Maven, and their result is recorded honestly. Sensitive: no
  - Results (2026-11-04): `cargo fmt --check` clean; clippy line-independent diff vs baseline: 0 added, 3 removed (old `format!` rewritten); `cargo test --lib` 15/15 pass.
  - Canonical helper `validation_log_run_or_skip` does not fit this fork: `cargo clippy --all-features` fails identically at 0ffc50b (`depends on crate sqlx v0.6.3 multiple times with different names`; the runtime features are mutually exclusive by design), `Cargo.lock` is gitignored (`--locked`), no `sqlx prepare`. Gated on this plan's validation instead (LD-002).
  - Integration tests (run with `HOME` redirected to a scratch dir, because `postgres_tokio` calls `PgAccess::purge()`, which deletes the whole `~/Library/Caches/pg-embed`, the cache the live InSync cluster runs from): `migration_tokio` and `postgres_tokio2` **fail then hang, identically at 0ffc50b**. Cause: upstream fixtures use `PG_V13` (13.9.0); Maven has no `darwin-arm64v8` 13.9.0 jar (404). At 0ffc50b the 404 HTML page is written as `darwin-arm64v8-13.9.0.zip` (A243 reproduced); on this branch it is a `DownloadFailure` naming the url and `404 Not Found`. The hang is pre-existing: `acquire_postgres` never clears `PgAcquisitionStatus::InProgress` on failure, so the other `#[serial]` tests in the process wait in `acquisition_needed()` forever. Not fixed here (follow-up). `postgres_tokio1` needs the non-default `rt_tokio` feature, so plain `cargo test` skips it.
  - Gate encoded as `.claude/validate.sh` (LD-P14kpre-010).
- [ ] 7. **PR + report-back**: push, open a PR against `fix/fsync-on` with fmt/clippy/test results. There is no CI in the fork, so the local gate decides. Merge-on-green is granted within Zhanjii/pg-embed. Report to the orchestrator (`main:0.0`): the merge sha InSync should pin, plus the setting names and defaults (`host` = `https://repo1.maven.org`, `local_binaries_dir` = `None`). Sensitive: no

## Not handled (intentional)

- Version check of the local binaries (for example running `postgres --version`). A wrong-version bundle fails in initdb/postgres with Postgres's own error. InSync controls what it bundles.
- Local archive (zip/jar) input (LD-003).
- A full download-URL override for mirrors with a non-Maven layout (LD-004). The mirror must mirror the layout.
- Retries or timeouts on the download (unchanged behavior).
- The `surf` / async-std feature paths. `pg_fetch` already uses `reqwest` unconditionally, so they are untouched.
- The 48 pre-existing clippy errors (LD-002).
- Pre-existing: a failed acquisition leaves `PgAcquisitionStatus::InProgress` set, so other `PgEmbed` instances in the same process wait forever. It does not affect InSync (one instance per process; the next launch is a new process). Follow-up candidate.
- A mirror that answers `200 OK` with an HTML login or SSO page (Artifactory/Nexus behind auth) is still written and unpacked as a zip, and the unpack fails with `InvalidPgPackage`. A zip magic-byte check would catch it earlier. Out of scope: this plan covers status codes only (challenge-loop round 4 NIT).
- Upstream integration fixtures pin `PG_V13`, which 404s on darwin-arm64 (pre-existing).
