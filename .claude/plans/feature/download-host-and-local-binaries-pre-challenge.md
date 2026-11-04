---
pre_challenge: true
method: challenge-loop
branch: feature/download-host-and-local-binaries
diff_hash: 30cb5065c676126ae1aa396c81bee7dbf2f84c784098efecd8a34c59d2a6f3ec
validation: passed
subdir_audit: passed
timestamp: 2026-11-04T23:06:21Z
iterations: 15
converged: true
---

## [CHALLENGE-LOOP] Summary

**Iterations:** 15
**Converged:** Yes. Iteration 15 returned zero new BLOCKER/WARNING/CONVENTION findings, and none were ASKED.
**Reviewer model:** Opus (pinned) for all 15 rounds. No round was docs-only.
**Total findings:** 35 actionable (0 BLOCKERs, 25 WARNINGs, 10 CONVENTIONs) plus NITs
**Fixed:** 33 | **Deferred:** 2 | **Asked (awaiting user):** 0

**Gate:** the canonical cargo sequence cannot run on this fork (`--all-features` fails at base 0ffc50b because the runtime features pull three renamed copies of sqlx; `Cargo.lock` is gitignored). The repo's committed `.claude/validate.sh` replaces it (LD-P14kpre-010). It runs fmt check, a build of all targets, `cargo test --lib`, and clippy with no new warnings against a committed upstream baseline. 6.0, every pre-round gate and 6j all passed. Final run: 43/43 lib tests.

**Review bar change (external directive, not chosen by this agent):** during round 14 the orchestrator sent an UPDATE on the owner's behalf, quoted: "A finding counts only if it is (a) in the three features' code paths (error_for_status on the download, the `host` mirror setting, `local_binaries_dir`) and (b) a realistic failure for a single-process caller acquiring once. Everything else, including pre-existing upstream concurrency or cancellation behaviour and teardown limits, is a NIT". It is recorded as LD-P14kpre-REVIEWBAR. Round 15's reviewer got the unchanged template, saw the full diff, and was given this severity bar.

### Per-Iteration Breakdown

#### Iteration 1
`challenge-round=1 reviewer_model=opus docs_only=false`
**New findings:** 0 BLOCKERs, 2 WARNINGs, 2 CONVENTIONs, 7 NITs
- [WARNING] src/pg_fetch.rs: credentials in a mirror `host` leak into logs and errors. FIXED (55ffd09): redacted everywhere.
- [WARNING] src/pg_access.rs: local mode could still download if the bundle vanished after `new`. FIXED (55ffd09): re-validates and never downloads.
- [CONVENTION] plan: LD-009 did not cover validate.sh or the baseline. FIXED (55ffd09): LD-010.
- [CONVENTION] plan: item 6 had a run-on sentence. FIXED (55ffd09).
- NITs taken: clippy-ran check in the gate, baseline regenerated at head, bounded error-body read, `#[test]` for a sync test.

#### Iteration 2
`challenge-round=2 reviewer_model=opus docs_only=false`
**New findings:** 0 BLOCKERs, 2 WARNINGs, 0 CONVENTIONs, 5 NITs
- [WARNING] src/pg_fetch.rs:55: the new public field breaks full struct literals and the CHANGELOG did not say so. FIXED (cc9282a).
- [WARNING] src/pg_access.rs:122: `share/` was not validated. FIXED (cc9282a). NITs taken: UTF-8 path rejection (protects the upstream `Drop` unwrap), test rename, constant placement.

#### Iteration 3
`challenge-round=3 reviewer_model=opus docs_only=false`
**New findings:** 0 BLOCKERs, 2 WARNINGs, 0 CONVENTIONs, 6 NITs
- [WARNING] src/pg_access.rs:71: local mode needed and populated the user cache dir. FIXED (cdd000e): `cache_dir` is the install dir, and `acquire_postgres` refuses in local mode.
- [WARNING] src/pg_access.rs:296: blocking stat calls in an async fn. FIXED (cdd000e): documented as deliberate.

#### Iteration 4
`challenge-round=4 reviewer_model=opus docs_only=false`
**New findings:** 0 BLOCKERs, 2 WARNINGs, 1 CONVENTION, 5 NITs
- [WARNING] src/postgres.rs:141: the local-mode guard read the mutable public `fetch_settings`. FIXED (eaf5b54): `PgAccess::uses_local_binaries()`.
- [WARNING] src/pg_access.rs:39: stale field docs, and `write_pg_zip` was unguarded. FIXED (eaf5b54).
- [CONVENTION] git log: commit subjects in the form `<branch> -- address challenge-loop iteration N findings`. DEFERRED: this is the exact format this skill's 6g prescribes; the squash/merge message will be imperative.

#### Iteration 5
`challenge-round=5 reviewer_model=opus docs_only=false`
**New findings:** 0 BLOCKERs, 2 WARNINGs, 0 CONVENTIONs, 5 NITs. **Duplicates:** 1 (commit subjects, deferred).
- [WARNING] src/pg_access.rs:79: the local-mode meaning of public `cache_dir` was not in the CHANGELOG. FIXED (167a717). Checked that InSync never reads `PgAccess`.
- [WARNING] src/pg_access.rs:164: split distro layouts are rejected. FIXED (167a717): documented in the README and the field doc.

#### Iteration 6
`challenge-round=6 reviewer_model=opus docs_only=false`
**New findings:** 0 BLOCKERs, 2 WARNINGs, 2 CONVENTIONs, 4 NITs. **Duplicates:** 2 (cache_dir: confirmed resolved, field doc strengthened; commit subjects).
- [WARNING] src/pg_access.rs:79: a relative `local_binaries_dir` depended on the process cwd. FIXED (12d7986): made absolute once, without `canonicalize`.
- [WARNING] src/pg_fetch.rs:160: the error body was logged at warn and could echo Authorization. FIXED (12d7986): moved to debug.
- [CONVENTION] plan: superseded LDs were not marked. FIXED (12d7986).
- [CONVENTION] src/pg_fetch.rs:33: no reason given for MAX_LOGGED_BODY_BYTES. FIXED (12d7986).

#### Iteration 7
`challenge-round=7 reviewer_model=opus docs_only=false`
**New findings:** 0 BLOCKERs, 2 WARNINGs, 0 CONVENTIONs, 6 NITs. **Duplicates:** 1.
- [WARNING] src/pg_fetch.rs:654: the debug body still leaked under RUST_LOG=debug (the iteration-6 fix was inadequate). FIXED (12449bf): the body is withheld when the request carried credentials.
- [WARNING] src/postgres.rs:154: a failed acquisition left InProgress set and waiters hung (pre-existing, now more reachable). FIXED (12449bf): status is reset and the waiter retries.

#### Iteration 8
`challenge-round=8 reviewer_model=opus docs_only=false`
**New findings:** 0 BLOCKERs, 1 WARNING, 0 CONVENTIONs, 4 NITs. **Duplicates:** 1.
- [WARNING] src/pg_access.rs:388: every waiter retried at once and raced on one zip. FIXED (8f5a2b7): atomic single-winner claim.

#### Iteration 9
`challenge-round=9 reviewer_model=opus docs_only=false`
**New findings:** 0 BLOCKERs, 1 WARNING, 0 new CONVENTIONs, 4 NITs. **Duplicates:** 2.
- [WARNING] src/pg_access.rs:416: the claim leaked on a query-only call or on cancellation. FIXED (dd0fc60): drop guard plus doc.
- [CONVENTION] debug-level body departs from the defensive-logging convention. FIXED by recording it in the plan (LD-019) and in the PR description.

#### Iteration 10
`challenge-round=10 reviewer_model=opus docs_only=false`
**New findings:** 0 BLOCKERs, 2 WARNINGs, 1 CONVENTION, 4 NITs. **Duplicates:** 1.
- [WARNING] src/postgres.rs:154: the guard was armed after an awaited lock. FIXED (1d37bf1).
- [WARNING] CHANGELOG: the `acquisition_needed` contract change was not listed as breaking. FIXED (1d37bf1).
- [CONVENTION] plan: stale test count. FIXED (1d37bf1).

#### Iteration 11
`challenge-round=11 reviewer_model=opus docs_only=false`
**New findings:** 0 BLOCKERs, 2 WARNINGs, 0 CONVENTIONs, 4 NITs. **Duplicates:** 1.
- [WARNING] src/postgres.rs:138: no vertical slice for a failed acquire. FIXED (ac7a657): test against a local 404 server with a tempdir cache.
- [WARNING] src/postgres.rs:162: a cancelled guard could remove a peer's entry. FIXED (ac7a657): owner ids, and an owner-checked failure reset.

#### Iteration 12
`challenge-round=12 reviewer_model=opus docs_only=false`
**New findings:** 0 BLOCKERs, 2 WARNINGs, 2 CONVENTIONs, 4 NITs. **Duplicates:** 1.
- [WARNING] src/postgres.rs:154: no end-to-end cancellation test. FIXED (6141b41).
- [WARNING] src/pg_access.rs:540: the unclaimed first-acquisition path can overwrite a retry claim, so the CHANGELOG overstated. FIXED (6141b41): wording narrowed and the exception documented.
- [CONVENTION] src/pg_access.rs:145: stale `PgAccess::new` doc. FIXED (6141b41).
- [CONVENTION] src/pg_fetch.rs:149: missing "deliberately debug" comment. FIXED (6141b41).

#### Iteration 13
`challenge-round=13 reviewer_model=opus docs_only=false`
**New findings:** 0 BLOCKERs, 2 WARNINGs, 0 new CONVENTIONs, 3 NITs. **Duplicates:** 2.
- [WARNING] src/postgres.rs: the cancellation test depended on timing. FIXED (b391390): accept-signal `select!`.
- [WARNING] src/pg_access.rs:202: the guard's runtime-teardown limit was not documented. FIXED (b391390): added to Not handled. NIT taken: error-body read timeout.

#### Iteration 14
`challenge-round=14 reviewer_model=opus docs_only=false`
**New findings:** 0 BLOCKERs, 1 WARNING, 2 CONVENTIONs, 2 NITs. **Duplicates:** 1.
- [WARNING] src/pg_access.rs:66: ownership was per instance, not per attempt (cancel, then same-instance retry, under lock contention). DEFERRED by external directive: the orchestrator's review-bar UPDATE arrived during this round and reclassified it as a NIT (multi-instance cancellation behaviour that InSync's single acquisition never exercises). The in-flight fix was stopped before it changed any file. The finding is listed as a known limit in the PR body.
- [CONVENTION] plan: stale validation results. FIXED (ba05138).
- [CONVENTION] rustdoc for `acquire_postgres`, `write_pg_zip` and `acquisition_needed`. FIXED (6a51248).

#### Iteration 15
`challenge-round=15 reviewer_model=opus docs_only=false`
**New findings:** 0 BLOCKERs, 0 WARNINGs, 0 CONVENTIONs, 7 NITs
**Converged.** No new actionable findings under LD-P14kpre-REVIEWBAR.

### Final Ledger

| # | Iter | Category | File:Line | Description | Status | Resolution |
|---|------|----------|-----------|-------------|--------|------------|
| 1 | 1 | WARNING | src/pg_fetch.rs | Mirror credentials leak to logs/errors | FIXED | 55ffd09 |
| 2 | 1 | WARNING | src/pg_access.rs | Local mode could download after the bundle vanished | FIXED | 55ffd09 |
| 3 | 1 | CONVENTION | plan | Gate not recorded as an LD | FIXED | 55ffd09 |
| 4 | 1 | CONVENTION | plan | Run-on checklist item | FIXED | 55ffd09 |
| 5 | 2 | WARNING | src/pg_fetch.rs:55 | Breaking change not in CHANGELOG | FIXED | cc9282a |
| 6 | 2 | WARNING | src/pg_access.rs:122 | `share/` not validated | FIXED | cc9282a |
| 7 | 3 | WARNING | src/pg_access.rs:71 | Local mode used the user cache dir | FIXED | cdd000e |
| 8 | 3 | WARNING | src/pg_access.rs:296 | Blocking stat in async | FIXED | cdd000e (documented) |
| 9 | 4 | WARNING | src/postgres.rs:141 | Guard read mutable public settings | FIXED | eaf5b54 |
| 10 | 4 | WARNING | src/pg_access.rs:39 | Stale docs; `write_pg_zip` unguarded | FIXED | eaf5b54 |
| 11 | 4 | CONVENTION | git log | Process-style commit subjects | DEFERRED | Format prescribed by challenge-loop 6g; merge message will be imperative |
| 12 | 5 | WARNING | src/pg_access.rs:79 | Local-mode `cache_dir` meaning not in CHANGELOG | FIXED | 167a717 |
| 13 | 5 | WARNING | src/pg_access.rs:164 | Split distro layouts rejected silently | FIXED | 167a717 |
| 14 | 6 | WARNING | src/pg_access.rs:79 | Relative path depended on cwd | FIXED | 12d7986 |
| 15 | 6 | WARNING | src/pg_fetch.rs:160 | Error body at warn could echo Authorization | FIXED | 12d7986 (superseded by #18) |
| 16 | 6 | CONVENTION | plan | Superseded LDs unmarked | FIXED | 12d7986 |
| 17 | 6 | CONVENTION | src/pg_fetch.rs:33 | Constant lacked a reason | FIXED | 12d7986 |
| 18 | 7 | WARNING | src/pg_fetch.rs:654 | Debug body still leaked credentials | FIXED | 12449bf |
| 19 | 7 | WARNING | src/postgres.rs:154 | Failed acquisition left InProgress | FIXED | 12449bf |
| 20 | 8 | WARNING | src/pg_access.rs:388 | All waiters retried at once | FIXED | 8f5a2b7 |
| 21 | 9 | WARNING | src/pg_access.rs:416 | Claim leak on query/cancel | FIXED | dd0fc60 |
| 22 | 10 | WARNING | src/postgres.rs:154 | Guard armed after an await | FIXED | 1d37bf1 |
| 23 | 10 | WARNING | CHANGELOG | `acquisition_needed` contract change unlisted | FIXED | 1d37bf1 |
| 24 | 10 | CONVENTION | plan | Stale test count | FIXED | 1d37bf1 |
| 25 | 11 | WARNING | src/postgres.rs:138 | No vertical slice for failed acquire | FIXED | ac7a657 |
| 26 | 11 | WARNING | src/postgres.rs:162 | Guard could remove a peer's entry | FIXED | ac7a657 |
| 27 | 12 | WARNING | src/postgres.rs:154 | No end-to-end cancellation test | FIXED | 6141b41 |
| 28 | 12 | WARNING | src/pg_access.rs:540 | Retry guarantee overstated | FIXED | 6141b41 (documented) |
| 29 | 12 | CONVENTION | src/pg_access.rs:145 | Stale `new` doc | FIXED | 6141b41 |
| 30 | 12 | CONVENTION | src/pg_fetch.rs:149 | Missing deliberate-debug comment | FIXED | 6141b41 |
| 31 | 13 | WARNING | src/postgres.rs | Timing-dependent cancellation test | FIXED | b391390 |
| 32 | 13 | WARNING | src/pg_access.rs:202 | Teardown limit undocumented | FIXED | b391390 (documented) |
| 33 | 14 | WARNING | src/pg_access.rs:66 | Ownership per instance, not per attempt | DEFERRED | Reclassified NIT by orchestrator review-bar UPDATE (LD-P14kpre-REVIEWBAR); known limit in PR body |
| 34 | 14 | CONVENTION | plan | Stale validation results | FIXED | ba05138 |
| 35 | 14 | CONVENTION | src/postgres.rs:135 | Public API rustdoc incomplete | FIXED | 6a51248 |

### Outstanding questions (ASKED, still unresolved when the run ended)
- None.

### NITs (non-blocking, across all iterations; known limits for the PR body)
- Ownership is per instance, not per attempt: a cancel followed by an immediate same-instance retry under lock contention can let a peer overlap (iteration 14, #33).
- A `host` without a scheme gives `<unparseable download url>` in the message; reqwest's source error still names the problem (iteration 15).
- The bounded body read can overshoot by one chunk before truncation; memory stays bounded (iteration 15).
- No log-capture test that a credentialed body is withheld; only the returned error is asserted (iteration 15).
- A `200 OK` HTML login page from a mirror is still unpacked as a zip; a zip magic-byte check is a follow-up (iterations 4 and 15).
- The unix execute check accepts any execute bit, not an exact `access(X_OK)` (iteration 8).
- Urls in logs are normalized by the url parser (iterations 6, 9 and 14; documented on `host`).
- Validation stats are blocking calls in async fns; negligible (iterations 2, 3, 7 and 13).
- The first-acquisition (`Undefined`) path is not claimed atomically (pre-existing); half-unpacked trees can be reused (pre-existing).
- The cancellation guard cannot reset the status outside a runtime or during runtime shutdown (process teardown only).
- The clippy multiset baseline can hide a new warning with identical text in the same file when an upstream instance disappears (documented in validate.sh).

### Strengths (across all iterations)
- The status check stops error pages before any disk write. The body read is bounded and time-limited, and a typed `DownloadFailure` names the redacted url and status (every round).
- Credentials are handled end to end: userinfo is redacted, bodies are withheld when credentialed, and reqwest's error source is pinned by a test (rounds 2, 4, 7 and 15).
- Local mode is fail-closed and validated before any side effect, decided only by `PgAccess`, and never writes into the user's install (rounds 4, 5 and 15).
- Backward compatible for InSync: no new error variants, a defaulted field, and a compile-guard test for the exact call shapes (every round).
- Tests use local one-shot servers and tempdirs with no network, never touch the real pg-embed cache, and include vertical slices through `PgEmbed::acquire_postgres` (rounds 11–15).
- The clippy-baseline gate keeps the fork rebase-clean without letting new warnings in (rounds 2, 7 and 15).
