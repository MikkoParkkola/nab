#!/bin/sh
# Compile the vendored-crate lint cap and export it for later steps.
# GitHub Actions reads GITHUB_ENV. A local shell can capture the printed path.
set -eu
root=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
mkdir -p "$root/target"
out="$root/target/cap-vendor-lints"
if [ "${RUNNER_OS:-}" = "Windows" ]; then
    out="${out}.exe"
fi
rustc "$root/scripts/cap_vendor_lints.rs" -o "$out"
if [ -n "${GITHUB_ENV:-}" ]; then
    printf 'RUSTC_WRAPPER=%s\n' "$out" >> "$GITHUB_ENV"
fi
printf '%s\n' "$out"
