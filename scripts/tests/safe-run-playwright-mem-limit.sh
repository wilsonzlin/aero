#!/usr/bin/env bash
# Regression test for scripts/safe-run.sh Playwright memory-limit auto-bump.
#
# Chromium/WebAssembly E2E runs can reserve huge amounts of *virtual* address space. Under
# `RLIMIT_AS` (as set by safe-run.sh), the default `AERO_MEM_LIMIT=12G` can be too small and
# cause Playwright/Chromium crashes or `WebAssembly.Memory()` allocation failures.
#
# safe-run.sh should therefore bump the memory limit for common Playwright entrypoints when the
# caller has not explicitly set AERO_MEM_LIMIT, while keeping a separate bump for WASM-heavy Node
# unit tests.
#
# Run:
#   bash ./scripts/tests/safe-run-playwright-mem-limit.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"

tmpdir="$(mktemp -d "${TMPDIR:-/tmp}/aero-safe-run-playwright-mem-limit-test.XXXXXX")"
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

extract_mem_limit() {
  local out="$1"
  local line=""
  line="$(echo "${out}" | grep -E "^\[safe-run\] Timeout:" | head -n 1 || true)"
  if [[ -z "${line}" ]]; then
    fail "missing [safe-run] Timeout line in output:\n${out}"
  fi
  # Example: "[safe-run] Timeout: 600s, Memory: 256G"
  echo "${line}" | sed -E 's/^\[safe-run\] Timeout: [0-9]+s, Memory: (.*)$/\1/'
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
# Case 1: `npm run test:e2e` uses AERO_PLAYWRIGHT_MEM_LIMIT when AERO_MEM_LIMIT is unset.
###############################################################################
out="$(
  PATH="${bin_dir}:${PATH}" \
  AERO_TIMEOUT=600 \
  AERO_NODE_TEST_MEM_LIMIT=7G \
  AERO_PLAYWRIGHT_MEM_LIMIT=11G \
  bash "${test_repo}/scripts/safe-run.sh" npm run test:e2e 2>&1 >/dev/null
)"
mem_val="$(extract_mem_limit "${out}")"
assert_eq "npm-test-e2e-uses-playwright-mem-limit-when-aero-mem-unset" "${mem_val}" "11G"

###############################################################################
# Case 1b: `npm -w web run test:e2e` also uses AERO_PLAYWRIGHT_MEM_LIMIT when AERO_MEM_LIMIT is unset.
###############################################################################
out="$(
  PATH="${bin_dir}:${PATH}" \
  AERO_TIMEOUT=600 \
  AERO_NODE_TEST_MEM_LIMIT=7G \
  AERO_PLAYWRIGHT_MEM_LIMIT=11G \
  bash "${test_repo}/scripts/safe-run.sh" npm -w web run test:e2e 2>&1 >/dev/null
)"
mem_val="$(extract_mem_limit "${out}")"
assert_eq "npm-workspace-test-e2e-uses-playwright-mem-limit-when-aero-mem-unset" "${mem_val}" "11G"

###############################################################################
# Case 2: AERO_MEM_LIMIT overrides the Playwright memory-limit bump.
###############################################################################
out="$(
  PATH="${bin_dir}:${PATH}" \
  AERO_TIMEOUT=600 \
  AERO_MEM_LIMIT=3G \
  AERO_PLAYWRIGHT_MEM_LIMIT=11G \
  bash "${test_repo}/scripts/safe-run.sh" npm run test:e2e 2>&1 >/dev/null
)"
mem_val="$(extract_mem_limit "${out}")"
assert_eq "npm-test-e2e-respects-explicit-aero-mem-limit" "${mem_val}" "3G"

###############################################################################
# Case 3: `cargo xtask ... --e2e` also uses AERO_PLAYWRIGHT_MEM_LIMIT when AERO_MEM_LIMIT is unset.
###############################################################################
out="$(
  PATH="${bin_dir}:${PATH}" \
  AERO_TIMEOUT=600 \
  AERO_PLAYWRIGHT_MEM_LIMIT=11G \
  bash "${test_repo}/scripts/safe-run.sh" cargo xtask input --e2e 2>&1 >/dev/null
)"
mem_val="$(extract_mem_limit "${out}")"
assert_eq "cargo-xtask-e2e-uses-playwright-mem-limit-when-aero-mem-unset" "${mem_val}" "11G"

###############################################################################
# Case 4: Non-Playwright npm commands should use the Node test mem bump, not the Playwright bump.
###############################################################################
out="$(
  PATH="${bin_dir}:${PATH}" \
  AERO_TIMEOUT=600 \
  AERO_NODE_TEST_MEM_LIMIT=7G \
  AERO_PLAYWRIGHT_MEM_LIMIT=11G \
  bash "${test_repo}/scripts/safe-run.sh" npm run test:unit 2>&1 >/dev/null
)"
mem_val="$(extract_mem_limit "${out}")"
assert_eq "npm-test-unit-uses-node-test-mem-limit" "${mem_val}" "7G"

###############################################################################
# Case 5: `npx playwright test` uses AERO_PLAYWRIGHT_MEM_LIMIT when AERO_MEM_LIMIT is unset.
###############################################################################
out="$(
  PATH="${bin_dir}:${PATH}" \
  AERO_TIMEOUT=600 \
  AERO_PLAYWRIGHT_MEM_LIMIT=11G \
  bash "${test_repo}/scripts/safe-run.sh" npx playwright test 2>&1 >/dev/null
)"
mem_val="$(extract_mem_limit "${out}")"
assert_eq "npx-playwright-test-uses-playwright-mem-limit-when-aero-mem-unset" "${mem_val}" "11G"

###############################################################################
# Case 6: `playwright test` uses AERO_PLAYWRIGHT_MEM_LIMIT when AERO_MEM_LIMIT is unset.
###############################################################################
out="$(
  PATH="${bin_dir}:${PATH}" \
  AERO_TIMEOUT=600 \
  AERO_PLAYWRIGHT_MEM_LIMIT=11G \
  bash "${test_repo}/scripts/safe-run.sh" playwright test 2>&1 >/dev/null
)"
mem_val="$(extract_mem_limit "${out}")"
assert_eq "playwright-test-uses-playwright-mem-limit-when-aero-mem-unset" "${mem_val}" "11G"

echo "All safe-run Playwright memory-limit checks passed."
