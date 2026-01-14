#!/usr/bin/env bash
set -euo pipefail

# Transitional wrapper.
#
# `cargo xtask test-all` is the canonical, cross-platform orchestrator used by both
# CI and developers. Keep this script around so existing workflows keep working,
# but avoid adding new logic here.
#
# This wrapper still normalizes/validates Aero's cross-tooling env vars so that:
# - legacy aliases emit warnings (e.g. AERO_WEB_DIR/WEB_DIR/AERO_WASM_DIR)
# - booleans normalize to 1/0 (e.g. AERO_REQUIRE_WEBGPU=true -> 1)
# - invalid paths fail fast with clear errors
#
# See: docs/env-vars.md

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

if ! command -v node >/dev/null 2>&1; then
  echo "error: missing required command: node" >&2
  exit 1
fi

node "$ROOT_DIR/scripts/check-node-version.mjs"

resolved_env="$(node "$ROOT_DIR/scripts/env/resolve.mjs" --format bash)" || exit $?
eval "$resolved_env"

exec cargo xtask test-all "$@"
