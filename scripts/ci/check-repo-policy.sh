#!/usr/bin/env bash
set -euo pipefail

# Repository policy check.
#
# Goal:
# - Prevent accidentally committing proprietary/disallowed fixtures (e.g. Windows ISOs).
# - Prevent adding oversized binary blobs that bloat the repo and slow CI.
#
# This script is intentionally lightweight and should run fast in CI.

SIZE_LIMIT_MB="${SIZE_LIMIT_MB:-20}"
SIZE_LIMIT_BYTES=$((SIZE_LIMIT_MB * 1024 * 1024))

# Forbidden extensions (case-insensitive, without the leading dot).
# Keep this list in sync with the "do not commit" patterns in `.gitignore`.
FORBIDDEN_EXTENSIONS=(
  bin
  iso
  img
  ima
  vhd
  vhdx
  vmdk
  vdi
  qcow
  qcow2
  raw
  dd
  dsk
  hdd
  ova
  ovf
  wim
  esd
  swm
  cab
  msu
  msi
  exe
  dll
  sys
  drv
  ocx
  cpl
  pdb
  rom
  fd
  efi
)

# Allowlist for otherwise-forbidden extensions (bash patterns).
#
# These must remain tiny, deterministic, and license-safe. CI enforces a much
# smaller size cap for allowlisted items.
ALLOWLIST_FORBIDDEN_EXTENSION_GLOBS=(
  assets/bios.bin
  test_images/boot_sectors/int_sanity.bin
  tests/fixtures/bootsector.bin
  tests/fixtures/realmode_vbe_test.bin
  tests/fixtures/boot/*.bin
  tests/fixtures/boot/*.img
  tools/qemu_diff/boot/boot.bin
)

# Forbidden path patterns (case-insensitive, bash patterns).
# These are tuned to catch likely-proprietary Windows fixtures.
FORBIDDEN_PATH_GLOBS=(
  *test_images/windows*
  *fixtures/windows*
)

# Allowlist for known-safe, intentionally committed fixtures that would
# otherwise be rejected by extension/path rules.
#
# IMPORTANT: Entries are pinned to a specific blob hash so that these files
# cannot silently drift into proprietary binaries. If you intentionally update a
# pinned fixture, update the expected blob hash here (and explain why).
declare -A ALLOWLIST_FORBIDDEN_BLOB_OIDS=(
  # Synthetic, deterministic pattern images used for disk streaming E2E tests.
  [tools/disk-streaming-browser-e2e/fixtures/secret.img]=ebae3de9a572e69924863ebc50a4baaf8b3c14ac
  [tools/disk-streaming-browser-e2e/fixtures/win7.img]=ea8e482b990b87c0f69d29fd1dd6a41d0f1a514b
  # Tiny text placeholders used by packaging tests (not real Windows drivers).
  [tools/packaging/aero_packager/testdata/drivers/amd64/testdrv/test.sys]=699c2af51fe608c5bd954289ad4234763b5730c4
  [tools/packaging/aero_packager/testdata/drivers/x86/testdrv/test.sys]=7d44972c5540378feef4752f2c47d658f1ad3236
  [tools/packaging/aero_packager/testdata/drivers/amd64/testdrv/test.dll]=727f8a1c3b89565599bf9fa0ef6c8a902edf07e0
  [tools/packaging/aero_packager/testdata/drivers/x86/testdrv/test.dll]=c33eb7dce4ce37f644d00db459b5a0e90822e726
)

# Allowlist for large blobs (bash patterns). Keep this small and justified.
# Example:
#   ALLOWLIST_LARGE_FILE_GLOBS=(fixtures/oss/small-linux.img)
ALLOWLIST_LARGE_FILE_GLOBS=()

# Stricter size cap for allowlisted disk/firmware-like blobs.
ALLOWLISTED_BINARY_SIZE_LIMIT_BYTES=$((1 * 1024 * 1024))

BASE_REF="${BASE_REF:-}"
HEAD_REF="${HEAD_REF:-HEAD}"

is_all_zeros_sha() {
  local sha="$1"
  [[ "$sha" =~ ^0+$ ]]
}

resolve_base_ref() {
  # GitHub push events sometimes use an all-zeros "before" SHA when creating a
  # branch. Treat that as unset so we can fall back to a usable base ref.
  if [[ -n "$BASE_REF" ]] && ! is_all_zeros_sha "$BASE_REF"; then
    return 0
  fi
  BASE_REF=""

  # On GitHub Actions push events, use the "before" sha if available.
  if [[ "${GITHUB_EVENT_NAME:-}" == "push" && -n "${GITHUB_EVENT_PATH:-}" && -f "${GITHUB_EVENT_PATH:-}" ]]; then
    if command -v python3 >/dev/null 2>&1; then
      local before
      before="$(python3 - "$GITHUB_EVENT_PATH" <<'PY'
import json,sys
try:
  with open(sys.argv[1], "r", encoding="utf-8") as f:
    ev = json.load(f)
  print(ev.get("before", "") or "")
except Exception:
  print("")
PY
)"
      if [[ -n "$before" ]] && ! is_all_zeros_sha "$before"; then
        BASE_REF="$before"
        return 0
      fi
    fi
  fi

  # Fallbacks for local runs.
  if git rev-parse --verify -q origin/main >/dev/null 2>&1; then
    BASE_REF="origin/main"
  elif git rev-parse --verify -q main >/dev/null 2>&1; then
    BASE_REF="main"
  else
    BASE_REF="HEAD~1"
  fi
}

resolve_base_ref

if ! git rev-parse --verify -q "$BASE_REF" >/dev/null 2>&1; then
  echo "Repo policy check: could not resolve BASE_REF='$BASE_REF'."
  echo "Hint: set BASE_REF explicitly (e.g. BASE_REF=origin/main)."
  exit 2
fi

if ! git rev-parse --verify -q "$HEAD_REF" >/dev/null 2>&1; then
  echo "Repo policy check: could not resolve HEAD_REF='$HEAD_REF'."
  exit 2
fi

echo "Repo policy check: scanning changes in '$BASE_REF...$HEAD_REF'"

forbidden_hits=()
oversize_hits=()

matches_any_glob_ci() {
  local value_lc="$1"
  shift
  local glob
  for glob in "$@"; do
    local glob_lc
    glob_lc="$(printf '%s' "$glob" | tr '[:upper:]' '[:lower:]')"
    if [[ "$value_lc" == $glob_lc ]]; then
      return 0
    fi
  done
  return 1
}

is_forbidden_extension() {
  local ext_lc="$1"
  local forbidden
  for forbidden in "${FORBIDDEN_EXTENSIONS[@]}"; do
    if [[ "$ext_lc" == "$forbidden" ]]; then
      return 0
    fi
  done
  return 1
}

human_bytes() {
  local bytes="$1"
  if command -v numfmt >/dev/null 2>&1; then
    numfmt --to=iec --suffix=B "$bytes"
  else
    echo "${bytes}B"
  fi
}

# Iterate through added/modified/renamed/copied paths.
while IFS= read -r -d '' status; do
  path=""
  case "$status" in
    R*|C*)
      # old path (ignored), new path (checked)
      IFS= read -r -d '' _old_path
      IFS= read -r -d '' path
      ;;
    *)
      IFS= read -r -d '' path
      ;;
  esac

  # Only check files that exist in HEAD_REF (handles edge cases around renames).
  if ! git cat-file -e "$HEAD_REF:$path" 2>/dev/null; then
    continue
  fi

  path_lc="$(printf '%s' "$path" | tr '[:upper:]' '[:lower:]')"

  allowlisted_forbidden=0
  expected_oid="${ALLOWLIST_FORBIDDEN_BLOB_OIDS[$path_lc]:-}"
  if [[ -n "$expected_oid" ]]; then
    allowlisted_forbidden=1
    actual_oid="$(git rev-parse "$HEAD_REF:$path")"
    if [[ "$actual_oid" != "$expected_oid" ]]; then
      forbidden_hits+=("$path|allowlisted fixture changed (expected blob $expected_oid, got $actual_oid)")
    fi
  fi

  filename="${path##*/}"
  ext=""
  if [[ "$filename" == *.* && "$filename" != .* ]]; then
    ext="${filename##*.}"
  fi
  ext_lc="$(printf '%s' "$ext" | tr '[:upper:]' '[:lower:]')"
  is_allowlisted_forbidden_ext=false
  if matches_any_glob_ci "$path_lc" "${ALLOWLIST_FORBIDDEN_EXTENSION_GLOBS[@]}"; then
    is_allowlisted_forbidden_ext=true
  fi

  if [[ "$allowlisted_forbidden" -eq 0 ]]; then
    if matches_any_glob_ci "$path_lc" "${FORBIDDEN_PATH_GLOBS[@]}"; then
      forbidden_hits+=("$path|forbidden path (matches windows fixture pattern)")
    fi

    if [[ -n "$ext_lc" ]] && is_forbidden_extension "$ext_lc" && [[ "$is_allowlisted_forbidden_ext" == "false" ]]; then
      forbidden_hits+=("$path|forbidden extension '.$ext_lc'")
    fi
  fi

  blob_size="$(git cat-file -s "$HEAD_REF:$path")"

  if [[ "$is_allowlisted_forbidden_ext" == "true" ]] && [[ "$blob_size" -gt "$ALLOWLISTED_BINARY_SIZE_LIMIT_BYTES" ]]; then
    oversize_hits+=("$path|$(human_bytes "$blob_size") (limit: 1MB for allowlisted binary fixtures)")
  fi

  if ! matches_any_glob_ci "$path_lc" "${ALLOWLIST_LARGE_FILE_GLOBS[@]}"; then
    if [[ "$blob_size" -gt "$SIZE_LIMIT_BYTES" ]]; then
      oversize_hits+=("$path|$(human_bytes "$blob_size") (limit: ${SIZE_LIMIT_MB}MB)")
    fi
  fi
done < <(git diff --name-status -z --diff-filter=ACMR "$BASE_REF...$HEAD_REF")

if (( ${#forbidden_hits[@]} == 0 && ${#oversize_hits[@]} == 0 )); then
  echo "Repo policy check: OK"
  exit 0
fi

echo
echo "ERROR: Repository policy violations detected."
echo

if (( ${#forbidden_hits[@]} > 0 )); then
  echo "Disallowed/proprietary fixture types detected:"
  for hit in "${forbidden_hits[@]}"; do
    path="${hit%%|*}"
    reason="${hit#*|}"
    echo "  - $path ($reason)"
    if [[ "${GITHUB_ACTIONS:-}" == "true" ]]; then
      echo "::error file=$path::Repository policy violation: $reason"
    fi
  done
  echo
fi

if (( ${#oversize_hits[@]} > 0 )); then
  echo "Oversized files detected (new/changed blobs should stay under ${SIZE_LIMIT_MB}MB):"
  for hit in "${oversize_hits[@]}"; do
    path="${hit%%|*}"
    detail="${hit#*|}"
    echo "  - $path ($detail)"
    if [[ "${GITHUB_ACTIONS:-}" == "true" ]]; then
      echo "::error file=$path::Repository policy violation: file too large ($detail)"
    fi
  done
  echo
fi

cat <<EOF
Remediation guidance:
  - Do NOT commit Windows installation media (ISO/WIM/etc), BIOS/firmware dumps,
    proprietary drivers, or other copyrighted binaries.
  - Keep fixtures small and open-source. Prefer generating fixtures at runtime
    (e.g., create minimal disk images during tests) or downloading them from a
    vetted external source as part of local-only test setup.
  - If you believe a large/open-source asset should live in-repo, discuss it
    first and add an explicit allowlist entry in:
      scripts/ci/check-repo-policy.sh

See also: docs/13-legal-considerations.md
See also: docs/FIXTURES.md
EOF

exit 1
