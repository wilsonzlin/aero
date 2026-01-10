#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Run Aero's full test stack (Rust, WASM, TypeScript, Playwright) with one command.

Usage:
  ./scripts/test-all.sh [options] [-- <extra playwright args>]

Options:
  --skip-rust           Skip Rust checks/tests (cargo fmt/clippy/test)
  --skip-wasm           Skip wasm-pack tests
  --skip-ts             Skip TypeScript unit tests (npm run test:unit)
  --skip-e2e            Skip Playwright smoke tests (npm run test:e2e)

  --webgpu              Run tests that require WebGPU (sets AERO_REQUIRE_WEBGPU=1)
  --no-webgpu           Do not require WebGPU (sets AERO_REQUIRE_WEBGPU=0) [default]

  --wasm-crate-dir <path>
                        Path (relative to repo root or absolute) to the wasm-pack crate dir
                        (defaults to $AERO_WASM_CRATE_DIR or a repo-default like crates/aero-wasm)
  --node-dir <path>     Path (relative to repo root or absolute) containing package.json
                        (defaults to $AERO_NODE_DIR or an auto-detected location)

  --pw-project <name>   Select a Playwright project (repeatable).
                        Example: --pw-project chromium --pw-project firefox

  -h, --help            Show this help.

Environment:
  AERO_REQUIRE_WEBGPU   If unset, defaults to 0 (to keep CI/dev behavior consistent).
  AERO_WASM_CRATE_DIR   Default wasm-pack crate directory (same as --wasm-crate-dir).
  AERO_NODE_DIR         Default Node workspace directory (same as --node-dir).

Examples:
  ./scripts/test-all.sh
  ./scripts/test-all.sh --skip-e2e
  ./scripts/test-all.sh --webgpu --pw-project chromium
  ./scripts/test-all.sh --pw-project chromium -- --grep smoke
EOF
}

die() {
  echo "error: $*" >&2
  exit 1
}

need_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    die "missing required command: $1"
  fi
}

run_in_dir() {
  local dir="$1"
  shift
  (cd "$dir" && "$@")
}

run_step() {
  local desc="$1"
  shift

  echo
  if [[ "${GITHUB_ACTIONS:-}" == "true" ]]; then
    echo "::group::$desc"
  else
    echo "==> $desc"
  fi

  # Temporarily disable `set -e` so we can always close the GitHub Actions log group.
  set +e
  "$@"
  local status=$?
  set -e

  if [[ "${GITHUB_ACTIONS:-}" == "true" ]]; then
    echo "::endgroup::"
  fi

  if [[ $status -ne 0 ]]; then
    exit "$status"
  fi
}

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

SKIP_RUST=0
SKIP_WASM=0
SKIP_TS=0
SKIP_E2E=0

AERO_REQUIRE_WEBGPU="${AERO_REQUIRE_WEBGPU:-0}"
WASM_CRATE_DIR="${AERO_WASM_CRATE_DIR:-${AERO_WASM_DIR:-}}"
NODE_DIR="${AERO_NODE_DIR:-${AERO_WEB_DIR:-}}"

PW_ARGS=()
PW_EXTRA_ARGS=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    -h | --help)
      usage
      exit 0
      ;;
    --skip-rust)
      SKIP_RUST=1
      ;;
    --skip-wasm)
      SKIP_WASM=1
      ;;
    --skip-ts | --skip-unit)
      SKIP_TS=1
      ;;
    --skip-e2e)
      SKIP_E2E=1
      ;;
    --webgpu | --require-webgpu)
      AERO_REQUIRE_WEBGPU=1
      ;;
    --no-webgpu | --no-require-webgpu)
      AERO_REQUIRE_WEBGPU=0
      ;;
    --wasm-crate-dir | --wasm-dir)
      shift
      [[ $# -gt 0 ]] || die "--wasm-crate-dir requires a value"
      WASM_CRATE_DIR="$1"
      ;;
    --wasm-crate-dir=* | --wasm-dir=*)
      WASM_CRATE_DIR="${1#*=}"
      ;;
    --node-dir | --web-dir)
      shift
      [[ $# -gt 0 ]] || die "--node-dir requires a value"
      NODE_DIR="$1"
      ;;
    --node-dir=* | --web-dir=*)
      NODE_DIR="${1#*=}"
      ;;
    --pw-project | --project)
      shift
      [[ $# -gt 0 ]] || die "--pw-project requires a value"
      PW_ARGS+=("--project=$1")
      ;;
    --pw-project=* | --project=*)
      PW_ARGS+=("--project=${1#*=}")
      ;;
    --)
      shift
      PW_EXTRA_ARGS=("$@")
      break
      ;;
    *)
      echo "error: unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
  shift
done

export AERO_REQUIRE_WEBGPU

normalize_dir() {
  local path="$1"
  if [[ -z "$path" ]]; then
    die "internal error: normalize_dir called with empty path"
  fi
  if [[ "$path" = /* ]]; then
    echo "$path"
  else
    echo "$ROOT_DIR/$path"
  fi
}

ensure_node_dir() {
  if [[ -n "$NODE_DIR" ]]; then
    NODE_DIR="$(normalize_dir "$NODE_DIR")"
    [[ -f "$NODE_DIR/package.json" ]] || die "package.json not found in node dir: $NODE_DIR"
    return
  fi

  local candidate
  for candidate in "$ROOT_DIR" "$ROOT_DIR/frontend" "$ROOT_DIR/web"; do
    if [[ -f "$candidate/package.json" ]]; then
      NODE_DIR="$candidate"
      return
    fi
  done

  die "unable to locate package.json; pass --node-dir <path> or set AERO_NODE_DIR"
}

ensure_wasm_crate_dir() {
  if [[ -n "$WASM_CRATE_DIR" ]]; then
    WASM_CRATE_DIR="$(normalize_dir "$WASM_CRATE_DIR")"
    [[ -f "$WASM_CRATE_DIR/Cargo.toml" ]] || die "Cargo.toml not found in wasm crate dir: $WASM_CRATE_DIR"
    return
  fi

  # Prefer canonical wasm-pack crate locations when present to avoid ambiguous
  # auto-detection (the workspace contains multiple `cdylib` crates).
  local candidate
  for candidate in \
    "$ROOT_DIR/crates/aero-wasm" \
    "$ROOT_DIR/crates/wasm" \
    "$ROOT_DIR/crates/aero-ipc" \
    "$ROOT_DIR/wasm" \
    "$ROOT_DIR/rust/wasm"; do
    if [[ -f "$candidate/Cargo.toml" ]]; then
      WASM_CRATE_DIR="$candidate"
      return
    fi
  done

  need_cmd cargo
  need_cmd python3
  [[ -f "$ROOT_DIR/Cargo.toml" ]] || die "Cargo.toml not found at repo root: $ROOT_DIR"

  local manifest_path
  manifest_path="$(
    cd "$ROOT_DIR" && python3 - <<'PY'
import json
import subprocess
import sys

meta = json.loads(subprocess.check_output(["cargo", "metadata", "--no-deps", "--format-version=1"]))
for pkg in meta.get("packages", []):
    for tgt in pkg.get("targets", []):
        if "cdylib" in tgt.get("kind", []):
            print(pkg.get("manifest_path", ""))
            sys.exit(0)
print("")
PY
  )"

  if [[ -z "$manifest_path" || "$manifest_path" == "null" ]]; then
    die "unable to auto-detect a wasm-pack crate (cdylib); pass --wasm-crate-dir <path> or set AERO_WASM_CRATE_DIR"
  fi

  WASM_CRATE_DIR="$(dirname "$manifest_path")"
  [[ -f "$WASM_CRATE_DIR/Cargo.toml" ]] || die "auto-detected wasm crate dir does not contain Cargo.toml: $WASM_CRATE_DIR"
}

CI_CARGO_ARGS=()
if [[ -f "$ROOT_DIR/Cargo.lock" ]]; then
  CI_CARGO_ARGS+=(--locked)
fi

if [[ $SKIP_RUST -eq 0 ]]; then
  need_cmd cargo
  [[ -f "$ROOT_DIR/Cargo.toml" ]] || die "Cargo.toml not found at repo root: $ROOT_DIR"

  run_step "Rust: cargo fmt --all -- --check" run_in_dir "$ROOT_DIR" cargo fmt --all -- --check
  run_step "Rust: cargo clippy" run_in_dir "$ROOT_DIR" cargo clippy "${CI_CARGO_ARGS[@]}" --workspace --all-targets --all-features -- -D warnings
  run_step "Rust: cargo test" run_in_dir "$ROOT_DIR" cargo test "${CI_CARGO_ARGS[@]}" --workspace --all-features
fi

if [[ $SKIP_WASM -eq 0 ]]; then
  need_cmd wasm-pack
  ensure_wasm_crate_dir

  run_step "WASM: wasm-pack test --node ($WASM_CRATE_DIR)" run_in_dir "$WASM_CRATE_DIR" wasm-pack test --node -- "${CI_CARGO_ARGS[@]}"
fi

if [[ $SKIP_TS -eq 0 ]]; then
  need_cmd npm
  ensure_node_dir

  run_step "TS: npm run test:unit ($NODE_DIR; AERO_REQUIRE_WEBGPU=$AERO_REQUIRE_WEBGPU)" env AERO_REQUIRE_WEBGPU="$AERO_REQUIRE_WEBGPU" run_in_dir "$NODE_DIR" npm run test:unit
fi

if [[ $SKIP_E2E -eq 0 ]]; then
  need_cmd npm
  ensure_node_dir

  run_step "E2E: npm run test:e2e ($NODE_DIR; AERO_REQUIRE_WEBGPU=$AERO_REQUIRE_WEBGPU)" env AERO_REQUIRE_WEBGPU="$AERO_REQUIRE_WEBGPU" run_in_dir "$NODE_DIR" npm run test:e2e -- "${PW_ARGS[@]}" "${PW_EXTRA_ARGS[@]}"
fi

echo
echo "==> All requested test steps passed."
