#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage:
  ./drivers/windows7/virtio-snd/tests/qemu/run-virtio-snd.sh [options] [-- <extra qemu args...>]

Launch a Windows 7 VM under QEMU with a virtio-snd device configured for the
strict Aero contract v1 identity when supported:
  - modern-only:    disable-legacy=on
  - revision-gated: x-pci-revision=0x01   (HWID: PCI\VEN_1AF4&DEV_1059&REV_01)

If your QEMU binary does not support these properties, the script falls back to
QEMU's default/transitional identity (DEV_1018) and prints instructions to use
the legacy INF: inf/aero-virtio-snd-legacy.inf.

Options:
  --arch <x86|x64>         Guest architecture (default: x64)
  --disk <path>            VM boot disk image path
                           Default: $AERO_VIRTIO_SND_DISK or $AERO_WINDOWS7_IMAGE
                                    or ./win7-<arch>.qcow2
  --disk-format <fmt>      QEMU drive format (qcow2|raw|...) (default: inferred)
  --mem <size>             Memory size passed to QEMU -m (default: 2048 for x86, 4096 for x64)
  --wav <path>             Output WAV path for QEMU's wav audiodev backend
                           Default: ./virtio-snd-<arch>.wav
  --qemu <path>            QEMU binary (default: $AERO_QEMU or qemu-system-i386/x86_64)
  --no-probe               Do not probe QEMU for virtio-snd property support
                           (always assume contract-v1 properties are accepted)
  --print                  Print the final QEMU command line and exit without running it
  -h, --help               Show this help

Environment variables (optional):
  AERO_QEMU                Path to QEMU binary (canonical repo env var)
  AERO_WINDOWS7_IMAGE       Default Windows 7 disk image path (canonical repo env var)
  AERO_VIRTIO_SND_DISK      Override disk image path
  AERO_VIRTIO_SND_ARCH      Override --arch
  AERO_VIRTIO_SND_MEM       Override --mem
  AERO_VIRTIO_SND_WAV       Override --wav

Examples:
  # x64, contract-v1 when supported, capture guest playback to a WAV:
  ./drivers/windows7/virtio-snd/tests/qemu/run-virtio-snd.sh --disk win7-x64.qcow2

  # Print the QEMU command without running it:
  ./drivers/windows7/virtio-snd/tests/qemu/run-virtio-snd.sh --print --arch x86

  # Pass additional QEMU args (e.g. use TCG-only, enable serial console):
  ./drivers/windows7/virtio-snd/tests/qemu/run-virtio-snd.sh -- --accel tcg -serial mon:stdio
EOF
}

arch="${AERO_VIRTIO_SND_ARCH:-x64}"
disk="${AERO_VIRTIO_SND_DISK:-${AERO_WINDOWS7_IMAGE:-}}"
disk_format=""
mem="${AERO_VIRTIO_SND_MEM:-}"
wav_path="${AERO_VIRTIO_SND_WAV:-}"
qemu_bin="${AERO_QEMU:-}"
print_only=0
no_probe=0
passthru_args=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --arch)
      arch="${2:-}"
      shift 2
      ;;
    --arch=*)
      arch="${1#--arch=}"
      shift
      ;;
    --disk)
      disk="${2:-}"
      shift 2
      ;;
    --disk=*)
      disk="${1#--disk=}"
      shift
      ;;
    --disk-format)
      disk_format="${2:-}"
      shift 2
      ;;
    --disk-format=*)
      disk_format="${1#--disk-format=}"
      shift
      ;;
    --mem|-m)
      mem="${2:-}"
      shift 2
      ;;
    --mem=*|-m=*)
      mem="${1#*=}"
      shift
      ;;
    --wav)
      wav_path="${2:-}"
      shift 2
      ;;
    --wav=*)
      wav_path="${1#--wav=}"
      shift
      ;;
    --qemu)
      qemu_bin="${2:-}"
      shift 2
      ;;
    --qemu=*)
      qemu_bin="${1#--qemu=}"
      shift
      ;;
    --no-probe)
      no_probe=1
      shift
      ;;
    --print)
      print_only=1
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

case "${arch}" in
  x86|i386)
    arch="x86"
    ;;
  x64|x86_64|amd64)
    arch="x64"
    ;;
  *)
    echo "Invalid --arch value: ${arch} (expected x86 or x64)" >&2
    exit 2
    ;;
esac

if [[ -z "${disk}" ]]; then
  disk="win7-${arch}.qcow2"
fi

if [[ -z "${mem}" ]]; then
  if [[ "${arch}" == "x86" ]]; then
    mem="2048"
  else
    mem="4096"
  fi
fi

if [[ -z "${wav_path}" ]]; then
  wav_path="virtio-snd-${arch}.wav"
fi

if [[ -z "${qemu_bin}" ]]; then
  if [[ "${arch}" == "x86" ]]; then
    if command -v qemu-system-i386 >/dev/null 2>&1; then
      qemu_bin="qemu-system-i386"
    elif command -v qemu-system-x86_64 >/dev/null 2>&1; then
      # Some distros don't ship qemu-system-i386, but x86_64 can still run 32-bit guests.
      qemu_bin="qemu-system-x86_64"
    else
      qemu_bin="qemu-system-i386"
    fi
  else
    qemu_bin="qemu-system-x86_64"
  fi
fi

cpu="qemu64"
if [[ "${arch}" == "x86" ]]; then
  cpu="qemu32"
fi

infer_disk_format() {
  local path="$1"
  case "$path" in
    *.qcow2|*.qcow)
      echo "qcow2"
      ;;
    *.raw|*.img)
      echo "raw"
      ;;
    *)
      echo ""
      ;;
  esac
}

if [[ -z "${disk_format}" ]]; then
  disk_format="$(infer_disk_format "${disk}")"
fi

probe_warned=0
qemu_has_binary() {
  command -v "${qemu_bin}" >/dev/null 2>&1 || [[ -x "${qemu_bin}" ]]
}

pick_virtio_snd_device() {
  # Prefer the common upstream name, but fall back to aliases when needed.
  local qemu="$1"

  if [[ "${no_probe}" -eq 1 ]]; then
    echo "virtio-sound-pci"
    return
  fi

  if ! qemu_has_binary; then
    if [[ "${probe_warned}" -eq 0 ]]; then
      echo "Warning: QEMU binary not found (${qemu_bin}); cannot probe device name/properties." >&2
      probe_warned=1
    fi
    echo "virtio-sound-pci"
    return
  fi

  local dev_help
  dev_help="$("$qemu" -device help 2>&1 || true)"
  if echo "$dev_help" | grep -qE '(^|[[:space:]])virtio-sound-pci([[:space:]]|$)'; then
    echo "virtio-sound-pci"
    return
  fi
  if echo "$dev_help" | grep -qE '(^|[[:space:]])virtio-snd-pci([[:space:]]|$)'; then
    echo "virtio-snd-pci"
    return
  fi

  echo "Error: QEMU does not appear to provide a virtio-snd PCI device." >&2
  echo "       Upgrade QEMU (known-good reference: 8.2.x)." >&2
  echo "       You can confirm with: ${qemu_bin} -device help | grep -E \"virtio-(sound|snd)-pci\"" >&2
  exit 1
}

device_type="$(pick_virtio_snd_device "${qemu_bin}")"

qemu_supports_contract_v1_props=1
if [[ "${no_probe}" -eq 0 ]]; then
  if qemu_has_binary; then
    dev_props="$("${qemu_bin}" -device "${device_type},help" 2>&1 || true)"
    if ! echo "${dev_props}" | grep -q "disable-legacy"; then
      qemu_supports_contract_v1_props=0
    fi
    if ! echo "${dev_props}" | grep -q "x-pci-revision"; then
      qemu_supports_contract_v1_props=0
    fi
  else
    # For --print in minimal environments, keep a useful command line even if we
    # cannot probe. Running without the properties is always safe, but the goal
    # of this helper is contract-v1 validation.
    qemu_supports_contract_v1_props=1
    if [[ "${probe_warned}" -eq 0 ]]; then
      echo "Warning: QEMU binary not found (${qemu_bin}); cannot probe virtio-snd properties." >&2
      echo "         Command line will assume contract-v1 properties are supported." >&2
      echo "         If QEMU rejects them, use inf/aero-virtio-snd-legacy.inf and remove disable-legacy=on." >&2
      probe_warned=1
    fi
  fi
fi

virtio_snd_opts=("audiodev=aerosnd0")
if [[ "${qemu_supports_contract_v1_props}" -eq 1 ]]; then
  virtio_snd_opts+=("disable-legacy=on" "x-pci-revision=0x01")
else
  echo "Note: your QEMU build does not support virtio-snd contract-v1 properties." >&2
  echo "      Missing one or both of: disable-legacy=on, x-pci-revision=0x01" >&2
  echo "      The strict Aero INF (inf/aero_virtio_snd.inf) will NOT bind." >&2
  echo "      Use the legacy INF instead: inf/aero-virtio-snd-legacy.inf" >&2
  echo "      (and run without disable-legacy=on / revision gating)." >&2
fi

drive_arg="file=${disk},if=ide"
if [[ -n "${disk_format}" ]]; then
  drive_arg+=",format=${disk_format}"
fi

cmd=(
  "${qemu_bin}"
  -machine pc,accel=kvm:tcg
  -m "${mem}"
  -cpu "${cpu}"
  -drive "${drive_arg}"
  -net nic,model=e1000 -net user
  -audiodev "wav,id=aerosnd0,path=${wav_path}"
  -device "${device_type},$(IFS=,; echo "${virtio_snd_opts[*]}")"
)

if [[ "${#passthru_args[@]}" -gt 0 ]]; then
  cmd+=("${passthru_args[@]}")
fi

print_cmd() {
  local i
  for i in "${!cmd[@]}"; do
    if [[ "$i" -eq 0 ]]; then
      printf '%q' "${cmd[$i]}"
    else
      printf ' \\\n  %q' "${cmd[$i]}"
    fi
  done
  printf '\n'
}

if [[ "${print_only}" -eq 1 ]]; then
  print_cmd
  exit 0
fi

if ! qemu_has_binary; then
  echo "Error: QEMU binary not found: ${qemu_bin}" >&2
  echo "       Install qemu-system-x86_64 (and/or qemu-system-i386), or set AERO_QEMU/--qemu." >&2
  exit 1
fi

exec "${cmd[@]}"
