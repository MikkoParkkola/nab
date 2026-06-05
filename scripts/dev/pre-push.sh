#!/usr/bin/env bash
# pre-push gate — local fmt + lint + lib-test parity with CI.
# Documentation-only pushes skip the Rust gate, matching the docs-aware
# `changes` filter in .github/workflows/ci.yml so local and CI agree.
# Bypass: SKIP_PREPUSH=1 (logged, audit-only).
# Wall-clock budget on warm cache: <60s.
set -euo pipefail

if [[ "${SKIP_PREPUSH:-0}" == "1" ]]; then
  echo "WARN: pre-push bypassed via SKIP_PREPUSH=1" >&2
  exit 0
fi

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

# rustc/sccache write per-compile temp files under $TMPDIR. Some sandboxes
# wipe /tmp mid-build, which breaks the compiler cache with errors like
# "error writing dependencies to .../deps.d: No such file or directory".
# Pin a stable, repo-local temp dir so the gate is hermetic.
STABLE_TMP="${REPO_ROOT}/target/.prepush-tmp"
mkdir -p "$STABLE_TMP"
export TMPDIR="$STABLE_TMP"

# Decide whether code changed, relative to the integration base (origin/main).
# Mirrors the `code` filter in .github/workflows/ci.yml.
base="origin/main"
git rev-parse --verify --quiet "${base}" >/dev/null 2>&1 || base="$(git rev-parse HEAD)"
changed="$(git diff --name-only "${base}"...HEAD 2>/dev/null || true)"

code_changed=0
if [[ -z "${changed}" ]]; then
  code_changed=1   # cannot classify — run the gate to stay safe
else
  while IFS= read -r f; do
    [[ -z "${f}" ]] && continue
    case "${f}" in
      src/*|crates/*|tests/*|benches/*|build.rs|Cargo.toml|Cargo.lock|rust-toolchain*|.cargo/*|.github/workflows/*)
        code_changed=1
        break
        ;;
    esac
  done <<< "${changed}"
fi

if [[ -f Cargo.toml && "${code_changed}" == "1" ]]; then
  echo "[pre-push] cargo fmt --check"
  cargo fmt --all --check 2>&1 | tail -20 || { echo "FAIL: cargo fmt"; exit 1; }

  echo "[pre-push] cargo clippy --lib -D warnings"
  cargo clippy --lib --no-deps --quiet -- -D warnings 2>&1 | tail -20 || { echo "FAIL: clippy"; exit 1; }

  echo "[pre-push] cargo test --lib"
  cargo test --lib --quiet 2>&1 | tail -10 || { echo "FAIL: cargo test --lib"; exit 1; }
  gate_result="cargo fmt+clippy+test green"
else
  echo "[pre-push] docs-only change — skipping Rust gate (parity with CI)"
  gate_result="docs-only, Rust gate skipped"
fi

tip="$(git rev-parse HEAD)"
if ! git log -1 --pretty=%B | grep -q '^Local-Tested: '; then
  git -c trailer.ifexists=replace commit --amend --no-edit \
    --trailer "Local-Tested: ${gate_result} @ ${tip}" >/dev/null || true
fi

echo "[pre-push] OK"
