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
#   ./run-win7-x64.sh [--multifunction] [--i8042-off] [--vectors N] <disk-image> [--] [<extra-qemu-args...>]
#
set -eu

usage() {
  cat >&2 <<'EOF'
Usage:
  run-win7-x64.sh [--multifunction] [--i8042-off] [--vectors N] <disk-image> [--] [<extra-qemu-args...>]

Args:
  <disk-image>     VM disk path (qcow2/vhd/raw/etc). Passed to QEMU as an IDE disk.

Options:
  --multifunction  Put keyboard + mouse on the same PCI slot (00:0a.0 + 00:0a.1).
  --i8042-off      Disable the emulated PS/2 controller (i8042=off). Only use after
                   the virtio-input driver is installed, otherwise you may lose input.
  --vectors N      Request an MSI-X table size from QEMU (`-device virtio-*-pci,...,vectors=N`).
                  Best-effort: requires QEMU support for the `vectors` property. Windows may still
                  grant fewer messages; drivers fall back.
  -h, --help       Show this help.

Environment overrides:
  QEMU_BIN=...         Override qemu-system-x86_64.
  QEMU_ACCEL=...       Override -machine ...,accel=... (defaults to kvm when available).
  QEMU_DISK_FORMAT=... Override disk format detection (e.g. qcow2/vpc/raw/...).

Notes:
  - Always includes: disable-legacy=on,x-pci-revision=0x01
  - Prints the exact QEMU command line before exec.
  - Extra QEMU args may be passed after <disk-image>. The optional '--' separator
    is supported for clarity.
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
vectors=

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
    --vectors|--msix-vectors)
      if [ $# -lt 2 ]; then
        echo "error: --vectors requires an integer argument" >&2
        usage
        exit 2
      fi
      vectors=$2
      shift 2
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

if [ -n "$vectors" ]; then
  case "$vectors" in
    ''|*[!0-9]*)
      echo "error: --vectors must be a positive integer" >&2
      exit 2
      ;;
    0)
      echo "error: --vectors must be a positive integer (got 0)" >&2
      exit 2
      ;;
  esac
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
if [ -n "$vectors" ]; then
  virtio_common="${virtio_common},vectors=${vectors}"
fi
if [ "$multifunction" -eq 1 ]; then
  virtio_kbd="virtio-keyboard-pci,addr=0x0a,multifunction=on,${virtio_common}"
  virtio_mouse="virtio-mouse-pci,addr=0x0a.1,${virtio_common}"
else
  virtio_kbd="virtio-keyboard-pci,${virtio_common}"
  virtio_mouse="virtio-mouse-pci,${virtio_common}"
fi

# Detect disk format to avoid QEMU's "format not specified" warning and to support
# qcow2/vhd/etc images without requiring users to hand-edit the command line.
#
# Override with:
#   QEMU_DISK_FORMAT=qcow2 ./run-win7-x64.sh ...
disk_format=${QEMU_DISK_FORMAT:-}
if [ -z "$disk_format" ] && command -v qemu-img >/dev/null 2>&1; then
  disk_format=$(qemu-img info "$disk" 2>/dev/null | sed -n 's/^file format: //p' | awk 'NR==1{print $1}')
fi
if [ -z "$disk_format" ]; then
  case "$disk" in
    *.qcow2|*.QCOW2|*.qcow|*.QCOW) disk_format=qcow2 ;;
    *.vhd|*.VHD|*.vpc|*.VPC) disk_format=vpc ;;
    *.vhdx|*.VHDX) disk_format=vhdx ;;
    *.vmdk|*.VMDK) disk_format=vmdk ;;
    *.vdi|*.VDI) disk_format=vdi ;;
    *.raw|*.RAW|*.img|*.IMG|*.bin|*.BIN|*.dd|*.DD) disk_format=raw ;;
    *) disk_format=raw ;;
  esac
fi
drive_arg="file=${disk},if=ide,format=${disk_format}"

# Build argv and then exec (so we can print the exact command line).
set -- \
  "$qemu_bin" \
  -machine "$machine" \
  -m 4096 \
  -cpu qemu64 \
  -drive "$drive_arg" \
  -device "$virtio_kbd" \
  -device "$virtio_mouse" \
  -net nic,model=e1000 \
  -net user \
  "$@"

echo "[virtio-input/qemu] exec:" >&2
print_cmd "$@" >&2
exec "$@"
