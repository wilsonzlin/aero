#!/bin/sh
#
# QEMU helper: Windows 7 SP1 x64 virtio-input bring-up (virtio keyboard + mouse).
#
# This script is intentionally POSIX-shell compatible (no bashisms) so it can be
# run on most hosts.
#
# Requirements encoded here (see docs/windows-device-contract.md):
# - Always use contract-friendly virtio-pci flags:
#     disable-legacy=on,x-pci-revision=0x01
# - Keep PS/2 input enabled by default to avoid losing input during driver install.
# - Optional `--multifunction` mirrors the Aero contract topology (00:0a.0 + 00:0a.1).
#
# Usage:
#   ./run-win7-x64.sh [--multifunction] [--i8042-off] <disk-image> [-- <extra-qemu-args...>]
#
set -eu

usage() {
  cat >&2 <<'EOF'
Usage:
  run-win7-x64.sh [--multifunction] [--i8042-off] <disk-image> [-- <extra-qemu-args...>]

Args:
  <disk-image>     VM disk path (qcow2/vhd/raw/etc). Passed to QEMU as an IDE disk.

Options:
  --multifunction  Put keyboard + mouse on the same PCI slot (00:0a.0 + 00:0a.1).
  --i8042-off      Disable the emulated PS/2 controller (i8042=off). Only use after
                   the virtio-input driver is installed, otherwise you may lose input.
  -h, --help       Show this help.

Notes:
  - Always includes: disable-legacy=on,x-pci-revision=0x01
  - Prints the exact QEMU command line before exec.
EOF
}

quote_sh() {
  # Print a shell-escaped representation of $1 that can be copy/pasted.
  #
  # Use single-quote escaping (POSIX):  abc'def -> 'abc'"'"'def'
  v=$1
  case $v in
    '')
      printf "''"
      ;;
    *[!A-Za-z0-9_.,:/@%+=-]*)
      # Contains characters that need quoting.
      printf "'%s'" "$(printf "%s" "$v" | sed "s/'/'\\\"'\\\"'/g")"
      ;;
    *)
      printf "%s" "$v"
      ;;
  esac
}

print_cmd() {
  first=1
  for arg in "$@"; do
    if [ "$first" -eq 0 ]; then
      printf ' '
    fi
    first=0
    quote_sh "$arg"
  done
  printf '\n'
}

multifunction=0
i8042_off=0

while [ $# -gt 0 ]; do
  case "$1" in
    --multifunction)
      multifunction=1
      shift
      ;;
    --i8042-off)
      i8042_off=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    --)
      shift
      break
      ;;
    -*)
      echo "error: unknown option: $1" >&2
      usage
      exit 2
      ;;
    *)
      break
      ;;
  esac
done

if [ $# -lt 1 ]; then
  echo "error: missing <disk-image> argument" >&2
  usage
  exit 2
fi

disk=$1
shift

if [ ! -e "$disk" ]; then
  echo "error: disk image not found: $disk" >&2
  exit 2
fi

# Optional separator between disk image and extra QEMU args.
if [ $# -gt 0 ] && [ "$1" = "--" ]; then
  shift
fi

qemu_bin=${QEMU_BIN:-qemu-system-x86_64}
if ! command -v "$qemu_bin" >/dev/null 2>&1; then
  echo "error: qemu binary not found: $qemu_bin" >&2
  echo "hint: install QEMU and/or set QEMU_BIN=/path/to/qemu-system-x86_64" >&2
  exit 2
fi

machine="pc"
accel=${QEMU_ACCEL:-}
if [ -z "$accel" ]; then
  accel=kvm
  # Auto-fallback to TCG when KVM is unavailable or inaccessible.
  if [ ! -c /dev/kvm ] || [ ! -r /dev/kvm ] || [ ! -w /dev/kvm ]; then
    accel=tcg
  fi
fi
machine="${machine},accel=${accel}"
if [ "$i8042_off" -eq 1 ]; then
  machine="${machine},i8042=off"
fi

virtio_common="disable-legacy=on,x-pci-revision=0x01"
if [ "$multifunction" -eq 1 ]; then
  virtio_kbd="virtio-keyboard-pci,addr=0x0a,multifunction=on,${virtio_common}"
  virtio_mouse="virtio-mouse-pci,addr=0x0a.1,${virtio_common}"
else
  virtio_kbd="virtio-keyboard-pci,${virtio_common}"
  virtio_mouse="virtio-mouse-pci,${virtio_common}"
fi

# Build argv and then exec (so we can print the exact command line).
set -- \
  "$qemu_bin" \
  -machine "$machine" \
  -m 4096 \
  -cpu qemu64 \
  -drive "file=${disk},if=ide" \
  -device "$virtio_kbd" \
  -device "$virtio_mouse" \
  -net nic,model=e1000 \
  -net user \
  "$@"

echo "[virtio-input/qemu] exec:" >&2
print_cmd "$@" >&2
exec "$@"
