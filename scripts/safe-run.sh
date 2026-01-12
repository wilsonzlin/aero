#!/bin/bash
# Run a command with both timeout and memory limit protections.
#
# DEFENSIVE: Assumes the command can hang, OOM, or misbehave in any way.
#
# Usage:
#   bash ./scripts/safe-run.sh <command...>
#   bash ./scripts/safe-run.sh cargo build --release --locked
#
# Default limits (override via environment):
#   AERO_TIMEOUT=600      (10 minutes)
#   AERO_MEM_LIMIT=12G    (12 GB virtual address space)
#
# Override example:
#   AERO_TIMEOUT=1200 AERO_MEM_LIMIT=16G bash ./scripts/safe-run.sh cargo build --release --locked

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Defaults - can be overridden via environment
TIMEOUT="${AERO_TIMEOUT:-600}"
MEM_LIMIT="${AERO_MEM_LIMIT:-12G}"

# Defensive defaults for shared-host agent execution.
#
# In constrained agent sandboxes we intermittently hit rustc panics like:
#   "failed to spawn helper thread (WouldBlock)"
#   "Unable to install ctrlc handler: ... WouldBlock (Resource temporarily unavailable)"
# when Cargo/rustc try to create too many threads/processes in parallel.
#
# Prefer reliability over speed: default to -j1 unless overridden.
#
# Override (preferred, shared with scripts/agent-env.sh):
#   export AERO_CARGO_BUILD_JOBS=2   # or 4, etc
#   bash ./scripts/safe-run.sh cargo test --locked
#
# Or override directly:
#   CARGO_BUILD_JOBS=2 bash ./scripts/safe-run.sh cargo test --locked
_aero_default_cargo_build_jobs=1
if [[ -n "${AERO_CARGO_BUILD_JOBS:-}" ]]; then
    # Canonical knob for agent sandboxes: override any pre-existing CARGO_BUILD_JOBS.
    if [[ "${AERO_CARGO_BUILD_JOBS}" =~ ^[1-9][0-9]*$ ]]; then
        export CARGO_BUILD_JOBS="${AERO_CARGO_BUILD_JOBS}"
    else
        echo "[safe-run] warning: invalid AERO_CARGO_BUILD_JOBS value: ${AERO_CARGO_BUILD_JOBS} (expected positive integer); using ${_aero_default_cargo_build_jobs}" >&2
        export CARGO_BUILD_JOBS="${_aero_default_cargo_build_jobs}"
    fi
elif [[ -z "${CARGO_BUILD_JOBS:-}" ]]; then
    export CARGO_BUILD_JOBS="${_aero_default_cargo_build_jobs}"
elif ! [[ "${CARGO_BUILD_JOBS}" =~ ^[1-9][0-9]*$ ]]; then
    echo "[safe-run] warning: invalid CARGO_BUILD_JOBS value: ${CARGO_BUILD_JOBS} (expected positive integer); using ${_aero_default_cargo_build_jobs}" >&2
    export CARGO_BUILD_JOBS="${_aero_default_cargo_build_jobs}"
fi
unset _aero_default_cargo_build_jobs 2>/dev/null || true

export RAYON_NUM_THREADS="${RAYON_NUM_THREADS:-$CARGO_BUILD_JOBS}"

# Reduce codegen parallelism per crate (avoids memory spikes / thread creation failures).
# Only apply when invoking cargo directly, and don't override an explicit codegen-units setting.
#
# Heuristic: align per-crate codegen parallelism with overall Cargo build parallelism so the total
# number of rustc worker threads remains bounded.
if [[ "${1:-}" == "cargo" || "${1:-}" == */cargo ]]; then
    if [[ "${RUSTFLAGS:-}" != *"codegen-units="* ]]; then
        _aero_codegen_units="${CARGO_BUILD_JOBS:-1}"

        # Allow explicit override without requiring users to manually edit RUSTFLAGS.
        # `AERO_CODEGEN_UNITS` is a shorthand alias for `AERO_RUST_CODEGEN_UNITS`.
        if [[ -n "${AERO_RUST_CODEGEN_UNITS:-}" ]]; then
            if [[ "${AERO_RUST_CODEGEN_UNITS}" =~ ^[1-9][0-9]*$ ]]; then
                _aero_codegen_units="${AERO_RUST_CODEGEN_UNITS}"
            else
                echo "[safe-run] warning: invalid AERO_RUST_CODEGEN_UNITS value: ${AERO_RUST_CODEGEN_UNITS} (expected positive integer); using ${_aero_codegen_units}" >&2
            fi
        elif [[ -n "${AERO_CODEGEN_UNITS:-}" ]]; then
            if [[ "${AERO_CODEGEN_UNITS}" =~ ^[1-9][0-9]*$ ]]; then
                _aero_codegen_units="${AERO_CODEGEN_UNITS}"
            else
                echo "[safe-run] warning: invalid AERO_CODEGEN_UNITS value: ${AERO_CODEGEN_UNITS} (expected positive integer); using ${_aero_codegen_units}" >&2
            fi
        fi
        if ! [[ "${_aero_codegen_units}" =~ ^[1-9][0-9]*$ ]]; then
            _aero_codegen_units=1
        fi

        # cap at 4 to avoid overly slow per-crate codegen when users opt into higher Cargo parallelism.
        # Opt out via AERO_RUST_CODEGEN_UNITS (or its shorthand alias, AERO_CODEGEN_UNITS).
        if [[ -z "${AERO_RUST_CODEGEN_UNITS:-}" && -z "${AERO_CODEGEN_UNITS:-}" && "${_aero_codegen_units}" -gt 4 ]]; then
            _aero_codegen_units=4
        fi

        export RUSTFLAGS="${RUSTFLAGS:-} -C codegen-units=${_aero_codegen_units}"
        export RUSTFLAGS="${RUSTFLAGS# }"
        unset _aero_codegen_units 2>/dev/null || true
    fi

    # LLVM lld defaults to using all available hardware threads when linking. On shared hosts this
    # can hit per-user thread limits (EAGAIN/"Resource temporarily unavailable"). Limit lld's
    # internal parallelism to match our overall Cargo build parallelism.
    #
    # Restrict this to Linux: other platforms may use different linkers that don't accept
    # `--threads=`.
    if [[ "$(uname 2>/dev/null || true)" == "Linux" ]]; then
        if [[ "${RUSTFLAGS:-}" != *"--threads="* ]]; then
            export RUSTFLAGS="${RUSTFLAGS:-} -C link-arg=-Wl,--threads=${CARGO_BUILD_JOBS:-1}"
            export RUSTFLAGS="${RUSTFLAGS# }"
        fi
    fi
fi

if [[ $# -lt 1 ]]; then
    echo "Usage: $0 <command...>" >&2
    echo "" >&2
    echo "Runs a command with timeout and memory limit protections." >&2
    echo "" >&2
    echo "Environment variables:" >&2
    echo "  AERO_TIMEOUT=600     Timeout in seconds (default: 600 = 10 min)" >&2
    echo "  AERO_MEM_LIMIT=12G   Memory limit (default: 12G)" >&2
    echo "  AERO_CARGO_BUILD_JOBS=1  Cargo parallelism for agent sandboxes (default: 1; overrides CARGO_BUILD_JOBS if set)" >&2
    echo "  AERO_SAFE_RUN_RUSTC_RETRIES=3  Retries for transient rustc thread spawn panics (default: 3; only for cargo commands)" >&2
    echo "  CARGO_BUILD_JOBS=1       Cargo parallelism override (used when AERO_CARGO_BUILD_JOBS is unset)" >&2
    echo "  AERO_RUST_CODEGEN_UNITS=4  rustc per-crate codegen-units override (default: min(CARGO_BUILD_JOBS, 4); alias: AERO_CODEGEN_UNITS)" >&2
    echo "" >&2
    echo "Examples:" >&2
    echo "  $0 cargo build --locked" >&2
    echo "  AERO_TIMEOUT=1200 $0 cargo build --release --locked" >&2
    echo "  AERO_MEM_LIMIT=8G $0 npm run build" >&2
    exit 1
fi

# If the working tree is partially broken (e.g. missing tracked files), fail with a
# clear, copy/paste remediation command.
for rel in "with-timeout.sh" "run_limited.sh"; do
    dep="${SCRIPT_DIR}/${rel}"
    # Treat 0-byte scripts as missing too; an empty helper script would make safe-run
    # silently skip enforcing timeouts/limits.
    if [[ ! -s "${dep}" ]]; then
        echo "[safe-run] error: missing/empty required script: scripts/${rel}" >&2
        echo "[safe-run] Your checkout may be incomplete. Try:" >&2
        echo "  git checkout -- scripts" >&2
        echo "  # or reset the whole working tree:" >&2
        echo "  git checkout -- ." >&2
        exit 1
    fi
done

echo "[safe-run] Command: $*" >&2
echo "[safe-run] Timeout: ${TIMEOUT}s, Memory: ${MEM_LIMIT}" >&2
echo "[safe-run] Started: $(date -Iseconds 2>/dev/null || date)" >&2

should_retry_rustc_thread_error() {
    local stderr_log="${1:-}"
    if [[ -z "${stderr_log}" || ! -f "${stderr_log}" ]]; then
        return 1
    fi

    # In shared agent sandboxes we intermittently hit rustc panics when it cannot spawn internal
    # helper threads due to OS thread limits (EAGAIN/WouldBlock). These failures are transient and
    # typically succeed after a short backoff.
    #
    # Example signatures:
    # - "failed to spawn helper thread: ... Resource temporarily unavailable"
    # - "failed to spawn work thread: ... Resource temporarily unavailable"
    # - "Unable to install ctrlc handler: ... Resource temporarily unavailable"
    if grep -q "failed to spawn helper thread" "${stderr_log}" \
        || grep -q "failed to spawn work thread" "${stderr_log}" \
        || grep -q "Unable to install ctrlc handler" "${stderr_log}"
    then
        return 0
    fi

    return 1
}

run_once() {
    local stderr_log="${1}"
    shift

    # Chain: timeout (with SIGKILL fallback) wraps memory-limited command.
    #
    # Use the shared helper so we support both GNU `timeout` and macOS `gtimeout`
    # consistently across scripts.
    #
    # Note: some agent environments lose executable bits in the working tree. Invoke
    # our helper via `bash` so safe-run still works even if scripts are 0644.
    bash "$SCRIPT_DIR/with-timeout.sh" "${TIMEOUT}" \
        bash "$SCRIPT_DIR/run_limited.sh" --as "$MEM_LIMIT" -- "$@" \
        2> >(tee "${stderr_log}" >&2)
}

# Retry Cargo commands when rustc hits transient OS resource limits. Keep the default small so real
# failures aren't hidden for too long.
MAX_RETRIES="${AERO_SAFE_RUN_RUSTC_RETRIES:-3}"
if ! [[ "${MAX_RETRIES}" =~ ^[0-9]+$ ]] || [[ "${MAX_RETRIES}" -lt 1 ]]; then
    MAX_RETRIES=1
fi

attempt=1
while true; do
    stderr_log="$(mktemp "${TMPDIR:-/tmp}/aero-safe-run-stderr.XXXXXX")"

    set +e
    run_once "${stderr_log}" "$@"
    status=$?
    set -e

    if [[ "${status}" -eq 0 ]]; then
        rm -f "${stderr_log}"
        exit 0
    fi

    if [[ "${attempt}" -lt "${MAX_RETRIES}" ]] \
        && [[ "${1:-}" == "cargo" || "${1:-}" == */cargo ]] \
        && should_retry_rustc_thread_error "${stderr_log}"
    then
        # Exponential backoff with jitter (2-4, 4-8, 8-16, ...).
        base=$((2 ** (attempt - 1)))
        delay=$((base + RANDOM % (base + 1)))
        echo "[safe-run] rustc hit transient resource limit; retrying in ${delay}s (attempt $((attempt + 1))/${MAX_RETRIES})" >&2
        sleep "${delay}"
        attempt=$((attempt + 1))
        rm -f "${stderr_log}"
        continue
    fi

    rm -f "${stderr_log}"
    exit "${status}"
done
