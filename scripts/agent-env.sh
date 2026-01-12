#!/bin/bash
# Source this file to set recommended environment variables for Aero development.
# Usage: source scripts/agent-env.sh
#
# DEFENSIVE DEFAULTS:
# - Memory-limited build parallelism
# - Timeouts are NOT set here (use with-timeout.sh or safe-run.sh explicitly)
# - File descriptor limits raised
# - Rustc wrapper issues handled
#
# These settings assume processes may misbehave. They prioritize reliability
# over maximum speed.

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
      if [[ -z "${HOME:-}" ]]; then
        echo "warning: cannot expand '~' in AERO_ISOLATE_CARGO_HOME because HOME is unset; using literal path: $custom" >&2
      else
        custom="${custom/#\~/$HOME}"
      fi
    fi
    # Treat non-absolute paths as relative to the repo root so the behavior is stable
    # even when sourcing from a different working directory.
    if [[ "$custom" != /* ]]; then
      custom="$REPO_ROOT/$custom"
    fi
    export CARGO_HOME="$custom"
    mkdir -p "$CARGO_HOME"
    # This script is sourced; avoid polluting the caller's environment with temp vars.
    unset custom 2>/dev/null || true
    ;;
esac

# Some environments configure a rustc wrapper (commonly `sccache`) either via environment
# variables (`RUSTC_WRAPPER=sccache`) or via global Cargo config (`~/.cargo/config.toml`).
#
# When the wrapper daemon is unhealthy, Cargo can fail with errors like:
#
#   sccache: error: failed to execute compile
#
# This script disables environment-based `sccache` wrappers by default for agent sandboxes;
# developers can opt back in by exporting the wrapper variables again after sourcing this file.
#
# If you also need to override a Cargo config `build.rustc-wrapper`, set
# `AERO_DISABLE_RUSTC_WRAPPER=1` before sourcing; this exports *empty* wrapper variables, which
# override Cargo config and disable wrappers entirely.
case "${AERO_DISABLE_RUSTC_WRAPPER:-}" in
  "" | 0 | false | FALSE | no | NO | off | OFF)
    # Check each wrapper variable for sccache (compatible with bash and zsh)
    _aero_check_sccache() {
      local val="$1"
      [[ "$val" == "sccache" || "$val" == */sccache || "$val" == "sccache.exe" || "$val" == */sccache.exe ]]
    }
    if _aero_check_sccache "${RUSTC_WRAPPER:-}"; then export RUSTC_WRAPPER=; fi
    if _aero_check_sccache "${RUSTC_WORKSPACE_WRAPPER:-}"; then export RUSTC_WORKSPACE_WRAPPER=; fi
    if _aero_check_sccache "${CARGO_BUILD_RUSTC_WRAPPER:-}"; then export CARGO_BUILD_RUSTC_WRAPPER=; fi
    if _aero_check_sccache "${CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER:-}"; then export CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER=; fi
    unset -f _aero_check_sccache 2>/dev/null || true
    ;;
  1 | true | TRUE | yes | YES | on | ON)
    export RUSTC_WRAPPER=
    export RUSTC_WORKSPACE_WRAPPER=
    export CARGO_BUILD_RUSTC_WRAPPER=
    export CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER=
    ;;
  *)
    echo "warning: unsupported AERO_DISABLE_RUSTC_WRAPPER value: ${AERO_DISABLE_RUSTC_WRAPPER}" >&2
    ;;
esac

# Rust/Cargo - balance speed vs memory (and thread limits)
#
# In constrained agent sandboxes we intermittently hit rustc panics like:
#   "failed to spawn helper thread (WouldBlock)"
# when Cargo runs too many rustc processes/threads in parallel, or when
# an address-space limit (RLIMIT_AS) is set too low for rustc/LLVM's virtual
# memory reservations. Prefer reliability over speed: default to -j1.
#
# If you still see rustc thread-spawn panics, try raising `AERO_MEM_LIMIT` for
# that command (or setting it to `unlimited`).
#
# Override:
#   export AERO_CARGO_BUILD_JOBS=2   # or 4, etc
#   source scripts/agent-env.sh
#
# (This is intentionally agent-only; CI should not source this script.)
_aero_default_cargo_build_jobs=1
if [[ -n "${AERO_CARGO_BUILD_JOBS:-}" ]]; then
  if [[ "${AERO_CARGO_BUILD_JOBS}" =~ ^[1-9][0-9]*$ ]]; then
    export CARGO_BUILD_JOBS="${AERO_CARGO_BUILD_JOBS}"
  else
    echo "warning: invalid AERO_CARGO_BUILD_JOBS value: ${AERO_CARGO_BUILD_JOBS} (expected positive integer); using ${_aero_default_cargo_build_jobs}" >&2
    export CARGO_BUILD_JOBS="${_aero_default_cargo_build_jobs}"
  fi
else
  export CARGO_BUILD_JOBS="${_aero_default_cargo_build_jobs}"
fi
unset _aero_default_cargo_build_jobs 2>/dev/null || true
export CARGO_INCREMENTAL=1

# rustc has its own internal worker thread pool (separate from Cargo's `-j` / build jobs).
# In constrained agent sandboxes, the default pool size (often `num_cpus`) can exceed per-user
# thread/process limits and cause rustc to ICE with:
#   Os { code: 11, kind: WouldBlock, message: "Resource temporarily unavailable" }
#
# Keep rustc's worker pool aligned with overall Cargo build parallelism for reliability.
export RUSTC_WORKER_THREADS="${RUSTC_WORKER_THREADS:-$CARGO_BUILD_JOBS}"

# rustc uses Rayon internally for query evaluation and other parallel work.
# When many agents share the same host, the default Rayon thread count (often `num_cpus`) can
# exceed per-user thread/process limits, causing rustc to ICE with:
#   Os { code: 11, kind: WouldBlock, message: "Resource temporarily unavailable" }
# when creating its global thread pool.
#
# Keep the Rayon pool size aligned with our overall Cargo build parallelism so builds remain
# reliable under contention.
export RAYON_NUM_THREADS="${RAYON_NUM_THREADS:-$CARGO_BUILD_JOBS}"

# Optional: reduce per-crate codegen parallelism (can reduce memory spikes).
#
# Do NOT force a default `-C codegen-units=...` here. In some constrained sandboxes,
# explicitly setting codegen-units has been observed to trigger rustc panics like:
#   "failed to spawn work/helper thread (WouldBlock)".
#
# If you want to set codegen-units for a specific shell/session, use:
#   export AERO_RUST_CODEGEN_UNITS=<n>   # alias: AERO_CODEGEN_UNITS
if [[ "${RUSTFLAGS:-}" != *"codegen-units="* ]]; then
  if [[ -n "${AERO_RUST_CODEGEN_UNITS:-}" || -n "${AERO_CODEGEN_UNITS:-}" ]]; then
    _aero_codegen_units="${AERO_RUST_CODEGEN_UNITS:-${AERO_CODEGEN_UNITS}}"
    if [[ "${_aero_codegen_units}" =~ ^[1-9][0-9]*$ ]]; then
      export RUSTFLAGS="${RUSTFLAGS:-} -C codegen-units=${_aero_codegen_units}"
      export RUSTFLAGS="${RUSTFLAGS# }"
    else
      echo "warning: invalid AERO_RUST_CODEGEN_UNITS/AERO_CODEGEN_UNITS value: ${_aero_codegen_units} (expected positive integer); skipping codegen-units override" >&2
    fi
    unset _aero_codegen_units 2>/dev/null || true
  fi
fi

# LLVM lld (used by the pinned Rust toolchain on Linux) defaults to using all available hardware
# threads when linking, which can also hit per-user thread limits on shared hosts. Limit lld's
# parallelism to match our overall build parallelism.
#
# ⚠️ WASM NOTE:
# When linking wasm32 targets, rustc typically invokes `rust-lld -flavor wasm` *directly*
# (not via `cc -Wl,...`). The `-Wl,` prefix is treated as a literal argument and causes:
#   rust-lld: error: unknown argument: -Wl,--threads=...
# Prefer passing lld's flag directly for wasm targets (`--threads=N`).
if [[ "$(uname 2>/dev/null || true)" == "Linux" ]]; then
  aero_target="${CARGO_BUILD_TARGET:-}"

  # If we already have the native-style `-Wl,--threads=...` in the environment but are building
  # wasm32 (via CARGO_BUILD_TARGET), rewrite it to the wasm-compatible form.
  if [[ "${aero_target}" == wasm32-* ]] && [[ "${RUSTFLAGS:-}" == *"-Wl,--threads="* ]]; then
    export RUSTFLAGS="${RUSTFLAGS//-C link-arg=-Wl,--threads=/-C link-arg=--threads=}"
    export RUSTFLAGS="${RUSTFLAGS# }"
  fi

  if [[ "${RUSTFLAGS:-}" != *"--threads="* ]]; then
    if [[ "${aero_target}" == wasm32-* ]]; then
      export RUSTFLAGS="${RUSTFLAGS:-} -C link-arg=--threads=${CARGO_BUILD_JOBS:-1}"
    else
      export RUSTFLAGS="${RUSTFLAGS:-} -C link-arg=-Wl,--threads=${CARGO_BUILD_JOBS:-1}"
    fi
    export RUSTFLAGS="${RUSTFLAGS# }"
  fi

  # WASM builds use rust-lld directly (no `cc` wrapper), so the `-Wl,` indirection used for native
  # linkers does not apply. Provide the same thread cap via per-target rustflags so `cargo ... --target
  # wasm32-unknown-unknown` does not fail with lld thread-spawn errors under tight process limits.
  if [[ "${CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS:-}" != *"--threads="* ]]; then
    export CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS="${CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS:-} -C link-arg=--threads=${CARGO_BUILD_JOBS:-1}"
    export CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS="${CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS# }"
  fi

  unset aero_target 2>/dev/null || true
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
echo "  RUSTC_WORKER_THREADS=$RUSTC_WORKER_THREADS"
echo "  RAYON_NUM_THREADS=$RAYON_NUM_THREADS"
echo "  RUSTFLAGS=${RUSTFLAGS:-}"
echo "  CARGO_INCREMENTAL=$CARGO_INCREMENTAL"
if [[ -n "${CARGO_HOME:-}" ]]; then
  echo "  CARGO_HOME=$CARGO_HOME"
fi
if [[ "${RUSTC_WRAPPER+x}" != "" ]]; then
  if [[ -n "${RUSTC_WRAPPER}" ]]; then
    echo "  RUSTC_WRAPPER=$RUSTC_WRAPPER"
  else
    echo "  RUSTC_WRAPPER=<disabled>"
  fi
fi
echo "  NODE_OPTIONS=$NODE_OPTIONS"
if [[ -n "${AERO_ALLOW_UNSUPPORTED_NODE:-}" ]]; then
  echo "  AERO_ALLOW_UNSUPPORTED_NODE=$AERO_ALLOW_UNSUPPORTED_NODE"
fi
echo "  PW_TEST_WORKERS=$PW_TEST_WORKERS"
echo ""
echo "⚠️  DEFENSIVE REMINDERS:"
echo "  • Always use timeouts:  bash ./scripts/with-timeout.sh 600 <command>"
echo "  • Always limit memory:  bash ./scripts/run_limited.sh --as 12G -- <command>"
echo "  • Or use both:          bash ./scripts/safe-run.sh <command>"
echo "  • If you see Permission denied running ./scripts/*.sh: run via bash (as above) or restore via git checkout/chmod"
echo "  • Verify outputs exist after builds (exit 0 ≠ success)"
echo ""
echo "📀 Windows 7 test ISO: /state/win7.iso"
