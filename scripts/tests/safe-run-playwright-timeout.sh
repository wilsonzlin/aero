#!/usr/bin/env bash
# Regression test for scripts/safe-run.sh Playwright timeout auto-bump.
#
# Playwright E2E runs often trigger a WebAssembly build step (`npm -w web run wasm:build`) which
# can exceed safe-run's default 10 minute timeout on cold caches. safe-run should therefore bump
# the timeout for common Playwright entrypoints when the caller has not explicitly set AERO_TIMEOUT.
#
# Run:
#   bash ./scripts/tests/safe-run-playwright-timeout.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"

tmpdir="$(mktemp -d "${TMPDIR:-/tmp}/aero-safe-run-playwright-timeout-test.XXXXXX")"
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

extract_timeout() {
  local out="$1"
  local line=""
  line="$(echo "${out}" | grep -E "^\[safe-run\] Timeout:" | head -n 1 || true)"
  if [[ -z "${line}" ]]; then
    fail "missing [safe-run] Timeout line in output:\n${out}"
  fi
  echo "${line}" | sed -E 's/^\[safe-run\] Timeout: ([0-9]+)s,.*/\1/'
}

###############################################################################
# Create a minimal, isolated repo root containing only the helper scripts safe-run needs.
###############################################################################
test_repo="${tmpdir}/repo"
mkdir -p "${test_repo}/scripts"
cp "${REPO_ROOT}/scripts/safe-run.sh" "${test_repo}/scripts/"
cp "${REPO_ROOT}/scripts/with-timeout.sh" "${test_repo}/scripts/"
cp "${REPO_ROOT}/scripts/run_limited.sh" "${test_repo}/scripts/"

###############################################################################
# Provide fake `npm` and `cargo` binaries so we can exercise safe-run's command classification
# without needing Node/Rust toolchains.
###############################################################################
bin_dir="${tmpdir}/bin"
mkdir -p "${bin_dir}"

cat > "${bin_dir}/npm" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
chmod +x "${bin_dir}/npm"

cat > "${bin_dir}/npx" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
chmod +x "${bin_dir}/npx"

cat > "${bin_dir}/playwright" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
chmod +x "${bin_dir}/playwright"

cat > "${bin_dir}/cargo" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
chmod +x "${bin_dir}/cargo"

###############################################################################
# Case 1: `npm run test:e2e` bumps timeout to AERO_PLAYWRIGHT_TIMEOUT when AERO_TIMEOUT is unset.
###############################################################################
out="$(
  PATH="${bin_dir}:${PATH}" \
  AERO_MEM_LIMIT=unlimited \
  AERO_PLAYWRIGHT_TIMEOUT=37 \
  bash "${test_repo}/scripts/safe-run.sh" npm run test:e2e 2>&1 >/dev/null
)"
timeout_val="$(extract_timeout "${out}")"
assert_eq "npm-test-e2e-uses-playwright-timeout-when-aero-timeout-unset" "${timeout_val}" "37"

###############################################################################
# Case 1b: `npm -w web run test:e2e` also bumps timeout to AERO_PLAYWRIGHT_TIMEOUT when AERO_TIMEOUT is unset.
###############################################################################
out="$(
  PATH="${bin_dir}:${PATH}" \
  AERO_MEM_LIMIT=unlimited \
  AERO_PLAYWRIGHT_TIMEOUT=37 \
  bash "${test_repo}/scripts/safe-run.sh" npm -w web run test:e2e 2>&1 >/dev/null
)"
timeout_val="$(extract_timeout "${out}")"
assert_eq "npm-workspace-test-e2e-uses-playwright-timeout-when-aero-timeout-unset" "${timeout_val}" "37"

###############################################################################
# Case 2: AERO_TIMEOUT overrides the Playwright timeout bump.
###############################################################################
out="$(
  PATH="${bin_dir}:${PATH}" \
  AERO_MEM_LIMIT=unlimited \
  AERO_TIMEOUT=17 \
  AERO_PLAYWRIGHT_TIMEOUT=37 \
  bash "${test_repo}/scripts/safe-run.sh" npm run test:e2e 2>&1 >/dev/null
)"
timeout_val="$(extract_timeout "${out}")"
assert_eq "npm-test-e2e-respects-explicit-aero-timeout" "${timeout_val}" "17"

###############################################################################
# Case 3: `cargo xtask ... --e2e` also bumps timeout to AERO_PLAYWRIGHT_TIMEOUT when AERO_TIMEOUT is unset.
###############################################################################
out="$(
  PATH="${bin_dir}:${PATH}" \
  AERO_MEM_LIMIT=unlimited \
  AERO_PLAYWRIGHT_TIMEOUT=37 \
  bash "${test_repo}/scripts/safe-run.sh" cargo xtask input --e2e 2>&1 >/dev/null
)"
timeout_val="$(extract_timeout "${out}")"
assert_eq "cargo-xtask-e2e-uses-playwright-timeout-when-aero-timeout-unset" "${timeout_val}" "37"

###############################################################################
# Case 4: Non-Playwright npm commands keep the default timeout.
###############################################################################
out="$(
  PATH="${bin_dir}:${PATH}" \
  AERO_MEM_LIMIT=unlimited \
  AERO_PLAYWRIGHT_TIMEOUT=37 \
  bash "${test_repo}/scripts/safe-run.sh" npm run test:unit 2>&1 >/dev/null
)"
timeout_val="$(extract_timeout "${out}")"
assert_eq "npm-test-unit-keeps-default-timeout" "${timeout_val}" "600"

###############################################################################
# Case 5: `npx playwright test` bumps timeout to AERO_PLAYWRIGHT_TIMEOUT when AERO_TIMEOUT is unset.
###############################################################################
out="$(
  PATH="${bin_dir}:${PATH}" \
  AERO_MEM_LIMIT=unlimited \
  AERO_PLAYWRIGHT_TIMEOUT=37 \
  bash "${test_repo}/scripts/safe-run.sh" npx playwright test 2>&1 >/dev/null
)"
timeout_val="$(extract_timeout "${out}")"
assert_eq "npx-playwright-test-uses-playwright-timeout-when-aero-timeout-unset" "${timeout_val}" "37"

###############################################################################
# Case 6: `playwright test` bumps timeout to AERO_PLAYWRIGHT_TIMEOUT when AERO_TIMEOUT is unset.
###############################################################################
out="$(
  PATH="${bin_dir}:${PATH}" \
  AERO_MEM_LIMIT=unlimited \
  AERO_PLAYWRIGHT_TIMEOUT=37 \
  bash "${test_repo}/scripts/safe-run.sh" playwright test 2>&1 >/dev/null
)"
timeout_val="$(extract_timeout "${out}")"
assert_eq "playwright-test-uses-playwright-timeout-when-aero-timeout-unset" "${timeout_val}" "37"

###############################################################################
# Case 7: `npm exec playwright test` bumps timeout to AERO_PLAYWRIGHT_TIMEOUT when AERO_TIMEOUT is unset.
###############################################################################
out="$(
  PATH="${bin_dir}:${PATH}" \
  AERO_MEM_LIMIT=unlimited \
  AERO_PLAYWRIGHT_TIMEOUT=37 \
  bash "${test_repo}/scripts/safe-run.sh" npm exec playwright test 2>&1 >/dev/null
)"
timeout_val="$(extract_timeout "${out}")"
assert_eq "npm-exec-playwright-test-uses-playwright-timeout-when-aero-timeout-unset" "${timeout_val}" "37"

echo "All safe-run Playwright timeout checks passed."
