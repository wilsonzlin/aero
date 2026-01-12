#!/usr/bin/env bash

set -euo pipefail

usage() {
  cat <<'EOF'
Usage:
  bash ./drivers/scripts/make-virtio-driver-iso.sh --virtio-win-iso <path> [options] [-- <extra pwsh args...>]

Builds `aero-virtio-win7-drivers.iso` on Linux/macOS by:
  1) Extracting the virtio-win ISO with `tools/virtio-win/extract.py`
  2) Invoking `drivers/scripts/make-virtio-driver-iso.ps1 -VirtioWinRoot <extracted>`

Options:
  --virtio-win-iso <path>   Path to virtio-win.iso (required)
  --out-iso <path>          Output ISO path (default: drivers/out/aero-virtio-win7-drivers.iso)
  --keep-extracted          Do not delete the temporary extracted virtio-win root
  -h, --help                Show this help

Any arguments after `--` are passed through to `make-virtio-driver-iso.ps1` (e.g.
`-Drivers viostor,netkvm` or `-StrictOptional`).

EOF
}

repo_root="$(
  cd "$(dirname "${BASH_SOURCE[0]}")/../.." >/dev/null 2>&1
  pwd
)"

virtio_iso=""
out_iso="${repo_root}/drivers/out/aero-virtio-win7-drivers.iso"
keep_extracted=0
passthru_args=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --virtio-win-iso)
      virtio_iso="${2:-}"
      shift 2
      ;;
    --out-iso)
      out_iso="${2:-}"
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
iso_ps1="${repo_root}/drivers/scripts/make-virtio-driver-iso.ps1"

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
  echo "pwsh not found on PATH. Install PowerShell 7 to run make-virtio-driver-iso.ps1." >&2
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

mkdir -p "$(dirname "$out_iso")"

pwsh -NoProfile -ExecutionPolicy Bypass -File "$iso_ps1" \
  -VirtioWinRoot "$extract_root" \
  -OutIso "$out_iso" \
  "${passthru_args[@]}"
