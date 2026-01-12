#!/usr/bin/env bash
# Convenience wrapper around `scripts/bench_compare.py`.
#
# This script exists primarily so documentation and CI snippets can run a
# benchmark comparison step via:
#   bash ./scripts/compare-benchmarks.sh
#
# With no arguments, it tries to auto-detect common Criterion output layouts
# used by our workflows:
#   - PR workflow:
#       target/bench-base/criterion  vs  target/bench-new/criterion
#   - main/scheduled workflow:
#       baseline/target/bench-new/criterion  vs  target/bench-new/criterion
#   - simple local run:
#       baseline/target/criterion  vs  target/criterion
#
# For full control (including custom paths), pass args through directly:
#   bash ./scripts/compare-benchmarks.sh --base <dir> --new <dir> [other flags...]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

usage() {
  cat >&2 <<'EOF'
Usage:
  bash ./scripts/compare-benchmarks.sh [--base <dir> --new <dir> ...]

This is a thin wrapper around:
  python3 scripts/bench_compare.py

If invoked with no args, it auto-detects Criterion result directories in common
CI/local layouts (see script header for details).

Examples:
  bash ./scripts/compare-benchmarks.sh
  bash ./scripts/compare-benchmarks.sh --base target/bench-base/criterion --new target/bench-new/criterion --profile pr-smoke
EOF
}

# If the caller passed explicit args, defer entirely to the Python implementation.
if [[ $# -gt 0 ]]; then
  exec python3 "$SCRIPT_DIR/bench_compare.py" "$@"
fi

base=""
new=""

profile="${AERO_BENCH_COMPARE_PROFILE:-}"
thresholds_file="${AERO_BENCH_THRESHOLDS_FILE:-bench/perf_thresholds.json}"
markdown_out="${AERO_BENCH_COMPARE_MARKDOWN_OUT:-bench_reports/compare.md}"
json_out="${AERO_BENCH_COMPARE_JSON_OUT:-bench_reports/compare.json}"

if [[ -d "$ROOT_DIR/target/bench-base/criterion" && -d "$ROOT_DIR/target/bench-new/criterion" ]]; then
  base="target/bench-base/criterion"
  new="target/bench-new/criterion"
  profile="${profile:-pr-smoke}"
elif [[ -d "$ROOT_DIR/baseline/target/bench-new/criterion" && -d "$ROOT_DIR/target/bench-new/criterion" ]]; then
  base="baseline/target/bench-new/criterion"
  new="target/bench-new/criterion"
  profile="${profile:-nightly}"
elif [[ -d "$ROOT_DIR/baseline/target/criterion" && -d "$ROOT_DIR/target/criterion" ]]; then
  base="baseline/target/criterion"
  new="target/criterion"
  profile="${profile:-nightly}"
else
  echo "error: Could not auto-detect baseline/new Criterion directories." >&2
  echo "" >&2
  usage
  exit 2
fi

exec python3 "$SCRIPT_DIR/bench_compare.py" \
  --base "$base" \
  --new "$new" \
  --thresholds-file "$thresholds_file" \
  --profile "$profile" \
  --markdown-out "$markdown_out" \
  --json-out "$json_out"
