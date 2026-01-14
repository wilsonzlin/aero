#!/usr/bin/env bash
#
# CI helper: run the AeroGPU regression suite.
#
# This is a fast(-ish) sanity check for the canonical AeroGPU ABI + device model wiring:
# - TypeScript protocol mirrors (ABI drift): `npm run test:protocol`
# - Rust protocol mirrors: `cargo test -p aero-protocol`
# - Shared device-side model: `cargo test -p aero-devices-gpu`
# - Canonical machine ring/fence/bridge plumbing (selected `aerogpu_*` regression tests)
#
# Usage:
#   bash ./scripts/ci/run-aerogpu-tests.sh
#
# Notes:
# - Uses `scripts/safe-run.sh` for timeout/memory protection (override via env):
#     AERO_TIMEOUT=1800 AERO_MEM_LIMIT=16G bash ./scripts/ci/run-aerogpu-tests.sh
#
# Keep this suite focused: it should catch common regressions without pulling in heavy end-to-end
# rendering tests (those live in `aero-d3d9`, `aero-d3d11`, and wgpu-backed e2e harnesses).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"

cd "${REPO_ROOT}"

# Cold builds for the graphics stack can exceed `safe-run`'s default 10 minute timeout. Use a
# larger default for this suite so it is robust in fresh CI sandboxes.
: "${AERO_TIMEOUT:=1800}"
export AERO_TIMEOUT

run() {
  bash ./scripts/safe-run.sh "$@"
}

run npm run test:protocol
run cargo test -p aero-protocol --locked
run cargo test -p aero-devices-gpu --locked

run cargo test -p aero-machine --locked \
  --test aerogpu_pci_enumeration \
  --test aerogpu_bar0_mmio_vblank \
  --test aerogpu_intx_asserts_on_irq_enable \
  --test aerogpu_intx_is_gated_on_pci_command_intx_disable \
  --test aerogpu_immediate_backend_completes_fence \
  --test aerogpu_features \
  --test aerogpu_submission_bridge \
  --test aerogpu_deferred_fence_completion \
  --test aerogpu_complete_fence_gating \
  --test aerogpu_vsync_fence_pacing \
  --test aerogpu_ring_noop_fence \
  --test aerogpu_backend_scanout_display_present \
  --test aerogpu_mmio_gpa_overflow
