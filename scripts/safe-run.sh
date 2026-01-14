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
#   AERO_NODE_TEST_MEM_LIMIT=256G  (fallback RLIMIT_AS for WASM-heavy Node test runs when AERO_MEM_LIMIT is unset)
#   AERO_PLAYWRIGHT_TIMEOUT=1800   (fallback timeout for Playwright/browser E2E runs when AERO_TIMEOUT is unset)
#   AERO_PLAYWRIGHT_MEM_LIMIT=256G (fallback RLIMIT_AS for Playwright/browser E2E runs when AERO_MEM_LIMIT is unset)
#
# Note: `cargo fuzz run` uses AddressSanitizer, which reserves a very large virtual address space
# for shadow memory (~16 TB on x86_64). Under `RLIMIT_AS` (as used by `run_limited.sh`), the default
# `AERO_MEM_LIMIT=12G` would cause fuzz targets to fail before executing with:
#   "ReserveShadowMemoryRange failed ... Perhaps you're using ulimit -v".
# `safe-run.sh` therefore disables the default address-space cap for `cargo fuzz run` unless the
# caller explicitly sets `AERO_MEM_LIMIT`.
#
# Note: Node-based test runners (including `node --test`, Vitest, and wasm-pack's `--node` runner)
# can require a much higher RLIMIT_AS than other commands because Node+WASM may reserve large
# amounts of *virtual* address space up-front (even when resident memory usage is small).
# `safe-run.sh` automatically bumps the default for `node --test` (and common wrappers like
# `npm run test:*` and `wasm-pack test --node`) unless `AERO_MEM_LIMIT` is explicitly set.
#
# Override example:
#   AERO_TIMEOUT=1200 AERO_MEM_LIMIT=16G bash ./scripts/safe-run.sh cargo build --release --locked

should_retry_rustc_thread_error() {
    local stderr_log="${1:-}"
    if [[ -z "${stderr_log}" || ! -f "${stderr_log}" ]]; then
        return 1
    fi

    local eagain_re="Resource temporarily unavailable|WouldBlock|os error 11|EAGAIN"

    # In shared agent sandboxes we intermittently hit rustc panics when it cannot spawn internal
    # helper threads/processes due to OS thread limits (EAGAIN/WouldBlock). These failures are
    # transient and typically succeed after a short backoff.
    #
    # Keep matching conservative: require either an exact/near-exact unwrap signature, or a rustc
    # panic context plus an EAGAIN/WouldBlock marker.
    #
    # Newer rustc versions can surface this as:
    #   thread 'rustc' panicked at 'called Result::unwrap() on an Err value: Os { code: 11, kind: WouldBlock, message: "Resource temporarily unavailable" }'
    local unwrap_eagain_re='called[[:space:]]+`?Result::unwrap\(\)`?[[:space:]]+on[[:space:]]+an[[:space:]]+`?Err`?[[:space:]]+value:[[:space:]]+(System\()?Os[[:space:]]*\{[[:space:]]*code:[[:space:]]*11,[[:space:]]*kind:[[:space:]]*WouldBlock'
    if grep -Eq "${unwrap_eagain_re}" "${stderr_log}"; then
        return 0
    fi
    # Some panic renderings wrap/line-wrap the `Os { ... }` struct or the unwrap header such that it
    # no longer appears on a single line. Treat the entire stderr log as one line (newlines -> spaces)
    # and retry if the near-exact unwrap(EAGAIN) signature matches.
    #
    # Use process substitution (instead of a pipe) so the match result doesn't depend on `pipefail`
    # semantics if the upstream `tr` exits early due to SIGPIPE after `grep -q` finds a match.
    if grep -Eq "${unwrap_eagain_re}" <(tr '\n' ' ' < "${stderr_log}"); then
        return 0
    fi
    if grep -Eq "thread 'rustc' panicked|panicked at" "${stderr_log}" \
        && grep -Eq "${eagain_re}" "${stderr_log}"
    then
        return 0
    fi

    # Example signatures:
    # - "failed to create helper thread: ... Resource temporarily unavailable"
    # - "failed to spawn helper thread: ... Resource temporarily unavailable"
    # - "failed to spawn work thread: ... Resource temporarily unavailable"
    # - "failed to spawn coordinator thread: ... Resource temporarily unavailable"
    # - "Unable to install ctrlc handler: ... Resource temporarily unavailable"
    # - "fork: retry: Resource temporarily unavailable"
    # - "failed to fork: Resource temporarily unavailable" (observed from some native tools)
    # - "could not exec the linker `cc`: ... Resource temporarily unavailable"
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
    if grep -qi "failed to fork" "${stderr_log}" \
        && grep -Eq "${eagain_re}" "${stderr_log}"
    then
        return 0
    fi

    # Cargo can also surface EAGAIN process spawn failures as a generic "could not execute process"
    # error (e.g. failing to spawn rustc at all).
    if grep -q "could not execute process" "${stderr_log}" \
        && grep -Eq "${eagain_re}" "${stderr_log}"
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
        && grep -Eq "${eagain_re}" "${stderr_log}"
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
        && grep -Eq "${eagain_re}" "${stderr_log}"
    then
        return 0
    fi

    # rustc can also panic inside `rustc_interface` when it fails to spawn threads / enter its
    # internal thread pool. Depending on where this happens, Cargo may not emit the usual
    # "failed to spawn helper thread" signature; it may only include the panic location.
    # Treat these as retryable when they are clearly caused by EAGAIN/WouldBlock.
    if grep -q "rustc_interface/src/util.rs" "${stderr_log}" \
        && grep -Eq "${eagain_re}" "${stderr_log}"
    then
        return 0
    fi

    # As a catch-all: rustc reports internal compiler errors (ICEs) with a generic banner.
    # If the ICE clearly stems from OS resource limits (EAGAIN/WouldBlock), retry/backoff is
    # usually sufficient.
    if grep -q "error: the compiler unexpectedly panicked" "${stderr_log}" \
        && grep -Eq "${eagain_re}" "${stderr_log}"
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
        && grep -Eq "${eagain_re}" "${stderr_log}"
    then
        return 0
    fi

    # Some failures show up wrapped as a thread pool build error rather than the direct rustc
    # "failed to spawn helper thread" signature (e.g. Rayon global pool init).
    if grep -q "ThreadPoolBuildError" "${stderr_log}" \
        && grep -Eq "${eagain_re}" "${stderr_log}"
    then
        return 0
    fi

    # Some native tools (e.g. LLVM lld) report EAGAIN thread failures as a C++ std::system_error.
    if grep -q "std::system_error" "${stderr_log}" \
        && grep -Eq "${eagain_re}" "${stderr_log}"
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
        && grep -Eq "${eagain_re}" "${stderr_log}"
    then
        return 0
    fi

    return 1
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# `cargo-fuzz` requires a nightly toolchain (`-Zsanitizer=...`). The repository root is pinned to
# stable via `rust-toolchain.toml`, but `fuzz/` has its own `rust-toolchain.toml` (nightly). When
# invoking `cargo fuzz ...` from the repo root, rustup will otherwise select the stable toolchain
# and the command will fail with "the option `Z` is only accepted on the nightly compiler".
#
# Make `bash ./scripts/safe-run.sh cargo fuzz ...` Just Work by auto-selecting the fuzz toolchain
# when the caller has not explicitly forced a toolchain already.
# In some CI/agent environments `RUSTUP_TOOLCHAIN` is globally forced to `stable-...`, which would
# break `cargo-fuzz`. If the caller hasn't explicitly opted into a nightly toolchain, override to
# the toolchain pinned in `fuzz/rust-toolchain.toml`.
if [[ "${1:-}" == "cargo" && "${2:-}" == "fuzz" ]] \
    && { [[ -z "${RUSTUP_TOOLCHAIN:-}" ]] || [[ "${RUSTUP_TOOLCHAIN}" != nightly* ]]; }
then
    fuzz_toolchain_toml="$REPO_ROOT/fuzz/rust-toolchain.toml"
    if [[ -f "${fuzz_toolchain_toml}" ]]; then
        # Parse: channel = "nightly-YYYY-MM-DD"
        toolchain="$(sed -n 's/^channel = \"\(.*\)\"/\1/p' "${fuzz_toolchain_toml}" | head -n 1)"
        if [[ -n "${toolchain}" ]]; then
            export RUSTUP_TOOLCHAIN="${toolchain}"
        fi
        unset toolchain
    fi
    unset fuzz_toolchain_toml
fi

# Defaults - can be overridden via environment
TIMEOUT="${AERO_TIMEOUT:-600}"
MEM_LIMIT="${AERO_MEM_LIMIT:-12G}"

# AddressSanitizer (used by `cargo fuzz run`) requires a huge virtual address space for its shadow
# mappings. When the caller hasn't explicitly chosen an address-space cap, disable the default
# limit so `safe-run.sh cargo fuzz run ...` can actually execute the fuzzer binary.
if [[ -z "${AERO_MEM_LIMIT:-}" ]] && [[ "${1:-}" == "cargo" ]]; then
    if [[ "${2:-}" == "fuzz" && "${3:-}" == "run" ]]; then
        MEM_LIMIT="unlimited"
    elif [[ "${2:-}" == +* && "${3:-}" == "fuzz" && "${4:-}" == "run" ]]; then
        MEM_LIMIT="unlimited"
    fi
fi

# Cargo registry cache contention can be a major slowdown when many agents share the same host
# (cargo prints: "Blocking waiting for file lock on package cache"). Mirror the opt-in behavior of
# `scripts/agent-env.sh` so callers can isolate Cargo state per checkout without needing to source
# that script first:
#
#   AERO_ISOLATE_CARGO_HOME=1 bash ./scripts/safe-run.sh cargo test --locked
#   AERO_ISOLATE_CARGO_HOME=/tmp/aero-cargo-home bash ./scripts/safe-run.sh cargo test --locked
#
# When enabled, we intentionally override any pre-existing `CARGO_HOME` so the isolation actually
# takes effect.
case "${AERO_ISOLATE_CARGO_HOME:-}" in
  "" | 0 | false | FALSE | no | NO | off | OFF)
    # Convenience: if a per-checkout Cargo home already exists (created by a previous run or by
    # `scripts/agent-env.sh`), prefer using it automatically as long as the caller hasn't set a
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
        echo "[safe-run] warning: cannot expand '~' in AERO_ISOLATE_CARGO_HOME because HOME is unset; using literal path: $custom" >&2
      else
        custom="${custom/#\~/$HOME}"
      fi
    elif [[ "$custom" == "~"* ]]; then
      echo "[safe-run] warning: AERO_ISOLATE_CARGO_HOME only supports '~' or '~/' expansion; using literal path: $custom" >&2
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
#   "called Result::unwrap() on an Err value: Os { code: 11, kind: WouldBlock, message: \"Resource temporarily unavailable\" }"
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

# Rayon uses this env var to size its global thread pool. If it's malformed, Rayon can fail to
# initialize and rustc may ICE. Sanitize it to a positive integer for reliability.
_aero_default_rayon_threads="${CARGO_BUILD_JOBS:-1}"
if ! [[ "${_aero_default_rayon_threads}" =~ ^[1-9][0-9]*$ ]]; then
    _aero_default_rayon_threads=1
fi
if [[ -z "${RAYON_NUM_THREADS:-}" ]]; then
    export RAYON_NUM_THREADS="${_aero_default_rayon_threads}"
elif ! [[ "${RAYON_NUM_THREADS}" =~ ^[1-9][0-9]*$ ]]; then
    echo "[safe-run] warning: invalid RAYON_NUM_THREADS value: ${RAYON_NUM_THREADS} (expected positive integer); using ${_aero_default_rayon_threads}" >&2
    export RAYON_NUM_THREADS="${_aero_default_rayon_threads}"
fi
unset _aero_default_rayon_threads 2>/dev/null || true

# Rust's built-in test harness (libtest) defaults to running tests with one thread per CPU core.
# Under shared-host contention this can exceed per-user thread limits (EAGAIN) and cause tests to
# fail before they even start.
#
# Keep the default aligned with our overall Cargo parallelism (`CARGO_BUILD_JOBS`) for reliability.
_aero_default_rust_test_threads="${CARGO_BUILD_JOBS:-1}"
if ! [[ "${_aero_default_rust_test_threads}" =~ ^[1-9][0-9]*$ ]]; then
    _aero_default_rust_test_threads=1
fi
if [[ -z "${RUST_TEST_THREADS:-}" ]]; then
    export RUST_TEST_THREADS="${_aero_default_rust_test_threads}"
elif ! [[ "${RUST_TEST_THREADS}" =~ ^[1-9][0-9]*$ ]]; then
    echo "[safe-run] warning: invalid RUST_TEST_THREADS value: ${RUST_TEST_THREADS} (expected positive integer); using ${_aero_default_rust_test_threads}" >&2
    export RUST_TEST_THREADS="${_aero_default_rust_test_threads}"
fi
unset _aero_default_rust_test_threads 2>/dev/null || true

# cargo-nextest runs test binaries in parallel, with its own concurrency setting
# (`--test-threads`, env: `NEXTEST_TEST_THREADS`). This is separate from libtest's
# `RUST_TEST_THREADS` and can also exceed per-user thread limits on shared hosts.
#
# Keep it aligned with our overall Cargo parallelism knob (`CARGO_BUILD_JOBS`) for reliability.
_aero_default_nextest_threads="${CARGO_BUILD_JOBS:-1}"
if ! [[ "${_aero_default_nextest_threads}" =~ ^[1-9][0-9]*$ ]]; then
    _aero_default_nextest_threads=1
fi
if [[ -z "${NEXTEST_TEST_THREADS:-}" ]]; then
    export NEXTEST_TEST_THREADS="${_aero_default_nextest_threads}"
elif [[ "${NEXTEST_TEST_THREADS}" == "num-cpus" ]]; then
    : # allow explicit opt-out
elif ! [[ "${NEXTEST_TEST_THREADS}" =~ ^[1-9][0-9]*$ ]]; then
    echo "[safe-run] warning: invalid NEXTEST_TEST_THREADS value: ${NEXTEST_TEST_THREADS} (expected positive integer or 'num-cpus'); using ${_aero_default_nextest_threads}" >&2
    export NEXTEST_TEST_THREADS="${_aero_default_nextest_threads}"
fi
unset _aero_default_nextest_threads 2>/dev/null || true

# rustc has its own internal worker thread pool (separate from Cargo's `-j` / build jobs).
# In constrained agent sandboxes, the default pool size (often `num_cpus`) can exceed
# per-user thread/process limits and cause rustc to ICE with:
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
    echo "[safe-run] warning: invalid RUSTC_WORKER_THREADS value: ${RUSTC_WORKER_THREADS} (expected positive integer); using ${_aero_default_rustc_worker_threads}" >&2
    export RUSTC_WORKER_THREADS="${_aero_default_rustc_worker_threads}"
fi
unset _aero_default_rustc_worker_threads 2>/dev/null || true

# Tokio defaults to sizing its runtime thread pool to `num_cpus`, which can exceed per-user thread
# limits in shared/contended sandboxes. Some Aero binaries read this repo-specific env var to size
# their Tokio runtime more conservatively (without changing production defaults).
#
# Keep it aligned with our overall agent parallelism knob (`CARGO_BUILD_JOBS`) for reliability.
_aero_default_tokio_worker_threads="${CARGO_BUILD_JOBS:-1}"
if ! [[ "${_aero_default_tokio_worker_threads}" =~ ^[1-9][0-9]*$ ]]; then
    _aero_default_tokio_worker_threads=1
fi
if [[ -z "${AERO_TOKIO_WORKER_THREADS:-}" ]]; then
    export AERO_TOKIO_WORKER_THREADS="${_aero_default_tokio_worker_threads}"
elif ! [[ "${AERO_TOKIO_WORKER_THREADS}" =~ ^[1-9][0-9]*$ ]]; then
    echo "[safe-run] warning: invalid AERO_TOKIO_WORKER_THREADS value: ${AERO_TOKIO_WORKER_THREADS} (expected positive integer); using ${_aero_default_tokio_worker_threads}" >&2
    export AERO_TOKIO_WORKER_THREADS="${_aero_default_tokio_worker_threads}"
fi
unset _aero_default_tokio_worker_threads 2>/dev/null || true

# Optional: reduce per-crate codegen parallelism (can reduce memory spikes).
#
# Do NOT force a default `-C codegen-units=...` here on the first attempt.
# In some constrained sandboxes, forcing codegen-units can negatively impact perf.
#
# However, when rustc hits a transient thread-spawn failure (EAGAIN/WouldBlock), we
# opportunistically inject `-C codegen-units=1` on *retries* (only when the user
# hasn't specified codegen-units themselves). This reduces rustc's per-crate
# codegen parallelism and makes retries more likely to succeed.
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
  git|git.exe)
    # `git` itself can hit transient OS resource limits under heavy contention (e.g. failing to
    # spawn internal helper threads). Allow safe-run to retry these in the same way as Cargo.
    is_retryable_cmd=true
    ;;
  bash|bash.exe|sh|sh.exe|python|python.exe|python3|python3.exe)
    # Wrapper/driver commands commonly used to invoke other tools (including Cargo). Mark these as
    # retryable so transient EAGAIN/WouldBlock failures from nested Rust tooling still get the
    # benefit of safe-run's retry/backoff.
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

    # Some environments set `RUSTFLAGS` globally to cap native linker thread parallelism (e.g.
    # `-C link-arg=-Wl,--threads=N`). When a nested build targets wasm32 (via wasm-pack or a
    # `cargo --target wasm32-*` subprocess), rustc invokes `rust-lld -flavor wasm` directly and
    # `rust-lld` does not understand `-Wl,`:
    #   rust-lld: error: unknown argument: -Wl,--threads=...
    #
    # Strip any `--threads`/`-Wl,--threads` linker args from global `RUSTFLAGS` so they don't leak
    # into wasm builds, and re-apply our thread caps via Cargo's per-target rustflags env vars.
    #
    # Note: we do this for *all* retryable commands (not just direct `cargo`) because wrapper tools
    # like `bash`, `npm`, or `wasm-pack` may spawn Cargo internally.

    _aero_add_lld_threads_rustflags_retryable() {
        local target="${1}"
        local threads="${CARGO_BUILD_JOBS:-1}"

        local target_upper
        target_upper="$(printf '%s' "${target}" | tr '[:lower:]' '[:upper:]')"
        local var="CARGO_TARGET_${target_upper}_RUSTFLAGS"
        var="${var//-/_}"
        var="${var//./_}"

        local current="${!var:-}"

        # Rewrite the native-style `-Wl,--threads=...` into the wasm-compatible form for wasm targets.
        if [[ "${target}" == wasm32-* ]] && [[ "${current}" == *"-Wl,--threads="* ]]; then
            current="${current//-C link-arg=-Wl,--threads=/-C link-arg=--threads=}"
            current="${current//-Clink-arg=-Wl,--threads=/-C link-arg=--threads=}"
        fi

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

    aero_host_target=""
    if command -v rustc >/dev/null 2>&1; then
        aero_host_target="$(rustc -vV 2>/dev/null | sed -n 's/^host: //p' | head -n1)"
    fi
    if [[ -n "${aero_host_target}" ]]; then
        _aero_add_lld_threads_rustflags_retryable "${aero_host_target}"
    fi
    if [[ -n "${CARGO_BUILD_TARGET:-}" ]]; then
        _aero_add_lld_threads_rustflags_retryable "${CARGO_BUILD_TARGET}"
    fi

    # Ensure nested wasm builds (e.g. wasm-pack) always have an lld threads cap.
    _aero_add_lld_threads_rustflags_retryable "wasm32-unknown-unknown"

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

    # Cargo also supports `CARGO_ENCODED_RUSTFLAGS` (Unit Separator-delimited) which applies to all
    # targets. If it contains `-Wl,--threads=...`, it can break nested wasm builds in the same way
    # as global `RUSTFLAGS`.
    if [[ "${CARGO_ENCODED_RUSTFLAGS:-}" == *"--threads="* || "${CARGO_ENCODED_RUSTFLAGS:-}" == *"-Wl,--threads="* ]]; then
        aero_sep=$'\x1f'
        aero_enc_rustflags=()
        # Split on the Unit Separator (0x1f).
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

    unset aero_host_target 2>/dev/null || true
    unset -f _aero_add_lld_threads_rustflags_retryable 2>/dev/null || true
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
    # ⚠️ RUSTFLAGS / WASM NOTE:
    # `RUSTFLAGS` applies to *all* targets. Injecting the native-style
    # `-C link-arg=-Wl,--threads=...` globally works for the host target, but breaks wasm builds
    # because rustc invokes `rust-lld -flavor wasm` directly and `rust-lld` does not understand
    # `-Wl,`:
    #   rust-lld: error: unknown argument: -Wl,--threads=...
    #
    # This can bite indirectly: e.g. `safe-run.sh cargo run -p xtask -- test-all` builds xtask for
    # the host target and then spawns `wasm-pack`, which in turn spawns `cargo --target wasm32-*`.
    # If we inject `-Wl,--threads=...` into `RUSTFLAGS`, the nested wasm build fails.
    #
    # Instead, set Cargo's per-target rustflags environment variables:
    #   CARGO_TARGET_<TRIPLE>_RUSTFLAGS
    #
    # This keeps the host linker capped while allowing nested wasm builds to succeed.
    #
    # Restrict this to Linux: other platforms may use different linkers that don't accept
    # `--threads=`.
    if [[ "$(uname 2>/dev/null || true)" == "Linux" ]]; then
        aero_lld_threads="${CARGO_BUILD_JOBS:-1}"

        _aero_add_lld_threads_rustflags() {
            local target="${1}"
            local threads="${aero_lld_threads}"

            # `CARGO_TARGET_<TRIPLE>_RUSTFLAGS` uses an uppercased triple with `-`/`.` replaced by
            # `_`. Avoid Bash 4+ `${var^^}` so this script remains compatible with older `/bin/bash`
            # (notably macOS, which still ships Bash 3.2).
            local target_upper
            target_upper="$(printf '%s' "${target}" | tr '[:lower:]' '[:upper:]')"
            local var="CARGO_TARGET_${target_upper}_RUSTFLAGS"
            var="${var//-/_}"
            var="${var//./_}"
            unset target_upper 2>/dev/null || true

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

        # Determine host target triple for default (no `--target`) Cargo invocations.
        aero_host_target=""
        if command -v rustc >/dev/null 2>&1; then
            aero_host_target="$(rustc -vV 2>/dev/null | sed -n 's/^host: //p' | head -n1)"
        fi

        # Determine the explicit Cargo target triple, if any.
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
        if [[ -z "${aero_target}" ]]; then
            aero_target="${aero_host_target}"
        fi

        # If RUSTFLAGS contains linker thread flags, strip them so they don't apply to every target.
        # We re-apply the limit via per-target env vars.
        if [[ "${RUSTFLAGS:-}" == *"--threads="* || "${RUSTFLAGS:-}" == *"-Wl,--threads="* ]]; then
            # Tokenize on whitespace (defensive: treat RUSTFLAGS as a simple space-separated list).
            # This intentionally does *not* try to preserve complex quoting; safe-run is designed to
            # sanitize agent environments, not to be a perfect shell parser.
            #
            # Remove both spellings:
            # - `-C link-arg=-Wl,--threads=N` / `-Clink-arg=-Wl,--threads=N`
            # - `-C link-arg=--threads=N`     / `-Clink-arg=--threads=N`
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

        # Like `RUSTFLAGS`, `CARGO_ENCODED_RUSTFLAGS` applies to *all* targets (but is delimited by
        # the Unit Separator 0x1f). Strip lld thread flags from it too so they don't leak into wasm32
        # link steps.
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

        if [[ -n "${aero_host_target}" ]]; then
            _aero_add_lld_threads_rustflags "${aero_host_target}"
        fi
        if [[ -n "${aero_target}" ]]; then
            _aero_add_lld_threads_rustflags "${aero_target}"
        fi
        # Ensure tools that spawn wasm builds (e.g. wasm-pack) also get a wasm32 threads cap.
        _aero_add_lld_threads_rustflags "wasm32-unknown-unknown"

        unset aero_lld_threads aero_target aero_host_target prev 2>/dev/null || true
        unset -f _aero_add_lld_threads_rustflags 2>/dev/null || true
    fi
fi

 # Node.js - cap V8 heap to avoid runaway memory.
 #
 # Mirror the defensive defaults from `scripts/agent-env.sh` so `safe-run.sh` can be used
 # standalone (without requiring users to source agent-env first).
 #
  # Keep any existing NODE_OPTIONS (e.g. --import hooks) while ensuring we have a sane
  # max-old-space-size set.
  # Node does *not* allow `--test-concurrency` in NODE_OPTIONS; strip it defensively so
  # `safe-run.sh node ...` works even if the outer environment (or older scripts) injected it.
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

if [[ $# -lt 1 ]]; then
    echo "Usage: $0 <command...>" >&2
    echo "" >&2
    echo "Runs a command with timeout and memory limit protections." >&2
    echo "" >&2
    echo "Environment variables:" >&2
    echo "  AERO_TIMEOUT=600     Timeout in seconds (default: 600 = 10 min)" >&2
    echo "  AERO_MEM_LIMIT=12G   Memory limit (default: 12G; for 'cargo fuzz run' safe-run defaults to 'unlimited' due to ASAN shadow memory)" >&2
    echo "  AERO_NODE_TEST_MEM_LIMIT=256G  Fallback memory limit for WASM-heavy Node test runners when AERO_MEM_LIMIT is unset (helps under RLIMIT_AS; applies to node --test, npm/vitest, wasm-pack --node)" >&2
    echo "  AERO_PLAYWRIGHT_MEM_LIMIT=256G  Fallback memory limit for Playwright/browser E2E runs when AERO_MEM_LIMIT is unset (browsers reserve huge virtual memory; too-low RLIMIT_AS can crash Chromium or break WebAssembly.Memory)" >&2
    echo "  AERO_PLAYWRIGHT_TIMEOUT=1800  Fallback timeout for Playwright/browser E2E runs when AERO_TIMEOUT is unset (cold WASM builds can exceed 10 minutes)" >&2
    echo "  AERO_ISOLATE_CARGO_HOME=1|<path>  Isolate Cargo state to ./.cargo-home (or a custom dir) to avoid registry lock contention on shared hosts" >&2
    echo "  AERO_DISABLE_RUSTC_WRAPPER=1  Force-disable rustc wrappers (clears RUSTC_WRAPPER env vars; overrides Cargo config build.rustc-wrapper)" >&2
    echo "  AERO_CARGO_BUILD_JOBS=1  Cargo parallelism for agent sandboxes (default: 1; overrides CARGO_BUILD_JOBS if set)" >&2
    echo "  AERO_SAFE_RUN_RUSTC_RETRIES=3  Retries for transient rustc EAGAIN/WouldBlock spawn panics (including unwrap(EAGAIN) panics) (default: 3; for cargo + common wrappers like npm/wasm-pack)" >&2
    echo "  CARGO_BUILD_JOBS=1       Cargo parallelism override (used when AERO_CARGO_BUILD_JOBS is unset)" >&2
    echo "  RUSTC_WORKER_THREADS=1   rustc internal worker threads (default: CARGO_BUILD_JOBS)" >&2
    echo "  RAYON_NUM_THREADS=1      Rayon global pool size (default: CARGO_BUILD_JOBS)" >&2
    echo "  RUST_TEST_THREADS=1      Rust test harness parallelism (default: CARGO_BUILD_JOBS)" >&2
    echo "  AERO_TOKIO_WORKER_THREADS=1  Tokio runtime worker threads for Aero binaries that support it (default: CARGO_BUILD_JOBS)" >&2
    echo "  NEXTEST_TEST_THREADS=1   cargo-nextest test concurrency (default: CARGO_BUILD_JOBS; accepts 'num-cpus' to opt out)" >&2
    echo "  AERO_RUST_CODEGEN_UNITS=<n>  Optional rustc per-crate codegen-units override (alias: AERO_CODEGEN_UNITS)" >&2
    echo "" >&2
    echo "Examples:" >&2
    echo "  $0 cargo build --locked" >&2
    echo "  AERO_TIMEOUT=1200 $0 cargo build --release --locked" >&2
    echo "  AERO_MEM_LIMIT=8G $0 npm run build" >&2
    exit 1
fi

# Node.js test runner defaults to running many test files in parallel, which can multiply memory
# usage (especially for WASM-heavy test suites). When running node tests under `safe-run.sh`, we
# prefer reliability over speed and align test concurrency with our overall agent parallelism knob
# (`CARGO_BUILD_JOBS`, default 1).
#
# Note: Node does *not* allow `--test-concurrency` in `NODE_OPTIONS` (it errors out with
# "is not allowed in NODE_OPTIONS"), so we inject it as a CLI argument only when invoking the test
# runner (`node --test ...`) and only if the caller didn't specify it already.
cmd0_arg="${1:-}"
cmd0_basename="${cmd0_arg##*/}"
unset cmd0_arg 2>/dev/null || true
if [[ "${cmd0_basename}" == "node" || "${cmd0_basename}" == "node.exe" ]]; then
    wants_test=false
    has_concurrency=false
    for arg in "${@:2}"; do
        if [[ "${arg}" == "--test" ]]; then
            wants_test=true
            continue
        fi
        case "${arg}" in
            --test-concurrency|--test-concurrency=*)
                has_concurrency=true
                break
                ;;
        esac
    done

    # Node+WASM can reserve a large amount of virtual address space (even when the actual resident
    # memory use is small). Under `RLIMIT_AS`, this can cause seemingly tiny wasm allocations to fail
    # with:
    #   RangeError: WebAssembly.Memory(): could not allocate memory
    #
    # When running the Node test runner we want `safe-run.sh node --test ...` to "just work", so if
    # the caller didn't explicitly pick an `AERO_MEM_LIMIT`, bump the default address-space cap to a
    # higher value that leaves enough headroom for V8/Wasm reservations.
    if [[ "${wants_test}" == "true" && -z "${AERO_MEM_LIMIT:-}" ]]; then
        MEM_LIMIT="${AERO_NODE_TEST_MEM_LIMIT:-256G}"
    fi

    if [[ "${wants_test}" == "true" && "${has_concurrency}" == "false" ]]; then
        injected=false
        new_args=("${1}")
        for arg in "${@:2}"; do
            new_args+=("${arg}")
            if [[ "${injected}" == "false" && "${arg}" == "--test" ]]; then
                new_args+=("--test-concurrency=${CARGO_BUILD_JOBS:-1}")
                injected=true
            fi
        done
        set -- "${new_args[@]}"
    fi
    unset wants_test has_concurrency injected new_args 2>/dev/null || true
fi

# Node+WASM and browser-based E2E tests can reserve large amounts of virtual address space.
# Under `RLIMIT_AS` the default `AERO_MEM_LIMIT=12G` can be too small:
# - Node + WASM heavy unit tests (via `node --test`, `vitest`, `wasm-pack --node`, ...)
# - Playwright/browser E2E runs, where Chromium + WASM (SharedArrayBuffer + threads) can require a
#   very large virtual address space and may otherwise crash or fail `WebAssembly.Memory()`.
#
# Since `RLIMIT_AS` (and the timeout wrapper) is inherited by child processes, bump defaults for
# common wrapper entrypoints that run tests (`npm`/`pnpm`/`yarn`/`npx`/`wasm-pack`) and for
# `cargo xtask ... --e2e` unless the caller has explicitly set `AERO_MEM_LIMIT`/`AERO_TIMEOUT`.
if [[ -z "${AERO_MEM_LIMIT:-}" || -z "${AERO_TIMEOUT:-}" ]]; then
    case "${cmd0_basename}" in
        npm|npm.exe|pnpm|pnpm.exe|yarn|yarn.exe|npx|npx.exe|wasm-pack|wasm-pack.exe)
            wants_node_wasm=false
            wants_playwright=false
            for arg in "${@:2}"; do
                case "${arg}" in
                    # Playwright/browser-based tests.
                    playwright|playwright:*|test:e2e|test:e2e:*|test:webgpu|test:gpu|test:coi|test:security-headers)
                        wants_playwright=true
                        break
                        ;;
                    # Node-based test runners that may load large WASM bundles.
                    test|test:*|test-*|vitest|vitest:*)
                        wants_node_wasm=true
                        ;;
                esac
            done
            if [[ "${wants_playwright}" == "true" ]]; then
                if [[ -z "${AERO_MEM_LIMIT:-}" ]]; then
                    MEM_LIMIT="${AERO_PLAYWRIGHT_MEM_LIMIT:-256G}"
                fi
                if [[ -z "${AERO_TIMEOUT:-}" ]]; then
                    TIMEOUT="${AERO_PLAYWRIGHT_TIMEOUT:-1800}"
                fi
            elif [[ "${wants_node_wasm}" == "true" ]]; then
                if [[ -z "${AERO_MEM_LIMIT:-}" ]]; then
                    MEM_LIMIT="${AERO_NODE_TEST_MEM_LIMIT:-256G}"
                fi
            fi
            unset wants_node_wasm wants_playwright arg 2>/dev/null || true
            ;;
        playwright|playwright.exe)
            if [[ -z "${AERO_MEM_LIMIT:-}" ]]; then
                MEM_LIMIT="${AERO_PLAYWRIGHT_MEM_LIMIT:-256G}"
            fi
            if [[ -z "${AERO_TIMEOUT:-}" ]]; then
                TIMEOUT="${AERO_PLAYWRIGHT_TIMEOUT:-1800}"
            fi
            ;;
        cargo|cargo.exe)
            # `cargo xtask ... --e2e` (via the repo's Cargo alias) runs Playwright.
            aero_args=("$@")
            # Skip optional toolchain selector: `cargo +nightly ...`
            idx=1
            if [[ ${#aero_args[@]} -gt 1 && "${aero_args[1]}" == +* ]]; then
                idx=2
            fi
            if [[ ${#aero_args[@]} -gt $idx && "${aero_args[$idx]}" == "xtask" ]]; then
                for arg in "${aero_args[@]:$((idx + 1))}"; do
                    if [[ "${arg}" == "--e2e" ]]; then
                        if [[ -z "${AERO_MEM_LIMIT:-}" ]]; then
                            MEM_LIMIT="${AERO_PLAYWRIGHT_MEM_LIMIT:-256G}"
                        fi
                        if [[ -z "${AERO_TIMEOUT:-}" ]]; then
                            TIMEOUT="${AERO_PLAYWRIGHT_TIMEOUT:-1800}"
                        fi
                        break
                    fi
                done
            fi
            unset aero_args idx arg 2>/dev/null || true
            ;;
    esac
fi
unset cmd0_basename 2>/dev/null || true

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
    echo "[safe-run] Cargo jobs: ${CARGO_BUILD_JOBS:-}  rustc worker threads: ${RUSTC_WORKER_THREADS:-}  rayon threads: ${RAYON_NUM_THREADS:-}  test threads: ${RUST_TEST_THREADS:-}  nextest threads: ${NEXTEST_TEST_THREADS:-}  tokio worker threads: ${AERO_TOKIO_WORKER_THREADS:-}" >&2
fi
echo "[safe-run] Started: $(date -Iseconds 2>/dev/null || date)" >&2


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
        # Cargo can be extremely slow under shared-host contention when multiple agents share the
        # same Cargo registry/cache (stderr includes: "Blocking waiting for file lock on package cache").
        #
        # This is not a failure, but it is often surprising and leads to timeouts. Provide a
        # proactive hint even on success so users know how to mitigate it.
        if grep -q "Blocking waiting for file lock on package cache" "${stderr_log}"; then
            # Only emit this hint when safe-run hasn't already been asked to isolate Cargo home.
             case "${AERO_ISOLATE_CARGO_HOME:-}" in
               "" | 0 | false | FALSE | no | NO | off | OFF)
                 echo "[safe-run] note: detected Cargo package-cache lock contention (\"Blocking waiting for file lock on package cache\")" >&2
                 echo "[safe-run] Tip: avoid shared Cargo registry lock contention by isolating Cargo state per checkout:" >&2
                 echo "[safe-run]   AERO_ISOLATE_CARGO_HOME=1 bash ./scripts/safe-run.sh ..." >&2
                 # If the user hasn't opted into a per-checkout Cargo home yet, create one so the
                 # next safe-run invocation will automatically pick it up (see the early
                 # `AERO_ISOLATE_CARGO_HOME` handling which auto-uses `./.cargo-home` when present).
                 #
                 # Keep this best-effort: do not fail the command if the directory cannot be
                 # created (e.g. read-only checkout).
                  _aero_default_cargo_home=""
                  if [[ -n "${HOME:-}" ]]; then
                    _aero_default_cargo_home="${HOME%/}/.cargo"
                  fi
                  _aero_effective_cargo_home="${CARGO_HOME:-}"
                  _aero_effective_cargo_home="${_aero_effective_cargo_home%/}"
                  if [[ ! -d "$REPO_ROOT/.cargo-home" ]] \
                    && { [[ -z "${_aero_effective_cargo_home}" ]] || [[ -n "${_aero_default_cargo_home}" && "${_aero_effective_cargo_home}" == "${_aero_default_cargo_home}" ]]; }
                  then
                    if mkdir -p "$REPO_ROOT/.cargo-home" 2>/dev/null; then
                      echo "[safe-run] note: created ./.cargo-home to reduce Cargo lock contention on future runs" >&2
                    else
                      echo "[safe-run] warning: failed to create ./.cargo-home (set AERO_ISOLATE_CARGO_HOME=1 or AERO_ISOLATE_CARGO_HOME=<path> to pick a custom path)" >&2
                    fi
                  fi
                  unset _aero_default_cargo_home _aero_effective_cargo_home 2>/dev/null || true
                  ;;
              esac
          fi
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

        # Cargo frequently hangs here when multiple agents contend for a shared Cargo registry
        # lock (stderr includes: "Blocking waiting for file lock on package cache"). Provide a
        # targeted remediation hint.
         if grep -q "Blocking waiting for file lock on package cache" "${stderr_log}"; then
             echo "[safe-run] Tip: avoid shared Cargo registry lock contention by isolating Cargo state:" >&2
             echo "[safe-run]   AERO_ISOLATE_CARGO_HOME=1 bash ./scripts/safe-run.sh ..." >&2
              _aero_default_cargo_home=""
              if [[ -n "${HOME:-}" ]]; then
                _aero_default_cargo_home="${HOME%/}/.cargo"
              fi
              _aero_effective_cargo_home="${CARGO_HOME:-}"
              _aero_effective_cargo_home="${_aero_effective_cargo_home%/}"
              if [[ ! -d "$REPO_ROOT/.cargo-home" ]] \
                && { [[ -z "${_aero_effective_cargo_home}" ]] || [[ -n "${_aero_default_cargo_home}" && "${_aero_effective_cargo_home}" == "${_aero_default_cargo_home}" ]]; }
              then
                if mkdir -p "$REPO_ROOT/.cargo-home" 2>/dev/null; then
                  echo "[safe-run] note: created ./.cargo-home to reduce Cargo lock contention on future runs" >&2
                else
                  echo "[safe-run] warning: failed to create ./.cargo-home (set AERO_ISOLATE_CARGO_HOME=1 or AERO_ISOLATE_CARGO_HOME=<path> to pick a custom path)" >&2
                fi
              fi
              unset _aero_default_cargo_home _aero_effective_cargo_home 2>/dev/null || true
          fi
      fi

     if [[ "${attempt}" -lt "${MAX_RETRIES}" ]] \
        && [[ "${is_retryable_cmd}" == "true" ]] \
        && should_retry_rustc_thread_error "${stderr_log}"
    then
        # If the user hasn't specified codegen-units, inject the most conservative setting for
        # retries to reduce rustc's per-crate codegen parallelism.
        if [[ "${RUSTFLAGS:-}" != *"codegen-units="* ]] \
            && [[ -z "${AERO_RUST_CODEGEN_UNITS:-}" ]] \
            && [[ -z "${AERO_CODEGEN_UNITS:-}" ]]
        then
            export RUSTFLAGS="${RUSTFLAGS:-} -C codegen-units=1"
            export RUSTFLAGS="${RUSTFLAGS# }"
            echo "[safe-run] info: injecting -C codegen-units=1 for retry to mitigate rustc thread spawn failures" >&2
        fi

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
fi
