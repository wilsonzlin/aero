#!/usr/bin/env bash
set -euo pipefail

# Prefer the pinned toolchain declared by `fuzz/rust-toolchain.toml`, but fall back to the
# generic `nightly` toolchain when:
# - the pinned toolchain isn't installed, or
# - the file can't be parsed.

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
PINNED=""
if [[ -f "${ROOT}/rust-toolchain.toml" ]]; then
  # rust-toolchain.toml format: `channel = "nightly-YYYY-MM-DD"`
  PINNED="$(awk -F'"' '/^channel[[:space:]]*=[[:space:]]*"/ { print $2; exit }' "${ROOT}/rust-toolchain.toml" || true)"
fi

if [[ -n "${PINNED}" ]] && rustup run "${PINNED}" rustc -V >/dev/null 2>&1; then
  exec rustup run "${PINNED}" rustc "$@"
fi

exec rustup run nightly rustc "$@"
