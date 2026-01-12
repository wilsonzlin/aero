#!/bin/bash
# Run a command with a timeout and SIGKILL fallback.
#
# DEFENSIVE: Assumes the command may hang forever or ignore SIGTERM.
#
# Usage:
#   bash ./scripts/with-timeout.sh <seconds> <command...>
#   bash ./scripts/with-timeout.sh 600 cargo build --release --locked
#
# On timeout:
# 1. Sends SIGTERM to the process
# 2. Waits 10 seconds for graceful shutdown
# 3. Sends SIGKILL if still running (non-negotiable)
#
# Exit codes:
#   0       Command completed successfully
#   1-123   Command failed with that exit code
#   124     Command timed out (SIGTERM sent)
#   137     Command killed (SIGKILL after ignoring SIGTERM)

set -euo pipefail

if [[ $# -lt 2 ]]; then
    echo "Usage: $0 <timeout-seconds> <command...>" >&2
    echo "Example: $0 600 cargo build --release --locked" >&2
    exit 1
fi

TIMEOUT_SECS="$1"
shift

# Validate timeout is a positive integer
if ! [[ "$TIMEOUT_SECS" =~ ^[0-9]+$ ]] || [[ "$TIMEOUT_SECS" -eq 0 ]]; then
    echo "[timeout] ERROR: Invalid timeout: $TIMEOUT_SECS (must be positive integer)" >&2
    exit 1
fi

# Find timeout command (GNU coreutils)
TIMEOUT_CMD=""
if command -v timeout &>/dev/null; then
    TIMEOUT_CMD="timeout"
elif command -v gtimeout &>/dev/null; then
    TIMEOUT_CMD="gtimeout"
else
    echo "[timeout] WARNING: 'timeout' command not found. Running without timeout!" >&2
    echo "[timeout] Install: brew install coreutils (macOS) or apt install coreutils (Linux)" >&2
    exec "$@"
fi

# CRITICAL: Use -k to send SIGKILL after grace period.
# Misbehaving code can ignore SIGTERM indefinitely.
exec "$TIMEOUT_CMD" -k 10 "${TIMEOUT_SECS}" "$@"
