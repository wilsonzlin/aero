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

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
BASE_DIR=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)

INF_CONTRACT="$BASE_DIR/inf/aero_virtio_snd.inf"
INF_DISABLED="$BASE_DIR/inf/virtio-snd.inf.disabled"

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

normalize() {
  # Lowercase, strip CRLF + whitespace to make matching robust to INF formatting.
  tr -d '\r\t ' < "$1" | tr 'A-Z' 'a-z'
}

require_contains_norm() {
  file="$1"
  needle="$2"
  msg="$3"
  if ! normalize "$file" | grep -Fq "$needle"; then
    fail "$msg"
  fi
}

require_not_contains_norm() {
  file="$1"
  needle="$2"
  msg="$3"
  if normalize "$file" | grep -Fq "$needle"; then
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

require_contains_norm \
  "$INF_CONTRACT" \
  'pci\ven_1af4&dev_1059&rev_01' \
  "inf/aero_virtio_snd.inf must contain HWID PCI\\VEN_1AF4&DEV_1059&REV_01"

require_not_contains_norm \
  "$INF_CONTRACT" \
  'dev_1018' \
  "inf/aero_virtio_snd.inf must not contain DEV_1018 (transitional virtio-snd)"

require_contains_norm \
  "$INF_CONTRACT" \
  'include=ks.inf,wdmaudio.inf' \
  "inf/aero_virtio_snd.inf must declare: Include = ks.inf, wdmaudio.inf"

require_contains_norm \
  "$INF_CONTRACT" \
  'needs=ks.registration,wdmaudio.registration' \
  "inf/aero_virtio_snd.inf must declare: Needs = KS.Registration, WDMAUDIO.Registration"

require_contains_norm \
  "$INF_CONTRACT" \
  'addinterface=%kscategory_render%' \
  "inf/aero_virtio_snd.inf must AddInterface for KSCATEGORY_RENDER"

require_contains_norm \
  "$INF_CONTRACT" \
  'addinterface=%kscategory_capture%' \
  "inf/aero_virtio_snd.inf must AddInterface for KSCATEGORY_CAPTURE"

require_contains_norm \
  "$INF_CONTRACT" \
  'catalogfile=aero_virtio_snd.cat' \
  "inf/aero_virtio_snd.inf must declare: CatalogFile = aero_virtio_snd.cat"

if [ -f "$INF_DISABLED" ]; then
  note "checking inf/virtio-snd.inf.disabled stays in sync..."
  tmp1=$(mktemp "${TMPDIR:-/tmp}/aero_virtio_snd.inf.XXXXXX") || fail "mktemp failed"
  tmp2=$(mktemp "${TMPDIR:-/tmp}/virtio-snd.inf.disabled.XXXXXX") || fail "mktemp failed"

  strip_leading_comment_header "$INF_CONTRACT" > "$tmp1"
  strip_leading_comment_header "$INF_DISABLED" > "$tmp2"

  if ! diff -u "$tmp1" "$tmp2" >/dev/null; then
    diff -u "$tmp1" "$tmp2" >&2 || true
    fail "inf/virtio-snd.inf.disabled is out of sync with inf/aero_virtio_snd.inf (ignoring leading comment headers)"
  fi
fi

note "OK"
