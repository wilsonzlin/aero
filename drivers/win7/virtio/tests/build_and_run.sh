#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT_DIR="$(mktemp -d)"
trap 'rm -rf "${OUT_DIR}"' EXIT

cc -std=c99 -Wall -Wextra -Werror \
  -I"${SCRIPT_DIR}/../virtio-core/portable" \
  -o "${OUT_DIR}/virtio_pci_cap_parser_test" \
  "${SCRIPT_DIR}/virtio_pci_cap_parser_test.c" \
  "${SCRIPT_DIR}/../virtio-core/portable/virtio_pci_cap_parser.c"

"${OUT_DIR}/virtio_pci_cap_parser_test"
