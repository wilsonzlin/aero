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
# when Cargo/rustc try to create too many threads/processes in parallel, or when
# the address-space limit (RLIMIT_AS) is set too low for rustc/LLVM's virtual
# memory reservations.
#
# Prefer reliability over speed: default to -j1 unless overridden.
# If you still hit rustc thread-spawn panics under safe-run, try raising
# `AERO_MEM_LIMIT` (or setting it to `unlimited`) for that invocation.
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

# rustc has its own internal worker thread pool (separate from Cargo's `-j` / build jobs).
# In constrained agent sandboxes, the default pool size (often `num_cpus`) can exceed
# per-user thread/process limits and cause rustc to ICE with:
#   Os { code: 11, kind: WouldBlock, message: "Resource temporarily unavailable" }
#
# Keep rustc's worker pool aligned with overall Cargo build parallelism for reliability.
export RUSTC_WORKER_THREADS="${RUSTC_WORKER_THREADS:-$CARGO_BUILD_JOBS}"

# Optional: reduce per-crate codegen parallelism (can reduce memory spikes).
#
# Do NOT force a default `-C codegen-units=...` here. In some constrained sandboxes,
# explicitly setting codegen-units has been observed to trigger rustc panics like:
#   "failed to spawn work/helper thread (WouldBlock)".
#
# If you want to set codegen-units for a specific invocation, use:
#   AERO_RUST_CODEGEN_UNITS=<n> (alias: AERO_CODEGEN_UNITS)
is_cargo_cmd=false
_aero_injected_codegen_units=0
_aero_injected_codegen_units_value=""
_aero_injected_codegen_units_is_explicit=0
if [[ "${1:-}" == "cargo" || "${1:-}" == */cargo ]]; then
    is_cargo_cmd=true
    if [[ "${RUSTFLAGS:-}" != *"codegen-units="* ]]; then
        # Allow explicit override without requiring users to manually edit RUSTFLAGS.
        # `AERO_CODEGEN_UNITS` is a shorthand alias for `AERO_RUST_CODEGEN_UNITS`.
        if [[ -n "${AERO_RUST_CODEGEN_UNITS:-}" || -n "${AERO_CODEGEN_UNITS:-}" ]]; then
            _aero_injected_codegen_units_is_explicit=1
            _aero_codegen_units="${AERO_RUST_CODEGEN_UNITS:-${AERO_CODEGEN_UNITS}}"
            if [[ "${_aero_codegen_units}" =~ ^[1-9][0-9]*$ ]]; then
                export RUSTFLAGS="${RUSTFLAGS:-} -C codegen-units=${_aero_codegen_units}"
                export RUSTFLAGS="${RUSTFLAGS# }"
                _aero_injected_codegen_units=1
                _aero_injected_codegen_units_value="${_aero_codegen_units}"
            else
                echo "[safe-run] warning: invalid AERO_RUST_CODEGEN_UNITS/AERO_CODEGEN_UNITS value: ${_aero_codegen_units} (expected positive integer); skipping codegen-units override" >&2
            fi
            unset _aero_codegen_units 2>/dev/null || true
        fi
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
    echo "  AERO_RUST_CODEGEN_UNITS=<n>  Optional rustc per-crate codegen-units override (alias: AERO_CODEGEN_UNITS)" >&2
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
    # - "failed to create helper thread: ... Resource temporarily unavailable"
    # - "failed to spawn helper thread: ... Resource temporarily unavailable"
    # - "failed to spawn work thread: ... Resource temporarily unavailable"
    # - "failed to spawn coordinator thread: ... Resource temporarily unavailable"
    # - "Unable to install ctrlc handler: ... Resource temporarily unavailable"
    # - "ThreadPoolBuildError { ... Resource temporarily unavailable }" (Rayon thread pool init)
    # - "std::system_error: Resource temporarily unavailable" (observed from linkers like lld)
    if grep -q "Unable to install ctrlc handler" "${stderr_log}"; then
        return 0
    fi
    if grep -q "failed to create helper thread" "${stderr_log}"; then
        return 0
    fi
    if grep -q "failed to spawn" "${stderr_log}" \
        && grep -Eq "Resource temporarily unavailable|WouldBlock|os error 11|EAGAIN" "${stderr_log}"
    then
        return 0
    fi

    # Some failures show up wrapped as a thread pool build error rather than the direct rustc
    # "failed to spawn helper thread" signature (e.g. Rayon global pool init).
    if grep -q "ThreadPoolBuildError" "${stderr_log}" \
        && grep -Eq "Resource temporarily unavailable|WouldBlock|os error 11|EAGAIN" "${stderr_log}"
    then
        return 0
    fi

    # Some native tools (e.g. LLVM lld) report EAGAIN thread failures as a C++ std::system_error.
    if grep -q "std::system_error" "${stderr_log}" \
        && grep -Eq "Resource temporarily unavailable|WouldBlock|os error 11|EAGAIN" "${stderr_log}"
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
    local status=$?

    # `>(...)` process substitution spawns the `tee` as a background job; ensure it has drained and
    # flushed stderr into `stderr_log` before we inspect it for retry patterns.
    wait
    return "${status}"
}

# Retry Cargo commands when rustc hits transient OS resource limits. Keep the default small so real
# failures aren't hidden for too long.
MAX_RETRIES="${AERO_SAFE_RUN_RUSTC_RETRIES:-3}"
if ! [[ "${MAX_RETRIES}" =~ ^[0-9]+$ ]] || [[ "${MAX_RETRIES}" -lt 1 ]]; then
    MAX_RETRIES=1
fi

attempt=1
while true; do
    # If we injected codegen-units and hit the thread-spawn ICE, fall back to the most conservative
    # setting on retry to reduce rustc's helper threads.
    #
    # If the user explicitly set AERO_RUST_CODEGEN_UNITS/AERO_CODEGEN_UNITS, respect it and do not
    # override; they can opt into the more conservative setting themselves if desired.
    if [[ "${attempt}" -gt 1 && "${_aero_injected_codegen_units:-0}" -eq 1 ]]; then
        if [[ "${_aero_injected_codegen_units_is_explicit:-0}" -eq 0 && -n "${_aero_injected_codegen_units_value:-}" && "${_aero_injected_codegen_units_value}" != "1" ]]; then
            export RUSTFLAGS="${RUSTFLAGS//codegen-units=${_aero_injected_codegen_units_value}/codegen-units=1}"
        fi
    fi

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
        && [[ "${is_cargo_cmd}" == "true" ]] \
        && should_retry_rustc_thread_error "${stderr_log}"
    then
        # Exponential backoff with jitter (2-4, 4-8, 8-16, ...).
        base=$((2 ** attempt))
        # Cap at 16 so we stay within the documented 16-32s backoff window for 4th+ retries.
        if [[ "${base}" -gt 16 ]]; then
            base=16
        fi
        delay=$((base + RANDOM % (base + 1)))
        echo "[safe-run] rustc hit transient resource limit; retrying in ${delay}s (attempt $((attempt + 1))/${MAX_RETRIES})" >&2
        sleep "${delay}"
        attempt=$((attempt + 1))
        rm -f "${stderr_log}"
        continue
    fi

    if [[ "${is_cargo_cmd}" == "true" ]] && should_retry_rustc_thread_error "${stderr_log}"; then
        echo "[safe-run] note: rustc hit an OS resource limit (EAGAIN/WouldBlock). If this persists, try raising AERO_MEM_LIMIT (e.g. 32G or unlimited) or lowering parallelism (AERO_CARGO_BUILD_JOBS=1, RAYON_NUM_THREADS=1)." >&2
    fi

    rm -f "${stderr_log}"
    exit "${status}"
done
