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
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Defaults - can be overridden via environment
TIMEOUT="${AERO_TIMEOUT:-600}"
MEM_LIMIT="${AERO_MEM_LIMIT:-12G}"

# Cargo registry cache contention can be a major slowdown when many agents share the same host
# (cargo prints: "Blocking waiting for file lock on package cache"). Mirror the opt-in behavior of
# `scripts/agent-env.sh` so callers can isolate Cargo state per checkout without needing to source
# that script first:
#
#   AERO_ISOLATE_CARGO_HOME=1 bash ./scripts/safe-run.sh cargo test --locked
#
# When enabled, we intentionally override any pre-existing `CARGO_HOME` so the isolation actually
# takes effect.
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
        echo "[safe-run] warning: cannot expand '~' in AERO_ISOLATE_CARGO_HOME because HOME is unset; using literal path: $custom" >&2
      else
        custom="${custom/#\~/$HOME}"
      fi
    fi
    # Treat non-absolute paths as relative to the repo root so the behavior is stable
    # even when invoking from a different working directory.
    if [[ "$custom" != /* ]]; then
      custom="$REPO_ROOT/$custom"
    fi
    export CARGO_HOME="$custom"
    mkdir -p "$CARGO_HOME"
    unset custom 2>/dev/null || true
    ;;
esac

# Some environments configure a rustc wrapper (commonly `sccache`) either via environment variables
# (`RUSTC_WRAPPER=sccache`) or via global Cargo config (`~/.cargo/config.toml`).
#
# When the wrapper daemon/socket is unhealthy, Cargo can fail with errors like:
#
#   sccache: error: failed to execute compile
#
# Mirror the behavior of `scripts/agent-env.sh` so `safe-run.sh` can be used standalone:
# - By default, disable environment-based *sccache* wrappers (but preserve other wrappers like
#   `ccache`).
# - If you need to override a Cargo config `build.rustc-wrapper`, set `AERO_DISABLE_RUSTC_WRAPPER=1`
#   to export *empty* wrapper variables, which override Cargo config and disable wrappers entirely.
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
    echo "[safe-run] warning: unsupported AERO_DISABLE_RUSTC_WRAPPER value: ${AERO_DISABLE_RUSTC_WRAPPER}" >&2
    ;;
esac

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
cmd0="${1:-}"
cmd0="${cmd0##*/}"

is_cargo_cmd=false
is_retryable_cmd=false
case "${cmd0}" in
  cargo|cargo.exe)
    # Direct Cargo invocation: apply extra Rust-specific env hardening.
    is_cargo_cmd=true
    is_retryable_cmd=true
    ;;
  cargo-*)
    # Cargo subcommand binaries (e.g. cargo-clippy, cargo-nextest). These still
    # run Rust tooling and are safe to retry on transient OS resource limits.
    is_retryable_cmd=true
    ;;
  npm|npm.exe|pnpm|pnpm.exe|yarn|yarn.exe|node|node.exe|npx|npx.exe|wasm-pack|wasm-pack.exe)
    # Common build/test drivers which may spawn Cargo/rustc internally (e.g.
    # `npm -w web run wasm:build`).
    is_retryable_cmd=true
    ;;
esac

# Some build drivers (notably `wasm-pack` and JS/TS orchestration scripts) may spawn `cargo` internally
# to build the wasm32 target, without passing `--target wasm32-...` through the top-level command
# line we see here. When linking wasm32, rustc invokes `rust-lld -flavor wasm` directly, so the
# native `-Wl,--threads=` indirection does not apply.
#
# Provide a conservative wasm32 lld thread cap via Cargo's per-target rustflags env var so even
# indirect builds (e.g. `safe-run.sh npm ...`) don't hit lld EAGAIN thread-spawn failures.
if [[ "${is_retryable_cmd}" == "true" ]] && [[ "$(uname 2>/dev/null || true)" == "Linux" ]]; then
    # If an environment has already injected the native-style `-Wl,--threads=...` into this wasm32
    # per-target variable, rewrite it into the wasm-compatible form.
    if [[ "${CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS:-}" == *"-Wl,--threads="* ]]; then
        export CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS="${CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS//-C link-arg=-Wl,--threads=/-C link-arg=--threads=}"
        export CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS="${CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS//-Clink-arg=-Wl,--threads=/-C link-arg=--threads=}"
        export CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS="${CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS# }"
    fi

    if [[ "${CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS:-}" != *"--threads="* ]]; then
        export CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS="${CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS:-} -C link-arg=--threads=${CARGO_BUILD_JOBS:-1}"
        export CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS="${CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS# }"
    fi
fi

if [[ "${is_cargo_cmd}" == "true" ]]; then
    if [[ "${RUSTFLAGS:-}" != *"codegen-units="* ]]; then
        # Allow explicit override without requiring users to manually edit RUSTFLAGS.
        # `AERO_CODEGEN_UNITS` is a shorthand alias for `AERO_RUST_CODEGEN_UNITS`.
        if [[ -n "${AERO_RUST_CODEGEN_UNITS:-}" || -n "${AERO_CODEGEN_UNITS:-}" ]]; then
            _aero_codegen_units="${AERO_RUST_CODEGEN_UNITS:-${AERO_CODEGEN_UNITS}}"
            if [[ "${_aero_codegen_units}" =~ ^[1-9][0-9]*$ ]]; then
                export RUSTFLAGS="${RUSTFLAGS:-} -C codegen-units=${_aero_codegen_units}"
                export RUSTFLAGS="${RUSTFLAGS# }"
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
    # ⚠️ WASM NOTE:
    # When linking wasm32 targets, rustc typically invokes `rust-lld -flavor wasm` *directly*
    # (not via `cc -Wl,...`). The `-Wl,` prefix is treated as a literal argument and causes:
    #   rust-lld: error: unknown argument: -Wl,--threads=...
    # Prefer passing lld's flag directly for wasm targets (`--threads=N`).
    #
    # Restrict this to Linux: other platforms may use different linkers that don't accept
    # `--threads=`.
    if [[ "$(uname 2>/dev/null || true)" == "Linux" ]]; then
        # Determine the Cargo build target so we can pick an appropriate `rust-lld` threads flag.
        #
        # Precedence matches Cargo itself:
        # - `cargo --target <triple>` / `--target=<triple>` overrides everything.
        # - Otherwise, fall back to `CARGO_BUILD_TARGET` (often set by agent shells or configs).
        aero_target=""
        prev=""
        for arg in "${@:2}"; do
            # Stop parsing at `--` because subsequent args are passed to the invoked binary
            # (e.g. test harness flags) and should not affect our Cargo target detection.
            if [[ "${arg}" == "--" ]]; then
                break
            fi
            if [[ "${prev}" == "--target" ]]; then
                aero_target="${arg}"
                break
            fi
            prev=""
            case "${arg}" in
                --target)
                    prev="--target"
                    continue
                    ;;
                --target=*)
                    aero_target="${arg#--target=}"
                    break
                    ;;
            esac
        done
        if [[ -z "${aero_target}" ]]; then
            aero_target="${CARGO_BUILD_TARGET:-}"
        fi

        # If a native environment has already injected `-Wl,--threads=...` into RUSTFLAGS (commonly
        # via `scripts/agent-env.sh`), rewrite it into the wasm-compatible form when the Cargo target
        # is wasm32. Otherwise, `rust-lld -flavor wasm` fails with:
        #   rust-lld: error: unknown argument: -Wl,--threads=...
        if [[ "${aero_target}" == wasm32-* ]] && [[ "${RUSTFLAGS:-}" == *"-Wl,--threads="* ]]; then
            # Handle both `-C link-arg=...` and `-Clink-arg=...` spellings.
            export RUSTFLAGS="${RUSTFLAGS//-C link-arg=-Wl,--threads=/-C link-arg=--threads=}"
            export RUSTFLAGS="${RUSTFLAGS//-Clink-arg=-Wl,--threads=/-C link-arg=--threads=}"
            export RUSTFLAGS="${RUSTFLAGS# }"
        fi

        if [[ "${RUSTFLAGS:-}" != *"--threads="* ]]; then
            aero_lld_threads="${CARGO_BUILD_JOBS:-1}"
            if [[ "${aero_target}" == wasm32-* ]]; then
                export RUSTFLAGS="${RUSTFLAGS:-} -C link-arg=--threads=${aero_lld_threads}"
            else
                export RUSTFLAGS="${RUSTFLAGS:-} -C link-arg=-Wl,--threads=${aero_lld_threads}"
            fi
            export RUSTFLAGS="${RUSTFLAGS# }"
        fi

        unset aero_lld_threads aero_target prev 2>/dev/null || true
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
    echo "  AERO_ISOLATE_CARGO_HOME=1  Override CARGO_HOME to ./.cargo-home (opt-in, avoids registry lock contention on shared hosts)" >&2
    echo "  AERO_DISABLE_RUSTC_WRAPPER=1  Force-disable rustc wrappers (clears RUSTC_WRAPPER env vars; overrides Cargo config build.rustc-wrapper)" >&2
    echo "  AERO_CARGO_BUILD_JOBS=1  Cargo parallelism for agent sandboxes (default: 1; overrides CARGO_BUILD_JOBS if set)" >&2
    echo "  AERO_SAFE_RUN_RUSTC_RETRIES=3  Retries for transient rustc/EAGAIN spawn failures (default: 3; for cargo + common wrappers like npm/wasm-pack)" >&2
    echo "  CARGO_BUILD_JOBS=1       Cargo parallelism override (used when AERO_CARGO_BUILD_JOBS is unset)" >&2
    echo "  RUSTC_WORKER_THREADS=1   rustc internal worker threads (default: CARGO_BUILD_JOBS)" >&2
    echo "  RAYON_NUM_THREADS=1      Rayon global pool size (default: CARGO_BUILD_JOBS)" >&2
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
if [[ "${is_cargo_cmd}" == "true" ]]; then
    echo "[safe-run] Cargo jobs: ${CARGO_BUILD_JOBS:-}  rustc worker threads: ${RUSTC_WORKER_THREADS:-}  rayon threads: ${RAYON_NUM_THREADS:-}" >&2
fi
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
    # - "fork: retry: Resource temporarily unavailable"
    # - "ThreadPoolBuildError { ... Resource temporarily unavailable }" (Rayon thread pool init)
    # - "std::system_error: Resource temporarily unavailable" (observed from linkers like lld)
    if grep -q "Unable to install ctrlc handler" "${stderr_log}"; then
        return 0
    fi
    if grep -q "failed to create helper thread" "${stderr_log}"; then
        return 0
    fi
    if grep -q "fork: retry: Resource temporarily unavailable" "${stderr_log}"; then
        return 0
    fi
    # Cargo can also surface EAGAIN process spawn failures as a generic "could not execute process"
    # error (e.g. failing to spawn rustc at all):
    #
    #   error: could not compile `foo` (lib)
    #
    #   Caused by:
    #     could not execute process `rustc ...` (never executed)
    #
    #   Caused by:
    #     Resource temporarily unavailable (os error 11)
    #
    # This is also transient under shared-host contention and benefits from retry/backoff.
    if grep -q "could not execute process" "${stderr_log}" \
        && grep -Eq "Resource temporarily unavailable|WouldBlock|os error 11|EAGAIN" "${stderr_log}"
    then
        return 0
    fi
    # Some build scripts (notably `autocfg`) collapse process-spawn failures into a generic
    # "could not execute rustc" error without preserving the underlying OS errno. This is
    # frequently transient under shared-host contention, so treat it as retryable.
    if grep -q "could not execute rustc" "${stderr_log}"; then
        return 0
    fi
    if grep -q "failed to spawn" "${stderr_log}" \
        && grep -Eq "Resource temporarily unavailable|WouldBlock|os error 11|EAGAIN" "${stderr_log}"
    then
        return 0
    fi

    # Cargo occasionally fails very early while probing the compiler for target-specific
    # information (e.g. `rustc - --print=cfg ...`) and reports it as:
    #
    #   error: failed to run `rustc` to learn about target-specific information
    #
    # When this is caused by transient OS resource limits (EAGAIN/WouldBlock), retry/backoff is
    # effective.
    if grep -q 'failed to run `rustc` to learn about target-specific information' "${stderr_log}" \
        && grep -Eq "Resource temporarily unavailable|WouldBlock|os error 11|EAGAIN" "${stderr_log}"
    then
        return 0
    fi

    # Some environments hit transient EAGAIN failures inside `git` itself (e.g. when Cargo fetches
    # git dependencies), which surface as:
    #
    #   fatal: unable to create threaded lstat: Resource temporarily unavailable
    #
    # This is also fixed by retry/backoff.
    if grep -q "unable to create threaded lstat" "${stderr_log}" \
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

    # Cargo/rustc can also surface transient EAGAIN process limits as an inability to exec the
    # system linker (commonly `cc`). This is typically transient on shared/limited sandboxes.
    #
    # Example signatures:
    # - "error: could not exec the linker `cc`: Resource temporarily unavailable (os error 11)"
    # - "error: could not execute process `cc` ...: Resource temporarily unavailable (os error 11)"
    if grep -Eq "could not exec|could not execute process" "${stderr_log}" \
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

# Retry Rust build/test commands when rustc hits transient OS resource limits. Keep the default
# small so real failures aren't hidden for too long.
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

    # Timeout exit codes from GNU coreutils `timeout`:
    # - 124: command timed out (SIGTERM sent)
    # - 137: command killed after ignoring SIGTERM (SIGKILL)
    #
    # These typically mean the command is legitimately slow (e.g. cold Rust build on a contended
    # host) or hung. Provide an actionable hint for the common "needs more time" case.
    if [[ "${status}" -eq 124 || "${status}" -eq 137 ]]; then
        next_timeout=$((TIMEOUT * 2))
        if [[ "${next_timeout}" -lt 1 ]]; then
            next_timeout=1200
        fi

        echo "[safe-run] error: command exceeded timeout of ${TIMEOUT}s" >&2
        echo "[safe-run] Tip: retry with a larger timeout, e.g.:" >&2
        printf "[safe-run]   AERO_TIMEOUT=%s bash ./scripts/safe-run.sh" "${next_timeout}" >&2
        for arg in "$@"; do
            printf " %q" "${arg}" >&2
        done
        printf "\n" >&2
    fi

    if [[ "${attempt}" -lt "${MAX_RETRIES}" ]] \
        && [[ "${is_retryable_cmd}" == "true" ]] \
        && should_retry_rustc_thread_error "${stderr_log}"
    then
        # Exponential backoff with jitter (2-4, 4-8, 8-16, ...).
        base=$((2 ** attempt))
        # Cap at 16 so we stay within the documented 16-32s backoff window for 4th+ retries.
        if [[ "${base}" -gt 16 ]]; then
            base=16
        fi
        delay=$((base + RANDOM % (base + 1)))
        echo "[safe-run] detected transient OS resource limit; retrying in ${delay}s (attempt $((attempt + 1))/${MAX_RETRIES})" >&2
        sleep "${delay}"
        attempt=$((attempt + 1))
        rm -f "${stderr_log}"
        continue
    fi

    if [[ "${is_retryable_cmd}" == "true" ]] && should_retry_rustc_thread_error "${stderr_log}"; then
        echo "[safe-run] note: detected an OS resource limit (EAGAIN/WouldBlock). If this persists, try raising AERO_MEM_LIMIT (e.g. 32G or unlimited), lowering parallelism (AERO_CARGO_BUILD_JOBS=1, RAYON_NUM_THREADS=1), or reducing codegen parallelism (AERO_RUST_CODEGEN_UNITS=1)." >&2
    fi

    rm -f "${stderr_log}"
    exit "${status}"
done
