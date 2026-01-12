#!/usr/bin/env bash
# Regression test for scripts/safe-run.sh Cargo home auto-selection behavior.
#
# safe-run supports isolating Cargo state per checkout (./.cargo-home) to avoid shared
# cache lock contention ("Blocking waiting for file lock on package cache").
#
# Some agent environments explicitly export the default Cargo home as `CARGO_HOME=$HOME/.cargo`.
# safe-run should treat this as *non-custom* so it can still auto-use / create `./.cargo-home`
# when lock contention is detected.
#
# Run:
#   bash ./scripts/tests/safe-run-cargo-home.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"

tmpdir="$(mktemp -d "${TMPDIR:-/tmp}/aero-safe-run-cargo-home-test.XXXXXX")"
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

# Create a minimal, isolated repo root containing only the helper scripts safe-run needs.
test_repo="${tmpdir}/repo"
mkdir -p "${test_repo}/scripts"
cp "${REPO_ROOT}/scripts/safe-run.sh" "${test_repo}/scripts/"
cp "${REPO_ROOT}/scripts/with-timeout.sh" "${test_repo}/scripts/"
cp "${REPO_ROOT}/scripts/run_limited.sh" "${test_repo}/scripts/"

home1="${tmpdir}/home1"
mkdir -p "${home1}/.cargo"

###############################################################################
# Case 1: If ./.cargo-home exists and CARGO_HOME is explicitly set to $HOME/.cargo
# (default), safe-run should auto-use ./.cargo-home.
###############################################################################
mkdir -p "${test_repo}/.cargo-home"

out="$(
  AERO_TIMEOUT=30 \
  AERO_MEM_LIMIT=unlimited \
  HOME="${home1}" \
  CARGO_HOME="${home1}/.cargo" \
  bash "${test_repo}/scripts/safe-run.sh" bash -c 'printf "%s" "${CARGO_HOME}"'
)"
assert_eq "auto-uses-cargo-home-when-cargo-home-is-default" "${out}" "${test_repo}/.cargo-home"

###############################################################################
# Case 2: If CARGO_HOME is custom, safe-run must not override it even if ./.cargo-home exists.
###############################################################################
custom1="${tmpdir}/custom-cargo1"
mkdir -p "${custom1}"

out="$(
  AERO_TIMEOUT=30 \
  AERO_MEM_LIMIT=unlimited \
  HOME="${home1}" \
  CARGO_HOME="${custom1}" \
  bash "${test_repo}/scripts/safe-run.sh" bash -c 'printf "%s" "${CARGO_HOME}"'
)"
assert_eq "preserves-custom-cargo-home" "${out}" "${custom1}"

###############################################################################
# Case 3: On a lock-contention hint, safe-run should *create* ./.cargo-home when
# CARGO_HOME is default-but-explicit (so the next invocation can auto-use it).
###############################################################################
rm -rf "${test_repo}/.cargo-home"
out="$(
  AERO_TIMEOUT=30 \
  AERO_MEM_LIMIT=unlimited \
  HOME="${home1}" \
  CARGO_HOME="${home1}/.cargo" \
  bash "${test_repo}/scripts/safe-run.sh" bash -c 'echo "Blocking waiting for file lock on package cache" >&2'
)"
if [[ -d "${test_repo}/.cargo-home" ]]; then
  echo "[ok] creates-cargo-home-on-lock-contention-when-default"
else
  fail "creates-cargo-home-on-lock-contention-when-default: expected ${test_repo}/.cargo-home to exist"
fi

###############################################################################
# Case 4: On a lock-contention hint, safe-run should *not* create ./.cargo-home
# when CARGO_HOME is custom.
###############################################################################
rm -rf "${test_repo}/.cargo-home"
custom2="${tmpdir}/custom-cargo2"
mkdir -p "${custom2}"

out="$(
  AERO_TIMEOUT=30 \
  AERO_MEM_LIMIT=unlimited \
  HOME="${home1}" \
  CARGO_HOME="${custom2}" \
  bash "${test_repo}/scripts/safe-run.sh" bash -c 'echo "Blocking waiting for file lock on package cache" >&2'
)"
if [[ -d "${test_repo}/.cargo-home" ]]; then
  fail "does-not-create-cargo-home-on-lock-contention-when-custom: expected ${test_repo}/.cargo-home to NOT exist"
else
  echo "[ok] does-not-create-cargo-home-on-lock-contention-when-custom"
fi

echo "All safe-run Cargo home checks passed."

