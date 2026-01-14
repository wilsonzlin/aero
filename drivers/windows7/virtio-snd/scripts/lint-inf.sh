#!/bin/sh
# SPDX-License-Identifier: MIT OR Apache-2.0

set -eu

# Ensure predictable ASCII-only case folding for `tr`/`awk tolower()` and stable
# `[[:space:]]` behavior across locales.
LC_ALL=C
export LC_ALL

fail() {
  echo "lint-inf: error: $*" >&2
  exit 1
}

note() {
  echo "lint-inf: $*" >&2
}

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    fail "required tool not found in PATH: $1"
  fi
}

for cmd in awk diff grep mktemp sed tr; do
  require_cmd "$cmd"
done

if ! diff -u /dev/null /dev/null >/dev/null 2>&1; then
  fail "diff does not support unified output (-u); please use a POSIX diff implementation"
fi

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

require_ascii_only() {
  file="$1"
  rel="$2"

  # INFs should remain ASCII-only for maximum compatibility with older Windows
  # tooling (including Win7-era INF parsers and Inf2Cat).
  #
  # `tr -d '\000-\177'` deletes all ASCII bytes, leaving only non-ASCII bytes.
  # If anything remains, the file is not ASCII-only.
  if tr -d '\000-\177' < "$file" | grep -q .; then
    fail "$rel contains non-ASCII bytes; keep INFs ASCII-only"
  fi
}

normalize_all() {
  # Lowercase, strip CRLF + whitespace to make matching robust to INF formatting.
  #
  # Used for invariants that must hold even if the string is mentioned in a
  # comment (e.g. "must not contain DEV_1018").
  tr -d '\r\t ' < "$1" | tr 'A-Z' 'a-z'
}

require_not_contains_norm_all() {
  file="$1"
  needle="$2"
  msg="$3"
  if normalize_all "$file" | grep -Fq -- "$needle"; then
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
      # Strip inline comments to avoid false positives from commented-out tokens
      # on otherwise active lines (e.g. "Foo = bar ; AddService = ...").
      sub(/[[:space:]]*;.*$/, "", line)
      if (line ~ /^[[:space:]]*$/) next

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
require_ascii_only "$INF_CONTRACT" "inf/aero_virtio_snd.inf"

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
  'AeroVirtioSnd_Install.NT.Interfaces' \
  'addinterface=%kscategory_render%,%ksname_wave%,aerovirtiosnd.wave.interface' \
  "inf/aero_virtio_snd.inf must register KSCATEGORY_RENDER on the Wave interface"

section_contains_norm \
  "$INF_CONTRACT" \
  'AeroVirtioSnd_Install.NT.Interfaces' \
  'addinterface=%kscategory_capture%,%ksname_wave%,aerovirtiosnd.capture.interface' \
  "inf/aero_virtio_snd.inf must register KSCATEGORY_CAPTURE on the Capture interface"

note "checking KS interface section wiring..."
section_contains_norm \
  "$INF_CONTRACT" \
  'AeroVirtioSnd.Wave.Interface' \
  'addreg=aerovirtiosnd.wave.interface.addreg' \
  "inf/aero_virtio_snd.inf must define [AeroVirtioSnd.Wave.Interface] -> AddReg"

section_contains_norm \
  "$INF_CONTRACT" \
  'AeroVirtioSnd.Capture.Interface' \
  'addreg=aerovirtiosnd.capture.interface.addreg' \
  "inf/aero_virtio_snd.inf must define [AeroVirtioSnd.Capture.Interface] -> AddReg"

section_contains_norm \
  "$INF_CONTRACT" \
  'AeroVirtioSnd.Wave.Interface.AddReg' \
  'hkr,,friendlyname,,%aerovirtiosnd.endpointdesc%' \
  "inf/aero_virtio_snd.inf must set a FriendlyName for the render endpoint"

section_contains_norm \
  "$INF_CONTRACT" \
  'AeroVirtioSnd.Capture.Interface.AddReg' \
  'hkr,,friendlyname,,%aerovirtiosnd.captureendpointdesc%' \
  "inf/aero_virtio_snd.inf must set a FriendlyName for the capture endpoint"

note "checking KS category GUID constants..."
section_contains_norm \
  "$INF_CONTRACT" \
  'Strings' \
  'kscategory_render="{65e8773e-8f56-11d0-a3b9-00a0c9223196}"' \
  "inf/aero_virtio_snd.inf must define KSCATEGORY_RENDER GUID in [Strings]"

section_contains_norm \
  "$INF_CONTRACT" \
  'Strings' \
  'kscategory_capture="{65e8773d-8f56-11d0-a3b9-00a0c9223196}"' \
  "inf/aero_virtio_snd.inf must define KSCATEGORY_CAPTURE GUID in [Strings]"

note "checking MSI interrupt management registrations..."
section_contains_norm \
  "$INF_CONTRACT" \
  'AeroVirtioSnd_Install.NT.HW' \
  'addreg=aerovirtiosnd_interruptmanagement_addreg' \
  "inf/aero_virtio_snd.inf must configure interrupt management via [AeroVirtioSnd_Install.NT.HW]"

section_contains_norm \
  "$INF_CONTRACT" \
  'AeroVirtioSnd_InterruptManagement_AddReg' \
  'msisupported,0x00010001,1' \
  "inf/aero_virtio_snd.inf must set MSISupported=1 under Interrupt Management"

section_contains_norm \
  "$INF_CONTRACT" \
  'AeroVirtioSnd_InterruptManagement_AddReg' \
  'messagenumberlimit,0x00010001,8' \
  "inf/aero_virtio_snd.inf must set MessageNumberLimit=8 under Interrupt Management"

section_contains_norm \
  "$INF_CONTRACT" \
  'Version' \
  'signature="$windowsnt$"' \
  'inf/aero_virtio_snd.inf must declare: Signature = "$WINDOWS NT$"'

section_contains_norm \
  "$INF_CONTRACT" \
  'Version' \
  'class=media' \
  "inf/aero_virtio_snd.inf must declare: Class = MEDIA"

section_contains_norm \
  "$INF_CONTRACT" \
  'Version' \
  'classguid={4d36e96c-e325-11ce-bfc1-08002be10318}' \
  "inf/aero_virtio_snd.inf must declare the MEDIA ClassGuid"

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

note "checking WDMAudio/PortCls subdevice wiring..."
section_contains_norm \
  "$INF_CONTRACT" \
  'AeroVirtioSnd.AddReg' \
  'associatedfilters,0x00010000,%ksname_wave%,%ksname_topology%' \
  "inf/aero_virtio_snd.inf must set AssociatedFilters = Wave,Topology"

section_contains_norm \
  "$INF_CONTRACT" \
  'AeroVirtioSnd.AddReg' \
  'drivers,subclasses,,"wave,topology"' \
  "inf/aero_virtio_snd.inf must set Drivers\\SubClasses to \"wave,topology\""

section_contains_norm \
  "$INF_CONTRACT" \
  'AeroVirtioSnd.AddReg' \
  'drivers,driver,,wdmaud.sys' \
  "inf/aero_virtio_snd.inf must set Drivers\\Driver = wdmaud.sys"

section_contains_norm \
  "$INF_CONTRACT" \
  'AeroVirtioSnd.AddReg' \
  'drivers\wave,driver,,aero_virtio_snd.sys' \
  "inf/aero_virtio_snd.inf must wire Drivers\\\\wave -> aero_virtio_snd.sys"

section_contains_norm \
  "$INF_CONTRACT" \
  'AeroVirtioSnd.AddReg' \
  'drivers\topology,driver,,aero_virtio_snd.sys' \
  "inf/aero_virtio_snd.inf must wire Drivers\\\\topology -> aero_virtio_snd.sys"

section_contains_norm \
  "$INF_CONTRACT" \
  'AeroVirtioSnd.AddReg' \
  'drivers\wave,description,,%aerovirtiosnd.endpointdesc%' \
  "inf/aero_virtio_snd.inf must set Drivers\\\\wave Description to %AeroVirtioSnd.EndpointDesc%"

section_contains_norm \
  "$INF_CONTRACT" \
  'AeroVirtioSnd.AddReg' \
  'drivers\topology,description,,%aerovirtiosnd.topologydesc%' \
  "inf/aero_virtio_snd.inf must set Drivers\\\\topology Description to %AeroVirtioSnd.TopologyDesc%"

section_contains_norm \
  "$INF_CONTRACT" \
  'AeroVirtioSnd.AddReg' \
  'hkr,%ksname_wave%,driver,,aero_virtio_snd.sys' \
  "inf/aero_virtio_snd.inf must set HKR\\\\Wave Driver = aero_virtio_snd.sys"

section_contains_norm \
  "$INF_CONTRACT" \
  'AeroVirtioSnd.AddReg' \
  'hkr,%ksname_topology%,driver,,aero_virtio_snd.sys' \
  "inf/aero_virtio_snd.inf must set HKR\\\\Topology Driver = aero_virtio_snd.sys"

section_contains_norm \
  "$INF_CONTRACT" \
  'AeroVirtioSnd.AddReg' \
  'hkr,%ksname_wave%,friendlyname,,%aerovirtiosnd.endpointdesc%' \
  "inf/aero_virtio_snd.inf must set HKR\\\\Wave FriendlyName = %AeroVirtioSnd.EndpointDesc%"

section_contains_norm \
  "$INF_CONTRACT" \
  'AeroVirtioSnd.AddReg' \
  'hkr,%ksname_topology%,friendlyname,,%aerovirtiosnd.topologydesc%' \
  "inf/aero_virtio_snd.inf must set HKR\\\\Topology FriendlyName = %AeroVirtioSnd.TopologyDesc%"

note "checking bring-up toggle defaults..."
section_contains_norm \
  "$INF_CONTRACT" \
  'AeroVirtioSnd.AddReg' \
  'hkr,parameters,forcenullbackend,0x00010003,0' \
  "inf/aero_virtio_snd.inf must set HKR\\Parameters\\ForceNullBackend default to 0"

section_contains_norm \
  "$INF_CONTRACT" \
  'AeroVirtioSnd.AddReg' \
  'hkr,parameters,allowpollingonly,0x00010003,0' \
  "inf/aero_virtio_snd.inf must set HKR\\Parameters\\AllowPollingOnly default to 0"

# Also seed these defaults under the device instance's hardware key (Device Parameters)
# so they are discoverable via Device Manager's "Device instance path".
section_contains_norm \
  "$INF_CONTRACT" \
  'AeroVirtioSnd_Install.NT.HW' \
  'aerovirtiosnd_parameters_addreg' \
  "inf/aero_virtio_snd.inf must apply AeroVirtioSnd_Parameters_AddReg via [AeroVirtioSnd_Install.NT.HW]"

section_contains_norm \
  "$INF_CONTRACT" \
  'AeroVirtioSnd_Parameters_AddReg' \
  'hkr,parameters,forcenullbackend,0x00010003,0' \
  "inf/aero_virtio_snd.inf must seed HKR\\Parameters\\ForceNullBackend under the hardware key"

section_contains_norm \
  "$INF_CONTRACT" \
  'AeroVirtioSnd_Parameters_AddReg' \
  'hkr,parameters,allowpollingonly,0x00010003,0' \
  "inf/aero_virtio_snd.inf must seed HKR\\Parameters\\AllowPollingOnly under the hardware key"

if [ -f "$INF_TRANSITIONAL" ]; then
  require_ascii_only "$INF_TRANSITIONAL" "inf/aero-virtio-snd-legacy.inf"
  note "checking transitional INF HWID binding..."
  section_contains_norm \
    "$INF_TRANSITIONAL" \
    'AeroVirtioSndLegacyModels.NTx86' \
    'pci\ven_1af4&dev_1018' \
    "inf/aero-virtio-snd-legacy.inf must bind PCI\\VEN_1AF4&DEV_1018 in [AeroVirtioSndLegacyModels.NTx86]"

  section_contains_norm \
    "$INF_TRANSITIONAL" \
    'AeroVirtioSndLegacyModels.NTamd64' \
    'pci\ven_1af4&dev_1018' \
    "inf/aero-virtio-snd-legacy.inf must bind PCI\\VEN_1AF4&DEV_1018 in [AeroVirtioSndLegacyModels.NTamd64]"

  note "checking transitional INF WDMAudio/PortCls wiring..."
  section_contains_norm \
    "$INF_TRANSITIONAL" \
    'AeroVirtioSndLegacy_Install.NT' \
    'include=ks.inf,wdmaudio.inf' \
    "inf/aero-virtio-snd-legacy.inf must declare: Include = ks.inf, wdmaudio.inf"

  section_contains_norm \
    "$INF_TRANSITIONAL" \
    'AeroVirtioSndLegacy_Install.NT' \
    'needs=ks.registration,wdmaudio.registration' \
    "inf/aero-virtio-snd-legacy.inf must declare: Needs = KS.Registration, WDMAUDIO.Registration"

  section_contains_norm \
    "$INF_TRANSITIONAL" \
    'AeroVirtioSndLegacy_Install.NT' \
    'copyfiles=aerovirtiosndlegacy.copyfiles' \
    "inf/aero-virtio-snd-legacy.inf must stage files via: CopyFiles = AeroVirtioSndLegacy.CopyFiles"

  section_contains_norm \
    "$INF_TRANSITIONAL" \
    'AeroVirtioSndLegacy_Install.NT' \
    'addreg=aerovirtiosndlegacy.addreg' \
    "inf/aero-virtio-snd-legacy.inf must apply registry settings via: AddReg = AeroVirtioSndLegacy.AddReg"

  note "checking transitional INF SYS/service consistency..."
  section_contains_norm \
    "$INF_TRANSITIONAL" \
    'DestinationDirs' \
    'aerovirtiosndlegacy.copyfiles=12' \
    "inf/aero-virtio-snd-legacy.inf must install SYS to %12% via: [DestinationDirs] AeroVirtioSndLegacy.CopyFiles = 12"

  section_contains_norm \
    "$INF_TRANSITIONAL" \
    'AeroVirtioSndLegacy.AddReg' \
    'ntmpdriver,,virtiosnd_legacy.sys' \
    "inf/aero-virtio-snd-legacy.inf must reference virtiosnd_legacy.sys via NTMPDriver"

  section_contains_norm \
    "$INF_TRANSITIONAL" \
    'AeroVirtioSndLegacy_Install.NT.Services' \
    'addservice=aeroviosnd_legacy' \
    "inf/aero-virtio-snd-legacy.inf must install the aeroviosnd_legacy service (AddService)"

  section_contains_norm \
    "$INF_TRANSITIONAL" \
    'AeroVirtioSndLegacy_Service_Inst' \
    'servicebinary=%12%\virtiosnd_legacy.sys' \
    "inf/aero-virtio-snd-legacy.inf must reference virtiosnd_legacy.sys via ServiceBinary"

  section_contains_norm \
    "$INF_TRANSITIONAL" \
    'AeroVirtioSndLegacy.CopyFiles' \
    'virtiosnd_legacy.sys' \
    "inf/aero-virtio-snd-legacy.inf must copy virtiosnd_legacy.sys (AeroVirtioSndLegacy.CopyFiles)"

  section_contains_norm \
    "$INF_TRANSITIONAL" \
    'SourceDisksFiles' \
    'virtiosnd_legacy.sys=1' \
    "inf/aero-virtio-snd-legacy.inf must list virtiosnd_legacy.sys under [SourceDisksFiles]"

  section_contains_norm \
    "$INF_TRANSITIONAL" \
    'Version' \
    'catalogfile=aero-virtio-snd-legacy.cat' \
    "inf/aero-virtio-snd-legacy.inf must declare: CatalogFile = aero-virtio-snd-legacy.cat"

  note "checking transitional INF bring-up toggle defaults..."
  section_contains_norm \
    "$INF_TRANSITIONAL" \
    'AeroVirtioSndLegacy.AddReg' \
    'hkr,parameters,forcenullbackend,0x00010003,0' \
    "inf/aero-virtio-snd-legacy.inf must set HKR\\Parameters\\ForceNullBackend default to 0"

  section_contains_norm \
    "$INF_TRANSITIONAL" \
    'AeroVirtioSndLegacy.AddReg' \
    'hkr,parameters,allowpollingonly,0x00010003,0' \
    "inf/aero-virtio-snd-legacy.inf must set HKR\\Parameters\\AllowPollingOnly default to 0"

  section_contains_norm \
    "$INF_TRANSITIONAL" \
    'AeroVirtioSndLegacy_Install.NT.HW' \
    'aerovirtiosndlegacy_parameters_addreg' \
    "inf/aero-virtio-snd-legacy.inf must apply AeroVirtioSndLegacy_Parameters_AddReg via [AeroVirtioSndLegacy_Install.NT.HW]"

  section_contains_norm \
    "$INF_TRANSITIONAL" \
    'AeroVirtioSndLegacy_Parameters_AddReg' \
    'hkr,parameters,forcenullbackend,0x00010003,0' \
    "inf/aero-virtio-snd-legacy.inf must seed HKR\\Parameters\\ForceNullBackend under the hardware key"

  section_contains_norm \
    "$INF_TRANSITIONAL" \
    'AeroVirtioSndLegacy_Parameters_AddReg' \
    'hkr,parameters,allowpollingonly,0x00010003,0' \
    "inf/aero-virtio-snd-legacy.inf must seed HKR\\Parameters\\AllowPollingOnly under the hardware key"

  note "checking transitional INF MSI interrupt management registrations..."
  section_contains_norm \
    "$INF_TRANSITIONAL" \
    'AeroVirtioSndLegacy_Install.NT.HW' \
    'addreg=aerovirtiosndlegacy_interruptmanagement_addreg' \
    "inf/aero-virtio-snd-legacy.inf must configure interrupt management via [AeroVirtioSndLegacy_Install.NT.HW]"

  section_contains_norm \
    "$INF_TRANSITIONAL" \
    'AeroVirtioSndLegacy_InterruptManagement_AddReg' \
    'msisupported,0x00010001,1' \
    "inf/aero-virtio-snd-legacy.inf must set MSISupported=1 under Interrupt Management"

  section_contains_norm \
    "$INF_TRANSITIONAL" \
    'AeroVirtioSndLegacy_InterruptManagement_AddReg' \
    'messagenumberlimit,0x00010001,8' \
    "inf/aero-virtio-snd-legacy.inf must set MessageNumberLimit=8 under Interrupt Management"
fi

if [ -f "$INF_IOPORT" ]; then
  require_ascii_only "$INF_IOPORT" "inf/aero-virtio-snd-ioport.inf"
  note "checking ioport legacy INF HWID binding..."
  section_contains_norm \
    "$INF_IOPORT" \
    'AeroVirtioSndIoPortModels.NTx86' \
    'pci\ven_1af4&dev_1018&rev_00' \
    "inf/aero-virtio-snd-ioport.inf must bind PCI\\VEN_1AF4&DEV_1018&REV_00 in [AeroVirtioSndIoPortModels.NTx86]"

  section_contains_norm \
    "$INF_IOPORT" \
    'AeroVirtioSndIoPortModels.NTamd64' \
    'pci\ven_1af4&dev_1018&rev_00' \
    "inf/aero-virtio-snd-ioport.inf must bind PCI\\VEN_1AF4&DEV_1018&REV_00 in [AeroVirtioSndIoPortModels.NTamd64]"

  # Guardrail: ensure we never accidentally loosen the match to DEV_1018 without revision gating.
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
      if (index(low, "pci\\ven_1af4&dev_1018") != 0 && index(low, "&rev_00") == 0) {
        print line
        exit 0
      }
    }
    END { exit 1 }
  ' "$INF_IOPORT" >/dev/null; then
    fail "inf/aero-virtio-snd-ioport.inf must not contain unqualified PCI\\VEN_1AF4&DEV_1018 matches (missing &REV_00)"
  fi

  note "checking ioport legacy INF WDMAudio/PortCls wiring..."
  section_contains_norm \
    "$INF_IOPORT" \
    'AeroVirtioSndIoPort_Install.NT' \
    'include=ks.inf,wdmaudio.inf' \
    "inf/aero-virtio-snd-ioport.inf must declare: Include = ks.inf, wdmaudio.inf"

  section_contains_norm \
    "$INF_IOPORT" \
    'AeroVirtioSndIoPort_Install.NT' \
    'needs=ks.registration,wdmaudio.registration' \
    "inf/aero-virtio-snd-ioport.inf must declare: Needs = KS.Registration, WDMAUDIO.Registration"

  section_contains_norm \
    "$INF_IOPORT" \
    'AeroVirtioSndIoPort_Install.NT' \
    'copyfiles=aerovirtiosndioport.copyfiles' \
    "inf/aero-virtio-snd-ioport.inf must stage files via: CopyFiles = AeroVirtioSndIoPort.CopyFiles"

  section_contains_norm \
    "$INF_IOPORT" \
    'AeroVirtioSndIoPort_Install.NT' \
    'addreg=aerovirtiosndioport.addreg' \
    "inf/aero-virtio-snd-ioport.inf must apply registry settings via: AddReg = AeroVirtioSndIoPort.AddReg"

  note "checking ioport legacy INF bring-up toggle defaults..."
  section_contains_norm \
    "$INF_IOPORT" \
    'AeroVirtioSndIoPort.AddReg' \
    'hkr,parameters,forcenullbackend,0x00010003,0' \
    "inf/aero-virtio-snd-ioport.inf must set HKR\\Parameters\\ForceNullBackend default to 0"

  section_contains_norm \
    "$INF_IOPORT" \
    'AeroVirtioSndIoPort_Install.NT.HW' \
    'aerovirtiosndioport_parameters_addreg' \
    "inf/aero-virtio-snd-ioport.inf must apply AeroVirtioSndIoPort_Parameters_AddReg via [AeroVirtioSndIoPort_Install.NT.HW]"

  section_contains_norm \
    "$INF_IOPORT" \
    'AeroVirtioSndIoPort_Parameters_AddReg' \
    'hkr,parameters,forcenullbackend,0x00010003,0' \
    "inf/aero-virtio-snd-ioport.inf must seed HKR\\Parameters\\ForceNullBackend under the hardware key"

  note "checking ioport legacy INF SYS/service consistency..."
  section_contains_norm \
    "$INF_IOPORT" \
    'AeroVirtioSndIoPort.AddReg' \
    'ntmpdriver,,virtiosnd_ioport.sys' \
    "inf/aero-virtio-snd-ioport.inf must reference virtiosnd_ioport.sys via NTMPDriver"

  section_contains_norm \
    "$INF_IOPORT" \
    'AeroVirtioSndIoPort_Install.NT.Services' \
    'addservice=aeroviosnd_ioport' \
    "inf/aero-virtio-snd-ioport.inf must install the aeroviosnd_ioport service (AddService)"

  section_contains_norm \
    "$INF_IOPORT" \
    'AeroVirtioSndIoPort_Service_Inst' \
    'servicebinary=%12%\virtiosnd_ioport.sys' \
    "inf/aero-virtio-snd-ioport.inf must reference virtiosnd_ioport.sys via ServiceBinary"

  section_contains_norm \
    "$INF_IOPORT" \
    'AeroVirtioSndIoPort.CopyFiles' \
    'virtiosnd_ioport.sys' \
    "inf/aero-virtio-snd-ioport.inf must copy virtiosnd_ioport.sys (AeroVirtioSndIoPort.CopyFiles)"

  note "checking ioport legacy INF catalog + installation directory..."
  section_contains_norm \
    "$INF_IOPORT" \
    'Version' \
    'catalogfile=aero-virtio-snd-ioport.cat' \
    "inf/aero-virtio-snd-ioport.inf must declare: CatalogFile = aero-virtio-snd-ioport.cat"

  section_contains_norm \
    "$INF_IOPORT" \
    'DestinationDirs' \
    'aerovirtiosndioport.copyfiles=12' \
    "inf/aero-virtio-snd-ioport.inf must install SYS to %12% via: [DestinationDirs] AeroVirtioSndIoPort.CopyFiles = 12"

  note "checking ioport legacy INF does not opt into MSI/MSI-X..."
  # The I/O-port legacy driver uses only line-based INTx (IoConnectInterrupt) and
  # does not support message interrupts. Guard against accidentally adding the
  # standard INF opt-in keys.
  if awk '
    {
      sub(/\r$/, "")
      line = $0
      # Skip full-line comments.
      if (line ~ /^[[:space:]]*;/) next
      # Strip inline comments.
      sub(/[[:space:]]*;.*$/, "", line)
      if (line ~ /^[[:space:]]*$/) next
      low = tolower(line)
      if (index(low, "messagesignaledinterruptproperties") != 0 ||
          index(low, "msisupported") != 0 ||
          index(low, "messagenumberlimit") != 0) {
        print line
        exit 0
      }
    }
    END { exit 1 }
  ' "$INF_IOPORT" >/dev/null; then
    fail "inf/aero-virtio-snd-ioport.inf must not opt into MSI/MSI-X (driver is INTx-only)"
  fi
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
if [ -f "$INF_ALIAS_ENABLED" ] && [ -f "$INF_ALIAS_DISABLED" ]; then
  fail "both inf/virtio-snd.inf and inf/virtio-snd.inf.disabled exist; keep only one to avoid multiple matching INFs"
fi

if [ -f "$INF_ALIAS_ENABLED" ]; then
  INF_ALIAS="$INF_ALIAS_ENABLED"
elif [ -f "$INF_ALIAS_DISABLED" ]; then
  INF_ALIAS="$INF_ALIAS_DISABLED"
fi

if [ -n "$INF_ALIAS" ]; then
  require_ascii_only "$INF_ALIAS" "inf/$(basename "$INF_ALIAS")"
  alias_basename=$(basename "$INF_ALIAS")
  note "checking inf/$alias_basename stays in sync..."
  contract_label='inf/aero_virtio_snd.inf'
  alias_label="inf/$alias_basename"
  tmp1=$(mktemp "${TMPDIR:-/tmp}/aero_virtio_snd.inf.XXXXXX") || fail "mktemp failed"
  tmp2=$(mktemp "${TMPDIR:-/tmp}/virtio-snd.inf.alias.XXXXXX") || fail "mktemp failed"

  strip_leading_comment_header "$INF_CONTRACT" > "$tmp1"
  strip_leading_comment_header "$INF_ALIAS" > "$tmp2"

  if ! diff -u "$tmp1" "$tmp2" >/dev/null; then
    # Show a unified diff, but label it with the real file paths to make CI logs
    # actionable (instead of referencing mktemp paths).
    if diff -u -L a -L b /dev/null /dev/null >/dev/null 2>&1; then
      diff -u -L "$contract_label" -L "$alias_label" "$tmp1" "$tmp2" >&2 || true
    else
      # Fallback for diff implementations without -L (label): rewrite headers.
      diff -u "$tmp1" "$tmp2" \
        | sed "1s|^--- .*|--- $contract_label|;2s|^+++ .*|+++ $alias_label|" >&2 || true
    fi
    fail "inf/$alias_basename is out of sync with inf/aero_virtio_snd.inf (ignoring leading comment headers)"
  fi
fi

note "OK"
