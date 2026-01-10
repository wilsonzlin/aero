#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

OUT_DIR="${ROOT_DIR}/test-images/freedos"
CACHE_DIR="${ROOT_DIR}/test-images/cache"

ZIP_URL="https://www.ibiblio.org/pub/micro/pc-stuff/freedos/files/distributions/1.4/FD14-FloppyEdition.zip"
ZIP_SHA256="45b1fa7c52dd996c3bfa5e352ffcd410781b952a6ad629f15a4c9ec4bbaefc5a"
ZIP_PATH="${CACHE_DIR}/FD14-FloppyEdition.zip"

# The patched image includes a small addition to FDAUTO.BAT that writes a
# known sentinel string to COM1 so CI can validate boot progress via serial.
OUT_IMG="${OUT_DIR}/fd14-boot-aero.img"

mkdir -p "${OUT_DIR}" "${CACHE_DIR}"

if ! command -v curl >/dev/null 2>&1; then
  echo "error: curl not found" >&2
  exit 1
fi
if ! command -v unzip >/dev/null 2>&1; then
  echo "error: unzip not found" >&2
  exit 1
fi
if ! command -v mcopy >/dev/null 2>&1 || ! command -v mtype >/dev/null 2>&1; then
  echo "error: mtools not found (need mcopy/mtype). Install it (e.g. \`apt-get install mtools\`)." >&2
  exit 1
fi

if [[ ! -f "${ZIP_PATH}" ]]; then
  echo "downloading FreeDOS floppy edition..."
  curl -L -o "${ZIP_PATH}" "${ZIP_URL}"
fi

echo "${ZIP_SHA256}  ${ZIP_PATH}" | sha256sum -c -

TMP_IMG="${OUT_IMG}.tmp"
unzip -p "${ZIP_PATH}" 144m/x86BOOT.img > "${TMP_IMG}"

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}" "${TMP_IMG}"' EXIT

mtype -i "${TMP_IMG}" ::fdauto.bat > "${TMP_DIR}/fdauto.bat"

awk 'NR==1 { print; print "echo AERO_FREEDOS_OK > COM1"; next } { print }' \
  "${TMP_DIR}/fdauto.bat" > "${TMP_DIR}/fdauto_patched.bat"

mcopy -o -i "${TMP_IMG}" "${TMP_DIR}/fdauto_patched.bat" ::fdauto.bat

mv "${TMP_IMG}" "${OUT_IMG}"
trap - EXIT
rm -rf "${TMP_DIR}"

echo "wrote ${OUT_IMG}"

