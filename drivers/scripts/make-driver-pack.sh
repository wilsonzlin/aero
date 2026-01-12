#!/usr/bin/env bash

set -euo pipefail

usage() {
  cat <<'EOF'
Usage:
  bash ./drivers/scripts/make-driver-pack.sh --virtio-win-iso <path> [options] [-- <extra pwsh args...>]

Builds `drivers/out/aero-win7-driver-pack` on Linux/macOS by:
  1) Extracting the virtio-win ISO with `tools/virtio-win/extract.py`
  2) Invoking `drivers/scripts/make-driver-pack.ps1 -VirtioWinRoot <extracted>`

Options:
  --virtio-win-iso <path>   Path to virtio-win.iso (required)
  --out-dir <dir>           Output directory for the driver pack (default: drivers/out)
  --keep-extracted          Do not delete the temporary extracted virtio-win root
  -h, --help                Show this help

Any arguments after `--` are passed through to `make-driver-pack.ps1` (e.g. `-NoZip`,
`-Drivers viostor,netkvm`, `-StrictOptional`).

The produced pack includes `manifest.json`, `THIRD_PARTY_NOTICES.md`, and (best-effort)
`licenses/virtio-win/` copied from the virtio-win ISO root when present.

Examples:
  # Default (best-effort include audio/input):
  bash ./drivers/scripts/make-driver-pack.sh --virtio-win-iso virtio-win.iso

  # Minimal pack, keep staging directory:
  bash ./drivers/scripts/make-driver-pack.sh --virtio-win-iso virtio-win.iso -- --NoZip -Drivers viostor,netkvm
EOF
}

repo_root="$(
  cd "$(dirname "${BASH_SOURCE[0]}")/../.." >/dev/null 2>&1
  pwd
)"

virtio_iso=""
out_dir="${repo_root}/drivers/out"
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
pack_ps1="${repo_root}/drivers/scripts/make-driver-pack.ps1"

if [[ ! -f "$extractor" ]]; then
  echo "Extractor not found: $extractor" >&2
  exit 1
fi
if [[ ! -f "$pack_ps1" ]]; then
  echo "Packaging script not found: $pack_ps1" >&2
  exit 1
fi

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
  echo "pwsh not found on PATH. Install PowerShell 7 to run make-driver-pack.ps1." >&2
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

pwsh -NoProfile -ExecutionPolicy Bypass -File "$pack_ps1" \
  -VirtioWinRoot "$extract_root" \
  -OutDir "$out_dir" \
  "${passthru_args[@]}"
