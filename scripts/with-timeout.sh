#!/bin/bash
# Run a command with a timeout, with graceful shutdown.
# Usage: ./scripts/with-timeout.sh <seconds> <command...>
# Example: ./scripts/with-timeout.sh 600 cargo build --release
#
# On timeout:
# 1. Sends SIGTERM to the process group
# 2. Waits 10 seconds
# 3. Sends SIGKILL if still running

set -e

if [[ $# -lt 2 ]]; then
    echo "Usage: $0 <timeout-seconds> <command...>"
    echo "Example: $0 600 cargo build --release"
    exit 1
fi

TIMEOUT_SECS="$1"
shift

echo "[timeout] Running with ${TIMEOUT_SECS}s timeout: $*"

# Use timeout with TERM signal, then KILL after 10s grace period
exec timeout --signal=TERM --kill-after=10s "${TIMEOUT_SECS}s" "$@"
