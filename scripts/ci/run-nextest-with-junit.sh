#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat >&2 <<'EOF'
Usage: run-nextest-with-junit.sh --output <path> -- <nextest args...>

Runs `cargo nextest run` and writes a JUnit XML report to <path>.

Compatibility:
- If the installed `cargo-nextest` supports `--junit-path`, it is used directly.
- Otherwise, falls back to `--message-format libtest-json` and converts the JSON
  stream to JUnit via `scripts/ci/nextest_libtest_json_to_junit.py`.

Examples:
  scripts/ci/run-nextest-with-junit.sh --output test-results/rust.xml -- --workspace --all-features
  scripts/ci/run-nextest-with-junit.sh --output test-results/pkg.xml -- -p aero-storage-server --all-features
EOF
}

output=""
max_output_bytes=""
args=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --output)
      output="${2:-}"
      shift 2
      ;;
    --max-output-bytes)
      max_output_bytes="${2:-}"
      shift 2
      ;;
    --)
      shift
      args=("$@")
      break
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown argument: $1" >&2
      usage
      exit 2
      ;;
  esac
done

if [[ -z "$output" ]]; then
  echo "error: --output is required" >&2
  usage
  exit 2
fi

mkdir -p "$(dirname "$output")"

if cargo nextest run --help 2>&1 | grep -q -- "--junit-path"; then
  cargo nextest run "${args[@]}" --junit-path "$output"
  exit 0
fi

converter_args=(--output "$output")
if [[ -n "$max_output_bytes" ]]; then
  converter_args+=(--max-output-bytes "$max_output_bytes")
fi

# libtest JSON output is currently behind an opt-in env var. The nextest CLI
# also supports an explicit message-format version to keep machine-readable
# output stable across future nextest changes.
NEXTEST_EXPERIMENTAL_LIBTEST_JSON=1 \
  cargo nextest run "${args[@]}" --message-format-version 0.1 --message-format libtest-json \
  | python3 scripts/ci/nextest_libtest_json_to_junit.py "${converter_args[@]}"

