#!/usr/bin/env bash
# Pre-PR gate for this fork, replacing the canonical cargo sequence.
#
# Why not the canonical one: `--all-features` cannot build here (the rt_* runtime
# features pull three renamed copies of sqlx and are mutually exclusive by design),
# Cargo.lock is gitignored so `--locked` has nothing to lock to, and the crate has
# no sqlx offline data. Upstream code also carries clippy warnings (errors only under
# -D warnings) we deliberately do not fix, to keep the fork rebase-clean against
# faokunega/pg-embed, so the clippy step fails only on warnings that are NOT in the
# committed baseline.
# Known gap: the per-message multiset comparison hides a new warning whose text matches a
# baseline entry in the same file if an upstream instance disappears in the same change.
#
# The integration tests in tests/ are not run: they download binaries from Maven,
# and postgres_tokio calls PgAccess::purge(), which deletes the user's whole
# pg-embed cache (the one a live InSync install runs from).
set -euo pipefail

cd "$(dirname "$0")/.."

cargo fmt --check
cargo build --all-targets
cargo test --lib

# Line numbers are stripped so unrelated edits that shift upstream code do not
# show up as new warnings; comm compares the sorted lists as multisets.
# The full clippy message text is compared, so a toolchain upgrade that rewords lints
# makes every baseline entry look new: regenerate the baseline then, from upstream code only.
CLIPPY_BASELINE=".claude/clippy-baseline.txt"
clippy_output=$(cargo clippy --all-targets --message-format=short 2>&1) || {
  echo "validate.sh: cargo clippy failed to run" >&2
  printf '%s\n' "$clippy_output" >&2
  exit 1
}
current_clippy_warnings=$(printf '%s\n' "$clippy_output" \
  | grep -E '^(src|tests)/' | sed -E 's/:[0-9]+:[0-9]+:/:/' | LC_ALL=C sort || true)
new_clippy_warnings=$(LC_ALL=C comm -13 <(LC_ALL=C sort "$CLIPPY_BASELINE") <(printf '%s\n' "$current_clippy_warnings") | sed '/^$/d')
if [ -n "$new_clippy_warnings" ]; then
  echo "validate.sh: clippy warnings not in $CLIPPY_BASELINE:" >&2
  printf '%s\n' "$new_clippy_warnings" >&2
  exit 1
fi
echo "validate.sh: fmt, build, lib tests and clippy (no new warnings vs baseline) passed"
