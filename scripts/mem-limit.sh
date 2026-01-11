#!/bin/bash
# Run a command with a memory limit using systemd-run.
# Usage: ./scripts/mem-limit.sh <limit> <command...>
# Example: ./scripts/mem-limit.sh 12G cargo build --release --locked
#
# The limit can be specified as: 12G, 8192M, etc.
# If systemd-run is not available (or cannot connect to a systemd bus), falls
# back to `prlimit`/`ulimit` so agents still get a hard-ish ceiling in
# containerized environments.

set -euo pipefail

if [[ $# -lt 2 ]]; then
    echo "Usage: $0 <memory-limit> <command...>"
    echo "Example: $0 12G cargo build --release --locked"
    exit 1
fi

LIMIT="$1"
shift

parse_limit_bytes() {
    local raw="$1"
    local num suffix mul
    if [[ "$raw" =~ ^([0-9]+)([KkMmGgTt])?[Bb]?$ ]]; then
        num="${BASH_REMATCH[1]}"
        suffix="${BASH_REMATCH[2]:-}"
    else
        echo "error: invalid memory limit: $raw (expected e.g. 12G, 8192M)" >&2
        exit 1
    fi

    mul=1
    case "$suffix" in
        "" ) mul=1 ;;
        K|k) mul=$((1024)) ;;
        M|m) mul=$((1024 * 1024)) ;;
        G|g) mul=$((1024 * 1024 * 1024)) ;;
        T|t) mul=$((1024 * 1024 * 1024 * 1024)) ;;
        *) echo "error: invalid memory suffix: $suffix" >&2; exit 1 ;;
    esac

    echo $((num * mul))
}

# Check if systemd-run is available and we can use it
if command -v systemd-run &>/dev/null; then
    # Try user scope first (no root required), fall back to system scope
    if systemd-run --user --scope -p MemoryMax="$LIMIT" true 2>/dev/null; then
        echo "[mem-limit] Running with ${LIMIT} memory limit (user scope)"
        exec systemd-run --user --scope -p MemoryMax="$LIMIT" -- "$@"
    elif systemd-run --scope -p MemoryMax="$LIMIT" true 2>/dev/null; then
        echo "[mem-limit] Running with ${LIMIT} memory limit (system scope)"
        exec systemd-run --scope -p MemoryMax="$LIMIT" -- "$@"
    fi
fi

LIMIT_BYTES="$(parse_limit_bytes "$LIMIT")"

if command -v prlimit &>/dev/null; then
    echo "[mem-limit] Running with ${LIMIT} memory limit via prlimit (RLIMIT_AS)"
    exec prlimit --as="$LIMIT_BYTES" -- "$@"
fi

LIMIT_KB=$(((LIMIT_BYTES + 1023) / 1024))
if ulimit -Sv "$LIMIT_KB" 2>/dev/null && ulimit -Hv "$LIMIT_KB" 2>/dev/null; then
    echo "[mem-limit] Running with ${LIMIT} memory limit via ulimit (RLIMIT_AS)"
    exec "$@"
fi

echo "[mem-limit] WARNING: unable to enforce memory limit (missing systemd-run/prlimit/ulimit), running without limit" >&2
exec "$@"
