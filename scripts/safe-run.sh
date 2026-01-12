#!/bin/bash
# Run a command with both timeout and memory limit protections.
#
# DEFENSIVE: Assumes the command can hang, OOM, or misbehave in any way.
#
# Usage:
#   ./scripts/safe-run.sh <command...>
#   ./scripts/safe-run.sh cargo build --release --locked
#
# Default limits (override via environment):
#   AERO_TIMEOUT=600      (10 minutes)
#   AERO_MEM_LIMIT=12G    (12 GB virtual address space)
#
# Override example:
#   AERO_TIMEOUT=1200 AERO_MEM_LIMIT=16G ./scripts/safe-run.sh cargo build --release --locked

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Defaults - can be overridden via environment
TIMEOUT="${AERO_TIMEOUT:-600}"
MEM_LIMIT="${AERO_MEM_LIMIT:-12G}"

# Defensive defaults for shared-host agent execution.
#
# These reduce the likelihood of hitting per-user process/thread limits which can cause rustc to
# ICE when it fails to spawn its internal Rayon thread pool.
#
# Callers can always override by setting these variables explicitly.
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}"
export RAYON_NUM_THREADS="${RAYON_NUM_THREADS:-$CARGO_BUILD_JOBS}"

if [[ $# -lt 1 ]]; then
    echo "Usage: $0 <command...>" >&2
    echo "" >&2
    echo "Runs a command with timeout and memory limit protections." >&2
    echo "" >&2
    echo "Environment variables:" >&2
    echo "  AERO_TIMEOUT=600     Timeout in seconds (default: 600 = 10 min)" >&2
    echo "  AERO_MEM_LIMIT=12G   Memory limit (default: 12G)" >&2
    echo "" >&2
    echo "Examples:" >&2
    echo "  $0 cargo build --locked" >&2
    echo "  AERO_TIMEOUT=1200 $0 cargo build --release --locked" >&2
    echo "  AERO_MEM_LIMIT=8G $0 npm run build" >&2
    exit 1
fi

echo "[safe-run] Command: $*" >&2
echo "[safe-run] Timeout: ${TIMEOUT}s, Memory: ${MEM_LIMIT}" >&2
echo "[safe-run] Started: $(date -Iseconds 2>/dev/null || date)" >&2

# Chain: timeout (with SIGKILL fallback) wraps memory-limited command.
#
# Use the shared helper so we support both GNU `timeout` and macOS `gtimeout`
# consistently across scripts.
#
# Note: some agent environments lose executable bits in the working tree. Invoke
# our helper via `bash` so safe-run still works even if scripts are 0644.
exec bash "$SCRIPT_DIR/with-timeout.sh" "${TIMEOUT}" bash "$SCRIPT_DIR/run_limited.sh" --as "$MEM_LIMIT" -- "$@"
