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
    # Convenience: if a per-checkout Cargo home already exists (created by a previous run or by
    # `scripts/safe-run.sh`), prefer using it automatically as long as the caller hasn't set a
    # custom `CARGO_HOME`.
    #
    # Treat the default `CARGO_HOME` (`$HOME/.cargo`) as non-custom even when exported in the
    # environment (some CI/agent sandboxes export it explicitly). This keeps the behavior stable
    # for developers with a truly custom Cargo home while still reducing global cache lock
    # contention for the common default case.
    _aero_default_cargo_home=""
    if [[ -n "${HOME:-}" ]]; then
      _aero_default_cargo_home="${HOME%/}/.cargo"
    fi
    _aero_effective_cargo_home="${CARGO_HOME:-}"
    _aero_effective_cargo_home="${_aero_effective_cargo_home%/}"
    if [[ -d "$REPO_ROOT/.cargo-home" ]] \
      && { [[ -z "${_aero_effective_cargo_home}" ]] || [[ -n "${_aero_default_cargo_home}" && "${_aero_effective_cargo_home}" == "${_aero_default_cargo_home}" ]]; }
    then
      export CARGO_HOME="$REPO_ROOT/.cargo-home"
    fi
    unset _aero_default_cargo_home _aero_effective_cargo_home 2>/dev/null || true
    ;;
  1 | true | TRUE | yes | YES | on | ON)
    export CARGO_HOME="$REPO_ROOT/.cargo-home"
    mkdir -p "$CARGO_HOME"
    ;;
  *)
    custom="$AERO_ISOLATE_CARGO_HOME"
    # Expand the common `~/` shorthand (tilde is not expanded inside variables).
    #
    # Only support `~` and `~/...` here; other forms like `~user/...` are shell-specific and would
    # require `eval`/`getent`-style expansion (which we intentionally avoid in an agent script).
    if [[ "$custom" == "~" || "$custom" == "~/"* ]]; then
      if [[ -z "${HOME:-}" ]]; then
        echo "warning: cannot expand '~' in AERO_ISOLATE_CARGO_HOME because HOME is unset; using literal path: $custom" >&2
      else
        custom="${custom/#\~/$HOME}"
      fi
    elif [[ "$custom" == "~"* ]]; then
      echo "warning: AERO_ISOLATE_CARGO_HOME only supports '~' or '~/' expansion; using literal path: $custom" >&2
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
#   "called Result::unwrap() on an Err value: Os { code: 11, kind: WouldBlock, message: \"Resource temporarily unavailable\" }"
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
_aero_default_rustc_worker_threads="${CARGO_BUILD_JOBS:-1}"
if ! [[ "${_aero_default_rustc_worker_threads}" =~ ^[1-9][0-9]*$ ]]; then
  _aero_default_rustc_worker_threads=1
fi
if [[ -z "${RUSTC_WORKER_THREADS:-}" ]]; then
  export RUSTC_WORKER_THREADS="${_aero_default_rustc_worker_threads}"
elif ! [[ "${RUSTC_WORKER_THREADS}" =~ ^[1-9][0-9]*$ ]]; then
  echo "warning: invalid RUSTC_WORKER_THREADS value: ${RUSTC_WORKER_THREADS} (expected positive integer); using ${_aero_default_rustc_worker_threads}" >&2
  export RUSTC_WORKER_THREADS="${_aero_default_rustc_worker_threads}"
fi
unset _aero_default_rustc_worker_threads 2>/dev/null || true

# rustc uses Rayon internally for query evaluation and other parallel work.
# When many agents share the same host, the default Rayon thread count (often `num_cpus`) can
# exceed per-user thread/process limits, causing rustc to ICE with:
#   Os { code: 11, kind: WouldBlock, message: "Resource temporarily unavailable" }
# when creating its global thread pool.
#
# Keep the Rayon pool size aligned with our overall Cargo build parallelism so builds remain
# reliable under contention.
_aero_default_rayon_threads="${CARGO_BUILD_JOBS:-1}"
if ! [[ "${_aero_default_rayon_threads}" =~ ^[1-9][0-9]*$ ]]; then
  _aero_default_rayon_threads=1
fi
if [[ -z "${RAYON_NUM_THREADS:-}" ]]; then
  export RAYON_NUM_THREADS="${_aero_default_rayon_threads}"
elif ! [[ "${RAYON_NUM_THREADS}" =~ ^[1-9][0-9]*$ ]]; then
  echo "warning: invalid RAYON_NUM_THREADS value: ${RAYON_NUM_THREADS} (expected positive integer); using ${_aero_default_rayon_threads}" >&2
  export RAYON_NUM_THREADS="${_aero_default_rayon_threads}"
fi
unset _aero_default_rayon_threads 2>/dev/null || true

# Rust's built-in test harness (libtest) defaults to running tests with one thread per CPU core.
# Under shared-host contention this can exceed per-user thread limits (EAGAIN) and cause tests to
# fail before they even start.
#
# Keep it aligned with our overall Cargo parallelism for reliability.
_aero_default_rust_test_threads="${CARGO_BUILD_JOBS:-1}"
if ! [[ "${_aero_default_rust_test_threads}" =~ ^[1-9][0-9]*$ ]]; then
  _aero_default_rust_test_threads=1
fi
if [[ -z "${RUST_TEST_THREADS:-}" ]]; then
  export RUST_TEST_THREADS="${_aero_default_rust_test_threads}"
elif ! [[ "${RUST_TEST_THREADS}" =~ ^[1-9][0-9]*$ ]]; then
  echo "warning: invalid RUST_TEST_THREADS value: ${RUST_TEST_THREADS} (expected positive integer); using ${_aero_default_rust_test_threads}" >&2
  export RUST_TEST_THREADS="${_aero_default_rust_test_threads}"
fi
unset _aero_default_rust_test_threads 2>/dev/null || true

# cargo-nextest runs tests in parallel with its own concurrency setting (env:
# `NEXTEST_TEST_THREADS`), which is separate from libtest's `RUST_TEST_THREADS`.
# Under shared-host contention, the default can exceed per-user thread limits and
# cause EAGAIN/WouldBlock failures.
#
# Keep it aligned with our overall Cargo parallelism for reliability.
_aero_default_nextest_threads="${CARGO_BUILD_JOBS:-1}"
if ! [[ "${_aero_default_nextest_threads}" =~ ^[1-9][0-9]*$ ]]; then
  _aero_default_nextest_threads=1
fi
if [[ -z "${NEXTEST_TEST_THREADS:-}" ]]; then
  export NEXTEST_TEST_THREADS="${_aero_default_nextest_threads}"
elif [[ "${NEXTEST_TEST_THREADS}" == "num-cpus" ]]; then
  : # allow explicit opt-out
elif ! [[ "${NEXTEST_TEST_THREADS}" =~ ^[1-9][0-9]*$ ]]; then
  echo "warning: invalid NEXTEST_TEST_THREADS value: ${NEXTEST_TEST_THREADS} (expected positive integer or 'num-cpus'); using ${_aero_default_nextest_threads}" >&2
  export NEXTEST_TEST_THREADS="${_aero_default_nextest_threads}"
fi
unset _aero_default_nextest_threads 2>/dev/null || true

# Tokio defaults to spawning one worker thread per CPU core for multi-thread runtimes.
# In thread-limited agent sandboxes this can exceed per-user thread limits and cause
# EAGAIN/WouldBlock failures. Some Aero binaries read this repo-specific env var to cap
# their Tokio worker thread count without changing production defaults.
#
# Keep it aligned with our overall Cargo parallelism knob for reliability.
_aero_default_tokio_worker_threads="${CARGO_BUILD_JOBS:-1}"
if ! [[ "${_aero_default_tokio_worker_threads}" =~ ^[1-9][0-9]*$ ]]; then
  _aero_default_tokio_worker_threads=1
fi
if [[ -z "${AERO_TOKIO_WORKER_THREADS:-}" ]]; then
  export AERO_TOKIO_WORKER_THREADS="${_aero_default_tokio_worker_threads}"
elif ! [[ "${AERO_TOKIO_WORKER_THREADS}" =~ ^[1-9][0-9]*$ ]]; then
  echo "warning: invalid AERO_TOKIO_WORKER_THREADS value: ${AERO_TOKIO_WORKER_THREADS} (expected positive integer); using ${_aero_default_tokio_worker_threads}" >&2
  export AERO_TOKIO_WORKER_THREADS="${_aero_default_tokio_worker_threads}"
fi
unset _aero_default_tokio_worker_threads 2>/dev/null || true

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
# ⚠️ RUSTFLAGS / WASM NOTE:
# `RUSTFLAGS` applies to *all* targets. Passing the native-style `-Wl,--threads=...` globally
# breaks wasm builds because rustc invokes `rust-lld -flavor wasm` directly and `rust-lld`
# does not understand `-Wl,`:
#   rust-lld: error: unknown argument: -Wl,--threads=...
#
# Instead of mutating `RUSTFLAGS`, set Cargo's **per-target** rustflags environment variables:
#   CARGO_TARGET_<TRIPLE>_RUSTFLAGS
#
# This keeps native builds capped while allowing `cargo --target wasm32-...` to work in the
# same shell after sourcing this script.
if [[ "$(uname 2>/dev/null || true)" == "Linux" ]]; then
  aero_target="${CARGO_BUILD_TARGET:-}"

  # If `RUSTFLAGS` contains linker thread flags, strip them so they don't apply to every target.
  # We re-apply a conservative cap via Cargo's per-target rustflags env vars.
  #
  # This avoids breaking nested wasm32 builds where rustc invokes `rust-lld -flavor wasm`
  # directly (which does not understand `-Wl,`).
  if [[ "${RUSTFLAGS:-}" == *"--threads="* || "${RUSTFLAGS:-}" == *"-Wl,--threads="* ]]; then
    aero_rustflags=()
    # shellcheck disable=SC2206
    aero_rustflags=(${RUSTFLAGS})
    new_rustflags=()
    i=0
    while [[ $i -lt ${#aero_rustflags[@]} ]]; do
      tok="${aero_rustflags[$i]}"
      next=""
      if [[ $((i + 1)) -lt ${#aero_rustflags[@]} ]]; then
        next="${aero_rustflags[$((i + 1))]}"
      fi

      if [[ "${tok}" == "-C" ]] && ([[ "${next}" == link-arg=-Wl,--threads=* ]] || [[ "${next}" == link-arg=--threads=* ]]); then
        i=$((i + 2))
        continue
      fi
      if [[ "${tok}" == -Clink-arg=-Wl,--threads=* ]] || [[ "${tok}" == -Clink-arg=--threads=* ]]; then
        i=$((i + 1))
        continue
      fi

      new_rustflags+=("${tok}")
      i=$((i + 1))
    done

    export RUSTFLAGS="${new_rustflags[*]}"
    export RUSTFLAGS="${RUSTFLAGS# }"
    unset aero_rustflags new_rustflags tok next i 2>/dev/null || true
  fi

  # Cargo also supports `CARGO_ENCODED_RUSTFLAGS` (Unit Separator-delimited). Treat it as equivalent
  # to global `RUSTFLAGS` for sanitization purposes.
  if [[ "${CARGO_ENCODED_RUSTFLAGS:-}" == *"--threads="* || "${CARGO_ENCODED_RUSTFLAGS:-}" == *"-Wl,--threads="* ]]; then
    aero_sep=$'\x1f'
    aero_enc_rustflags=()
    IFS="${aero_sep}" read -r -a aero_enc_rustflags <<< "${CARGO_ENCODED_RUSTFLAGS}"
    new_enc_rustflags=()
    i=0
    while [[ $i -lt ${#aero_enc_rustflags[@]} ]]; do
      tok="${aero_enc_rustflags[$i]}"
      next=""
      if [[ $((i + 1)) -lt ${#aero_enc_rustflags[@]} ]]; then
        next="${aero_enc_rustflags[$((i + 1))]}"
      fi

      if [[ "${tok}" == "-C" ]] && ([[ "${next}" == link-arg=-Wl,--threads=* ]] || [[ "${next}" == link-arg=--threads=* ]]); then
        i=$((i + 2))
        continue
      fi
      if [[ "${tok}" == -Clink-arg=-Wl,--threads=* ]] || [[ "${tok}" == -Clink-arg=--threads=* ]]; then
        i=$((i + 1))
        continue
      fi

      new_enc_rustflags+=("${tok}")
      i=$((i + 1))
    done

    enc_joined=""
    for tok in "${new_enc_rustflags[@]}"; do
      if [[ -n "${enc_joined}" ]]; then
        enc_joined+="${aero_sep}"
      fi
      enc_joined+="${tok}"
    done
    export CARGO_ENCODED_RUSTFLAGS="${enc_joined}"
    unset aero_sep aero_enc_rustflags new_enc_rustflags enc_joined tok next i 2>/dev/null || true
  fi

  # If we already have the native-style `-Wl,--threads=...` in the environment and the default
  # Cargo target is wasm32 (via CARGO_BUILD_TARGET), rewrite it to the wasm-compatible form.
  #
  # This keeps `cargo build` working for wasm32 even when some other tooling (or a previous shell
  # session) injected the native linker flag into `RUSTFLAGS`.
  if [[ "${aero_target}" == wasm32-* ]] && [[ "${RUSTFLAGS:-}" == *"-Wl,--threads="* ]]; then
    # Handle both `-C link-arg=...` and `-Clink-arg=...` spellings.
    export RUSTFLAGS="${RUSTFLAGS//-C link-arg=-Wl,--threads=/-C link-arg=--threads=}"
    export RUSTFLAGS="${RUSTFLAGS//-Clink-arg=-Wl,--threads=/-C link-arg=--threads=}"
    export RUSTFLAGS="${RUSTFLAGS# }"
  fi

  # Append a linker threads cap to `CARGO_TARGET_<TRIPLE>_RUSTFLAGS` if one is not already present.
  _aero_add_lld_threads_rustflags() {
    local target="${1}"
    local threads="${CARGO_BUILD_JOBS:-1}"
    # `CARGO_TARGET_<TRIPLE>_RUSTFLAGS` uses an uppercased triple with `-`/`.` replaced by `_`.
    #
    # Avoid Bash 4+ `${var^^}` so this script stays compatible with older `/bin/bash` (notably
    # macOS, which still ships Bash 3.2).
    local target_upper
    target_upper="$(printf '%s' "${target}" | tr '[:lower:]' '[:upper:]')"
    local var="CARGO_TARGET_${target_upper}_RUSTFLAGS"
    var="${var//-/_}"
    var="${var//./_}"

    local current="${!var:-}"

    # If something injected the native-style `-Wl,--threads=...` into this wasm target's rustflags,
    # rewrite it into the wasm-compatible form.
    if [[ "${target}" == wasm32-* ]] && [[ "${current}" == *"-Wl,--threads="* ]]; then
      current="${current//-C link-arg=-Wl,--threads=/-C link-arg=--threads=}"
      current="${current//-Clink-arg=-Wl,--threads=/-C link-arg=--threads=}"
    fi

    # Only add if we don't already have a threads flag (either form).
    if [[ "${current}" != *"--threads="* ]] && [[ "${current}" != *"-Wl,--threads="* ]]; then
      if [[ "${target}" == wasm32-* ]]; then
        current="${current} -C link-arg=--threads=${threads}"
      else
        current="${current} -C link-arg=-Wl,--threads=${threads}"
      fi
      current="${current# }"
    fi

    export "${var}=${current}"
  }

  # Cap the linker thread count for the host target (covers plain `cargo build` / `cargo test`).
  aero_host_target=""
  if command -v rustc >/dev/null 2>&1; then
    aero_host_target="$(rustc -vV 2>/dev/null | sed -n 's/^host: //p' | head -n1)"
  fi
  if [[ -n "${aero_host_target}" ]]; then
    _aero_add_lld_threads_rustflags "${aero_host_target}"
  fi

  # Cap the linker thread count for wasm32-unknown-unknown. This applies both to direct
  # `cargo --target wasm32-unknown-unknown` invocations and to tools like wasm-pack that spawn Cargo.
  _aero_add_lld_threads_rustflags "wasm32-unknown-unknown"

  # If the user configured a default Cargo build target, cap linker threads for that target too.
  # (This is safe even when it matches the host/wasm targets above.)
  if [[ -n "${CARGO_BUILD_TARGET:-}" ]]; then
    _aero_add_lld_threads_rustflags "${CARGO_BUILD_TARGET}"
  fi

  unset aero_host_target 2>/dev/null || true
  unset aero_target 2>/dev/null || true
  unset -f _aero_add_lld_threads_rustflags 2>/dev/null || true
fi

# Node.js - cap V8 heap to avoid runaway memory.
# Keep any existing NODE_OPTIONS (e.g. --import hooks) while ensuring we have a
# sane max-old-space-size set.
# Node does *not* allow `--test-concurrency` in NODE_OPTIONS; strip it defensively so
# `node` invocations work even if the outer environment (or older scripts) injected it.
if [[ "${NODE_OPTIONS:-}" == *"--test-concurrency"* ]]; then
  aero_node_options=()
  # shellcheck disable=SC2206
  aero_node_options=(${NODE_OPTIONS})
  new_node_options=()
  i=0
  while [[ $i -lt ${#aero_node_options[@]} ]]; do
    tok="${aero_node_options[$i]}"
    if [[ "${tok}" == "--test-concurrency="* ]]; then
      i=$((i + 1))
      continue
    fi
    if [[ "${tok}" == "--test-concurrency" ]]; then
      # Also drop the next token if it looks like a value (not another flag).
      if [[ $((i + 1)) -lt ${#aero_node_options[@]} ]]; then
        next="${aero_node_options[$((i + 1))]}"
        if [[ "${next}" != "-"* ]]; then
          i=$((i + 2))
          continue
        fi
      fi
      i=$((i + 1))
      continue
    fi
    new_node_options+=("${tok}")
    i=$((i + 1))
  done
  export NODE_OPTIONS="${new_node_options[*]}"
  export NODE_OPTIONS="${NODE_OPTIONS# }"
  unset aero_node_options new_node_options tok next i 2>/dev/null || true
fi
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
    # `.nvmrc` commonly allows a leading `v` prefix (e.g. `v22.11.0`). Strip it so we don't
    # incorrectly treat a matching Node major as a mismatch.
    expected_major="${expected_major#v}"
    expected_major="${expected_major#V}"
    current_major="$(node -p "process.versions.node.split('.')[0]" 2>/dev/null || true)"
    if [[ -n "${expected_major}" && -n "${current_major}" && "${current_major}" != "${expected_major}" ]]; then
      if [[ -z "${AERO_ALLOW_UNSUPPORTED_NODE:-}" ]]; then
        export AERO_ALLOW_UNSUPPORTED_NODE=1
      fi
      # When we auto-enable the unsupported-node bypass, also silence the non-fatal
      # "note: Node.js version differs from CI baseline" logs from `check:node` scripts.
      # This keeps local `npm test` output readable in agent sandboxes while preserving
      # CI enforcement (CI should not source this script).
      if [[ -z "${AERO_CHECK_NODE_QUIET:-}" ]]; then
        export AERO_CHECK_NODE_QUIET=1
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
echo "  RUST_TEST_THREADS=$RUST_TEST_THREADS"
echo "  NEXTEST_TEST_THREADS=$NEXTEST_TEST_THREADS"
echo "  AERO_TOKIO_WORKER_THREADS=$AERO_TOKIO_WORKER_THREADS"
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
