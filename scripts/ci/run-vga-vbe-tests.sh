#!/usr/bin/env bash
#
# CI helper: run the VGA/VBE/INT10 regression suite.
#
# This intentionally exercises the "boot display" stack end-to-end:
# - `aero-gpu-vga`: VGA/VBE device model + renderer
# - `firmware`: BIOS INT 10h VGA/VBE services
# - `aero-machine`: machine wiring + boot-sector integration tests
#
# Usage:
#   bash ./scripts/ci/run-vga-vbe-tests.sh
#
# Notes:
# - Uses `scripts/safe-run.sh` for timeout/memory protection (override via env):
#     AERO_TIMEOUT=3600 AERO_MEM_LIMIT=16G bash ./scripts/ci/run-vga-vbe-tests.sh
# - `cargo test` does not support globs for `--test`, so we enumerate integration
#   test binaries matching:
#     - `crates/aero-machine/tests/boot_int10_*.rs`
#     - `crates/aero-machine/tests/vga_*.rs`
#     - `crates/aero-machine/tests/aerogpu_legacy_*.rs`
#     - `crates/aero-machine/tests/bios_vga_sync.rs` (BIOS↔VGA/VBE sync regression tests)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"

cd "${REPO_ROOT}"

: "${AERO_TIMEOUT:=3600}"
export AERO_TIMEOUT

run() {
  bash ./scripts/safe-run.sh "$@"
}

run cargo test -p aero-gpu-vga --locked
run cargo test -p firmware --locked

shopt -s nullglob

boot_files=(crates/aero-machine/tests/boot_int10_*.rs)
if [[ ${#boot_files[@]} -eq 0 ]]; then
  echo "error: no aero-machine boot_int10_* integration tests found" >&2
  exit 1
fi

boot_args=()
for f in "${boot_files[@]}"; do
  boot_args+=(--test "$(basename "$f" .rs)")
done
run cargo test -p aero-machine --locked "${boot_args[@]}"

vga_files=(crates/aero-machine/tests/vga_*.rs)
if [[ ${#vga_files[@]} -eq 0 ]]; then
  echo "error: no aero-machine vga_* integration tests found" >&2
  exit 1
fi

vga_args=()
for f in "${vga_files[@]}"; do
  vga_args+=(--test "$(basename "$f" .rs)")
done
run cargo test -p aero-machine --locked "${vga_args[@]}"

legacy_files=(crates/aero-machine/tests/aerogpu_legacy_*.rs)
if [[ ${#legacy_files[@]} -eq 0 ]]; then
  echo "error: no aero-machine aerogpu_legacy_* integration tests found" >&2
  exit 1
fi

legacy_args=()
for f in "${legacy_files[@]}"; do
  legacy_args+=(--test "$(basename "$f" .rs)")
done
run cargo test -p aero-machine --locked "${legacy_args[@]}"

# Additional BIOS↔VGA/VBE sync regression coverage (palette mirroring, failed mode-set semantics,
# etc).
#
# This test binary is not named `boot_int10_*` or `vga_*`, but it is part of the same boot-display
# stack end-to-end contract.
if [[ -f crates/aero-machine/tests/bios_vga_sync.rs ]]; then
  run cargo test -p aero-machine --locked --test bios_vga_sync
fi
