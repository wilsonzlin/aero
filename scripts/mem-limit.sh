#!/usr/bin/env bash
set -euo pipefail

# Backwards-compatible wrapper around `scripts/run_limited.sh`.
#
# Historical usage across this repo/docs:
#   bash ./scripts/mem-limit.sh 12G <command...>
#
# Prefer `scripts/run_limited.sh` directly if you need other limits (CPU/stack).

if [[ $# -lt 2 ]]; then
  echo "usage: scripts/mem-limit.sh <size> <command...>" >&2
  exit 2
fi

limit="$1"
shift

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec bash "${script_dir}/run_limited.sh" --as "${limit}" -- "$@"
