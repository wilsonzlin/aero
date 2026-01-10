#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

SRC="${ROOT_DIR}/test_images/boot_sectors/int_sanity.asm"
OUT="${ROOT_DIR}/test_images/boot_sectors/int_sanity.bin"

if ! command -v nasm >/dev/null 2>&1; then
  echo "error: nasm not found. Install it (e.g. \`apt-get install nasm\`)." >&2
  exit 1
fi

nasm -f bin "${SRC}" -o "${OUT}"

SIZE="$(stat -c%s "${OUT}")"
if [[ "${SIZE}" -ne 512 ]]; then
  echo "error: expected ${OUT} to be exactly 512 bytes, got ${SIZE}" >&2
  exit 1
fi

echo "wrote ${OUT}"

