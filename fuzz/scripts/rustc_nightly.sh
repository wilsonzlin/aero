#!/usr/bin/env bash
set -euo pipefail

# Prefer the pinned toolchain used by `fuzz/rust-toolchain.toml`, but fall back to the
# generic `nightly` toolchain when the pinned one isn't installed (e.g. when the
# environment overrides `RUSTUP_TOOLCHAIN`).

PINNED="nightly-2025-12-08"

if rustup run "${PINNED}" rustc -V >/dev/null 2>&1; then
  exec rustup run "${PINNED}" rustc "$@"
fi

exec rustup run nightly rustc "$@"

