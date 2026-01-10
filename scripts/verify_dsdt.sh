#!/usr/bin/env bash
set -euo pipefail

if ! command -v iasl >/dev/null 2>&1; then
  echo "error: iasl not found in PATH (install ACPICA iasl to verify dsdt.asl)" >&2
  exit 1
fi

tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

# Use `-p` so verification doesn't write artifacts into the repo.
iasl -tc -p "$tmp_dir/dsdt" crates/firmware/acpi/dsdt.asl >/dev/null
