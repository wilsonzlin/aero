#!/bin/bash
# Source this file to set recommended environment variables for Aero development.
# Usage: source scripts/agent-env.sh

# Rust/Cargo - balance speed vs memory
export CARGO_BUILD_JOBS=4
export CARGO_INCREMENTAL=1

# Reduce codegen parallelism per crate to limit memory spikes.
# Keep any existing RUSTFLAGS, but don't re-add codegen-units when sourced twice.
if [[ "${RUSTFLAGS:-}" != *"codegen-units="* ]]; then
  export RUSTFLAGS="${RUSTFLAGS:-} -C codegen-units=4"
  export RUSTFLAGS="${RUSTFLAGS# }"
fi

# Node.js - cap V8 heap to avoid runaway memory
export NODE_OPTIONS="--max-old-space-size=4096"

# Node.js version guard:
# Some agent environments can't easily install the repo's pinned `.nvmrc` Node version.
# If the major version doesn't match, enable the opt-in bypass for `check-node-version.mjs`
# so `cargo xtask` and friends can still run (it will emit a warning instead of failing).
if command -v node >/dev/null 2>&1; then
  repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
  if [[ -f "${repo_root}/.nvmrc" ]]; then
    expected_major="$(cut -d. -f1 "${repo_root}/.nvmrc" | tr -d '\r\n ' | head -n1)"
    current_major="$(node -p "process.versions.node.split('.')[0]" 2>/dev/null || true)"
    if [[ -n "${expected_major}" && -n "${current_major}" && "${current_major}" != "${expected_major}" ]]; then
      if [[ -z "${AERO_ALLOW_UNSUPPORTED_NODE:-}" ]]; then
        export AERO_ALLOW_UNSUPPORTED_NODE=1
      fi
    fi
  fi
fi

# Playwright - single worker to avoid memory multiplication
export PW_TEST_WORKERS=1

# Ensure enough file descriptors for Chrome/Playwright
ulimit -n 4096 2>/dev/null || true

echo "Aero agent environment configured:"
echo "  CARGO_BUILD_JOBS=$CARGO_BUILD_JOBS"
echo "  RUSTFLAGS=$RUSTFLAGS"
echo "  CARGO_INCREMENTAL=$CARGO_INCREMENTAL"
echo "  NODE_OPTIONS=$NODE_OPTIONS"
if [[ -n "${AERO_ALLOW_UNSUPPORTED_NODE:-}" ]]; then
  echo "  AERO_ALLOW_UNSUPPORTED_NODE=$AERO_ALLOW_UNSUPPORTED_NODE"
fi
echo "  PW_TEST_WORKERS=$PW_TEST_WORKERS"
