#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

OUT="${ROOT_DIR}/tests/fixtures/boot/int_sanity.bin"

echo "note: scripts/build-int-sanity-bootsector.sh is a legacy wrapper." >&2
echo "note: the canonical, assembler-free generator is: cargo xtask fixtures" >&2
echo >&2

cd "${ROOT_DIR}"
cargo xtask fixtures

SIZE="$(stat -c%s "${OUT}")"
if [[ "${SIZE}" -ne 512 ]]; then
  echo "error: expected ${OUT} to be exactly 512 bytes, got ${SIZE}" >&2
  exit 1
fi

echo "wrote ${OUT}"
