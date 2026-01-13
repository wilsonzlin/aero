#!/usr/bin/env bash

set -euo pipefail

usage() {
  cat <<'EOF'
Usage:
  ./info-pci.sh [<qemu-bin>]

Print the QEMU `info pci` line(s) for virtio-input and fail if the expected
vendor/device ID is not present.

Defaults:
  - QEMU binary: qemu-system-x86_64 (override with QEMU_BIN or arg)
  - Expected ID: 1af4:1052

Examples:
  ./info-pci.sh
  QEMU_BIN=qemu-system-x86_64 ./info-pci.sh
  ./info-pci.sh /opt/qemu/bin/qemu-system-x86_64
EOF
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

qemu_bin="${QEMU_BIN:-qemu-system-x86_64}"
if [[ $# -gt 0 ]]; then
  qemu_bin="$1"
  shift
fi

if [[ $# -ne 0 ]]; then
  echo "Unexpected arguments: $*" >&2
  usage >&2
  exit 2
fi

if [[ "$qemu_bin" == */* ]]; then
  if [[ ! -x "$qemu_bin" ]]; then
    echo "QEMU binary is not executable: $qemu_bin" >&2
    exit 2
  fi
else
  if ! command -v "$qemu_bin" >/dev/null 2>&1; then
    echo "QEMU binary not found on PATH: $qemu_bin" >&2
    echo "Set QEMU_BIN=... or pass it as the first argument." >&2
    exit 2
  fi
fi

expected_id="1af4:1052"
qemu_args=(
  -nodefaults
  -machine q35
  -m 128
  -nographic
  -monitor stdio
  -device virtio-keyboard-pci
)

output="$(
  printf 'info pci\nquit\n' | "$qemu_bin" "${qemu_args[@]}" 2>&1
)" || {
  status=$?
  echo "$output" >&2
  exit "$status"
}

matches="$(printf '%s\n' "$output" | grep -i "$expected_id" || true)"
if [[ -n "$matches" ]]; then
  printf '%s\n' "$matches"
  exit 0
fi

echo "Expected to find PCI ID ${expected_id} in QEMU monitor output, but it was not present." >&2
echo "Command:" >&2
echo "  printf 'info pci\\nquit\\n' | ${qemu_bin} ${qemu_args[*]}" >&2
echo "" >&2
echo "$output" >&2
exit 1
