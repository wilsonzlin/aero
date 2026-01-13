#!/bin/sh
# SPDX-License-Identifier: MIT OR Apache-2.0

set -eu

fail() {
  echo "lint-inf: error: $*" >&2
  exit 1
}

note() {
  echo "lint-inf: $*" >&2
}

SCRIPT_DIR=$(CDPATH= cd "$(dirname "$0")" && pwd)
BASE_DIR=$(CDPATH= cd "$SCRIPT_DIR/.." && pwd)

INF_CONTRACT="$BASE_DIR/inf/aero_virtio_snd.inf"
INF_TRANSITIONAL="$BASE_DIR/inf/aero-virtio-snd-legacy.inf"
INF_IOPORT="$BASE_DIR/inf/aero-virtio-snd-ioport.inf"
INF_ALIAS_ENABLED="$BASE_DIR/inf/virtio-snd.inf"
INF_ALIAS_DISABLED="$BASE_DIR/inf/virtio-snd.inf.disabled"

tmp1=''
tmp2=''
cleanup() {
  if [ -n "$tmp1" ]; then rm -f "$tmp1"; fi
  if [ -n "$tmp2" ]; then rm -f "$tmp2"; fi
}
trap cleanup EXIT INT HUP TERM

require_file() {
  path="$1"
  rel="$2"
  if [ ! -f "$path" ]; then
    fail "missing required file: $rel"
  fi
}

normalize_all() {
  # Lowercase, strip CRLF + whitespace to make matching robust to INF formatting.
  #
  # Used for invariants that must hold even if the string is mentioned in a
  # comment (e.g. "must not contain DEV_1018").
  tr -d '\r\t ' < "$1" | tr 'A-Z' 'a-z'
}

normalize_nocomment() {
  # Lowercase + strip CRLF + whitespace, but ignore comment-only lines.
  #
  # This avoids false positives where a directive is present but commented out
  # (e.g. `; Include = ks.inf, wdmaudio.inf`).
  awk '
    {
      sub(/\r$/, "")
      if ($0 ~ /^[[:space:]]*;/) next
      print
    }
  ' "$1" | tr -d '\t ' | tr 'A-Z' 'a-z'
}

require_contains_norm() {
  file="$1"
  needle="$2"
  msg="$3"
  if ! normalize_nocomment "$file" | grep -Fq "$needle"; then
    fail "$msg"
  fi
}

require_not_contains_norm() {
  file="$1"
  needle="$2"
  msg="$3"
  if normalize_nocomment "$file" | grep -Fq "$needle"; then
    fail "$msg"
  fi
}

require_not_contains_norm_all() {
  file="$1"
  needle="$2"
  msg="$3"
  if normalize_all "$file" | grep -Fq "$needle"; then
    fail "$msg"
  fi
}

section_contains_norm() {
  file="$1"
  section="$2"
  needle="$3"
  msg="$4"

  # POSIX awk: avoid non-standard capture groups; do manual bracket stripping.
  section_lc=$(printf '%s' "$section" | tr 'A-Z' 'a-z')

  if ! LINT_INF_NEEDLE="$needle" awk -v section="$section_lc" '
    BEGIN {
      in_section = 0
      found = 0
      needle = tolower(ENVIRON["LINT_INF_NEEDLE"])
    }
    {
      sub(/\r$/, "")
      if ($0 ~ /^[[:space:]]*;/) next

      line = $0
      if (line ~ /^[[:space:]]*\[[^]]+\][[:space:]]*$/) {
        sub(/^[[:space:]]*\[/, "", line)
        sub(/\][[:space:]]*$/, "", line)
        line = tolower(line)
        in_section = (line == section)
        next
      }

      if (in_section) {
        gsub(/[[:space:]]+/, "", line)
        line = tolower(line)
        if (index(line, needle) != 0) {
          found = 1
          exit 0
        }
      }
    }
    END { if (!found) exit 1 }
  ' "$file"; then
    fail "$msg"
  fi
}

strip_leading_comment_header() {
  # Drop the leading comment banner so `virtio-snd.inf.disabled` can have an
  # alternate header while remaining functionally identical.
  #
  # INF comments are `;` at the start of the line (possibly after whitespace).
  awk '
    BEGIN { in_header = 1 }
    {
      sub(/\r$/, "")
      if (in_header) {
        if ($0 ~ /^[[:space:]]*$/) next
        if ($0 ~ /^[[:space:]]*;/) next
        in_header = 0
      }
      print
    }
  ' "$1"
}

require_file "$INF_CONTRACT" "inf/aero_virtio_snd.inf"

note "checking inf/aero_virtio_snd.inf invariants..."

note "checking HWID binding..."
section_contains_norm \
  "$INF_CONTRACT" \
  'AeroVirtioSndModels.NTx86' \
  'pci\ven_1af4&dev_1059&rev_01' \
  "inf/aero_virtio_snd.inf must bind PCI\\VEN_1AF4&DEV_1059&REV_01 in [AeroVirtioSndModels.NTx86]"

section_contains_norm \
  "$INF_CONTRACT" \
  'AeroVirtioSndModels.NTamd64' \
  'pci\ven_1af4&dev_1059&rev_01' \
  "inf/aero_virtio_snd.inf must bind PCI\\VEN_1AF4&DEV_1059&REV_01 in [AeroVirtioSndModels.NTamd64]"

# Guardrail: ensure we never accidentally loosen the match to DEV_1059 without
# revision gating. This driver intentionally matches only the contract-v1 HWID.
if awk '
  {
    sub(/\r$/, "")
    line = $0
    # Skip full-line comments.
    if (line ~ /^[[:space:]]*;/) next
    # Strip inline comments.
    sub(/[[:space:]]*;.*$/, "", line)
    if (line == "") next

    low = tolower(line)
    if (index(low, "pci\\ven_1af4&dev_1059") != 0 && index(low, "&rev_01") == 0) {
      print line
      exit 0
    }
  }
  END { exit 1 }
' "$INF_CONTRACT" >/dev/null; then
  fail "inf/aero_virtio_snd.inf must not contain unqualified PCI\\VEN_1AF4&DEV_1059 matches (missing &REV_01)"
fi

require_not_contains_norm_all \
  "$INF_CONTRACT" \
  'dev_1018' \
  "inf/aero_virtio_snd.inf must not contain DEV_1018 (transitional virtio-snd)"

section_contains_norm \
  "$INF_CONTRACT" \
  'AeroVirtioSnd_Install.NT' \
  'include=ks.inf,wdmaudio.inf' \
  "inf/aero_virtio_snd.inf must declare: Include = ks.inf, wdmaudio.inf"

section_contains_norm \
  "$INF_CONTRACT" \
  'AeroVirtioSnd_Install.NT' \
  'needs=ks.registration,wdmaudio.registration' \
  "inf/aero_virtio_snd.inf must declare: Needs = KS.Registration, WDMAUDIO.Registration"

section_contains_norm \
  "$INF_CONTRACT" \
  'AeroVirtioSnd_Install.NT' \
  'copyfiles=aerovirtiosnd.copyfiles' \
  "inf/aero_virtio_snd.inf must stage files via: CopyFiles = AeroVirtioSnd.CopyFiles"

section_contains_norm \
  "$INF_CONTRACT" \
  'AeroVirtioSnd_Install.NT' \
  'addreg=aerovirtiosnd.addreg' \
  "inf/aero_virtio_snd.inf must apply registry settings via: AddReg = AeroVirtioSnd.AddReg"

section_contains_norm \
  "$INF_CONTRACT" \
  'AeroVirtioSnd_Install.NT.Interfaces' \
  'addinterface=%kscategory_render%' \
  "inf/aero_virtio_snd.inf must AddInterface for KSCATEGORY_RENDER"

section_contains_norm \
  "$INF_CONTRACT" \
  'AeroVirtioSnd_Install.NT.Interfaces' \
  'addinterface=%kscategory_capture%' \
  "inf/aero_virtio_snd.inf must AddInterface for KSCATEGORY_CAPTURE"

section_contains_norm \
  "$INF_CONTRACT" \
  'Version' \
  'catalogfile=aero_virtio_snd.cat' \
  "inf/aero_virtio_snd.inf must declare: CatalogFile = aero_virtio_snd.cat"

note "checking SYS/CAT name consistency..."
section_contains_norm \
  "$INF_CONTRACT" \
  'DestinationDirs' \
  'aerovirtiosnd.copyfiles=12' \
  "inf/aero_virtio_snd.inf must install SYS to %12% via: [DestinationDirs] AeroVirtioSnd.CopyFiles = 12"

section_contains_norm \
  "$INF_CONTRACT" \
  'AeroVirtioSnd.AddReg' \
  'ntmpdriver,,aero_virtio_snd.sys' \
  "inf/aero_virtio_snd.inf must reference aero_virtio_snd.sys via NTMPDriver"

note "checking bring-up toggle defaults..."
section_contains_norm \
  "$INF_CONTRACT" \
  'AeroVirtioSnd.AddReg' \
  'hkr,parameters,forcenullbackend,0x00010001,0' \
  "inf/aero_virtio_snd.inf must set HKR\\Parameters\\ForceNullBackend default to 0"

section_contains_norm \
  "$INF_CONTRACT" \
  'AeroVirtioSnd.AddReg' \
  'hkr,parameters,allowpollingonly,0x00010001,0' \
  "inf/aero_virtio_snd.inf must set HKR\\Parameters\\AllowPollingOnly default to 0"

if [ -f "$INF_TRANSITIONAL" ]; then
  note "checking transitional INF bring-up toggle defaults..."
  section_contains_norm \
    "$INF_TRANSITIONAL" \
    'AeroVirtioSndLegacy.AddReg' \
    'hkr,parameters,forcenullbackend,0x00010001,0' \
    "inf/aero-virtio-snd-legacy.inf must set HKR\\Parameters\\ForceNullBackend default to 0"

  section_contains_norm \
    "$INF_TRANSITIONAL" \
    'AeroVirtioSndLegacy.AddReg' \
    'hkr,parameters,allowpollingonly,0x00010001,0' \
    "inf/aero-virtio-snd-legacy.inf must set HKR\\Parameters\\AllowPollingOnly default to 0"
fi

if [ -f "$INF_IOPORT" ]; then
  note "checking ioport legacy INF bring-up toggle defaults..."
  section_contains_norm \
    "$INF_IOPORT" \
    'AeroVirtioSndIoPort.AddReg' \
    'hkr,parameters,forcenullbackend,0x00010001,0' \
    "inf/aero-virtio-snd-ioport.inf must set HKR\\Parameters\\ForceNullBackend default to 0"
fi

section_contains_norm \
  "$INF_CONTRACT" \
  'AeroVirtioSnd_Service_Inst' \
  'servicebinary=%12%\aero_virtio_snd.sys' \
  "inf/aero_virtio_snd.inf must reference aero_virtio_snd.sys via ServiceBinary"

section_contains_norm \
  "$INF_CONTRACT" \
  'AeroVirtioSnd_Install.NT.Services' \
  'addservice=aero_virtio_snd' \
  "inf/aero_virtio_snd.inf must install the aero_virtio_snd service (AddService)"

section_contains_norm \
  "$INF_CONTRACT" \
  'AeroVirtioSnd.CopyFiles' \
  'aero_virtio_snd.sys' \
  "inf/aero_virtio_snd.inf must copy aero_virtio_snd.sys (AeroVirtioSnd.CopyFiles)"

section_contains_norm \
  "$INF_CONTRACT" \
  'SourceDisksFiles' \
  'aero_virtio_snd.sys=1' \
  "inf/aero_virtio_snd.inf must list aero_virtio_snd.sys under [SourceDisksFiles]"

INF_ALIAS=""
if [ -f "$INF_ALIAS_ENABLED" ]; then
  INF_ALIAS="$INF_ALIAS_ENABLED"
elif [ -f "$INF_ALIAS_DISABLED" ]; then
  INF_ALIAS="$INF_ALIAS_DISABLED"
fi

if [ -n "$INF_ALIAS" ]; then
  alias_basename=$(basename "$INF_ALIAS")
  note "checking inf/$alias_basename stays in sync..."
  tmp1=$(mktemp "${TMPDIR:-/tmp}/aero_virtio_snd.inf.XXXXXX") || fail "mktemp failed"
  tmp2=$(mktemp "${TMPDIR:-/tmp}/virtio-snd.inf.alias.XXXXXX") || fail "mktemp failed"

  strip_leading_comment_header "$INF_CONTRACT" > "$tmp1"
  strip_leading_comment_header "$INF_ALIAS" > "$tmp2"

  if ! diff -u "$tmp1" "$tmp2" >/dev/null; then
    # Show a unified diff, but label it with the real file paths to make CI logs
    # actionable (instead of referencing mktemp paths).
    diff -u -L "$INF_CONTRACT" -L "$INF_ALIAS" "$tmp1" "$tmp2" >&2 || true
    fail "inf/$alias_basename is out of sync with inf/aero_virtio_snd.inf (ignoring leading comment headers)"
  fi
fi

note "OK"
