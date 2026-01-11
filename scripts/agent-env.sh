#!/bin/bash
# Source this file to set recommended environment variables for Aero development.
# Usage: source scripts/agent-env.sh

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Cargo registry cache contention can be a major slowdown when many agents share
# the same host (cargo prints: "Blocking waiting for file lock on package cache").
# Opt into a per-checkout Cargo home to avoid that contention.
#
# Usage:
#   export AERO_ISOLATE_CARGO_HOME=1                     # use "$REPO_ROOT/.cargo-home"
#   export AERO_ISOLATE_CARGO_HOME="$REPO_ROOT/.cargo-home" # equivalent explicit path
#   export AERO_ISOLATE_CARGO_HOME="/tmp/aero-cargo-home" # custom directory
#   source scripts/agent-env.sh
#
# Note: this intentionally overrides any pre-existing `CARGO_HOME` so the isolation
# actually takes effect.
case "${AERO_ISOLATE_CARGO_HOME:-}" in
  "" | 0 | false | FALSE | no | NO | off | OFF)
    ;;
  1 | true | TRUE | yes | YES | on | ON)
    export CARGO_HOME="$REPO_ROOT/.cargo-home"
    mkdir -p "$CARGO_HOME"
    ;;
  *)
    custom="$AERO_ISOLATE_CARGO_HOME"
    # Expand the common `~/` shorthand (tilde is not expanded inside variables).
    if [[ "$custom" == "~"* ]]; then
      custom="${custom/#\~/$HOME}"
    fi
    # Treat non-absolute paths as relative to the repo root so the behavior is stable
    # even when sourcing from a different working directory.
    if [[ "$custom" != /* ]]; then
      custom="$REPO_ROOT/$custom"
    fi
    export CARGO_HOME="$custom"
    mkdir -p "$CARGO_HOME"
    ;;
esac

# Rust/Cargo - balance speed vs memory
export CARGO_BUILD_JOBS=4
export CARGO_INCREMENTAL=1

# Reduce codegen parallelism per crate to limit memory spikes.
# Keep any existing RUSTFLAGS, but don't re-add codegen-units when sourced twice.
if [[ "${RUSTFLAGS:-}" != *"codegen-units="* ]]; then
  export RUSTFLAGS="${RUSTFLAGS:-} -C codegen-units=4"
  export RUSTFLAGS="${RUSTFLAGS# }"
fi

# Node.js - cap V8 heap to avoid runaway memory.
# Keep any existing NODE_OPTIONS (e.g. --import hooks) while ensuring we have a
# sane max-old-space-size set.
if [[ "${NODE_OPTIONS:-}" != *"--max-old-space-size="* ]]; then
  export NODE_OPTIONS="${NODE_OPTIONS:-} --max-old-space-size=4096"
  export NODE_OPTIONS="${NODE_OPTIONS# }"
fi

# Node.js version guard:
# Some agent environments can't easily install the repo's pinned `.nvmrc` Node version.
# If the major version doesn't match, enable the opt-in bypass for `check-node-version.mjs`
# so `cargo xtask` and friends can still run (it will emit a warning instead of failing).
if command -v node >/dev/null 2>&1; then
  if [[ -f "${REPO_ROOT}/.nvmrc" ]]; then
    expected_major="$(cut -d. -f1 "${REPO_ROOT}/.nvmrc" | tr -d '\r\n ' | head -n1)"
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
if [[ -n "${CARGO_HOME:-}" ]]; then
  echo "  CARGO_HOME=$CARGO_HOME"
fi
echo "  NODE_OPTIONS=$NODE_OPTIONS"
if [[ -n "${AERO_ALLOW_UNSUPPORTED_NODE:-}" ]]; then
  echo "  AERO_ALLOW_UNSUPPORTED_NODE=$AERO_ALLOW_UNSUPPORTED_NODE"
fi
echo "  PW_TEST_WORKERS=$PW_TEST_WORKERS"
