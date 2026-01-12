#!/usr/bin/env bash
# Regression test for scripts/safe-run.sh RUSTFLAGS sanitization + per-target lld thread caps.
#
# The safe-run wrapper may be used with commands that *indirectly* spawn Cargo (e.g. `bash -lc`,
# `npm`, `wasm-pack`). Some CI/agent environments also set global RUSTFLAGS like:
#   RUSTFLAGS="-C link-arg=-Wl,--threads=1"
#
# This breaks nested wasm32 builds because rustc invokes `rust-lld -flavor wasm` directly and
# `rust-lld` does not understand `-Wl,`:
#   rust-lld: error: unknown argument: -Wl,--threads=...
#
# safe-run should strip linker thread flags from global RUSTFLAGS and apply thread caps via
# per-target env vars instead (CARGO_TARGET_<TRIPLE>_RUSTFLAGS).
#
# Run:
#   bash ./scripts/tests/safe-run-rustflags-sanitization.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"

if [[ "$(uname 2>/dev/null || true)" != "Linux" ]]; then
  echo "Skipping safe-run rustflags sanitization checks on non-Linux." >&2
  exit 0
fi

tmpdir="$(mktemp -d "${TMPDIR:-/tmp}/aero-safe-run-rustflags-test.XXXXXX")"
cleanup() {
  rm -rf "${tmpdir}"
}
trap cleanup EXIT

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

assert_eq() {
  local name="$1"
  local got="$2"
  local want="$3"
  if [[ "${got}" == "${want}" ]]; then
    echo "[ok] ${name}"
  else
    fail "${name}: expected ${want}, got ${got}"
  fi
}

assert_contains() {
  local name="$1"
  local haystack="$2"
  local needle="$3"
  if [[ "${haystack}" == *"${needle}"* ]]; then
    echo "[ok] ${name}"
  else
    fail "${name}: expected to contain ${needle}, got ${haystack}"
  fi
}

assert_not_contains() {
  local name="$1"
  local haystack="$2"
  local needle="$3"
  if [[ "${haystack}" == *"${needle}"* ]]; then
    fail "${name}: expected to NOT contain ${needle}, got ${haystack}"
  else
    echo "[ok] ${name}"
  fi
}

# Create a minimal, isolated repo root containing only the helper scripts safe-run needs.
test_repo="${tmpdir}/repo"
mkdir -p "${test_repo}/scripts"
cp "${REPO_ROOT}/scripts/safe-run.sh" "${test_repo}/scripts/"
cp "${REPO_ROOT}/scripts/with-timeout.sh" "${test_repo}/scripts/"
cp "${REPO_ROOT}/scripts/run_limited.sh" "${test_repo}/scripts/"

###############################################################################
# Case 1: Wrapper command with contaminated global RUSTFLAGS.
###############################################################################
out="$(
  AERO_TIMEOUT=30 \
  AERO_MEM_LIMIT=unlimited \
  CARGO_BUILD_JOBS=7 \
  CARGO_BUILD_TARGET=x86_64-unknown-linux-gnu \
  RUSTFLAGS="-C link-arg=-Wl,--threads=99 -Clink-arg=--threads=100 -C opt-level=2" \
  bash "${test_repo}/scripts/safe-run.sh" bash -c '
    printf "RUSTFLAGS=%s\n" "${RUSTFLAGS}"
    printf "WASM_RUSTFLAGS=%s\n" "${CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS}"
    printf "TARGET_RUSTFLAGS=%s\n" "${CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS}"
  '
)"

rustflags="$(printf '%s\n' "${out}" | sed -n 's/^RUSTFLAGS=//p')"
wasm_rustflags="$(printf '%s\n' "${out}" | sed -n 's/^WASM_RUSTFLAGS=//p')"
target_rustflags="$(printf '%s\n' "${out}" | sed -n 's/^TARGET_RUSTFLAGS=//p')"

assert_eq "strips-thread-flags-from-global-rustflags" "${rustflags}" "-C opt-level=2"
assert_contains "injects-wasm32-threads-cap" "${wasm_rustflags}" "--threads=7"
assert_not_contains "wasm32-flags-do-not-use-wl-indirection" "${wasm_rustflags}" "-Wl,--threads="
assert_contains "injects-build-target-threads-cap" "${target_rustflags}" "-Wl,--threads=7"

###############################################################################
# Case 2: If the wasm32 per-target rustflags already contains `-Wl,--threads=...`, safe-run should
# rewrite it into the wasm-compatible `--threads=...` form without overriding the chosen value.
###############################################################################
out="$(
  AERO_TIMEOUT=30 \
  AERO_MEM_LIMIT=unlimited \
  CARGO_BUILD_JOBS=7 \
  CARGO_BUILD_TARGET=x86_64-unknown-linux-gnu \
  RUSTFLAGS="-C opt-level=2 -C link-arg=-Wl,--threads=99" \
  CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS="-C link-arg=-Wl,--threads=9 -C opt-level=3" \
  bash "${test_repo}/scripts/safe-run.sh" bash -c '
    printf "RUSTFLAGS=%s\n" "${RUSTFLAGS}"
    printf "WASM_RUSTFLAGS=%s\n" "${CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS}"
  '
)"

rustflags="$(printf '%s\n' "${out}" | sed -n 's/^RUSTFLAGS=//p')"
wasm_rustflags="$(printf '%s\n' "${out}" | sed -n 's/^WASM_RUSTFLAGS=//p')"

assert_eq "still-strips-thread-flags-from-global-rustflags" "${rustflags}" "-C opt-level=2"
assert_contains "rewrites-wasm32-wl-threads-into-wasm-threads" "${wasm_rustflags}" "--threads=9"
assert_contains "preserves-existing-wasm32-flags" "${wasm_rustflags}" "opt-level=3"
assert_not_contains "rewritten-wasm32-flags-do-not-contain-wl-threads" "${wasm_rustflags}" "-Wl,--threads="

echo "All safe-run rustflags sanitization checks passed."

