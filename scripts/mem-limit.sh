#!/bin/bash
# Run a command with a memory limit using systemd-run.
# Usage: ./scripts/mem-limit.sh <limit> <command...>
# Example: ./scripts/mem-limit.sh 12G cargo build --release --locked
#
# The limit can be specified as: 12G, 8192M, etc.
# If systemd-run is not available (non-systemd system), falls back to running
# the command directly with a warning.

set -e

if [[ $# -lt 2 ]]; then
    echo "Usage: $0 <memory-limit> <command...>"
    echo "Example: $0 12G cargo build --release --locked"
    exit 1
fi

LIMIT="$1"
shift

# Check if systemd-run is available and we can use it
if command -v systemd-run &>/dev/null; then
    # Try user scope first (no root required), fall back to system scope
    if systemd-run --user --scope -p MemoryMax="$LIMIT" true 2>/dev/null; then
        echo "[mem-limit] Running with ${LIMIT} memory limit (user scope)"
        exec systemd-run --user --scope -p MemoryMax="$LIMIT" -- "$@"
    elif systemd-run --scope -p MemoryMax="$LIMIT" true 2>/dev/null; then
        echo "[mem-limit] Running with ${LIMIT} memory limit (system scope)"
        exec systemd-run --scope -p MemoryMax="$LIMIT" -- "$@"
    else
        echo "[mem-limit] WARNING: systemd-run available but scopes failed, running without limit"
        exec "$@"
    fi
else
    echo "[mem-limit] WARNING: systemd-run not available, running without memory limit"
    exec "$@"
fi
