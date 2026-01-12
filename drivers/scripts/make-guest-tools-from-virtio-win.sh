#!/usr/bin/env bash

set -euo pipefail

usage() {
  cat <<'EOF'
Usage:
  bash ./drivers/scripts/make-guest-tools-from-virtio-win.sh --virtio-win-iso <path> [options] [-- <extra pwsh args...>]

Builds `aero-guest-tools.iso` + `aero-guest-tools.zip` on Linux/macOS by:
  1) Extracting the virtio-win ISO with `tools/virtio-win/extract.py`
  2) Invoking `drivers/scripts/make-guest-tools-from-virtio-win.ps1 -VirtioWinRoot <extracted>`

Options:
  --virtio-win-iso <path>      Path to virtio-win.iso (required)
  --out-dir <dir>              Output directory (default: dist/guest-tools)
  --version <ver>              Package version (default: 0.0.0)
  --build-id <id>              Build ID (default: local)
  --profile <minimal|full>     Packaging profile (default: full)
  --signing-policy <policy>    Signing policy (test|production|none) (default: none)
                                (legacy aliases: testsigning/test-signing -> test; nointegritychecks -> none; whql/prod -> production)
  --keep-extracted             Do not delete the temporary extracted virtio-win root
  -h, --help                   Show this help

Any arguments after `--` are passed through to `make-guest-tools-from-virtio-win.ps1`
(e.g. `-Drivers ...`, `-StrictOptional`, `-SpecPath ...`, `-CleanStage`).

EOF
}

repo_root="$(
  cd "$(dirname "${BASH_SOURCE[0]}")/../.." >/dev/null 2>&1
  pwd
)"

virtio_iso=""
out_dir="${repo_root}/dist/guest-tools"
version="0.0.0"
build_id="local"
profile="full"
signing_policy="none"
keep_extracted=0
passthru_args=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --virtio-win-iso)
      virtio_iso="${2:-}"
      shift 2
      ;;
    --out-dir)
      out_dir="${2:-}"
      shift 2
      ;;
    --version)
      version="${2:-}"
      shift 2
      ;;
    --build-id)
      build_id="${2:-}"
      shift 2
      ;;
    --profile)
      profile="${2:-}"
      shift 2
      ;;
    --signing-policy)
      signing_policy="${2:-}"
      shift 2
      ;;
    --keep-extracted)
      keep_extracted=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    --)
      shift
      passthru_args=("$@")
      break
      ;;
    *)
      echo "Unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ -z "${virtio_iso}" ]]; then
  echo "--virtio-win-iso is required" >&2
  usage >&2
  exit 2
fi

extractor="${repo_root}/tools/virtio-win/extract.py"
guest_ps1="${repo_root}/drivers/scripts/make-guest-tools-from-virtio-win.ps1"

python_bin=""
for c in python3 python; do
  if command -v "$c" >/dev/null 2>&1; then
    python_bin="$c"
    break
  fi
done
if [[ -z "$python_bin" ]]; then
  echo "Python not found on PATH (need python3)" >&2
  exit 1
fi

if ! command -v pwsh >/dev/null 2>&1; then
  echo "pwsh not found on PATH. Install PowerShell 7 to run make-guest-tools-from-virtio-win.ps1." >&2
  exit 1
fi

if ! command -v cargo >/dev/null 2>&1; then
  echo "cargo not found on PATH. Install Rust to build the Guest Tools packager." >&2
  exit 1
fi

tmp_root="$(mktemp -d 2>/dev/null || mktemp -d -t aero-virtio-win)"
extract_root="${tmp_root}/virtio-win-root"

cleanup() {
  if [[ "$keep_extracted" -eq 1 ]]; then
    echo "Keeping extracted virtio-win root: $extract_root" >&2
    return
  fi
  rm -rf "$tmp_root"
}
trap cleanup EXIT

"$python_bin" "$extractor" --virtio-win-iso "$virtio_iso" --out-root "$extract_root"

mkdir -p "$out_dir"

pwsh -NoProfile -ExecutionPolicy Bypass -File "$guest_ps1" \
  -VirtioWinRoot "$extract_root" \
  -OutDir "$out_dir" \
  -Version "$version" \
  -BuildId "$build_id" \
  -Profile "$profile" \
  -SigningPolicy "$signing_policy" \
  "${passthru_args[@]}"
