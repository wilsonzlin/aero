#!/usr/bin/env bash
set -euo pipefail

if ! command -v iasl >/dev/null 2>&1; then
  echo "error: iasl not found in PATH (install ACPICA iasl to validate ACPI tables)" >&2
  exit 1
fi

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

# 1) Verify the human-readable ASL source compiles cleanly.
bash "${ROOT_DIR}/scripts/verify_dsdt.sh"

# 2) Decompile + recompile the shipped AML tables.
#
# This catches regressions where the generated AML is not accepted by ACPICA,
# even if our internal checksum/structure checks still pass.
shopt -s nullglob
aml_tables=("${ROOT_DIR}/crates/firmware/acpi/"*.aml)
if [[ "${#aml_tables[@]}" -eq 0 ]]; then
  echo "error: no AML tables found under crates/firmware/acpi" >&2
  exit 1
fi

for table in "${aml_tables[@]}"; do
  base="$(basename "$table")"
  prefix="${base%.*}"

  cp "$table" "${tmp_dir}/${base}"

  (
    cd "$tmp_dir"
    # Decompile emits `${prefix}.dsl` in the current directory.
    iasl -d "${base}" >/dev/null

    dsl="${prefix}.dsl"
    if [[ ! -f "${dsl}" ]]; then
      echo "error: expected iasl to produce ${dsl} when decompiling ${base}" >&2
      ls -la >&2 || true
      exit 1
    fi

    # Recompile the decompiled DSL. Use `-p` so outputs stay in the temp dir.
    iasl -tc -p "${prefix}_recompiled" "${dsl}" >/dev/null
  )
done

echo "ACPI validation via iasl succeeded."

