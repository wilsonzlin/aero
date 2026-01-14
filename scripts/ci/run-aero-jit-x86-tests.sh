#!/usr/bin/env bash
#
# CI helper: run `aero-jit-x86` lints + tests.
#
# Usage:
#   bash ./scripts/ci/run-aero-jit-x86-tests.sh
#
# Notes:
# - Uses `scripts/safe-run.sh` for timeout/memory protection (override via env):
#     AERO_TIMEOUT=1200 AERO_MEM_LIMIT=16G bash ./scripts/ci/run-aero-jit-x86-tests.sh
# - `aero-jit-x86` can exceed `safe-run`'s default 10 minute timeout on cold caches, so this script
#   bumps the default to 20 minutes to be more robust in fresh sandboxes.
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"

cd "${REPO_ROOT}"

# Cold builds for the JIT stack can exceed `safe-run`'s default 10 minute timeout.
: "${AERO_TIMEOUT:=1200}"
export AERO_TIMEOUT

run() {
  bash ./scripts/safe-run.sh "$@"
}

run cargo clippy -p aero-jit-x86 --all-targets --all-features --locked -- -D warnings
run cargo test -p aero-jit-x86 --locked
