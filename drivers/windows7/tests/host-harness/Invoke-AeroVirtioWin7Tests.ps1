# SPDX-License-Identifier: MIT OR Apache-2.0

[CmdletBinding()]
param(
  # QEMU system binary (e.g. qemu-system-x86_64)
  [Parameter(Mandatory = $true)]
  [string]$QemuSystem,

  # Windows 7 disk image that is already installed + provisioned to run the selftest at boot.
  [Parameter(Mandatory = $true)]
  [string]$DiskImagePath,

  # Where to write the captured COM1 serial output.
  [Parameter(Mandatory = $false)]
  [string]$SerialLogPath = "./win7-virtio-serial.log",

  [Parameter(Mandatory = $false)]
  [int]$MemoryMB = 2048,

  [Parameter(Mandatory = $false)]
  [int]$Smp = 2,

  # If set, run QEMU in snapshot mode for the main disk (writes are discarded on exit).
  [Parameter(Mandatory = $false)]
  [switch]$Snapshot,

  # If set, use QEMU's transitional virtio-pci devices (legacy + modern).
  # By default this harness uses modern-only (disable-legacy=on) virtio-pci devices so
  # Win7 drivers can bind to the Aero contract v1 IDs (DEV_1041/DEV_1042/DEV_1052/DEV_1059) and
  # revision gate (REV_01).
  #
  # Transitional mode is primarily a backcompat option for older QEMU builds and/or older guest images:
  # - It uses QEMU defaults for virtio-blk/net and relaxes per-test marker requirements.
  # - It attempts to attach virtio-input keyboard/mouse devices (virtio-keyboard-pci + virtio-mouse-pci) when
  #   the QEMU binary advertises them; otherwise it warns that the guest virtio-input selftest will likely FAIL.
  #   In transitional mode, virtio-input may enumerate with the older transitional ID space (e.g. DEV_1011)
  #   depending on QEMU, so the guest must have a driver package that binds the IDs your QEMU build exposes.
  [Parameter(Mandatory = $false)]
  [switch]$VirtioTransitional,

  # Optional MSI-X vector count to request from virtio-pci devices via the QEMU
  # `vectors=` device property (when supported by the running QEMU build).
  #
  # When set (> 0), the harness appends `,vectors=<N>` to the virtio-net/blk/input/snd
  # `-device` args that it creates.
  #
  # The harness probes whether each QEMU device advertises the `vectors` property (via
  # `-device <name>,help`). If unsupported, it fails fast with a clear error so users
  # don't get an opaque QEMU startup failure.
  #
  # Typical values: 2, 4, 8. Windows may still allocate fewer MSI-X messages than
  # requested; the Aero drivers are expected to fall back to the number of vectors
  # actually granted (including single-vector MSI-X or INTx).
  [Parameter(Mandatory = $false)]
  [int]$VirtioMsixVectors = 0,

  # Optional per-device MSI-X vector overrides. When set (> 0), these override -VirtioMsixVectors for the
  # corresponding device class.
  #
  # Note: these require QEMU support for the `vectors` device property.
  [Parameter(Mandatory = $false)]
  [Alias("VirtioNetMsixVectors")]
  [int]$VirtioNetVectors = 0,

  [Parameter(Mandatory = $false)]
  [Alias("VirtioBlkMsixVectors")]
  [int]$VirtioBlkVectors = 0,

  [Parameter(Mandatory = $false)]
  [Alias("VirtioSndMsixVectors")]
  [int]$VirtioSndVectors = 0,

  [Parameter(Mandatory = $false)]
  [Alias("VirtioInputMsixVectors")]
  [int]$VirtioInputVectors = 0,

  # If set, disable MSI-X for virtio-pci devices created by the harness (virtio-net/blk/input/snd)
  # by appending `,vectors=0` to each virtio `-device` arg (INTx-only mode). This requires the QEMU
  # virtio `vectors` device property.
  [Parameter(Mandatory = $false)]
  [Alias("ForceIntx", "IntxOnly")]
  [switch]$VirtioDisableMsix,

  # If set, require INTx interrupt mode for attached virtio devices (blk/net/input/snd).
  # Fails if the guest reports MSI/MSI-X via virtio-*-irq markers.
  [Parameter(Mandatory = $false)]
  [switch]$RequireIntx,

  # If set, require MSI/MSI-X interrupt mode for attached virtio devices (blk/net/input/snd).
  # Fails if the guest reports INTx via virtio-*-irq markers.
  [Parameter(Mandatory = $false)]
  [switch]$RequireMsi,
 
  # If set, require that the guest selftest was provisioned with `--expect-blk-msi`
  # (i.e. the guest emits `AERO_VIRTIO_SELFTEST|CONFIG|...|expect_blk_msi=1`).
  #
  # This is useful in MSI/MSI-X-specific CI to fail deterministically when running against
  # a mis-provisioned image (where the guest selftest would otherwise accept INTx fallback).
  # Provisioning hint: bake `--expect-blk-msi` into the guest scheduled task via `New-AeroWin7TestImage.ps1 -ExpectBlkMsi`.
  [Parameter(Mandatory = $false)]
  [switch]$RequireExpectBlkMsi,

  # If set, fail if the guest virtio-blk selftest reports non-zero StorPort recovery counters
  # (abort_srb/reset_device_srb/reset_bus_srb/pnp_srb/ioctl_reset) when available from either:
  # - legacy fields on the guest virtio-blk marker, or
  # - the dedicated guest virtio-blk-counters marker.
  [Parameter(Mandatory = $false)]
  [switch]$RequireNoBlkRecovery,

  # If set, fail if the guest virtio-blk-counters marker reports non-zero abort/reset_device/reset_bus.
  # This is a looser check than -RequireNoBlkRecovery (ignores pnp/ioctl_reset).
  #
  # Backward compatibility: if the guest does not emit the dedicated virtio-blk-counters marker at all,
  # fall back to legacy fields on the guest virtio-blk marker (abort_srb/reset_device_srb/reset_bus_srb).
  # If virtio-blk-counters is present but reports SKIP, counters are treated as unavailable and this does
  # not fall back.
  [Parameter(Mandatory = $false)]
  [switch]$FailOnBlkRecovery,

  # If set, fail if the guest virtio-blk-reset-recovery marker reports non-zero reset_detected/hw_reset_bus.
  #
  # Backward compatibility: if the dedicated marker is missing, fall back to the legacy miniport diagnostic line
  # `virtio-blk-miniport-reset-recovery|INFO|...` (WARN treated as unavailable).
  #
  # Note: This is best-effort; if neither source is present, this requirement does not fail.
  [Parameter(Mandatory = $false)]
  [switch]$RequireNoBlkResetRecovery,

  # If set, fail if the guest virtio-blk-reset-recovery marker reports non-zero hw_reset_bus.
  # This is a looser check than -RequireNoBlkResetRecovery (ignores reset_detected).
  #
  # Backward compatibility: if the dedicated marker is missing, fall back to the legacy miniport diagnostic line
  # `virtio-blk-miniport-reset-recovery|INFO|...` (WARN treated as unavailable).
  #
  # Note: This is best-effort; if neither source is present, this requirement does not fail.
  [Parameter(Mandatory = $false)]
  [switch]$FailOnBlkResetRecovery,
  
  # If set, fail if the guest virtio-blk miniport flags diagnostic reports any non-zero
  # removed/surprise_removed/reset_in_progress/reset_pending bits (best-effort; ignores missing/WARN markers).
  [Parameter(Mandatory = $false)]
  [switch]$RequireNoBlkMiniportFlags,
  
  # If set, fail if the guest virtio-blk miniport flags diagnostic reports device removal activity
  # (removed or surprise_removed set). This is a looser check than -RequireNoBlkMiniportFlags.
  [Parameter(Mandatory = $false)]
  [switch]$FailOnBlkMiniportFlags,

  # If set, require the guest virtio-blk-reset marker to PASS (treat SKIP/FAIL/missing as failure).
  #
  # Note: The guest image must be provisioned with `--test-blk-reset` (or env var equivalent) so the
  # guest selftest runs the miniport reset/recovery test.
  [Parameter(Mandatory = $false)]
  [Alias("WithVirtioBlkReset", "EnableVirtioBlkReset", "RequireVirtioBlkReset")]
  [switch]$WithBlkReset,

  # If set, inject deterministic keyboard/mouse events via QMP (prefers `input-send-event`, with backcompat fallbacks) and require the guest
  # virtio-input end-to-end event delivery marker (`virtio-input-events`) to PASS.
  #
  # Also emits a host marker for each injection attempt:
  #   AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_EVENTS_INJECT|PASS/FAIL|attempt=<n>|...
  #
  # Note: The guest image must be provisioned with `--test-input-events` (or env var equivalent) so the
  # guest selftest runs the read-report loop.
  [Parameter(Mandatory = $false)]
  [Alias("WithVirtioInputEvents", "EnableVirtioInputEvents", "RequireVirtioInputEvents")]
  [switch]$WithInputEvents,

  # If set, require the guest virtio-input-leds marker to PASS. This validates the virtio-input statusq output path
  # end-to-end (user-mode HID write -> KMDF HID minidriver -> virtqueue).
  #
  # Note: The guest image must be provisioned with `--test-input-leds` (or env var equivalent) so the guest selftest
  # runs the LED write test and emits the marker.
  # Newer guest selftests also accept `--test-input-led` and emit the legacy marker for compatibility.
  [Parameter(Mandatory = $false)]
  [Alias("WithVirtioInputLeds", "EnableVirtioInputLeds", "RequireVirtioInputLeds")]
  [switch]$WithInputLeds,

  # If set, inject deterministic Consumer Control (media key) events via QMP (`input-send-event`) and require the guest
  # virtio-input-media-keys marker to PASS.
  #
  # Also emits a host marker for each injection attempt:
  #   AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_MEDIA_KEYS_INJECT|PASS/FAIL|attempt=<n>|kbd_mode=device/broadcast
  #
  # Note: The guest image must be provisioned with `--test-input-media-keys` (or env var equivalent) so the
  # guest selftest runs the read-report loop.
  [Parameter(Mandatory = $false)]
  [Alias("WithVirtioInputMediaKeys", "EnableVirtioInputMediaKeys", "RequireVirtioInputMediaKeys")]
  [switch]$WithInputMediaKeys,

  # If set, require the guest virtio-input-led marker (keyboard LED output -> statusq) to PASS.
  #
  # Note: The guest image must be provisioned with `--test-input-led` (or env var equivalent) so the
  # guest selftest emits the marker.
  [Parameter(Mandatory = $false)]
  [Alias("WithVirtioInputLed", "EnableVirtioInputLed", "RequireVirtioInputLed")]
  [switch]$WithInputLed,

  # If set, also inject vertical + horizontal scroll wheel events (QMP rel axes: wheel/vscroll + hscroll/hwheel; with
  # best-effort axis name fallbacks) and
  # require the guest virtio-input-wheel marker to PASS.
  # This implies -WithInputEvents.
  [Parameter(Mandatory = $false)]
  [Alias("WithVirtioInputWheel", "EnableVirtioInputWheel", "RequireVirtioInputWheel")]
  [switch]$WithInputWheel,

  # If set, also inject and require additional virtio-input end-to-end markers:
  #   - virtio-input-events-modifiers (Shift/Ctrl/Alt + F1)
  #   - virtio-input-events-buttons   (side/extra mouse buttons)
  #   - virtio-input-events-wheel     (mouse wheel)
  #
  # This implies -WithInputEvents, and requires the guest image provisioned with the corresponding
  # guest selftest flags/env vars (e.g. --test-input-events-extended).
  [Parameter(Mandatory = $false)]
  [Alias("WithInputEventsExtra")]
  [switch]$WithInputEventsExtended,

  # If set, attach a virtio-tablet-pci device, inject deterministic absolute-pointer events via QMP `input-send-event` (required; no backcompat fallback),
  # and require the guest virtio-input-tablet-events marker to PASS.
  #
  # Also emits a host marker for each injection attempt:
  #   AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_TABLET_EVENTS_INJECT|PASS/FAIL|attempt=<n>|tablet_mode=device/broadcast
  #
  # Note: The guest image must be provisioned with `--test-input-tablet-events` (alias: `--test-tablet-events`)
  # (or env var equivalent, e.g. AERO_VIRTIO_SELFTEST_TEST_INPUT_TABLET_EVENTS=1 or
  # AERO_VIRTIO_SELFTEST_TEST_TABLET_EVENTS=1) so the guest selftest runs the read-report loop.
  [Parameter(Mandatory = $false)]
  [Alias("WithVirtioInputTabletEvents", "EnableVirtioInputTabletEvents", "RequireVirtioInputTabletEvents", "WithTabletEvents", "EnableTabletEvents")]
  [switch]$WithInputTabletEvents,

  # If set, attach a virtio-tablet-pci device in addition to the virtio keyboard/mouse.
  # Unlike -WithInputTabletEvents, this does not inject QMP events or require the guest marker.
  [Parameter(Mandatory = $false)]
  [Alias("EnableVirtioTablet")]
  [switch]$WithVirtioTablet,

  # If set, run an end-to-end virtio-blk runtime resize test:
  # - wait for the guest marker:
  #     AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|READY|disk=<N>|old_bytes=<u64>
  # - grow the backing device via QMP (blockdev-resize/block_resize)
  # - require the guest virtio-blk-resize marker to PASS (not SKIP/FAIL/missing)
  #
  # Note: The guest image must be provisioned with `--test-blk-resize` (or env var equivalent)
  # so it arms the polling loop and emits READY/PASS/FAIL markers.
  [Parameter(Mandatory = $false)]
  [Alias("WithVirtioBlkResize", "EnableVirtioBlkResize", "RequireVirtioBlkResize")]
  [switch]$WithBlkResize,

  # Delta in MiB to grow the virtio-blk backing device when -WithBlkResize is enabled.
  [Parameter(Mandatory = $false)]
  [int]$BlkResizeDeltaMiB = 64,

  # If set, run a QMP `query-pci` preflight to validate QEMU-emitted virtio PCI Vendor/Device/Revision IDs.
  # In default (contract-v1) mode this enforces VEN_1AF4 + DEV_1041/DEV_1042/DEV_1052[/DEV_1059] and REV_01.
  # In transitional mode this is permissive and only asserts that at least one VEN_1AF4 device exists.
  [Parameter(Mandatory = $false)]
  [Alias("QmpPreflightPci")]
  [switch]$QemuPreflightPci,

  # If set, require that the corresponding virtio PCI function has MSI-X enabled.
  # Verification is performed via QMP/QEMU introspection (query-pci or HMP `info pci` fallback).
  #
  # For virtio-net, this also requires the guest to report running in MSI-X mode via:
  #   AERO_VIRTIO_SELFTEST|TEST|virtio-net-msix|PASS|mode=msix|...
  # Tip: to make the guest fail-fast, provision it with:
  #   aero-virtio-selftest.exe --require-net-msix
  # (or env var AERO_VIRTIO_SELFTEST_REQUIRE_NET_MSIX=1). When provisioning via New-AeroWin7TestImage.ps1,
  # pass -RequireNetMsix.
  #
  # For virtio-blk, this also requires the guest to report running in MSI-X mode via:
  #   AERO_VIRTIO_SELFTEST|TEST|virtio-blk-msix|PASS|mode=msix|...
  # Tip: to make the guest fail-fast when virtio-blk is not using MSI/MSI-X, provision it with:
  #   aero-virtio-selftest.exe --expect-blk-msi
  # (or env var AERO_VIRTIO_SELFTEST_EXPECT_BLK_MSI=1). When provisioning via New-AeroWin7TestImage.ps1,
  # pass -ExpectBlkMsi.
  #
  # For virtio-snd, this also requires the guest to report running in MSI-X mode via:
  #   AERO_VIRTIO_SELFTEST|TEST|virtio-snd-msix|PASS|mode=msix|...
  # Tip: to make the guest fail-fast, provision it with:
  #   aero-virtio-selftest.exe --require-snd-msix
  # (or env var AERO_VIRTIO_SELFTEST_REQUIRE_SND_MSIX=1). When provisioning via New-AeroWin7TestImage.ps1,
  # pass -RequireSndMsix.
  [Parameter(Mandatory = $false)]
  [Alias("RequireNetMsix")]
  [switch]$RequireVirtioNetMsix,

  [Parameter(Mandatory = $false)]
  [Alias("RequireBlkMsix")]
  [switch]$RequireVirtioBlkMsix,

  [Parameter(Mandatory = $false)]
  [Alias("RequireSndMsix")]
  [switch]$RequireVirtioSndMsix,

  # If set, require the guest virtio-input-msix marker to report mode=msix.
  # This is optional so older guest selftest binaries (which don't emit the marker) can still run.
  #
  # Tip: to make the guest fail-fast (and emit RESULT=FAIL when virtio-input is not using MSI-X), provision the guest
  # selftest with:
  #   aero-virtio-selftest.exe --require-input-msix
  # (or env var AERO_VIRTIO_SELFTEST_REQUIRE_INPUT_MSIX=1). When provisioning via New-AeroWin7TestImage.ps1,
  # pass -RequireInputMsix.
  [Parameter(Mandatory = $false)]
  [Alias("RequireInputMsix")]
  [switch]$RequireVirtioInputMsix,

  # If set, require the guest virtio-input-binding marker to PASS (ensures at least one virtio-input PCI device is
  # present and bound to the expected Aero driver service).
  #
  # In default (non-transitional) mode this is already enforced via per-test marker requirements; this flag is useful
  # to enforce the same check in -VirtioTransitional mode.
  [Parameter(Mandatory = $false)]
  [switch]$RequireVirtioInputBinding,

  # If set, require at least one checksum-offloaded TX packet in the virtio-net driver.
  # This checks the guest marker:
  #   AERO_VIRTIO_SELFTEST|TEST|virtio-net-offload-csum|PASS|tx_csum=...
  [Parameter(Mandatory = $false)]
  [Alias("RequireVirtioNetCsumOffload")]
  [switch]$RequireNetCsumOffload,
 
  # If set, require at least one UDP checksum-offloaded TX packet in the virtio-net driver.
  # This checks the guest marker:
  #   AERO_VIRTIO_SELFTEST|TEST|virtio-net-offload-csum|PASS|tx_udp=... (or tx_udp4/tx_udp6)
  [Parameter(Mandatory = $false)]
  [Alias("RequireVirtioNetUdpCsumOffload")]
  [switch]$RequireNetUdpCsumOffload,

  # If set, stream newly captured COM1 serial output to stdout while waiting.
  [Parameter(Mandatory = $false)]
  [switch]$FollowSerial,

  [Parameter(Mandatory = $false)]
  [int]$TimeoutSeconds = 600,

  # HTTP server port on the host.
  # Guest reaches it at:
  #   http://10.0.2.2:<port>/aero-virtio-selftest
  # and the virtio-net selftest also fetches:
  #   http://10.0.2.2:<port>/aero-virtio-selftest-large
  [Parameter(Mandatory = $false)]
  [int]$HttpPort = 18080,

  [Parameter(Mandatory = $false)]
  # Base HTTP path. The harness also serves a deterministic 1 MiB payload at "${HttpPath}-large".
  [string]$HttpPath = "/aero-virtio-selftest",

  # Optional per-request HTTP log output path (useful for CI artifacts).
  # When set, the harness appends one line per request:
  #   <method> <path> <status_code> <bytes>
  # Logging is best-effort and must never fail the harness due to I/O errors.
  [Parameter(Mandatory = $false)]
  [string]$HttpLogPath = "",

  # UDP echo server port on the host (loopback).
  # Guest reaches it at:
  #   10.0.2.2:<port>
  [Parameter(Mandatory = $false)]
  [int]$UdpPort = 18081,

  # If set, do not start the host UDP echo server and do not require the guest virtio-net-udp marker.
  # This is useful when running the harness against older guest selftest binaries that do not yet
  # implement the UDP test.
  [Parameter(Mandatory = $false)]
  [switch]$DisableUdp,

  # If set, run an end-to-end virtio-net link flap regression test coordinated by QMP `set_link`.
  # The harness waits for:
  #   AERO_VIRTIO_SELFTEST|TEST|virtio-net-link-flap|READY
  # then toggles the virtio-net link DOWN/UP and requires:
  #   AERO_VIRTIO_SELFTEST|TEST|virtio-net-link-flap|PASS
  #
  # Note: this requires a guest image provisioned with `--test-net-link-flap` (or env var equivalent),
  # and a QEMU build that supports QMP `set_link`.
  [Parameter(Mandatory = $false)]
  [Alias("WithVirtioNetLinkFlap", "EnableVirtioNetLinkFlap", "RequireVirtioNetLinkFlap")]
  [switch]$WithNetLinkFlap,

  # If set, attach a virtio-snd device (virtio-sound-pci / virtio-snd-pci).
  # Note: the guest selftest always emits virtio-snd markers (playback + capture + duplex), but will report SKIP if the
  # virtio-snd PCI device is missing or the test was disabled. When -WithVirtioSnd is enabled, the harness
  # requires virtio-snd, virtio-snd-capture, and virtio-snd-duplex to PASS.
  [Parameter(Mandatory = $false)]
  [Alias("EnableVirtioSnd", "RequireVirtioSnd")]
  [switch]$WithVirtioSnd,

  # If set, require the guest virtio-snd-buffer-limits marker to PASS.
  #
  # Note: this requires:
  # - a guest image provisioned with `--test-snd-buffer-limits` (or env var AERO_VIRTIO_SELFTEST_TEST_SND_BUFFER_LIMITS=1)
  #   (for example via New-AeroWin7TestImage.ps1 -TestSndBufferLimits), and
  # - -WithVirtioSnd (so a virtio-snd device is attached).
  [Parameter(Mandatory = $false)]
  [Alias("WithVirtioSndBufferLimits", "EnableSndBufferLimits", "EnableVirtioSndBufferLimits", "RequireVirtioSndBufferLimits")]
  [switch]$WithSndBufferLimits,

  # NOTE: `-WithVirtioInputEvents` / `-RequireVirtioInputEvents` / `-EnableVirtioInputEvents` are accepted as aliases for `-WithInputEvents`
  # for backwards compatibility.

  # Audio backend for virtio-snd.
  # - none: no host audio (device exists)
  # - wav:  capture deterministic audio output to a wav file
  [Parameter(Mandatory = $false)]
  [ValidateSet("none", "wav")]
  [string]$VirtioSndAudioBackend = "none",

  # Output wav path when VirtioSndAudioBackend is "wav".
  [Parameter(Mandatory = $false)]
  [string]$VirtioSndWavPath = "",

  # If set, verify that the virtio-snd wav capture contains non-silent PCM audio.
  # This closes the loop between guest-side playback success and host-side audio backend output.
  [Parameter(Mandatory = $false)]
  [switch]$VerifyVirtioSndWav,

  # Peak absolute sample threshold for VerifyVirtioSndWav (16-bit PCM units; used as a reference scale).
  [Parameter(Mandatory = $false)]
  [int]$VirtioSndWavPeakThreshold = 200,

  # RMS threshold for VerifyVirtioSndWav (16-bit PCM units; used as a reference scale).
  [Parameter(Mandatory = $false)]
  [int]$VirtioSndWavRmsThreshold = 50,

  # If set, print the computed QEMU argument list and exit 0 without launching QEMU (or starting the HTTP server).
  [Parameter(Mandatory = $false)]
  [Alias("PrintQemuArgs")]
  [switch]$DryRun,

  # Extra args passed verbatim to QEMU (advanced use).
  [Parameter(Mandatory = $false)]
  [string[]]$QemuExtraArgs = @()
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

. (Join-Path $PSScriptRoot "AeroVirtioWin7QemuArgs.ps1")

# Stable QOM `id=` values for virtio-input devices so QMP can target them explicitly.
$script:VirtioInputKeyboardQmpId = "aero_virtio_kbd0"
$script:VirtioInputMouseQmpId = "aero_virtio_mouse0"
$script:VirtioInputTabletQmpId = "aero_virtio_tablet0"
# Stable QOM `id=` value for the virtio-net device so QMP `set_link` can target it deterministically.
$script:VirtioNetQmpId = "aero_virtio_net0"
if ($VerifyVirtioSndWav) {
  if (-not $WithVirtioSnd) {
    throw "-VerifyVirtioSndWav requires -WithVirtioSnd/-RequireVirtioSnd/-EnableVirtioSnd."
  }
  if ($VirtioSndAudioBackend -ne "wav") {
    throw "-VerifyVirtioSndWav requires -VirtioSndAudioBackend wav."
  }
  if ($VirtioSndWavPeakThreshold -lt 0) {
    throw "-VirtioSndWavPeakThreshold must be >= 0."
  }
  if ($VirtioSndWavRmsThreshold -lt 0) {
    throw "-VirtioSndWavRmsThreshold must be >= 0."
  }
}

if ($RequireVirtioSndMsix -and (-not $WithVirtioSnd)) {
  throw "-RequireVirtioSndMsix/-RequireSndMsix requires -WithVirtioSnd/-RequireVirtioSnd/-EnableVirtioSnd."
}

if ($WithSndBufferLimits -and (-not $WithVirtioSnd)) {
  throw "-WithSndBufferLimits/-WithVirtioSndBufferLimits/-RequireVirtioSndBufferLimits/-EnableSndBufferLimits/-EnableVirtioSndBufferLimits requires -WithVirtioSnd/-RequireVirtioSnd/-EnableVirtioSnd (the buffer limits stress test only runs when a virtio-snd device is attached)."
}

if ($VirtioTransitional -and $WithVirtioSnd) {
  throw "-VirtioTransitional is incompatible with -WithVirtioSnd/-RequireVirtioSnd/-EnableVirtioSnd (virtio-snd testing requires modern-only virtio-pci + contract revision overrides)."
}

if ($VirtioMsixVectors -lt 0) {
  throw "-VirtioMsixVectors must be a positive integer."
}
if ($VirtioNetVectors -lt 0) {
  throw "-VirtioNetVectors must be a positive integer."
}
if ($VirtioBlkVectors -lt 0) {
  throw "-VirtioBlkVectors must be a positive integer."
}
if ($VirtioSndVectors -lt 0) {
  throw "-VirtioSndVectors must be a positive integer."
}
if ($VirtioInputVectors -lt 0) {
  throw "-VirtioInputVectors must be a positive integer."
}

if ($RequireIntx -and $RequireMsi) {
  throw "-RequireIntx and -RequireMsi are mutually exclusive."
}

if ($VirtioDisableMsix -and (($VirtioMsixVectors -gt 0) -or ($VirtioNetVectors -gt 0) -or ($VirtioBlkVectors -gt 0) -or ($VirtioSndVectors -gt 0) -or ($VirtioInputVectors -gt 0))) {
  throw "-VirtioDisableMsix is mutually exclusive with -VirtioMsixVectors/-Virtio*Vectors (INTx-only mode disables MSI-X by forcing vectors=0)."
}
if ($VirtioDisableMsix -and ($RequireVirtioNetMsix -or $RequireVirtioBlkMsix -or $RequireVirtioSndMsix -or $RequireVirtioInputMsix)) {
  throw "-VirtioDisableMsix is incompatible with -RequireVirtio*Msix (aliases: -RequireNetMsix/-RequireBlkMsix/-RequireSndMsix/-RequireInputMsix) (MSI-X is disabled)."
}

if ($VirtioSndVectors -gt 0 -and (-not $WithVirtioSnd)) {
  throw "-VirtioSndVectors requires -WithVirtioSnd/-RequireVirtioSnd/-EnableVirtioSnd."
}

function Resolve-AeroWin7QemuMsixVectors {
  [CmdletBinding()]
  param(
    [Parameter(Mandatory = $true)]
    [string]$QemuSystem,
    [Parameter(Mandatory = $true)]
    [string]$DeviceName,
    [Parameter(Mandatory = $true)]
    [int]$Vectors,
    [Parameter(Mandatory = $true)]
    [string]$ParamName,

    # If set, validate QEMU `vectors` property support even when Vectors=0 (used for -VirtioDisableMsix).
    [Parameter(Mandatory = $false)]
    [switch]$ForceCheck
  )

  $helpText = $null
  if ($Vectors -le 0 -and (-not $ForceCheck)) { return 0 }
  if ($DryRun) { return $Vectors }

  $helpText = Get-AeroWin7QemuDeviceHelpText -QemuSystem $QemuSystem -DeviceName $DeviceName
  if ($helpText -notmatch "(?m)^\s*vectors\b") {
    throw "QEMU device '$DeviceName' does not advertise a 'vectors' property. $ParamName requires it. Disable the flag or upgrade QEMU."
  }
  return $Vectors
}

function Assert-AeroWin7QemuAcceptsVectorsZero {
  [CmdletBinding()]
  param(
    [Parameter(Mandatory = $true)]
    [string]$QemuSystem,
    [Parameter(Mandatory = $true)]
    [string]$DeviceName
  )

  try {
    # Use QEMU's `-device <name>,help` path to validate that `vectors=0` is accepted.
    $null = Get-AeroWin7QemuDeviceHelpText -QemuSystem $QemuSystem -DeviceName "$DeviceName,vectors=0"
  } catch {
    throw "QEMU rejected 'vectors=0' for device '$DeviceName' (required by -VirtioDisableMsix). Upgrade QEMU or omit -VirtioDisableMsix. $_"
  }
}

if ($HttpPort -le 0 -or $HttpPort -gt 65535) {
  throw "-HttpPort must be in the range 1..65535."
}

if ([string]::IsNullOrEmpty($HttpPath) -or (-not $HttpPath.StartsWith("/"))) {
  throw "-HttpPath must start with '/'."
}
if ($HttpPath -match "\s") {
  throw "-HttpPath must not contain whitespace."
}

if ($QemuSystem -match "[\\/\\\\]" -and (Test-Path -LiteralPath $QemuSystem -PathType Container)) {
  throw "-QemuSystem must be a QEMU system binary path (got a directory): $QemuSystem"
}
if (-not $DryRun) {
  if ($QemuSystem -match "[\\/\\\\]") {
    if (-not (Test-Path -LiteralPath $QemuSystem -PathType Leaf)) {
      throw "-QemuSystem must be a QEMU system binary path (file not found): $QemuSystem"
    }
  } else {
    try {
      $null = Get-Command -Name $QemuSystem -CommandType Application -ErrorAction Stop
    } catch {
      throw "-QemuSystem must be on PATH (qemu-system binary not found): $QemuSystem"
    }
  }
}

if ($MemoryMB -le 0) {
  throw "-MemoryMB must be a positive integer."
}
if ($Smp -le 0) {
  throw "-Smp must be a positive integer."
}
if ($TimeoutSeconds -le 0) {
  throw "-TimeoutSeconds must be a positive integer."
}

if (-not $DisableUdp) {
  if ($UdpPort -le 0 -or $UdpPort -gt 65535) {
    throw "-UdpPort must be in the range 1..65535."
  }
}

$virtioSndPciDeviceName = ""
if ($WithVirtioSnd -and (-not $DryRun)) {
  # Fail fast with a clear message if the selected QEMU binary lacks virtio-snd support (or the
  # device properties needed for the Aero contract v1 identity).
  $virtioSndPciDeviceName = Assert-AeroWin7QemuSupportsVirtioSndPciDevice -QemuSystem $QemuSystem
} elseif ($WithVirtioSnd) {
  # Dry-run must not execute any QEMU subprocess probes. Default to the modern virtio-snd device name
  # for the purpose of printing a copy/pasteable argv. Real runs still probe QEMU and may select
  # `virtio-snd-pci` on older builds.
  $virtioSndPciDeviceName = "virtio-sound-pci"
}

function Start-AeroSelftestHttpServer {
  param(
    [Parameter(Mandatory = $true)] [int]$Port,
    [Parameter(Mandatory = $true)] [string]$Path
  )

  $listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, $Port)
  $listener.Start()
  return $listener
}

function Start-AeroSelftestUdpEchoServer {
  param(
    [Parameter(Mandatory = $true)] [int]$Port
  )

  $sock = [System.Net.Sockets.Socket]::new(
    [System.Net.Sockets.AddressFamily]::InterNetwork,
    [System.Net.Sockets.SocketType]::Dgram,
    [System.Net.Sockets.ProtocolType]::Udp
  )
  try {
    $sock.Bind([System.Net.IPEndPoint]::new([System.Net.IPAddress]::Loopback, $Port))
    return $sock
  } catch {
    try { $sock.Close() } catch { }
    throw
  }
}

function Try-HandleAeroUdpEchoRequest {
  param(
    [Parameter(Mandatory = $true)] $Socket,
    [Parameter(Mandatory = $false)] [int]$MaxDatagramSize = 2048
  )

  if ($null -eq $Socket) { return $false }

  # Handle a small bounded number of datagrams per call so we never starve the main wait loop.
  $handledAny = $false
  $buf = New-Object byte[] ([Math]::Max(1, $MaxDatagramSize + 1))
  $remote = [System.Net.EndPoint]([System.Net.IPEndPoint]::new([System.Net.IPAddress]::Any, 0))

  for ($i = 0; $i -lt 16; $i++) {
    try {
      # Non-blocking poll (0 microseconds).
      if (-not $Socket.Poll(0, [System.Net.Sockets.SelectMode]::SelectRead)) { break }
      $n = $Socket.ReceiveFrom($buf, 0, $buf.Length, [System.Net.Sockets.SocketFlags]::None, [ref]$remote)
      if ($n -le 0) { break }
      $handledAny = $true

      # Drop oversize datagrams (bounded/deterministic).
      if ($n -gt $MaxDatagramSize) { continue }

      $null = $Socket.SendTo($buf, 0, $n, [System.Net.Sockets.SocketFlags]::None, $remote)
    } catch {
      # Best-effort: never fail the harness due to UDP socket errors.
      break
    }
  }

  return $handledAny
}

$script:AeroSelftestLargePayload = $null
function Get-AeroSelftestLargePayload {
  # Lazily construct a deterministic 1 MiB payload (0..255 repeating).
  #
  # Avoid doing 1M PowerShell loop iterations for every request; this keeps the
  # harness responsive while the guest is downloading the body.
  if ($null -ne $script:AeroSelftestLargePayload) { return $script:AeroSelftestLargePayload }

  $size = 1048576
  $payload = New-Object byte[] $size

  $pattern = New-Object byte[] 256
  for ($i = 0; $i -lt 256; $i++) {
    $pattern[$i] = [byte]$i
  }

  for ($offset = 0; $offset -lt $size; $offset += 256) {
    [System.Buffer]::BlockCopy($pattern, 0, $payload, $offset, 256)
  }

  $script:AeroSelftestLargePayload = $payload
  return $payload
}

function Get-AeroSelftestLargePath {
  param(
    [Parameter(Mandatory = $true)] [string]$Path
  )

  # Compute "<path>-large" but insert before any query/fragment delimiter so a
  # configured HttpPath like "/foo?x=y" becomes "/foo-large?x=y".
  #
  # For backwards compatibility, the request handler also accepts "$Path-large"
  # verbatim (even when it would technically place "-large" after the query).
  $q = $Path.IndexOf("?")
  $h = $Path.IndexOf("#")
  $insertPos = -1
  if ($q -ge 0 -and $h -ge 0) {
    $insertPos = [Math]::Min($q, $h)
  } elseif ($q -ge 0) {
    $insertPos = $q
  } elseif ($h -ge 0) {
    $insertPos = $h
  }

  if ($insertPos -lt 0) { return "$Path-large" }
  return $Path.Insert($insertPos, "-large")
}

function Try-HandleAeroHttpRequest {
  param(
    [Parameter(Mandatory = $true)] $Listener,
    [Parameter(Mandatory = $true)] [string]$Path,
    [Parameter(Mandatory = $false)] [string]$HttpLogPath = ""
  )

  if (-not $Listener.Pending()) { return $false }

  $client = $Listener.AcceptTcpClient()
  # Defensive timeouts: if the guest connects but stalls (or stops reading mid-body),
  # don't block the harness wait loop indefinitely.
  $client.ReceiveTimeout = 60000
  $client.SendTimeout = 60000
  try {
    try {
      $stream = $client.GetStream()
      $stream.ReadTimeout = 60000
      $stream.WriteTimeout = 60000
      # Use Latin-1 so we can safely read an arbitrary binary upload body via StreamReader
      # without losing bytes > 0x7F. Headers are ASCII-compatible.
      $enc = [System.Text.Encoding]::GetEncoding(28591)
      $reader = [System.IO.StreamReader]::new($stream, $enc, $false, 4096, $true)
      $requestLine = $reader.ReadLine()
      if ($null -eq $requestLine) { return $true }

      # Read+parse headers.
      $headers = @{}
      while ($true) {
        $line = $reader.ReadLine()
        if ($null -eq $line -or $line.Length -eq 0) { break }
        $idx = $line.IndexOf(":")
        if ($idx -gt 0) {
          $name = $line.Substring(0, $idx).Trim().ToLowerInvariant()
          $value = $line.Substring($idx + 1).Trim()
          if (-not [string]::IsNullOrEmpty($name)) {
            $headers[$name] = $value
          }
        }
      }

      $method = ""
      $reqPath = ""
      if ($requestLine -match "^(GET|HEAD|POST)\s+(\S+)\s+HTTP/") {
        $method = $Matches[1].ToUpperInvariant()
        $reqPath = $Matches[2]
      }

      $isHead = $method -eq "HEAD"
      $isPost = $method -eq "POST"

      $basePathOk = $reqPath -eq $Path
      $largePathOk = $reqPath -eq "$Path-large" -or $reqPath -eq (Get-AeroSelftestLargePath -Path $Path)

      $statusCode = 404
      $statusLine = "HTTP/1.1 404 Not Found"
      $contentType = "text/plain"
      $bodyBytes = [System.Text.Encoding]::ASCII.GetBytes("NOT_FOUND`n")
      $etagHeader = $null
      $extraHeaders = @()

      if (-not [string]::IsNullOrEmpty($method)) {
        if ($isPost) {
          # POST is only supported for the large endpoint (upload verification).
          if ($largePathOk) {
            $expectedLen = 1048576
            $cl = 0
            if ($headers.ContainsKey("content-length")) {
              [int]::TryParse($headers["content-length"], [ref]$cl) | Out-Null
            }

            $uploadOk = $false
            $uploadSha256 = ""
            if ($cl -eq $expectedLen) {
              if ($headers.ContainsKey("expect")) {
                $expectValue = $headers["expect"]
                if (-not [string]::IsNullOrEmpty($expectValue) -and $expectValue.ToLowerInvariant().Contains("100-continue")) {
                  # Some HTTP clients (including WinHTTP) may send `Expect: 100-continue` for large uploads.
                  # Reply with an interim 100 so the client proceeds to send the request body.
                  $continueBytes = [System.Text.Encoding]::ASCII.GetBytes("HTTP/1.1 100 Continue`r`n`r`n")
                  $stream.Write($continueBytes, 0, $continueBytes.Length)
                  $stream.Flush()
                }
              }

              $sha = [System.Security.Cryptography.SHA256]::Create()
              $emptyBytes = New-Object byte[] 0
              $charBuf = New-Object char[] 8192
              $byteBuf = New-Object byte[] 8192
              $remaining = $expectedLen
              while ($remaining -gt 0) {
                $want = [Math]::Min($charBuf.Length, $remaining)
                $readChars = $reader.Read($charBuf, 0, $want)
                if ($readChars -le 0) { break }
                $nBytes = $enc.GetBytes($charBuf, 0, $readChars, $byteBuf, 0)
                $null = $sha.TransformBlock($byteBuf, 0, $nBytes, $null, 0)
                $remaining -= $nBytes
              }
              if ($remaining -eq 0) {
                $null = $sha.TransformFinalBlock($emptyBytes, 0, 0)
                $uploadSha256 = ([System.BitConverter]::ToString($sha.Hash).Replace("-", "").ToLowerInvariant())
                if ($uploadSha256 -eq "fbbab289f7f94b25736c58be46a994c441fd02552cc6022352e3d86d2fab7c83") {
                  $uploadOk = $true
                }
              }
              $sha.Dispose()
            }

            if ($uploadSha256.Length -gt 0) {
              $extraHeaders += "X-Aero-Upload-SHA256: $uploadSha256"
            }

            if ($uploadOk) {
              $statusCode = 200
              $statusLine = "HTTP/1.1 200 OK"
              $bodyBytes = [System.Text.Encoding]::ASCII.GetBytes("OK`n")
            } else {
              $statusCode = 400
              $statusLine = "HTTP/1.1 400 Bad Request"
              $bodyBytes = [System.Text.Encoding]::ASCII.GetBytes("BAD_UPLOAD`n")
            }
          }
        } else {
          # GET/HEAD.
          if ($basePathOk) {
            $statusCode = 200
            $statusLine = "HTTP/1.1 200 OK"
            $bodyBytes = [System.Text.Encoding]::ASCII.GetBytes("OK`n")
          } elseif ($largePathOk) {
            $statusCode = 200
            $statusLine = "HTTP/1.1 200 OK"
            # Deterministic 1 MiB payload (0..255 repeating) for sustained virtio-net TX/RX stress.
            $contentType = "application/octet-stream"
            $etagHeader = "ETag: `"8505ae4435522325`""
            $bodyBytes = Get-AeroSelftestLargePayload
          }
        }
      }

      $hdrLines = @(
        $statusLine,
        "Content-Type: $contentType",
        "Content-Length: $($bodyBytes.Length)",
        "Cache-Control: no-store",
        $etagHeader,
        $extraHeaders,
        "Connection: close",
        "",
        ""
      ) | Where-Object { $null -ne $_ -and $_.Length -gt 0 }
      $hdr = ($hdrLines + @("", "")) -join "`r`n"

      $hdrBytes = [System.Text.Encoding]::ASCII.GetBytes($hdr)
      $stream.Write($hdrBytes, 0, $hdrBytes.Length)
      if (-not $isHead) {
        # Write in chunks so a large body (1 MiB) can't block forever behind a single large write
        # if the guest stalls mid-transfer. Socket/stream timeouts are still enforced.
        $chunkSize = 65536
        for ($offset = 0; $offset -lt $bodyBytes.Length; $offset += $chunkSize) {
          $count = [Math]::Min($chunkSize, $bodyBytes.Length - $offset)
          $stream.Write($bodyBytes, $offset, $count)
        }
      }
      $stream.Flush()

      if (-not [string]::IsNullOrEmpty($HttpLogPath)) {
        # Best-effort logging: never fail the harness due to log I/O.
        try {
          $bytesSent = if ($isHead) { 0 } else { $bodyBytes.Length }
          $logMethod = if ([string]::IsNullOrEmpty($method)) { "?" } else { $method }
          $logPath = if ([string]::IsNullOrEmpty($reqPath)) { "?" } else { $reqPath }
          $line = "$logMethod $logPath $statusCode $bytesSent$([Environment]::NewLine)"
          [System.IO.File]::AppendAllText($HttpLogPath, $line, [System.Text.Encoding]::UTF8)
        } catch { }
      }
      return $true
    } catch {
      # Best-effort: never fail the harness due to an HTTP socket error; the guest selftest
      # will report connectivity failures via serial markers.
      return $true
    }
  } finally {
    $client.Close()
  }
}

function Read-NewText {
  param(
    [Parameter(Mandatory = $true)] [string]$Path,
    [Parameter(Mandatory = $true)] [ref]$Position
  )

  if (-not (Test-Path -LiteralPath $Path)) { return "" }

  $fs = [System.IO.File]::Open($Path, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, [System.IO.FileShare]::ReadWrite)
  try {
    $null = $fs.Seek($Position.Value, [System.IO.SeekOrigin]::Begin)
    $buf = New-Object byte[] 8192
    $n = $fs.Read($buf, 0, $buf.Length)
    if ($n -le 0) { return "" }
    $Position.Value += $n

    return [System.Text.Encoding]::UTF8.GetString($buf, 0, $n)
  } finally {
    $fs.Dispose()
  }
}

function Wait-AeroSelftestResult {
  param(
    [Parameter(Mandatory = $true)] [string]$SerialLogPath,
    [Parameter(Mandatory = $true)] [System.Diagnostics.Process]$QemuProcess,
    [Parameter(Mandatory = $true)] [int]$TimeoutSeconds,
    [Parameter(Mandatory = $true)] $HttpListener,
    [Parameter(Mandatory = $true)] [string]$HttpPath,
    [Parameter(Mandatory = $false)] [string]$HttpLogPath = "",
    [Parameter(Mandatory = $false)] $UdpSocket = $null,
    [Parameter(Mandatory = $true)] [bool]$FollowSerial,
    # When $true, require per-test markers so older selftest binaries cannot accidentally pass.
    [Parameter(Mandatory = $false)] [bool]$RequirePerTestMarkers = $true,
    # When true, require the guest virtio-net-udp marker to PASS.
    [Parameter(Mandatory = $false)] [bool]$RequireVirtioNetUdpPass = $true,
    # When true, require the guest virtio-blk-reset marker to PASS (not SKIP/FAIL/missing).
    [Parameter(Mandatory = $false)] [bool]$RequireVirtioBlkResetPass = $false,
    # If true, require the optional virtio-net-link-flap marker to PASS (host will toggle link via QMP `set_link`).
    [Parameter(Mandatory = $false)]
    [Alias("EnableVirtioNetLinkFlap")]
    [bool]$RequireVirtioNetLinkFlapPass = $false,
    # If true, a virtio-snd device was attached, so the virtio-snd selftest must actually run and pass
    # (not be skipped via --disable-snd).
    [Parameter(Mandatory = $true)] [bool]$RequireVirtioSndPass,
    # If true, require the optional virtio-snd-buffer-limits stress test marker to PASS.
    # This is intended to be paired with provisioning the guest with `--test-snd-buffer-limits`
    # (or env var AERO_VIRTIO_SELFTEST_TEST_SND_BUFFER_LIMITS=1).
    [Parameter(Mandatory = $false)] [bool]$RequireVirtioSndBufferLimitsPass = $false,
    # If true, require the optional virtio-blk-resize marker to PASS and orchestrate a QMP resize.
    # The guest must be provisioned with `--test-blk-resize` (or env var equivalent).
    [Parameter(Mandatory = $false)] [bool]$RequireVirtioBlkResizePass = $false,
    # Delta in MiB to grow the virtio-blk backing device when RequireVirtioBlkResizePass is enabled.
    [Parameter(Mandatory = $false)] [int]$VirtioBlkResizeDeltaMiB = 64,
    # If true, require at least one checksum-offloaded TX packet from virtio-net.
    [Parameter(Mandatory = $false)] [bool]$RequireNetCsumOffload = $false,
    # If true, require at least one UDP checksum-offloaded TX packet from virtio-net.
    [Parameter(Mandatory = $false)] [bool]$RequireNetUdpCsumOffload = $false,
    # If true, require the optional virtio-input-events marker to PASS (host will inject events via QMP).
    [Parameter(Mandatory = $false)]
    [Alias("EnableVirtioInputEvents")]
    [bool]$RequireVirtioInputEventsPass = $false,
    # If true, require the optional virtio-input-media-keys marker to PASS (host will inject Consumer Control
    # (media key) events via QMP).
    [Parameter(Mandatory = $false)]
    [Alias("EnableVirtioInputMediaKeys")]
    [bool]$RequireVirtioInputMediaKeysPass = $false,
    # If true, require the optional virtio-input-led marker to PASS (keyboard LED output -> virtio statusq).
    [Parameter(Mandatory = $false)]
    [Alias("EnableVirtioInputLed")]
    [bool]$RequireVirtioInputLedPass = $false,
    # If true, require the optional virtio-input-leds marker to PASS (keyboard LED/statusq output report smoke test).
    [Parameter(Mandatory = $false)]
    [bool]$RequireVirtioInputLedsPass = $false,
    # If true, require the optional virtio-input-tablet-events marker to PASS (host will inject absolute-pointer events
    # via QMP).
    [Parameter(Mandatory = $false)]
    [Alias("EnableVirtioInputTabletEvents")]
    [bool]$RequireVirtioInputTabletEventsPass = $false,
    # If true, also require the optional virtio-input-wheel marker to PASS (host will inject wheel/hscroll via QMP).
    [Parameter(Mandatory = $false)]
    [Alias("EnableVirtioInputWheel")]
    [bool]$RequireVirtioInputWheelPass = $false,
    # If true, require additional virtio-input-events-* markers (modifiers/buttons/wheel) to PASS.
    [Parameter(Mandatory = $false)]
    [Alias("EnableVirtioInputEventsExtended")]
    [bool]$RequireVirtioInputEventsExtendedPass = $false,

    # If true, require virtio-net-msix marker to report PASS|mode=msix.
    [Parameter(Mandatory = $false)]
    [bool]$RequireVirtioNetMsix = $false,

    # If true, require the virtio-input-msix marker to report mode=msix.
    [Parameter(Mandatory = $false)]
    [bool]$RequireVirtioInputMsixPass = $false,
    # If true, require the virtio-input-binding marker to report PASS (PCI binding/service validation).
    [Parameter(Mandatory = $false)]
    [bool]$RequireVirtioInputBindingPass = $false,
    # If true, require the guest selftest to be configured to fail virtio-blk on INTx (expect MSI/MSI-X),
    # i.e. `AERO_VIRTIO_SELFTEST|CONFIG|...|expect_blk_msi=1`.
    [Parameter(Mandatory = $false)]
    [bool]$RequireExpectBlkMsi = $false,
    # Best-effort QMP channel for input injection.
    [Parameter(Mandatory = $false)] [string]$QmpHost = "127.0.0.1",
    [Parameter(Mandatory = $false)] [Nullable[int]]$QmpPort = $null
  )

  $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
  $pos = 0L
  $tail = ""
  $configExpectBlkMsi = $null
  $sawConfigExpectBlkMsi = $false
  $configUdpPort = $null
  $sawConfigUdpPort = $false
  $udpHostPort = $null
  if ($null -ne $UdpSocket) {
    try {
      $udpHostPort = ([System.Net.IPEndPoint]$UdpSocket.LocalEndPoint).Port
    } catch { }
  }
  $virtioBlkMarkerTime = $null
  $sawVirtioBlkPass = $false
  $sawVirtioBlkFail = $false
  $sawVirtioBlkResizeReady = $false
  $sawVirtioBlkResizePass = $false
  $sawVirtioBlkResizeFail = $false
  $sawVirtioBlkResizeSkip = $false
  $blkResizeOldBytes = $null
  $blkResizeNewBytes = $null
  $blkResizeRequested = $false
  $sawVirtioBlkResetPass = $false
  $sawVirtioBlkResetSkip = $false
  $sawVirtioBlkResetFail = $false
  $sawVirtioInputPass = $false
  $sawVirtioInputFail = $false
  $sawVirtioInputBindPass = $false
  $sawVirtioInputBindFail = $false
  $sawVirtioInputBindingPass = $false
  $sawVirtioInputBindingFail = $false
  $sawVirtioInputBindingSkip = $false
  $virtioInputMarkerTime = $null
  $sawVirtioInputLedsPass = $false
  $sawVirtioInputLedsFail = $false
  $sawVirtioInputLedsSkip = $false
  $sawVirtioInputEventsReady = $false
  $sawVirtioInputEventsPass = $false
  $sawVirtioInputEventsFail = $false
  $sawVirtioInputEventsSkip = $false
  $sawVirtioInputWheelPass = $false
  $sawVirtioInputWheelFail = $false
  $sawVirtioInputWheelSkip = $false
  $sawVirtioInputEventsModifiersPass = $false
  $sawVirtioInputEventsModifiersFail = $false
  $sawVirtioInputEventsModifiersSkip = $false
  $sawVirtioInputEventsButtonsPass = $false
  $sawVirtioInputEventsButtonsFail = $false
  $sawVirtioInputEventsButtonsSkip = $false
  $sawVirtioInputEventsWheelPass = $false
  $sawVirtioInputEventsWheelFail = $false
  $sawVirtioInputEventsWheelSkip = $false
  $inputEventsInjectAttempts = 0
  $maxInputEventsInjectAttempts = if ($RequireVirtioInputEventsExtendedPass) { 30 } else { 20 }
  $nextInputEventsInject = [DateTime]::UtcNow
  $sawVirtioInputMediaKeysReady = $false
  $sawVirtioInputMediaKeysPass = $false
  $sawVirtioInputMediaKeysFail = $false
  $sawVirtioInputMediaKeysSkip = $false
  $sawVirtioInputLedPass = $false
  $sawVirtioInputLedFail = $false
  $sawVirtioInputLedSkip = $false
  $inputMediaKeysInjectAttempts = 0
  $nextInputMediaKeysInject = [DateTime]::UtcNow
  $sawVirtioInputTabletEventsReady = $false
  $sawVirtioInputTabletEventsPass = $false
  $sawVirtioInputTabletEventsFail = $false
  $sawVirtioInputTabletEventsSkip = $false
  $inputTabletEventsInjectAttempts = 0
  $nextInputTabletEventsInject = [DateTime]::UtcNow
  $sawVirtioSndPass = $false
  $sawVirtioSndSkip = $false
  $sawVirtioSndFail = $false
  $sawVirtioSndCapturePass = $false
  $sawVirtioSndCaptureSkip = $false
  $sawVirtioSndCaptureFail = $false
  $sawVirtioSndDuplexPass = $false
  $sawVirtioSndDuplexSkip = $false
  $sawVirtioSndDuplexFail = $false
  $sawVirtioSndBufferLimitsPass = $false
  $sawVirtioSndBufferLimitsSkip = $false
  $sawVirtioSndBufferLimitsFail = $false
  $sawVirtioNetPass = $false
  $sawVirtioNetFail = $false
  $virtioNetMarkerTime = $null
  $sawVirtioNetUdpPass = $false
  $sawVirtioNetUdpFail = $false
  $sawVirtioNetUdpSkip = $false
  $sawVirtioNetMsixModeMsix = $false
  $sawVirtioNetLinkFlapReady = $false
  $sawVirtioNetLinkFlapPass = $false
  $sawVirtioNetLinkFlapFail = $false
  $sawVirtioNetLinkFlapSkip = $false
  $didNetLinkFlap = $false

  function Test-VirtioInputMsixRequirement {
    param(
      [Parameter(Mandatory = $true)] [string]$Tail,
      # Optional: if provided, fall back to parsing the full serial log when the rolling tail buffer does not
      # contain the virtio-input-msix marker (e.g. because the tail was truncated).
      [Parameter(Mandatory = $false)] [string]$SerialLogPath = ""
    )
 
    if (-not $RequireVirtioInputMsixPass) { return $null }
 
    $prefix = "AERO_VIRTIO_SELFTEST|TEST|virtio-input-msix|"
    $line = Try-ExtractLastAeroMarkerLine -Tail $Tail -Prefix $prefix -SerialLogPath $SerialLogPath
    if ($null -eq $line) {
      return @{ Result = "MISSING_VIRTIO_INPUT_MSIX"; Tail = $Tail }
    }
 
    $fields = @{}
    foreach ($tok in $line.Split("|")) {
      $idx = $tok.IndexOf("=")
      if ($idx -le 0) { continue }
      $k = $tok.Substring(0, $idx).Trim()
      $v = $tok.Substring($idx + 1).Trim()
      if (-not [string]::IsNullOrEmpty($k)) {
        $fields[$k] = $v
      }
    }
 
    if ($line -match "\|FAIL(\||$)") {
      $reason = "virtio-input-msix marker reported FAIL"
      if ($fields.ContainsKey("reason")) { $reason += " reason=$($fields["reason"])" }
      if ($fields.ContainsKey("err")) { $reason += " err=$($fields["err"])" }
      return @{ Result = "VIRTIO_INPUT_MSIX_REQUIRED"; Tail = $Tail; MsixReason = $reason }
    }
    if ($line -match "\|SKIP(\||$)") {
      $reason = "virtio-input-msix marker reported SKIP"
      if ($fields.ContainsKey("reason")) { $reason += " reason=$($fields["reason"])" }
      if ($fields.ContainsKey("err")) { $reason += " err=$($fields["err"])" }
      return @{ Result = "VIRTIO_INPUT_MSIX_REQUIRED"; Tail = $Tail; MsixReason = $reason }
    }
 
    if (-not $fields.ContainsKey("mode")) {
      return @{ Result = "VIRTIO_INPUT_MSIX_REQUIRED"; Tail = $Tail; MsixReason = "virtio-input-msix marker missing mode=... field" }
    }
 
    $mode = [string]$fields["mode"]
    if ($mode -ne "msix") {
      $msgs = "?"
      if ($fields.ContainsKey("messages")) { $msgs = [string]$fields["messages"] }
      return @{ Result = "VIRTIO_INPUT_MSIX_REQUIRED"; Tail = $Tail; MsixReason = "mode=$mode (expected msix) messages=$msgs" }
    }
 
    return $null
  }

  while ((Get-Date) -lt $deadline) {
    $null = Try-HandleAeroHttpRequest -Listener $HttpListener -Path $HttpPath -HttpLogPath $HttpLogPath
    if ($null -ne $UdpSocket) {
      $null = Try-HandleAeroUdpEchoRequest -Socket $UdpSocket
    }

    $chunk = Read-NewText -Path $SerialLogPath -Position ([ref]$pos)
    if ($chunk.Length -gt 0) {
      if ($FollowSerial) {
        [Console]::Out.Write($chunk)
      }

      # Keep a rolling tail buffer for marker parsing.
      # Trim the existing tail *before* appending the new chunk so we avoid allocating a temporary
      # tail+chunk string larger than the cap. If the new chunk itself exceeds the cap, keep only
      # its last N characters.
      $maxTailLen = 131072
      if ($chunk.Length -ge $maxTailLen) {
        $tail = $chunk.Substring($chunk.Length - $maxTailLen)
      } else {
        $maxOld = $maxTailLen - $chunk.Length
        if ($tail.Length -gt $maxOld) { $tail = $tail.Substring($tail.Length - $maxOld) }
        $tail += $chunk
      }

      if ($RequireExpectBlkMsi -and (-not $sawConfigExpectBlkMsi)) {
        # Parse the guest selftest CONFIG marker to ensure the image was provisioned
        # with `--expect-blk-msi` (expect_blk_msi=1). This provides deterministic
        # harness-side gating for MSI/MSI-X-specific CI.
        $prefix = "AERO_VIRTIO_SELFTEST|CONFIG|"
        $matches = [regex]::Matches($tail, [regex]::Escape($prefix) + "[^`r`n]*")
        if ($matches.Count -gt 0) {
          $line = $matches[$matches.Count - 1].Value
          if ($line -match "(?:^|\|)expect_blk_msi=(0|1)(?:\||$)") {
            $configExpectBlkMsi = $Matches[1]
            $sawConfigExpectBlkMsi = $true
            if ($configExpectBlkMsi -ne "1") {
              return @{ Result = "EXPECT_BLK_MSI_NOT_SET"; Tail = $tail }
            }
          }
        }
      }
      if (($null -ne $UdpSocket) -and (-not $sawConfigUdpPort) -and $tail.Contains("AERO_VIRTIO_SELFTEST|CONFIG|")) {
        # If a host UDP echo server is running, ensure it matches the guest selftest's configured udp_port.
        $prefix = "AERO_VIRTIO_SELFTEST|CONFIG|"
        $matches = [regex]::Matches($tail, [regex]::Escape($prefix) + "[^`r`n]*")
        if ($matches.Count -gt 0) {
          $line = $matches[$matches.Count - 1].Value
          if ($line -match "(?:^|\|)udp_port=([0-9]+)(?:\||$)") {
            $configUdpPort = [int]$Matches[1]
            $sawConfigUdpPort = $true
            if (($null -ne $udpHostPort) -and ($configUdpPort -ne $udpHostPort)) {
              return @{ Result = "UDP_PORT_MISMATCH"; Tail = $tail; GuestPort = $configUdpPort; HostPort = $udpHostPort }
            }
          }
        }
      }

      if (-not $sawVirtioBlkPass -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-blk\|PASS") {
        $sawVirtioBlkPass = $true
        if ($null -eq $virtioBlkMarkerTime) { $virtioBlkMarkerTime = [DateTime]::UtcNow }
      }
      if (-not $sawVirtioBlkFail -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-blk\|FAIL") {
        $sawVirtioBlkFail = $true
        if ($null -eq $virtioBlkMarkerTime) { $virtioBlkMarkerTime = [DateTime]::UtcNow }
      }

      if (-not $sawVirtioBlkResizeReady -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-blk-resize\|READY") {
        $matches = [regex]::Matches($tail, "AERO_VIRTIO_SELFTEST\|TEST\|virtio-blk-resize\|READY\|[^\r\n]+")
        if ($matches.Count -gt 0) {
          $line = $matches[$matches.Count - 1].Value
          if ($line -match "old_bytes=([0-9]+)") {
            $blkResizeOldBytes = [UInt64]$Matches[1]
            try {
              $deltaBytes = [UInt64]$VirtioBlkResizeDeltaMiB * 1024 * 1024
              $blkResizeNewBytes = Compute-AeroVirtioBlkResizeNewBytes -OldBytes $blkResizeOldBytes -DeltaBytes $deltaBytes
            } catch {
              # Leave new_bytes unset; QMP resize attempt will fail and report diagnostics.
              $blkResizeNewBytes = $null
            }
          }
        }
        $sawVirtioBlkResizeReady = $true
      }
      if (-not $sawVirtioBlkResizePass -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-blk-resize\|PASS") {
        $sawVirtioBlkResizePass = $true
      }
      if (-not $sawVirtioBlkResizeFail -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-blk-resize\|FAIL") {
        $sawVirtioBlkResizeFail = $true
      }
      if (-not $sawVirtioBlkResizeSkip -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-blk-resize\|SKIP") {
        $sawVirtioBlkResizeSkip = $true
      }
      if (-not $sawVirtioBlkResetPass -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-blk-reset\|PASS") {
        $sawVirtioBlkResetPass = $true
      }
      if (-not $sawVirtioBlkResetSkip -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-blk-reset\|SKIP") {
        $sawVirtioBlkResetSkip = $true
      }
      if (-not $sawVirtioBlkResetFail -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-blk-reset\|FAIL") {
        $sawVirtioBlkResetFail = $true
      }
      if ($RequireVirtioBlkResetPass) {
        if ($sawVirtioBlkResetSkip) { return @{ Result = "VIRTIO_BLK_RESET_SKIPPED"; Tail = $tail } }
        if ($sawVirtioBlkResetFail) { return @{ Result = "VIRTIO_BLK_RESET_FAILED"; Tail = $tail } }
      }
      if (-not $sawVirtioInputPass -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input\|PASS") {
        $sawVirtioInputPass = $true
        if ($null -eq $virtioInputMarkerTime) { $virtioInputMarkerTime = [DateTime]::UtcNow }
      }
      if (-not $sawVirtioInputFail -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input\|FAIL") {
        $sawVirtioInputFail = $true
        if ($null -eq $virtioInputMarkerTime) { $virtioInputMarkerTime = [DateTime]::UtcNow }
      }
      if (-not $sawVirtioInputBindPass -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-bind\|PASS") {
        $sawVirtioInputBindPass = $true
      }
      if (-not $sawVirtioInputBindFail -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-bind\|FAIL") {
        $sawVirtioInputBindFail = $true
      }
      if (-not $sawVirtioInputBindingPass -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-binding\|PASS") {
        $sawVirtioInputBindingPass = $true
      }
      if (-not $sawVirtioInputBindingFail -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-binding\|FAIL") {
        $sawVirtioInputBindingFail = $true
      }
      if (-not $sawVirtioInputBindingSkip -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-binding\|SKIP") {
        $sawVirtioInputBindingSkip = $true
      }
      if (-not $sawVirtioInputLedsPass -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-leds\|PASS") {
        $sawVirtioInputLedsPass = $true
      }
      if (-not $sawVirtioInputLedsFail -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-leds\|FAIL") {
        $sawVirtioInputLedsFail = $true
      }
      if (-not $sawVirtioInputLedsSkip -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-leds\|SKIP") {
        $sawVirtioInputLedsSkip = $true
      }
      if (-not $sawVirtioInputEventsReady -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-events\|READY") {
        $sawVirtioInputEventsReady = $true
      }
      if (-not $sawVirtioInputEventsPass -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-events\|PASS") {
        $sawVirtioInputEventsPass = $true
      }
      if (-not $sawVirtioInputEventsFail -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-events\|FAIL") {
        $sawVirtioInputEventsFail = $true
      }
      if (-not $sawVirtioInputEventsSkip -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-events\|SKIP") {
        $sawVirtioInputEventsSkip = $true
      }
      if (-not $sawVirtioInputMediaKeysReady -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-media-keys\|READY") {
        $sawVirtioInputMediaKeysReady = $true
      }
      if (-not $sawVirtioInputMediaKeysPass -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-media-keys\|PASS") {
        $sawVirtioInputMediaKeysPass = $true
      }
      if (-not $sawVirtioInputMediaKeysFail -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-media-keys\|FAIL") {
        $sawVirtioInputMediaKeysFail = $true
      }
      if (-not $sawVirtioInputMediaKeysSkip -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-media-keys\|SKIP") {
        $sawVirtioInputMediaKeysSkip = $true
      }
      if (-not $sawVirtioInputLedPass -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-led\|PASS") {
        $sawVirtioInputLedPass = $true
      }
      if (-not $sawVirtioInputLedFail -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-led\|FAIL") {
        $sawVirtioInputLedFail = $true
      }
      if (-not $sawVirtioInputLedSkip -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-led\|SKIP") {
        $sawVirtioInputLedSkip = $true
      }
      if (-not $sawVirtioInputWheelPass -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-wheel\|PASS") {
        $sawVirtioInputWheelPass = $true
      }
      if (-not $sawVirtioInputWheelFail -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-wheel\|FAIL") {
        $sawVirtioInputWheelFail = $true
      }
      if (-not $sawVirtioInputWheelSkip -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-wheel\|SKIP") {
        $sawVirtioInputWheelSkip = $true
      }
      if (-not $sawVirtioInputEventsModifiersPass -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-events-modifiers\|PASS") {
        $sawVirtioInputEventsModifiersPass = $true
      }
      if (-not $sawVirtioInputEventsModifiersFail -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-events-modifiers\|FAIL") {
        $sawVirtioInputEventsModifiersFail = $true
      }
      if (-not $sawVirtioInputEventsModifiersSkip -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-events-modifiers\|SKIP") {
        $sawVirtioInputEventsModifiersSkip = $true
      }
      if (-not $sawVirtioInputEventsButtonsPass -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-events-buttons\|PASS") {
        $sawVirtioInputEventsButtonsPass = $true
      }
      if (-not $sawVirtioInputEventsButtonsFail -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-events-buttons\|FAIL") {
        $sawVirtioInputEventsButtonsFail = $true
      }
      if (-not $sawVirtioInputEventsButtonsSkip -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-events-buttons\|SKIP") {
        $sawVirtioInputEventsButtonsSkip = $true
      }
      if (-not $sawVirtioInputEventsWheelPass -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-events-wheel\|PASS") {
        $sawVirtioInputEventsWheelPass = $true
      }
      if (-not $sawVirtioInputEventsWheelFail -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-events-wheel\|FAIL") {
        $sawVirtioInputEventsWheelFail = $true
      }
      if (-not $sawVirtioInputEventsWheelSkip -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-events-wheel\|SKIP") {
        $sawVirtioInputEventsWheelSkip = $true
      }

      # If input LED/statusq testing is required, fail fast when the guest reports SKIP/FAIL for virtio-input-leds.
      if ($RequireVirtioInputLedsPass) {
        if ($sawVirtioInputLedsSkip) { return @{ Result = "VIRTIO_INPUT_LEDS_SKIPPED"; Tail = $tail } }
        if ($sawVirtioInputLedsFail) { return @{ Result = "VIRTIO_INPUT_LEDS_FAILED"; Tail = $tail } }
      }

      # If input events are required, fail fast when the guest reports SKIP/FAIL for virtio-input-events.
      # This saves time when the guest image was provisioned without `--test-input-events`, or when the
      # end-to-end input report delivery path is broken.
      if ($RequireVirtioInputEventsPass) {
        if ($sawVirtioInputEventsSkip) { return @{ Result = "VIRTIO_INPUT_EVENTS_SKIPPED"; Tail = $tail } }
        if ($sawVirtioInputEventsFail) { return @{ Result = "VIRTIO_INPUT_EVENTS_FAILED"; Tail = $tail } }
        if ($RequireVirtioInputEventsExtendedPass) {
          if ($sawVirtioInputEventsModifiersSkip -or $sawVirtioInputEventsButtonsSkip -or $sawVirtioInputEventsWheelSkip) {
            return @{ Result = "VIRTIO_INPUT_EVENTS_EXTENDED_SKIPPED"; Tail = $tail }
          }
          if ($sawVirtioInputEventsModifiersFail -or $sawVirtioInputEventsButtonsFail -or $sawVirtioInputEventsWheelFail) {
            return @{ Result = "VIRTIO_INPUT_EVENTS_EXTENDED_FAILED"; Tail = $tail }
          }
        }
      }
      if ($RequireVirtioBlkResizePass) {
        if ($sawVirtioBlkResizeSkip) { return @{ Result = "VIRTIO_BLK_RESIZE_SKIPPED"; Tail = $tail } }
        if ($sawVirtioBlkResizeFail) { return @{ Result = "VIRTIO_BLK_RESIZE_FAILED"; Tail = $tail } }
      }
      if ($RequireVirtioInputMediaKeysPass) {
        if ($sawVirtioInputMediaKeysSkip) { return @{ Result = "VIRTIO_INPUT_MEDIA_KEYS_SKIPPED"; Tail = $tail } }
        if ($sawVirtioInputMediaKeysFail) { return @{ Result = "VIRTIO_INPUT_MEDIA_KEYS_FAILED"; Tail = $tail } }
      }
      if ($RequireVirtioInputLedPass) {
        if ($sawVirtioInputLedSkip) { return @{ Result = "VIRTIO_INPUT_LED_SKIPPED"; Tail = $tail } }
        if ($sawVirtioInputLedFail) { return @{ Result = "VIRTIO_INPUT_LED_FAILED"; Tail = $tail } }
      }
      if ($RequireVirtioInputWheelPass) {
        if ($sawVirtioInputWheelSkip) { return @{ Result = "VIRTIO_INPUT_WHEEL_SKIPPED"; Tail = $tail } }
        if ($sawVirtioInputWheelFail) { return @{ Result = "VIRTIO_INPUT_WHEEL_FAILED"; Tail = $tail } }
      }

      if ($RequireVirtioBlkResetPass) {
        if ($sawVirtioBlkResetSkip) { return @{ Result = "VIRTIO_BLK_RESET_SKIPPED"; Tail = $tail } }
        if ($sawVirtioBlkResetFail) { return @{ Result = "VIRTIO_BLK_RESET_FAILED"; Tail = $tail } }
      }

      if (-not $sawVirtioInputTabletEventsReady -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-tablet-events\|READY") {
        $sawVirtioInputTabletEventsReady = $true
      }
      if (-not $sawVirtioInputTabletEventsPass -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-tablet-events\|PASS") {
        $sawVirtioInputTabletEventsPass = $true
      }
      if (-not $sawVirtioInputTabletEventsFail -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-tablet-events\|FAIL") {
        $sawVirtioInputTabletEventsFail = $true
      }
      if (-not $sawVirtioInputTabletEventsSkip -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-tablet-events\|SKIP") {
        $sawVirtioInputTabletEventsSkip = $true
      }

      # If tablet events are required, fail fast when the guest reports SKIP/FAIL for virtio-input-tablet-events.
      if ($RequireVirtioInputTabletEventsPass) {
        if ($sawVirtioInputTabletEventsSkip) { return @{ Result = "VIRTIO_INPUT_TABLET_EVENTS_SKIPPED"; Tail = $tail } }
        if ($sawVirtioInputTabletEventsFail) { return @{ Result = "VIRTIO_INPUT_TABLET_EVENTS_FAILED"; Tail = $tail } }
      }
      if (-not $sawVirtioSndPass -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-snd\|PASS") {
        $sawVirtioSndPass = $true
      }
      if (-not $sawVirtioSndSkip -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-snd\|SKIP") {
        $sawVirtioSndSkip = $true
      }
      if (-not $sawVirtioSndFail -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-snd\|FAIL") {
        $sawVirtioSndFail = $true
      }
      if (-not $sawVirtioSndCapturePass -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-snd-capture\|PASS") {
        $sawVirtioSndCapturePass = $true
      }
      if (-not $sawVirtioSndCaptureSkip -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-snd-capture\|SKIP") {
        $sawVirtioSndCaptureSkip = $true
      }
      if (-not $sawVirtioSndCaptureFail -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-snd-capture\|FAIL") {
        $sawVirtioSndCaptureFail = $true
      }
      if (-not $sawVirtioSndDuplexPass -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-snd-duplex\|PASS") {
        $sawVirtioSndDuplexPass = $true
      }
      if (-not $sawVirtioSndDuplexSkip -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-snd-duplex\|SKIP") {
        $sawVirtioSndDuplexSkip = $true
      }
      if (-not $sawVirtioSndDuplexFail -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-snd-duplex\|FAIL") {
        $sawVirtioSndDuplexFail = $true
      }
      if (-not $sawVirtioSndBufferLimitsPass -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-snd-buffer-limits\|PASS") {
        $sawVirtioSndBufferLimitsPass = $true
      }
      if (-not $sawVirtioSndBufferLimitsSkip -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-snd-buffer-limits\|SKIP") {
        $sawVirtioSndBufferLimitsSkip = $true
      }
      if (-not $sawVirtioSndBufferLimitsFail -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-snd-buffer-limits\|FAIL") {
        $sawVirtioSndBufferLimitsFail = $true
      }
      if (-not $sawVirtioNetPass -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-net\|PASS") {
        $sawVirtioNetPass = $true
        if ($null -eq $virtioNetMarkerTime) { $virtioNetMarkerTime = [DateTime]::UtcNow }
      }
      if (-not $sawVirtioNetFail -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-net\|FAIL") {
        $sawVirtioNetFail = $true
        if ($null -eq $virtioNetMarkerTime) { $virtioNetMarkerTime = [DateTime]::UtcNow }
      }
      if (-not $sawVirtioNetUdpPass -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-net-udp\|PASS") {
        $sawVirtioNetUdpPass = $true
      }
      if (-not $sawVirtioNetUdpFail -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-net-udp\|FAIL") {
        $sawVirtioNetUdpFail = $true
      }
      if (-not $sawVirtioNetUdpSkip -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-net-udp\|SKIP") {
        $sawVirtioNetUdpSkip = $true
      }

      if (-not $sawVirtioNetMsixModeMsix -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-net-msix\|PASS\|mode=msix") {
        $sawVirtioNetMsixModeMsix = $true
      }
      if (-not $sawVirtioNetLinkFlapReady -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-net-link-flap\|READY") {
        $sawVirtioNetLinkFlapReady = $true
      }
      if (-not $sawVirtioNetLinkFlapPass -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-net-link-flap\|PASS") {
        $sawVirtioNetLinkFlapPass = $true
      }
      if (-not $sawVirtioNetLinkFlapFail -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-net-link-flap\|FAIL") {
        $sawVirtioNetLinkFlapFail = $true
      }
      if (-not $sawVirtioNetLinkFlapSkip -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-net-link-flap\|SKIP") {
        $sawVirtioNetLinkFlapSkip = $true
      }

      if ($RequireVirtioSndBufferLimitsPass) {
        if ($sawVirtioSndBufferLimitsSkip) { return @{ Result = "VIRTIO_SND_BUFFER_LIMITS_SKIPPED"; Tail = $tail } }
        if ($sawVirtioSndBufferLimitsFail) { return @{ Result = "VIRTIO_SND_BUFFER_LIMITS_FAILED"; Tail = $tail } }
      }

      # If net link flap is required, fail fast when the guest reports SKIP/FAIL.
      if ($RequireVirtioNetLinkFlapPass) {
        if ($sawVirtioNetLinkFlapSkip) { return @{ Result = "VIRTIO_NET_LINK_FLAP_SKIPPED"; Tail = $tail } }
        if ($sawVirtioNetLinkFlapFail) { return @{ Result = "VIRTIO_NET_LINK_FLAP_FAILED"; Tail = $tail } }
      }

      if ($tail -match "AERO_VIRTIO_SELFTEST\|RESULT\|PASS") {
        if ($RequireExpectBlkMsi -and ((-not $sawConfigExpectBlkMsi) -or $configExpectBlkMsi -ne "1")) {
          return @{ Result = "EXPECT_BLK_MSI_NOT_SET"; Tail = $tail }
        }
        if ($RequireVirtioInputBindingPass) {
          if ((-not $sawVirtioInputBindingPass) -and (-not $sawVirtioInputBindingFail) -and (-not $sawVirtioInputBindingSkip)) {
            # Tail truncation fallback: if we didn't observe the marker in the rolling tail scan, try
            # extracting the last marker line from the full serial log (when available). This avoids
            # false negatives when a large read chunk truncates earlier marker output from `$tail`.
            $prefix = "AERO_VIRTIO_SELFTEST|TEST|virtio-input-binding|"
            $line = Try-ExtractLastAeroMarkerLine -Tail $tail -Prefix $prefix -SerialLogPath $SerialLogPath
            if ($null -ne $line) {
              if ($line -match "\|PASS(\||$)") { $sawVirtioInputBindingPass = $true }
              if ($line -match "\|FAIL(\||$)") { $sawVirtioInputBindingFail = $true }
              if ($line -match "\|SKIP(\||$)") { $sawVirtioInputBindingSkip = $true }
            }
          }
          if ($sawVirtioInputBindingFail) {
            return @{ Result = "VIRTIO_INPUT_BINDING_FAILED"; Tail = $tail }
          }
          if (-not $sawVirtioInputBindingPass) {
            if ($sawVirtioInputBindingSkip) { return @{ Result = "VIRTIO_INPUT_BINDING_SKIPPED"; Tail = $tail } }
            return @{ Result = "MISSING_VIRTIO_INPUT_BINDING"; Tail = $tail }
          }
        }
        if ($RequireVirtioNetLinkFlapPass) {
          if ($sawVirtioNetLinkFlapFail) {
            return @{ Result = "VIRTIO_NET_LINK_FLAP_FAILED"; Tail = $tail }
          }
          if (-not $sawVirtioNetLinkFlapPass) {
            if ($sawVirtioNetLinkFlapSkip) { return @{ Result = "VIRTIO_NET_LINK_FLAP_SKIPPED"; Tail = $tail } }
            return @{ Result = "MISSING_VIRTIO_NET_LINK_FLAP"; Tail = $tail }
          }
        }
        if ($RequirePerTestMarkers) {
          # Require per-test markers so older selftest binaries cannot accidentally pass the host harness.
          if ($sawVirtioBlkFail) {
            return @{ Result = "VIRTIO_BLK_FAILED"; Tail = $tail }
          }
          if (-not $sawVirtioBlkPass) {
            return @{ Result = "MISSING_VIRTIO_BLK"; Tail = $tail }
          }
          if ($RequireVirtioBlkResizePass) {
            if ($sawVirtioBlkResizeFail) { return @{ Result = "VIRTIO_BLK_RESIZE_FAILED"; Tail = $tail } }
            if (-not $sawVirtioBlkResizePass) {
              if ($sawVirtioBlkResizeSkip) { return @{ Result = "VIRTIO_BLK_RESIZE_SKIPPED"; Tail = $tail } }
              return @{ Result = "MISSING_VIRTIO_BLK_RESIZE"; Tail = $tail }
            }
          }
          if ($sawVirtioInputFail) {
            return @{ Result = "VIRTIO_INPUT_FAILED"; Tail = $tail }
          }
          if (-not $sawVirtioInputPass) {
            return @{ Result = "MISSING_VIRTIO_INPUT"; Tail = $tail }
          }
          if ($sawVirtioInputBindFail) {
            return @{ Result = "VIRTIO_INPUT_BIND_FAILED"; Tail = $tail }
          }
          if (-not $sawVirtioInputBindPass) {
            return @{ Result = "MISSING_VIRTIO_INPUT_BIND"; Tail = $tail }
          }

          # Also ensure the virtio-snd markers are present (playback + capture), so older selftest binaries
          # that predate virtio-snd testing cannot accidentally pass.
          if ($sawVirtioSndFail) {
            return @{ Result = "VIRTIO_SND_FAILED"; Tail = $tail }
          }
          if (-not ($sawVirtioSndPass -or $sawVirtioSndSkip)) {
            return @{ Result = "MISSING_VIRTIO_SND"; Tail = $tail }
          }
          if ($RequireVirtioSndPass -and (-not $sawVirtioSndPass)) {
            return @{ Result = "VIRTIO_SND_SKIPPED"; Tail = $tail }
          }

          if ($sawVirtioSndCaptureFail) {
            return @{ Result = "VIRTIO_SND_CAPTURE_FAILED"; Tail = $tail }
          }
          if (-not ($sawVirtioSndCapturePass -or $sawVirtioSndCaptureSkip)) {
            return @{ Result = "MISSING_VIRTIO_SND_CAPTURE"; Tail = $tail }
          }
          if ($RequireVirtioSndPass -and (-not $sawVirtioSndCapturePass)) {
            return @{ Result = "VIRTIO_SND_CAPTURE_SKIPPED"; Tail = $tail }
          }

          if ($sawVirtioSndDuplexFail) {
            return @{ Result = "VIRTIO_SND_DUPLEX_FAILED"; Tail = $tail }
          }
          if (-not ($sawVirtioSndDuplexPass -or $sawVirtioSndDuplexSkip)) {
            return @{ Result = "MISSING_VIRTIO_SND_DUPLEX"; Tail = $tail }
          }
          if ($RequireVirtioSndPass -and (-not $sawVirtioSndDuplexPass)) {
            return @{ Result = "VIRTIO_SND_DUPLEX_SKIPPED"; Tail = $tail }
          }

          if ($RequireVirtioSndBufferLimitsPass) {
            if ($sawVirtioSndBufferLimitsFail) {
              return @{ Result = "VIRTIO_SND_BUFFER_LIMITS_FAILED"; Tail = $tail }
            }
            if (-not $sawVirtioSndBufferLimitsPass) {
              if ($sawVirtioSndBufferLimitsSkip) {
                return @{ Result = "VIRTIO_SND_BUFFER_LIMITS_SKIPPED"; Tail = $tail }
              }
              return @{ Result = "MISSING_VIRTIO_SND_BUFFER_LIMITS"; Tail = $tail }
            }
          }

          if ($sawVirtioNetFail) {
            return @{ Result = "VIRTIO_NET_FAILED"; Tail = $tail }
          }
          if (-not $sawVirtioNetPass) {
            return @{ Result = "MISSING_VIRTIO_NET"; Tail = $tail }
          }
          if ($RequireVirtioNetUdpPass) {
            if ($sawVirtioNetUdpFail) {
              return @{ Result = "VIRTIO_NET_UDP_FAILED"; Tail = $tail }
            }
            if (-not $sawVirtioNetUdpPass) {
              if ($sawVirtioNetUdpSkip) { return @{ Result = "VIRTIO_NET_UDP_SKIPPED"; Tail = $tail } }
              return @{ Result = "MISSING_VIRTIO_NET_UDP"; Tail = $tail }
            }
          }

          if ($RequireVirtioNetMsix -and (-not $sawVirtioNetMsixModeMsix)) {
            $chk = Test-AeroVirtioNetMsixMarker -Tail $tail -SerialLogPath $SerialLogPath
            if (-not $chk.Ok) {
              return @{ Result = "VIRTIO_NET_MSIX_REQUIRED"; Tail = $tail; MsixReason = $chk.Reason }
            }
          }

          if ($RequireVirtioNetLinkFlapPass) {
            if ($sawVirtioNetLinkFlapFail) { return @{ Result = "VIRTIO_NET_LINK_FLAP_FAILED"; Tail = $tail } }
            if (-not $sawVirtioNetLinkFlapPass) {
              if ($sawVirtioNetLinkFlapSkip) { return @{ Result = "VIRTIO_NET_LINK_FLAP_SKIPPED"; Tail = $tail } }
              return @{ Result = "MISSING_VIRTIO_NET_LINK_FLAP"; Tail = $tail }
            }
          }

          if ($RequireVirtioInputLedsPass) {
            if ($sawVirtioInputLedsFail) {
              return @{ Result = "VIRTIO_INPUT_LEDS_FAILED"; Tail = $tail }
            }
            if (-not $sawVirtioInputLedsPass) {
              if ($sawVirtioInputLedsSkip) {
                return @{ Result = "VIRTIO_INPUT_LEDS_SKIPPED"; Tail = $tail }
              }
              return @{ Result = "MISSING_VIRTIO_INPUT_LEDS"; Tail = $tail }
            }
          }

          if ($RequireVirtioInputEventsPass) {
            if ($sawVirtioInputEventsFail) {
              return @{ Result = "VIRTIO_INPUT_EVENTS_FAILED"; Tail = $tail }
            }
            if (-not $sawVirtioInputEventsPass) {
              if ($sawVirtioInputEventsSkip) {
                return @{ Result = "VIRTIO_INPUT_EVENTS_SKIPPED"; Tail = $tail }
              }
              return @{ Result = "MISSING_VIRTIO_INPUT_EVENTS"; Tail = $tail }
            }
            if ($RequireVirtioInputEventsExtendedPass) {
              if ($sawVirtioInputEventsModifiersFail -or $sawVirtioInputEventsButtonsFail -or $sawVirtioInputEventsWheelFail) {
                return @{ Result = "VIRTIO_INPUT_EVENTS_EXTENDED_FAILED"; Tail = $tail }
              }
              if ($sawVirtioInputEventsModifiersSkip -or $sawVirtioInputEventsButtonsSkip -or $sawVirtioInputEventsWheelSkip) {
                return @{ Result = "VIRTIO_INPUT_EVENTS_EXTENDED_SKIPPED"; Tail = $tail }
              }
              if (-not ($sawVirtioInputEventsModifiersPass -and $sawVirtioInputEventsButtonsPass -and $sawVirtioInputEventsWheelPass)) {
                return @{ Result = "MISSING_VIRTIO_INPUT_EVENTS_EXTENDED"; Tail = $tail }
              }
            }
          }
          if ($RequireVirtioInputMediaKeysPass) {
            if ($sawVirtioInputMediaKeysFail) { return @{ Result = "VIRTIO_INPUT_MEDIA_KEYS_FAILED"; Tail = $tail } }
            if (-not $sawVirtioInputMediaKeysPass) {
              if ($sawVirtioInputMediaKeysSkip) { return @{ Result = "VIRTIO_INPUT_MEDIA_KEYS_SKIPPED"; Tail = $tail } }
              return @{ Result = "MISSING_VIRTIO_INPUT_MEDIA_KEYS"; Tail = $tail }
            }
          }
          if ($RequireVirtioInputTabletEventsPass) {
            if ($sawVirtioInputTabletEventsFail) {
              return @{ Result = "VIRTIO_INPUT_TABLET_EVENTS_FAILED"; Tail = $tail }
            }
            if (-not $sawVirtioInputTabletEventsPass) {
              if ($sawVirtioInputTabletEventsSkip) {
                return @{ Result = "VIRTIO_INPUT_TABLET_EVENTS_SKIPPED"; Tail = $tail }
              }
              return @{ Result = "MISSING_VIRTIO_INPUT_TABLET_EVENTS"; Tail = $tail }
            }
          }
          if ($RequireVirtioInputWheelPass) {
            if ($sawVirtioInputWheelFail) { return @{ Result = "VIRTIO_INPUT_WHEEL_FAILED"; Tail = $tail } }
            if (-not $sawVirtioInputWheelPass) {
              if ($sawVirtioInputWheelSkip) { return @{ Result = "VIRTIO_INPUT_WHEEL_SKIPPED"; Tail = $tail } }
              return @{ Result = "MISSING_VIRTIO_INPUT_WHEEL"; Tail = $tail }
            }
          }
          if ($RequireVirtioInputLedPass) {
            if ($sawVirtioInputLedFail) { return @{ Result = "VIRTIO_INPUT_LED_FAILED"; Tail = $tail } }
            if (-not $sawVirtioInputLedPass) {
              if ($sawVirtioInputLedSkip) { return @{ Result = "VIRTIO_INPUT_LED_SKIPPED"; Tail = $tail } }
              return @{ Result = "MISSING_VIRTIO_INPUT_LED"; Tail = $tail }
            }
          }

          if ($RequireVirtioBlkResetPass) {
            if ($sawVirtioBlkResetFail) {
              return @{ Result = "VIRTIO_BLK_RESET_FAILED"; Tail = $tail }
            }
            if (-not $sawVirtioBlkResetPass) {
              if ($sawVirtioBlkResetSkip) {
                return @{ Result = "VIRTIO_BLK_RESET_SKIPPED"; Tail = $tail }
              }
              return @{ Result = "MISSING_VIRTIO_BLK_RESET"; Tail = $tail }
            }
          }

          $msixCheck = Test-VirtioInputMsixRequirement -Tail $tail -SerialLogPath $SerialLogPath
          if ($null -ne $msixCheck) { return $msixCheck }

          if ($RequireNetCsumOffload -or $RequireNetUdpCsumOffload) {
            $csum = Get-AeroVirtioNetOffloadCsumStatsFromTail -Tail $tail -SerialLogPath $SerialLogPath
            if ($null -eq $csum) {
              if ($RequireNetCsumOffload) { return @{ Result = "MISSING_VIRTIO_NET_CSUM_OFFLOAD"; Tail = $tail } }
              return @{ Result = "MISSING_VIRTIO_NET_UDP_CSUM_OFFLOAD"; Tail = $tail }
            }
            if ($csum.Status -ne "PASS") {
              if ($RequireNetCsumOffload) { return @{ Result = "VIRTIO_NET_CSUM_OFFLOAD_FAILED"; Tail = $tail } }
              return @{ Result = "VIRTIO_NET_UDP_CSUM_OFFLOAD_FAILED"; Tail = $tail }
            }
 
            if ($RequireNetCsumOffload) {
              if ($null -eq $csum.TxCsum) { return @{ Result = "VIRTIO_NET_CSUM_OFFLOAD_MISSING_FIELDS"; Tail = $tail } }
              if ($csum.TxCsum -le 0) { return @{ Result = "VIRTIO_NET_CSUM_OFFLOAD_ZERO"; Tail = $tail } }
            }
 
            if ($RequireNetUdpCsumOffload) {
              $txUdp = $csum.TxUdp
              $txUdp4 = $csum.TxUdp4
              $txUdp6 = $csum.TxUdp6
              if ($null -eq $txUdp) {
                if (($null -eq $txUdp4) -and ($null -eq $txUdp6)) {
                  return @{ Result = "VIRTIO_NET_UDP_CSUM_OFFLOAD_MISSING_FIELDS"; Tail = $tail }
                }
                $txUdp = [UInt64]0
                if ($null -ne $txUdp4) { $txUdp += $txUdp4 }
                if ($null -ne $txUdp6) { $txUdp += $txUdp6 }
              }
 
              if ($txUdp -le 0) { return @{ Result = "VIRTIO_NET_UDP_CSUM_OFFLOAD_ZERO"; Tail = $tail } }
            }
          }
          return @{ Result = "PASS"; Tail = $tail }
        }

        if ($RequireVirtioSndPass) {
          if ($sawVirtioSndFail) {
            return @{ Result = "VIRTIO_SND_FAILED"; Tail = $tail }
          }
          if ($sawVirtioSndPass) {
            if ($sawVirtioSndCaptureFail) { return @{ Result = "VIRTIO_SND_CAPTURE_FAILED"; Tail = $tail } }
            if ($sawVirtioSndDuplexFail) { return @{ Result = "VIRTIO_SND_DUPLEX_FAILED"; Tail = $tail } }
            if ($sawVirtioSndCapturePass) {
              if ($sawVirtioSndDuplexPass) {
                if ($RequireVirtioSndBufferLimitsPass) {
                  if ($sawVirtioSndBufferLimitsFail) { return @{ Result = "VIRTIO_SND_BUFFER_LIMITS_FAILED"; Tail = $tail } }
                  if (-not $sawVirtioSndBufferLimitsPass) {
                    if ($sawVirtioSndBufferLimitsSkip) { return @{ Result = "VIRTIO_SND_BUFFER_LIMITS_SKIPPED"; Tail = $tail } }
                    return @{ Result = "MISSING_VIRTIO_SND_BUFFER_LIMITS"; Tail = $tail }
                  }
                }
                if ($RequireVirtioInputEventsPass) {
                  if ($sawVirtioInputEventsFail) { return @{ Result = "VIRTIO_INPUT_EVENTS_FAILED"; Tail = $tail } }
                  if (-not $sawVirtioInputEventsPass) {
                    if ($sawVirtioInputEventsSkip) { return @{ Result = "VIRTIO_INPUT_EVENTS_SKIPPED"; Tail = $tail } }
                    return @{ Result = "MISSING_VIRTIO_INPUT_EVENTS"; Tail = $tail }
                  }
                  if ($RequireVirtioInputEventsExtendedPass) {
                    if ($sawVirtioInputEventsModifiersFail -or $sawVirtioInputEventsButtonsFail -or $sawVirtioInputEventsWheelFail) {
                      return @{ Result = "VIRTIO_INPUT_EVENTS_EXTENDED_FAILED"; Tail = $tail }
                    }
                    if ($sawVirtioInputEventsModifiersSkip -or $sawVirtioInputEventsButtonsSkip -or $sawVirtioInputEventsWheelSkip) {
                      return @{ Result = "VIRTIO_INPUT_EVENTS_EXTENDED_SKIPPED"; Tail = $tail }
                    }
                    if (-not ($sawVirtioInputEventsModifiersPass -and $sawVirtioInputEventsButtonsPass -and $sawVirtioInputEventsWheelPass)) {
                      return @{ Result = "MISSING_VIRTIO_INPUT_EVENTS_EXTENDED"; Tail = $tail }
                    }
                  }
                }
                if ($RequireVirtioInputMediaKeysPass) {
                  if ($sawVirtioInputMediaKeysFail) { return @{ Result = "VIRTIO_INPUT_MEDIA_KEYS_FAILED"; Tail = $tail } }
                  if (-not $sawVirtioInputMediaKeysPass) {
                    if ($sawVirtioInputMediaKeysSkip) { return @{ Result = "VIRTIO_INPUT_MEDIA_KEYS_SKIPPED"; Tail = $tail } }
                    return @{ Result = "MISSING_VIRTIO_INPUT_MEDIA_KEYS"; Tail = $tail }
                  }
                }
                if ($RequireVirtioInputTabletEventsPass) {
                  if ($sawVirtioInputTabletEventsFail) { return @{ Result = "VIRTIO_INPUT_TABLET_EVENTS_FAILED"; Tail = $tail } }
                  if (-not $sawVirtioInputTabletEventsPass) {
                    if ($sawVirtioInputTabletEventsSkip) { return @{ Result = "VIRTIO_INPUT_TABLET_EVENTS_SKIPPED"; Tail = $tail } }
                    return @{ Result = "MISSING_VIRTIO_INPUT_TABLET_EVENTS"; Tail = $tail }
                  }
                }
                if ($RequireVirtioInputWheelPass) {
                  if ($sawVirtioInputWheelFail) { return @{ Result = "VIRTIO_INPUT_WHEEL_FAILED"; Tail = $tail } }
                  if (-not $sawVirtioInputWheelPass) {
                    if ($sawVirtioInputWheelSkip) { return @{ Result = "VIRTIO_INPUT_WHEEL_SKIPPED"; Tail = $tail } }
                    return @{ Result = "MISSING_VIRTIO_INPUT_WHEEL"; Tail = $tail }
                  }
                }
                if ($RequireVirtioInputLedPass) {
                  if ($sawVirtioInputLedFail) { return @{ Result = "VIRTIO_INPUT_LED_FAILED"; Tail = $tail } }
                  if (-not $sawVirtioInputLedPass) {
                    if ($sawVirtioInputLedSkip) { return @{ Result = "VIRTIO_INPUT_LED_SKIPPED"; Tail = $tail } }
                    return @{ Result = "MISSING_VIRTIO_INPUT_LED"; Tail = $tail }
                  }
                }
                if ($RequireVirtioBlkResetPass) {
                  if ($sawVirtioBlkResetFail) { return @{ Result = "VIRTIO_BLK_RESET_FAILED"; Tail = $tail } }
                  if (-not $sawVirtioBlkResetPass) {
                    if ($sawVirtioBlkResetSkip) { return @{ Result = "VIRTIO_BLK_RESET_SKIPPED"; Tail = $tail } }
                    return @{ Result = "MISSING_VIRTIO_BLK_RESET"; Tail = $tail }
                  }
                }
                if ($RequireVirtioNetMsix -and (-not $sawVirtioNetMsixModeMsix)) {
                  $chk = Test-AeroVirtioNetMsixMarker -Tail $tail -SerialLogPath $SerialLogPath
                  if (-not $chk.Ok) {
                    return @{ Result = "VIRTIO_NET_MSIX_REQUIRED"; Tail = $tail; MsixReason = $chk.Reason }
                  }
                }
                if ($RequireVirtioNetLinkFlapPass) {
                  if ($sawVirtioNetLinkFlapFail) { return @{ Result = "VIRTIO_NET_LINK_FLAP_FAILED"; Tail = $tail } }
                  if (-not $sawVirtioNetLinkFlapPass) {
                    if ($sawVirtioNetLinkFlapSkip) { return @{ Result = "VIRTIO_NET_LINK_FLAP_SKIPPED"; Tail = $tail } }
                    return @{ Result = "MISSING_VIRTIO_NET_LINK_FLAP"; Tail = $tail }
                  }
                }
                $msixCheck = Test-VirtioInputMsixRequirement -Tail $tail -SerialLogPath $SerialLogPath
                if ($null -ne $msixCheck) { return $msixCheck }

                if ($RequireNetCsumOffload -or $RequireNetUdpCsumOffload) {
                  $csum = Get-AeroVirtioNetOffloadCsumStatsFromTail -Tail $tail -SerialLogPath $SerialLogPath
                  if ($null -eq $csum) {
                    if ($RequireNetCsumOffload) { return @{ Result = "MISSING_VIRTIO_NET_CSUM_OFFLOAD"; Tail = $tail } }
                    return @{ Result = "MISSING_VIRTIO_NET_UDP_CSUM_OFFLOAD"; Tail = $tail }
                  }
                  if ($csum.Status -ne "PASS") {
                    if ($RequireNetCsumOffload) { return @{ Result = "VIRTIO_NET_CSUM_OFFLOAD_FAILED"; Tail = $tail } }
                    return @{ Result = "VIRTIO_NET_UDP_CSUM_OFFLOAD_FAILED"; Tail = $tail }
                  }
 
                  if ($RequireNetCsumOffload) {
                    if ($null -eq $csum.TxCsum) { return @{ Result = "VIRTIO_NET_CSUM_OFFLOAD_MISSING_FIELDS"; Tail = $tail } }
                    if ($csum.TxCsum -le 0) { return @{ Result = "VIRTIO_NET_CSUM_OFFLOAD_ZERO"; Tail = $tail } }
                  }
 
                  if ($RequireNetUdpCsumOffload) {
                    $txUdp = $csum.TxUdp
                    $txUdp4 = $csum.TxUdp4
                    $txUdp6 = $csum.TxUdp6
                    if ($null -eq $txUdp) {
                      if (($null -eq $txUdp4) -and ($null -eq $txUdp6)) {
                        return @{ Result = "VIRTIO_NET_UDP_CSUM_OFFLOAD_MISSING_FIELDS"; Tail = $tail }
                      }
                      $txUdp = [UInt64]0
                      if ($null -ne $txUdp4) { $txUdp += $txUdp4 }
                      if ($null -ne $txUdp6) { $txUdp += $txUdp6 }
                    }
 
                    if ($txUdp -le 0) { return @{ Result = "VIRTIO_NET_UDP_CSUM_OFFLOAD_ZERO"; Tail = $tail } }
                  }
                }
                return @{ Result = "PASS"; Tail = $tail }
              }
              if ($sawVirtioSndDuplexSkip) {
                return @{ Result = "VIRTIO_SND_DUPLEX_SKIPPED"; Tail = $tail }
              }
              return @{ Result = "MISSING_VIRTIO_SND_DUPLEX"; Tail = $tail }
            }
            if ($sawVirtioSndCaptureSkip) {
              return @{ Result = "VIRTIO_SND_CAPTURE_SKIPPED"; Tail = $tail }
            }
            return @{ Result = "MISSING_VIRTIO_SND_CAPTURE"; Tail = $tail }
          }
          if ($sawVirtioSndSkip) {
            return @{ Result = "VIRTIO_SND_SKIPPED"; Tail = $tail }
          }
          return @{ Result = "MISSING_VIRTIO_SND"; Tail = $tail }
        }

        if ($RequireVirtioBlkResizePass) {
          if ($sawVirtioBlkResizeFail) { return @{ Result = "VIRTIO_BLK_RESIZE_FAILED"; Tail = $tail } }
          if (-not $sawVirtioBlkResizePass) {
            if ($sawVirtioBlkResizeSkip) { return @{ Result = "VIRTIO_BLK_RESIZE_SKIPPED"; Tail = $tail } }
            return @{ Result = "MISSING_VIRTIO_BLK_RESIZE"; Tail = $tail }
          }
        }

        if ($RequireVirtioInputLedsPass) {
          if ($sawVirtioInputLedsFail) { return @{ Result = "VIRTIO_INPUT_LEDS_FAILED"; Tail = $tail } }
          if (-not $sawVirtioInputLedsPass) {
            if ($sawVirtioInputLedsSkip) { return @{ Result = "VIRTIO_INPUT_LEDS_SKIPPED"; Tail = $tail } }
            return @{ Result = "MISSING_VIRTIO_INPUT_LEDS"; Tail = $tail }
          }
        }

        if ($RequireVirtioInputEventsPass) {
          if ($sawVirtioInputEventsFail) { return @{ Result = "VIRTIO_INPUT_EVENTS_FAILED"; Tail = $tail } }
          if (-not $sawVirtioInputEventsPass) {
            if ($sawVirtioInputEventsSkip) { return @{ Result = "VIRTIO_INPUT_EVENTS_SKIPPED"; Tail = $tail } }
            return @{ Result = "MISSING_VIRTIO_INPUT_EVENTS"; Tail = $tail }
          }
          if ($RequireVirtioInputEventsExtendedPass) {
            if ($sawVirtioInputEventsModifiersFail -or $sawVirtioInputEventsButtonsFail -or $sawVirtioInputEventsWheelFail) {
              return @{ Result = "VIRTIO_INPUT_EVENTS_EXTENDED_FAILED"; Tail = $tail }
            }
            if ($sawVirtioInputEventsModifiersSkip -or $sawVirtioInputEventsButtonsSkip -or $sawVirtioInputEventsWheelSkip) {
              return @{ Result = "VIRTIO_INPUT_EVENTS_EXTENDED_SKIPPED"; Tail = $tail }
            }
            if (-not ($sawVirtioInputEventsModifiersPass -and $sawVirtioInputEventsButtonsPass -and $sawVirtioInputEventsWheelPass)) {
              return @{ Result = "MISSING_VIRTIO_INPUT_EVENTS_EXTENDED"; Tail = $tail }
            }
          }
        }
        if ($RequireVirtioInputMediaKeysPass) {
          if ($sawVirtioInputMediaKeysFail) { return @{ Result = "VIRTIO_INPUT_MEDIA_KEYS_FAILED"; Tail = $tail } }
          if (-not $sawVirtioInputMediaKeysPass) {
            if ($sawVirtioInputMediaKeysSkip) { return @{ Result = "VIRTIO_INPUT_MEDIA_KEYS_SKIPPED"; Tail = $tail } }
            return @{ Result = "MISSING_VIRTIO_INPUT_MEDIA_KEYS"; Tail = $tail }
          }
        }
        if ($RequireVirtioInputTabletEventsPass) {
          if ($sawVirtioInputTabletEventsFail) { return @{ Result = "VIRTIO_INPUT_TABLET_EVENTS_FAILED"; Tail = $tail } }
          if (-not $sawVirtioInputTabletEventsPass) {
            if ($sawVirtioInputTabletEventsSkip) { return @{ Result = "VIRTIO_INPUT_TABLET_EVENTS_SKIPPED"; Tail = $tail } }
            return @{ Result = "MISSING_VIRTIO_INPUT_TABLET_EVENTS"; Tail = $tail }
          }
        }
        if ($RequireVirtioInputWheelPass) {
          if ($sawVirtioInputWheelFail) { return @{ Result = "VIRTIO_INPUT_WHEEL_FAILED"; Tail = $tail } }
          if (-not $sawVirtioInputWheelPass) {
            if ($sawVirtioInputWheelSkip) { return @{ Result = "VIRTIO_INPUT_WHEEL_SKIPPED"; Tail = $tail } }
            return @{ Result = "MISSING_VIRTIO_INPUT_WHEEL"; Tail = $tail }
          }
        }
        if ($RequireVirtioInputLedPass) {
          if ($sawVirtioInputLedFail) { return @{ Result = "VIRTIO_INPUT_LED_FAILED"; Tail = $tail } }
          if (-not $sawVirtioInputLedPass) {
            if ($sawVirtioInputLedSkip) { return @{ Result = "VIRTIO_INPUT_LED_SKIPPED"; Tail = $tail } }
            return @{ Result = "MISSING_VIRTIO_INPUT_LED"; Tail = $tail }
          }
        }
        if ($RequireVirtioNetLinkFlapPass) {
          if ($sawVirtioNetLinkFlapFail) { return @{ Result = "VIRTIO_NET_LINK_FLAP_FAILED"; Tail = $tail } }
          if (-not $sawVirtioNetLinkFlapPass) {
            if ($sawVirtioNetLinkFlapSkip) { return @{ Result = "VIRTIO_NET_LINK_FLAP_SKIPPED"; Tail = $tail } }
            return @{ Result = "MISSING_VIRTIO_NET_LINK_FLAP"; Tail = $tail }
          }
        }

        if ($RequireVirtioBlkResetPass) {
          if ($sawVirtioBlkResetFail) { return @{ Result = "VIRTIO_BLK_RESET_FAILED"; Tail = $tail } }
          if (-not $sawVirtioBlkResetPass) {
            if ($sawVirtioBlkResetSkip) { return @{ Result = "VIRTIO_BLK_RESET_SKIPPED"; Tail = $tail } }
            return @{ Result = "MISSING_VIRTIO_BLK_RESET"; Tail = $tail }
          }
        }
        if ($RequireVirtioNetMsix -and (-not $sawVirtioNetMsixModeMsix)) {
          $chk = Test-AeroVirtioNetMsixMarker -Tail $tail -SerialLogPath $SerialLogPath
          if (-not $chk.Ok) {
            return @{ Result = "VIRTIO_NET_MSIX_REQUIRED"; Tail = $tail; MsixReason = $chk.Reason }
          }
        }

        $msixCheck = Test-VirtioInputMsixRequirement -Tail $tail -SerialLogPath $SerialLogPath
        if ($null -ne $msixCheck) { return $msixCheck }

        if ($RequireNetCsumOffload -or $RequireNetUdpCsumOffload) {
          $csum = Get-AeroVirtioNetOffloadCsumStatsFromTail -Tail $tail -SerialLogPath $SerialLogPath
          if ($null -eq $csum) {
            if ($RequireNetCsumOffload) { return @{ Result = "MISSING_VIRTIO_NET_CSUM_OFFLOAD"; Tail = $tail } }
            return @{ Result = "MISSING_VIRTIO_NET_UDP_CSUM_OFFLOAD"; Tail = $tail }
          }
          if ($csum.Status -ne "PASS") {
            if ($RequireNetCsumOffload) { return @{ Result = "VIRTIO_NET_CSUM_OFFLOAD_FAILED"; Tail = $tail } }
            return @{ Result = "VIRTIO_NET_UDP_CSUM_OFFLOAD_FAILED"; Tail = $tail }
          }
 
          if ($RequireNetCsumOffload) {
            if ($null -eq $csum.TxCsum) { return @{ Result = "VIRTIO_NET_CSUM_OFFLOAD_MISSING_FIELDS"; Tail = $tail } }
            if ($csum.TxCsum -le 0) { return @{ Result = "VIRTIO_NET_CSUM_OFFLOAD_ZERO"; Tail = $tail } }
          }
 
          if ($RequireNetUdpCsumOffload) {
            $txUdp = $csum.TxUdp
            $txUdp4 = $csum.TxUdp4
            $txUdp6 = $csum.TxUdp6
            if ($null -eq $txUdp) {
              if (($null -eq $txUdp4) -and ($null -eq $txUdp6)) {
                return @{ Result = "VIRTIO_NET_UDP_CSUM_OFFLOAD_MISSING_FIELDS"; Tail = $tail }
              }
              $txUdp = [UInt64]0
              if ($null -ne $txUdp4) { $txUdp += $txUdp4 }
              if ($null -ne $txUdp6) { $txUdp += $txUdp6 }
            }
 
            if ($txUdp -le 0) { return @{ Result = "VIRTIO_NET_UDP_CSUM_OFFLOAD_ZERO"; Tail = $tail } }
          }
        }
        return @{ Result = "PASS"; Tail = $tail }
      }
      if ($tail -match "AERO_VIRTIO_SELFTEST\|RESULT\|FAIL") {
        if ($RequireExpectBlkMsi -and ((-not $sawConfigExpectBlkMsi) -or $configExpectBlkMsi -ne "1")) {
          return @{ Result = "EXPECT_BLK_MSI_NOT_SET"; Tail = $tail }
        }
        # Surface virtio-snd bring-up toggle failures as a dedicated failure token so CI logs
        # remain actionable (ForceNullBackend disables the virtio transport and makes host wav
        # verification silent).
        if (($tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-snd\|FAIL\|force_null_backend") -or
            ($tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-snd-capture\|FAIL\|force_null_backend") -or
            ($tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-snd-duplex\|FAIL\|force_null_backend")) {
          return @{ Result = "VIRTIO_SND_FORCE_NULL_BACKEND"; Tail = $tail }
        }
        return @{ Result = "FAIL"; Tail = $tail }
      }
    }

    # When requested, inject keyboard/mouse events after the guest has armed the user-mode HID report read loop
    # (virtio-input-events|READY). Inject multiple times on a short interval to reduce flakiness from timing
    # windows (reports may be dropped when no read is pending).
    #
    # When requested, resize the virtio-blk backing device after the guest has armed its polling loop
    # (virtio-blk-resize|READY).
    #
    # If the guest never emits READY/SKIP/PASS/FAIL after completing virtio-input, assume the guest selftest
    # is too old (or misconfigured) and fail early to avoid burning the full virtio-net timeout.
    if ($RequireVirtioBlkResizePass -and ($null -ne $virtioBlkMarkerTime) -and (-not $sawVirtioBlkResizeReady) -and (-not $sawVirtioBlkResizePass) -and (-not $sawVirtioBlkResizeFail) -and (-not $sawVirtioBlkResizeSkip)) {
      $delta = ([DateTime]::UtcNow - $virtioBlkMarkerTime).TotalSeconds
      if ($delta -ge 20) { return @{ Result = "MISSING_VIRTIO_BLK_RESIZE"; Tail = $tail } }
    }
    if ($RequireVirtioBlkResizePass -and $sawVirtioBlkResizeReady -and (-not $sawVirtioBlkResizePass) -and (-not $sawVirtioBlkResizeFail) -and (-not $sawVirtioBlkResizeSkip) -and (-not $blkResizeRequested)) {
      if (($null -eq $QmpPort) -or ($QmpPort -le 0)) {
        return @{ Result = "QMP_BLK_RESIZE_FAILED"; Tail = $tail }
      }
      if (($null -eq $blkResizeOldBytes) -or ($null -eq $blkResizeNewBytes)) {
        return @{ Result = "QMP_BLK_RESIZE_FAILED"; Tail = $tail }
      }
      $blkResizeRequested = $true
      $ok = Try-AeroQmpResizeVirtioBlk -Host $QmpHost -Port ([int]$QmpPort) -OldBytes ([UInt64]$blkResizeOldBytes) -NewBytes ([UInt64]$blkResizeNewBytes) -DriveId "drive0"
      if (-not $ok) {
        return @{ Result = "QMP_BLK_RESIZE_FAILED"; Tail = $tail }
      }
    }

    if ($RequireVirtioInputLedsPass -and ($null -ne $virtioInputMarkerTime) -and (-not $sawVirtioInputLedsPass) -and (-not $sawVirtioInputLedsFail) -and (-not $sawVirtioInputLedsSkip)) {
      $delta = ([DateTime]::UtcNow - $virtioInputMarkerTime).TotalSeconds
      if ($delta -ge 20) { return @{ Result = "MISSING_VIRTIO_INPUT_LEDS"; Tail = $tail } }
    }
    if ($RequireVirtioInputEventsPass -and ($null -ne $virtioInputMarkerTime) -and (-not $sawVirtioInputEventsReady) -and (-not $sawVirtioInputEventsPass) -and (-not $sawVirtioInputEventsFail) -and (-not $sawVirtioInputEventsSkip)) {
      $delta = ([DateTime]::UtcNow - $virtioInputMarkerTime).TotalSeconds
      if ($delta -ge 20) { return @{ Result = "MISSING_VIRTIO_INPUT_EVENTS"; Tail = $tail } }
    }
    if ($RequireVirtioInputMediaKeysPass -and ($null -ne $virtioInputMarkerTime) -and (-not $sawVirtioInputMediaKeysReady) -and (-not $sawVirtioInputMediaKeysPass) -and (-not $sawVirtioInputMediaKeysFail) -and (-not $sawVirtioInputMediaKeysSkip)) {
      $delta = ([DateTime]::UtcNow - $virtioInputMarkerTime).TotalSeconds
      if ($delta -ge 20) { return @{ Result = "MISSING_VIRTIO_INPUT_MEDIA_KEYS"; Tail = $tail } }
    }
    if ($RequireVirtioInputLedPass -and ($null -ne $virtioInputMarkerTime) -and (-not $sawVirtioInputLedPass) -and (-not $sawVirtioInputLedFail) -and (-not $sawVirtioInputLedSkip)) {
      $delta = ([DateTime]::UtcNow - $virtioInputMarkerTime).TotalSeconds
      if ($delta -ge 20) { return @{ Result = "MISSING_VIRTIO_INPUT_LED"; Tail = $tail } }
    }
    if ($RequireVirtioInputTabletEventsPass -and ($null -ne $virtioInputMarkerTime) -and (-not $sawVirtioInputTabletEventsReady) -and (-not $sawVirtioInputTabletEventsPass) -and (-not $sawVirtioInputTabletEventsFail) -and (-not $sawVirtioInputTabletEventsSkip)) {
      $delta = ([DateTime]::UtcNow - $virtioInputMarkerTime).TotalSeconds
      if ($delta -ge 20) { return @{ Result = "MISSING_VIRTIO_INPUT_TABLET_EVENTS"; Tail = $tail } }
    }

    if ($RequireVirtioNetLinkFlapPass -and ($null -ne $virtioNetMarkerTime) -and (-not $sawVirtioNetLinkFlapReady) -and (-not $sawVirtioNetLinkFlapPass) -and (-not $sawVirtioNetLinkFlapFail) -and (-not $sawVirtioNetLinkFlapSkip)) {
      $delta = ([DateTime]::UtcNow - $virtioNetMarkerTime).TotalSeconds
      if ($delta -ge 20) { return @{ Result = "MISSING_VIRTIO_NET_LINK_FLAP"; Tail = $tail } }
    }

    if ($RequireVirtioInputEventsPass -and $sawVirtioInputEventsReady -and (-not $sawVirtioInputEventsPass) -and (-not $sawVirtioInputEventsFail) -and (-not $sawVirtioInputEventsSkip)) {
      if (($null -eq $QmpPort) -or ($QmpPort -le 0)) {
        return @{ Result = "QMP_INPUT_INJECT_FAILED"; Tail = $tail }
      }
      if ($inputEventsInjectAttempts -lt $maxInputEventsInjectAttempts -and [DateTime]::UtcNow -ge $nextInputEventsInject) {
        $inputEventsInjectAttempts++
        $nextInputEventsInject = [DateTime]::UtcNow.AddMilliseconds(500)
        $ok = Try-AeroQmpInjectVirtioInputEvents -Host $QmpHost -Port ([int]$QmpPort) -Attempt $inputEventsInjectAttempts -WithWheel:($RequireVirtioInputWheelPass -or $RequireVirtioInputEventsExtendedPass) -Extended:$RequireVirtioInputEventsExtendedPass
        if (-not $ok) {
          return @{ Result = "QMP_INPUT_INJECT_FAILED"; Tail = $tail }
        }
      }
    }

    if ($RequireVirtioInputMediaKeysPass -and $sawVirtioInputMediaKeysReady -and (-not $sawVirtioInputMediaKeysPass) -and (-not $sawVirtioInputMediaKeysFail) -and (-not $sawVirtioInputMediaKeysSkip)) {
      if (($null -eq $QmpPort) -or ($QmpPort -le 0)) {
        return @{ Result = "QMP_MEDIA_KEYS_UNSUPPORTED"; Tail = $tail }
      }
      if ($inputMediaKeysInjectAttempts -lt 20 -and [DateTime]::UtcNow -ge $nextInputMediaKeysInject) {
        $inputMediaKeysInjectAttempts++
        $nextInputMediaKeysInject = [DateTime]::UtcNow.AddMilliseconds(500)
        $ok = Try-AeroQmpInjectVirtioInputMediaKeys -Host $QmpHost -Port ([int]$QmpPort) -Attempt $inputMediaKeysInjectAttempts
        if (-not $ok) {
          return @{ Result = "QMP_MEDIA_KEYS_UNSUPPORTED"; Tail = $tail }
        }
      }
    }

    if ($RequireVirtioInputTabletEventsPass -and $sawVirtioInputTabletEventsReady -and (-not $sawVirtioInputTabletEventsPass) -and (-not $sawVirtioInputTabletEventsFail) -and (-not $sawVirtioInputTabletEventsSkip)) {
      if (($null -eq $QmpPort) -or ($QmpPort -le 0)) {
        return @{ Result = "QMP_INPUT_TABLET_INJECT_FAILED"; Tail = $tail }
      }
      if ($inputTabletEventsInjectAttempts -lt 20 -and [DateTime]::UtcNow -ge $nextInputTabletEventsInject) {
        $inputTabletEventsInjectAttempts++
        $nextInputTabletEventsInject = [DateTime]::UtcNow.AddMilliseconds(500)
        $ok = Try-AeroQmpInjectVirtioInputTabletEvents -Host $QmpHost -Port ([int]$QmpPort) -Attempt $inputTabletEventsInjectAttempts
        if (-not $ok) {
          return @{ Result = "QMP_INPUT_TABLET_INJECT_FAILED"; Tail = $tail }
        }
      }
    }

    if ($RequireVirtioNetLinkFlapPass -and $sawVirtioNetLinkFlapReady -and (-not $sawVirtioNetLinkFlapPass) -and (-not $sawVirtioNetLinkFlapFail) -and (-not $sawVirtioNetLinkFlapSkip) -and (-not $didNetLinkFlap)) {
      if (($null -eq $QmpPort) -or ($QmpPort -le 0)) {
        return @{ Result = "QMP_SET_LINK_FAILED"; Tail = $tail }
      }
      $didNetLinkFlap = $true

      # Prefer targeting the stable virtio-net QOM id via QMP:
      #   name = $script:VirtioNetQmpId
      $names = @($script:VirtioNetQmpId, "net0")
      $res = Try-AeroQmpSetLink -Host $QmpHost -Port ([int]$QmpPort) -Names $names -Up:$false
      if (-not $res.Ok) {
        if ($res.Unsupported) { return @{ Result = "QMP_SET_LINK_UNSUPPORTED"; Tail = $tail } }
        return @{ Result = "QMP_SET_LINK_FAILED"; Tail = $tail }
      }
      $targetName = [string]$res.Name
      Write-Host "AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_LINK_FLAP|REQUEST|phase=down|name=$(Sanitize-AeroMarkerValue $targetName)"

      Start-Sleep -Seconds 3

      $namesUp = @()
      if (-not [string]::IsNullOrEmpty($targetName)) { $namesUp += $targetName }
      $namesUp += @($script:VirtioNetQmpId, "net0")
      $res2 = Try-AeroQmpSetLink -Host $QmpHost -Port ([int]$QmpPort) -Names $namesUp -Up:$true
      if (-not $res2.Ok) {
        if ($res2.Unsupported) { return @{ Result = "QMP_SET_LINK_UNSUPPORTED"; Tail = $tail } }
        return @{ Result = "QMP_SET_LINK_FAILED"; Tail = $tail }
      }
      if (-not [string]::IsNullOrEmpty([string]$res2.Name)) { $targetName = [string]$res2.Name }
      Write-Host "AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_LINK_FLAP|REQUEST|phase=up|name=$(Sanitize-AeroMarkerValue $targetName)"
      Write-Host "AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_LINK_FLAP|PASS|name=$(Sanitize-AeroMarkerValue $targetName)|down_delay_sec=3"
    }

    if ($QemuProcess.HasExited) {
      return @{
        Result = "QEMU_EXITED"
        Tail = $tail
      }
    }

    Start-Sleep -Milliseconds 250
  }

  return @{
    Result = "TIMEOUT"
    Tail = $tail
  }
}

function Get-AeroVirtioSoundDeviceArg {
  param(
    [Parameter(Mandatory = $true)] [string]$QemuSystem,
    # If true, require modern-only virtio-pci enumeration (disable-legacy=on) and contract revision (x-pci-revision=0x01).
    [Parameter(Mandatory = $true)] [bool]$ModernOnly,

    # Optional MSI-X vector count (`vectors=` device property).
    [Parameter(Mandatory = $false)] [int]$MsixVectors = 0,
    # If set, append `,vectors=0` to disable MSI-X and force legacy INTx.
    [Parameter(Mandatory = $false)] [switch]$DisableMsix,
    # Optional name of the PowerShell parameter that requested this override (for clearer warnings).
    [Parameter(Mandatory = $false)] [string]$VectorsParamName = "-VirtioMsixVectors",

    # Optional pre-resolved virtio-snd PCI device name (virtio-sound-pci / virtio-snd-pci).
    [Parameter(Mandatory = $false)] [string]$DeviceName = ""
  )

  $deviceName = $DeviceName
  if ([string]::IsNullOrEmpty($deviceName)) {
    # Determine which QEMU virtio-snd PCI device name is available and validate it supports
    # the Aero contract v1 configuration we need.
    #
    # The strict Aero INF (`aero_virtio_snd.inf`) matches only the modern virtio-snd PCI ID
    # (`PCI\VEN_1AF4&DEV_1059`) and requires `REV_01`, so we must:
    #   - force modern-only virtio-pci enumeration (`disable-legacy=on` => `DEV_1059`)
    #   - force PCI Revision ID 0x01 (`x-pci-revision=0x01` => `REV_01`)
    if ($ModernOnly) {
      $deviceName = Assert-AeroWin7QemuSupportsVirtioSndPciDevice -QemuSystem $QemuSystem
    } else {
      $deviceName = Resolve-AeroVirtioSndPciDeviceName -QemuSystem $QemuSystem
    }
  }

  $helpText = $null
  if ($ModernOnly -or $MsixVectors -gt 0 -or $DisableMsix) {
    $helpText = Get-AeroWin7QemuDeviceHelpText -QemuSystem $QemuSystem -DeviceName $deviceName
  }

  if ($ModernOnly) {
    if ($helpText -notmatch "(?m)^\s*disable-legacy\b") {
      throw "QEMU device '$deviceName' does not expose 'disable-legacy'. AERO-W7-VIRTIO v1 virtio-snd requires modern-only virtio-pci enumeration (DEV_1059). Upgrade QEMU."
    }
    if ($helpText -notmatch "(?m)^\s*x-pci-revision\b") {
      throw "QEMU device '$deviceName' does not expose 'x-pci-revision'. AERO-W7-VIRTIO v1 virtio-snd requires PCI Revision ID 0x01 (REV_01). Upgrade QEMU."
    }
  }

  if ($DisableMsix -and ($helpText -notmatch "(?m)^\s*vectors\b")) {
    throw "QEMU device '$deviceName' does not advertise a 'vectors' property; cannot disable MSI-X via vectors=0. Upgrade QEMU or omit -VirtioDisableMsix."
  }
  if ($DisableMsix) {
    Assert-AeroWin7QemuAcceptsVectorsZero -QemuSystem $QemuSystem -DeviceName $deviceName
  }
  if ($MsixVectors -gt 0 -and ($helpText -notmatch "(?m)^\s*vectors\b")) {
    throw "QEMU device '$deviceName' does not advertise a 'vectors' property. $VectorsParamName=$MsixVectors requires it. Disable the flag or upgrade QEMU."
  }

  $arg = "$deviceName"
  if ($ModernOnly) {
    $arg += ",disable-legacy=on,x-pci-revision=0x01"
  }
  $arg += ",audiodev=snd0"
  if ($DisableMsix) {
    $arg += ",vectors=0"
  } elseif ($MsixVectors -gt 0) {
    $arg += ",vectors=$MsixVectors"
  }
  return $arg
}

function Sanitize-AeroMarkerValue {
  param(
    [Parameter(Mandatory = $true)] [string]$Value
  )
  return $Value.Replace("|", "/").Replace("`r", " ").Replace("`n", " ").Trim()
}

function Try-ExtractLastAeroMarkerLine {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    [Parameter(Mandatory = $true)] [string]$Prefix,
    # Optional: if provided, fall back to parsing the full serial log when the rolling tail buffer does not
    # contain the marker (e.g. because the tail was truncated).
    [Parameter(Mandatory = $false)] [string]$SerialLogPath = ""
  )

  $matches = [regex]::Matches($Tail, [regex]::Escape($Prefix) + "[^`r`n]*")
  if ($matches.Count -gt 0) {
    return $matches[$matches.Count - 1].Value
  }

  if ((-not [string]::IsNullOrEmpty($SerialLogPath)) -and (Test-Path -LiteralPath $SerialLogPath)) {
    # Tail truncation fallback: scan the full serial log line-by-line.
    $last = $null
    try {
      $fs = [System.IO.File]::Open($SerialLogPath, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, [System.IO.FileShare]::ReadWrite)
      try {
        $sr = [System.IO.StreamReader]::new($fs, [System.Text.Encoding]::UTF8, $true, 4096, $true)
        try {
          while ($true) {
            $l = $sr.ReadLine()
            if ($null -eq $l) { break }
            $t = $l.Trim()
            if ($t.StartsWith($Prefix)) {
              $last = $t
            }
          }
        } finally {
          $sr.Dispose()
        }
      } finally {
        $fs.Dispose()
      }
    } catch { }
    if ($null -ne $last) {
      return $last
    }
  }

  return $null
}

function Try-ExtractVirtioSndSkipReason {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    # Optional: if provided, fall back to parsing the full serial log when the rolling tail buffer does not
    # contain the virtio-snd SKIP diagnostic line (e.g. because the tail was truncated).
    [Parameter(Mandatory = $false)] [string]$SerialLogPath = ""
  )

  if ($Tail -match "virtio-snd: skipped \(enable with --test-snd\)") { return "guest_not_configured_with_--test-snd" }
  if ($Tail -match "virtio-snd: .*device not detected") { return "device_missing" }
  if ($Tail -match "virtio-snd: disabled by --disable-snd") { return "--disable-snd" }

  if ((-not [string]::IsNullOrEmpty($SerialLogPath)) -and (Test-Path -LiteralPath $SerialLogPath)) {
    # Tail truncation fallback: scan the full serial log line-by-line and keep the last reason we recognize.
    $last = $null
    try {
      $fs = [System.IO.File]::Open($SerialLogPath, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, [System.IO.FileShare]::ReadWrite)
      try {
        $sr = [System.IO.StreamReader]::new($fs, [System.Text.Encoding]::UTF8, $true, 4096, $true)
        try {
          while ($true) {
            $l = $sr.ReadLine()
            if ($null -eq $l) { break }
            $t = $l.Trim()
            if ($t -match "virtio-snd: skipped \(enable with --test-snd\)") { $last = "guest_not_configured_with_--test-snd" }
            elseif ($t -match "virtio-snd: .*device not detected") { $last = "device_missing" }
            elseif ($t -match "virtio-snd: disabled by --disable-snd") { $last = "--disable-snd" }
          }
        } finally {
          $sr.Dispose()
        }
      } finally {
        $fs.Dispose()
      }
    } catch { }
    if ($null -ne $last) {
      return $last
    }
  }

  return $null
}

function Try-EmitAeroVirtioBlkIrqMarker {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    # Optional: if provided, fall back to parsing the full serial log when the rolling tail buffer does not
    # contain any virtio-blk IRQ marker data (e.g. because the tail was truncated).
    [Parameter(Mandatory = $false)] [string]$SerialLogPath = ""
  )

  # Collect IRQ fields from (a) the virtio-blk per-test marker (IOCTL-derived fields) and/or
  # (b) standalone diagnostics:
  #   - `virtio-blk-miniport-irq|...` (best-effort miniport IOCTL diagnostics; may include messages/message_count + MSI-X vectors)
  #   - `virtio-blk-irq|...` (cfgmgr32 resource enumeration / Windows-assigned IRQ mode)
  #
  # Prefer the per-test marker when present, but fill in missing fields from the standalone
  # diagnostics so the host marker is still produced for older selftest binaries.
  $blkPrefix = "AERO_VIRTIO_SELFTEST|TEST|virtio-blk|"
  $blkMatches = [regex]::Matches($Tail, [regex]::Escape($blkPrefix) + "[^`r`n]*")
  $blkLine = $null
  if ($blkMatches.Count -gt 0) {
    $blkLine = $blkMatches[$blkMatches.Count - 1].Value
  }

  $fields = @{}
  $addLineFields = {
    param([string]$Line)
    if ([string]::IsNullOrEmpty($Line)) { return }
    foreach ($tok in $Line.Split("|")) {
      $idx = $tok.IndexOf("=")
      if ($idx -le 0) { continue }
      $k = $tok.Substring(0, $idx)
      $v = $tok.Substring($idx + 1)
      if (-not [string]::IsNullOrEmpty($k) -and (-not $fields.ContainsKey($k))) {
        $fields[$k] = $v
      }
    }
  }

  # Always merge fields from the per-test marker (may include irq_mode/msix_* fields).
  & $addLineFields $blkLine

  # Best-effort: accept legacy/selftest-internal virtio-blk IRQ marker variants (if any).
  foreach ($prefix in @(
      "AERO_VIRTIO_SELFTEST|TEST|virtio-blk-irq|",
      "AERO_VIRTIO_SELFTEST|TEST|virtio-blk|IRQ|",
      "AERO_VIRTIO_SELFTEST|TEST|virtio-blk|INFO|",
      "AERO_VIRTIO_SELFTEST|MARKER|virtio-blk-irq|"
    )) {
    $matches = [regex]::Matches($Tail, [regex]::Escape($prefix) + "[^`r`n]*")
    if ($matches.Count -gt 0) {
      & $addLineFields $matches[$matches.Count - 1].Value
      break
    }
  }

  # Standalone diagnostics markers emitted by the guest:
  # - miniport IOCTL diagnostics (best-effort; depends on miniport contract):
  #     virtio-blk-miniport-irq|INFO|mode=msi|messages=...|message_count=...|msix_config_vector=...|msix_queue0_vector=...
  # - resource enumeration (PnP):
  #     virtio-blk-irq|INFO|mode=msi|messages=...
  #
  # Prefer miniport diagnostics when present (newer guests reserve `virtio-blk-irq|...` for
  # cfgmgr32/Windows-assigned IRQ resources).
  $miniportIrqMatches = [regex]::Matches($Tail, "(?m)^\s*virtio-blk-miniport-irq\|[^`r`n]*")
  if ($miniportIrqMatches.Count -gt 0) {
    & $addLineFields $miniportIrqMatches[$miniportIrqMatches.Count - 1].Value
  }
  $irqMatches = [regex]::Matches($Tail, "(?m)^\s*virtio-blk-irq\|[^`r`n]*")
  if ($irqMatches.Count -gt 0) {
    & $addLineFields $irqMatches[$irqMatches.Count - 1].Value
  }

  $sawBlkLineInTail = ($blkMatches.Count -gt 0)
  $sawBlkIrqLineInTail = ($irqMatches.Count -gt 0 -or $miniportIrqMatches.Count -gt 0)

  if ((-not [string]::IsNullOrEmpty($SerialLogPath)) -and (Test-Path -LiteralPath $SerialLogPath) -and (-not ($sawBlkLineInTail -and $sawBlkIrqLineInTail))) {
    # Tail truncation fallback: scan the full serial log line-by-line and keep the last blk markers we care about.
    $lastBlkLine = $null
    $lastBlkIrqLine = $null
    $lastBlkMiniportIrqLine = $null
    try {
      $fs = [System.IO.File]::Open($SerialLogPath, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, [System.IO.FileShare]::ReadWrite)
      try {
        $sr = [System.IO.StreamReader]::new($fs, [System.Text.Encoding]::UTF8, $true, 4096, $true)
        try {
          while ($true) {
            $line = $sr.ReadLine()
            if ($null -eq $line) { break }
            if ($line -match "^\s*AERO_VIRTIO_SELFTEST\|TEST\|virtio-blk\|") {
              $lastBlkLine = $line.Trim()
            }
            if ($line -match "^\s*virtio-blk-miniport-irq\|") {
              $lastBlkMiniportIrqLine = $line.Trim()
            }
            if ($line -match "^\s*virtio-blk-irq\|") {
              $lastBlkIrqLine = $line.Trim()
            }
          }
        } finally {
          $sr.Dispose()
        }
      } finally {
        $fs.Dispose()
      }
    } catch { }

    if ($null -eq $blkLine -and $null -ne $lastBlkLine) {
      $blkLine = $lastBlkLine
      & $addLineFields $lastBlkLine
    }
    if ($null -ne $lastBlkMiniportIrqLine) {
      & $addLineFields $lastBlkMiniportIrqLine
    }
    if ($null -ne $lastBlkIrqLine) {
      & $addLineFields $lastBlkIrqLine
    }
  }

  if ($fields.Count -eq 0) { return }

  $mode = $null
  # Prefer `irq_mode` from the per-test marker (IOCTL-derived) over `mode` from the standalone diagnostics,
  # since the IOCTL-based path can distinguish MSI vs MSI-X based on vector assignment.
  if ($fields.ContainsKey("irq_mode")) { $mode = $fields["irq_mode"] }
  elseif ($fields.ContainsKey("mode")) { $mode = $fields["mode"] }
  elseif ($fields.ContainsKey("interrupt_mode")) { $mode = $fields["interrupt_mode"] }

  $messages = $null
  # Prefer canonical `irq_message_count` when present.
  if ($fields.ContainsKey("irq_message_count")) { $messages = $fields["irq_message_count"] }
  elseif ($fields.ContainsKey("message_count")) { $messages = $fields["message_count"] }
  elseif ($fields.ContainsKey("messages")) { $messages = $fields["messages"] }
  elseif ($fields.ContainsKey("irq_messages")) { $messages = $fields["irq_messages"] }
  elseif ($fields.ContainsKey("msi_messages")) { $messages = $fields["msi_messages"] }

  $vectors = $null
  if ($fields.ContainsKey("irq_vectors")) { $vectors = $fields["irq_vectors"] }
  elseif ($fields.ContainsKey("vectors")) { $vectors = $fields["vectors"] }

  $msiVector = $null
  if ($fields.ContainsKey("msi_vector")) { $msiVector = $fields["msi_vector"] }
  elseif ($fields.ContainsKey("vector")) { $msiVector = $fields["vector"] }
  elseif ($fields.ContainsKey("irq_vector")) { $msiVector = $fields["irq_vector"] }

  $msixConfigVector = $null
  if ($fields.ContainsKey("msix_config_vector")) { $msixConfigVector = $fields["msix_config_vector"] }

  $msixQueueVector = $null
  if ($fields.ContainsKey("msix_queue_vector")) { $msixQueueVector = $fields["msix_queue_vector"] }
  elseif ($fields.ContainsKey("msix_queue0_vector")) { $msixQueueVector = $fields["msix_queue0_vector"] }

  # Some guest diagnostics (notably virtio-blk miniport IOCTL output) intentionally conflate MSI and MSI-X
  # in the reported `mode` field (e.g. `mode=msi` even when MSI-X vectors are assigned). If we see
  # `mode=msi` plus any MSI-X vector index, treat this as MSI-X for the stable host marker so it
  # matches the per-test marker semantics (`irq_mode=msix`).
  if (-not [string]::IsNullOrEmpty($mode) -and $mode.Trim().ToLowerInvariant() -eq "msi") {
    foreach ($vec in @($msixConfigVector, $msixQueueVector)) {
      if ([string]::IsNullOrEmpty($vec)) { continue }
      $v = ([string]$vec).Trim().ToLowerInvariant()
      if ([string]::IsNullOrEmpty($v) -or $v -eq "none" -or $v -eq "0xffff" -or $v -eq "65535") { continue }
      $mode = "msix"
      break
    }
  }

  if (-not $mode -and -not $messages -and -not $vectors -and -not $msiVector -and -not $msixConfigVector -and -not $msixQueueVector) { return }

  $status = "INFO"
  if (-not [string]::IsNullOrEmpty($blkLine)) {
    if ($blkLine -match "\|FAIL(\||$)") { $status = "FAIL" }
    elseif ($blkLine -match "\|PASS(\||$)") { $status = "PASS" }
  }

  $out = "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_IRQ|$status"
  if ($mode) { $out += "|irq_mode=$(Sanitize-AeroMarkerValue $mode)" }
  if ($messages) { $out += "|irq_message_count=$(Sanitize-AeroMarkerValue $messages)" }
  if ($vectors) { $out += "|irq_vectors=$(Sanitize-AeroMarkerValue $vectors)" }
  if ($msiVector) { $out += "|msi_vector=$(Sanitize-AeroMarkerValue $msiVector)" }
  if ($msixConfigVector) { $out += "|msix_config_vector=$(Sanitize-AeroMarkerValue $msixConfigVector)" }
  if ($msixQueueVector) { $out += "|msix_queue_vector=$(Sanitize-AeroMarkerValue $msixQueueVector)" }

  # Preserve any additional interrupt-related fields so the marker stays useful for debugging
  # and remains forward-compatible when the guest appends new `irq_*` / `msi_*` / `msix_*` keys.
  #
  # Keep ordering stable: base keys above, then extra `irq_*` keys sorted, then extra `msi_*`/`msix_*` keys sorted.
  $ordered = @("irq_mode", "irq_message_count", "irq_vectors", "msi_vector", "msix_config_vector", "msix_queue_vector")
  $orderedSet = @{}
  foreach ($k in $ordered) { $orderedSet[$k] = $true }

  foreach ($k in ($fields.Keys | Where-Object { $_.StartsWith("irq_") -and (-not $orderedSet.ContainsKey($_)) } | Sort-Object)) {
    $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
  }
  foreach ($k in ($fields.Keys | Where-Object { ($_.StartsWith("msi_") -or $_.StartsWith("msix_")) -and (-not $orderedSet.ContainsKey($_)) } | Sort-Object)) {
    $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
  }
  Write-Host $out
}

function Test-AeroVirtioBlkMsixMarker {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    # Optional: if provided, fall back to parsing the full serial log when the rolling tail buffer does not
    # contain the virtio-blk-msix marker (e.g. because the tail was truncated).
    [Parameter(Mandatory = $false)] [string]$SerialLogPath = ""
  )

  $prefix = "AERO_VIRTIO_SELFTEST|TEST|virtio-blk-msix|"
  $matches = [regex]::Matches($Tail, [regex]::Escape($prefix) + "[^`r`n]*")
  $line = $null
  if ($matches.Count -gt 0) {
    $line = $matches[$matches.Count - 1].Value
  }

  if (($null -eq $line) -and (-not [string]::IsNullOrEmpty($SerialLogPath)) -and (Test-Path -LiteralPath $SerialLogPath)) {
    # Tail truncation fallback: scan the full serial log line-by-line.
    $last = $null
    try {
      $fs = [System.IO.File]::Open($SerialLogPath, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, [System.IO.FileShare]::ReadWrite)
      try {
        $sr = [System.IO.StreamReader]::new($fs, [System.Text.Encoding]::UTF8, $true, 4096, $true)
        try {
          while ($true) {
            $l = $sr.ReadLine()
            if ($null -eq $l) { break }
            $t = $l.Trim()
            if ($t.StartsWith($prefix)) {
              $last = $t
            }
          }
        } finally {
          $sr.Dispose()
        }
      } finally {
        $fs.Dispose()
      }
    } catch { }
    if ($null -ne $last) {
      $line = $last
    }
  }

  if ($null -eq $line) {
    return @{ Ok = $false; Reason = "missing virtio-blk-msix marker (guest selftest too old?)" }
  }

  $fields = @{}
  foreach ($tok in $line.Split("|")) {
    $idx = $tok.IndexOf("=")
    if ($idx -le 0) { continue }
    $k = $tok.Substring(0, $idx)
    $v = $tok.Substring($idx + 1)
    if (-not [string]::IsNullOrEmpty($k)) {
      $fields[$k] = $v
    }
  }

  if ($line -match "\|FAIL(\||$)") {
    $reason = "virtio-blk-msix marker reported FAIL"
    if ($fields.ContainsKey("reason")) { $reason += " reason=$($fields["reason"])" }
    if ($fields.ContainsKey("err")) { $reason += " err=$($fields["err"])" }
    return @{ Ok = $false; Reason = $reason }
  }
  if ($line -match "\|SKIP(\||$)") {
    $reason = "virtio-blk-msix marker reported SKIP"
    if ($fields.ContainsKey("reason")) { $reason += " reason=$($fields["reason"])" }
    if ($fields.ContainsKey("err")) { $reason += " err=$($fields["err"])" }
    return @{ Ok = $false; Reason = $reason }
  }

  if (-not $fields.ContainsKey("mode")) {
    return @{ Ok = $false; Reason = "virtio-blk-msix marker missing mode=... field" }
  }

  $mode = [string]$fields["mode"]
  if ($mode -ne "msix") {
    $msgs = "?"
    if ($fields.ContainsKey("messages")) { $msgs = [string]$fields["messages"] }
    return @{ Ok = $false; Reason = "mode=$mode (expected msix) messages=$msgs" }
  }

  return @{ Ok = $true; Reason = "ok" }
}

function Test-AeroVirtioNetMsixMarker {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    # Optional: if provided, fall back to parsing the full serial log when the rolling tail buffer does not
    # contain the virtio-net-msix marker (e.g. because the tail was truncated).
    [Parameter(Mandatory = $false)] [string]$SerialLogPath = ""
  )

  $prefix = "AERO_VIRTIO_SELFTEST|TEST|virtio-net-msix|"
  $matches = [regex]::Matches($Tail, [regex]::Escape($prefix) + "[^`r`n]*")
  $line = $null
  if ($matches.Count -gt 0) {
    $line = $matches[$matches.Count - 1].Value
  }

  if (($null -eq $line) -and (-not [string]::IsNullOrEmpty($SerialLogPath)) -and (Test-Path -LiteralPath $SerialLogPath)) {
    # Tail truncation fallback: scan the full serial log line-by-line.
    $last = $null
    try {
      $fs = [System.IO.File]::Open($SerialLogPath, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, [System.IO.FileShare]::ReadWrite)
      try {
        $sr = [System.IO.StreamReader]::new($fs, [System.Text.Encoding]::UTF8, $true, 4096, $true)
        try {
          while ($true) {
            $l = $sr.ReadLine()
            if ($null -eq $l) { break }
            $t = $l.Trim()
            if ($t.StartsWith($prefix)) {
              $last = $t
            }
          }
        } finally {
          $sr.Dispose()
        }
      } finally {
        $fs.Dispose()
      }
    } catch { }
    if ($null -ne $last) {
      $line = $last
    }
  }

  if ($null -eq $line) {
    return @{ Ok = $false; Reason = "missing virtio-net-msix marker (guest selftest too old?)" }
  }

  $fields = @{}
  foreach ($tok in $line.Split("|")) {
    $idx = $tok.IndexOf("=")
    if ($idx -le 0) { continue }
    $k = $tok.Substring(0, $idx)
    $v = $tok.Substring($idx + 1)
    if (-not [string]::IsNullOrEmpty($k)) {
      $fields[$k] = $v
    }
  }

  if ($line -match "\|FAIL(\||$)") {
    $reason = "virtio-net-msix marker reported FAIL"
    if ($fields.ContainsKey("reason")) { $reason += " reason=$($fields["reason"])" }
    if ($fields.ContainsKey("err")) { $reason += " err=$($fields["err"])" }
    return @{ Ok = $false; Reason = $reason }
  }
  if ($line -match "\|SKIP(\||$)") {
    $reason = "virtio-net-msix marker reported SKIP"
    if ($fields.ContainsKey("reason")) { $reason += " reason=$($fields["reason"])" }
    if ($fields.ContainsKey("err")) { $reason += " err=$($fields["err"])" }
    return @{ Ok = $false; Reason = $reason }
  }

  if (-not $fields.ContainsKey("mode")) {
    return @{ Ok = $false; Reason = "virtio-net-msix marker missing mode=... field" }
  }

  $mode = [string]$fields["mode"]
  if ($mode -ne "msix") {
    $msgs = "?"
    if ($fields.ContainsKey("messages")) { $msgs = [string]$fields["messages"] }
    return @{ Ok = $false; Reason = "mode=$mode (expected msix) messages=$msgs" }
  }

  return @{ Ok = $true; Reason = "ok" }
}

function Test-AeroVirtioSndMsixMarker {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    # Optional: if provided, fall back to parsing the full serial log when the rolling tail buffer does not
    # contain the virtio-snd-msix marker (e.g. because the tail was truncated).
    [Parameter(Mandatory = $false)] [string]$SerialLogPath = ""
  )

  $prefix = "AERO_VIRTIO_SELFTEST|TEST|virtio-snd-msix|"
  $matches = [regex]::Matches($Tail, [regex]::Escape($prefix) + "[^`r`n]*")
  $line = $null
  if ($matches.Count -gt 0) {
    $line = $matches[$matches.Count - 1].Value
  }

  if (($null -eq $line) -and (-not [string]::IsNullOrEmpty($SerialLogPath)) -and (Test-Path -LiteralPath $SerialLogPath)) {
    # Tail truncation fallback: scan the full serial log line-by-line.
    $last = $null
    try {
      $fs = [System.IO.File]::Open($SerialLogPath, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, [System.IO.FileShare]::ReadWrite)
      try {
        $sr = [System.IO.StreamReader]::new($fs, [System.Text.Encoding]::UTF8, $true, 4096, $true)
        try {
          while ($true) {
            $l = $sr.ReadLine()
            if ($null -eq $l) { break }
            $t = $l.Trim()
            if ($t.StartsWith($prefix)) {
              $last = $t
            }
          }
        } finally {
          $sr.Dispose()
        }
      } finally {
        $fs.Dispose()
      }
    } catch { }
    if ($null -ne $last) {
      $line = $last
    }
  }

  if ($null -eq $line) {
    return @{ Ok = $false; Reason = "missing virtio-snd-msix marker (guest selftest too old?)" }
  }

  $fields = @{}
  foreach ($tok in $line.Split("|")) {
    $idx = $tok.IndexOf("=")
    if ($idx -le 0) { continue }
    $k = $tok.Substring(0, $idx)
    $v = $tok.Substring($idx + 1)
    if (-not [string]::IsNullOrEmpty($k)) {
      $fields[$k] = $v
    }
  }

  if ($line -match "\|FAIL(\||$)") {
    $reason = "virtio-snd-msix marker reported FAIL"
    if ($fields.ContainsKey("reason")) { $reason += " reason=$($fields["reason"])" }
    if ($fields.ContainsKey("err")) { $reason += " err=$($fields["err"])" }
    return @{ Ok = $false; Reason = $reason }
  }
  if ($line -match "\|SKIP(\||$)") {
    $reason = "virtio-snd-msix marker reported SKIP"
    if ($fields.ContainsKey("reason")) { $reason += " reason=$($fields["reason"])" }
    if ($fields.ContainsKey("err")) { $reason += " err=$($fields["err"])" }
    return @{ Ok = $false; Reason = $reason }
  }

  if (-not $fields.ContainsKey("mode")) {
    return @{ Ok = $false; Reason = "virtio-snd-msix marker missing mode=... field" }
  }

  $mode = [string]$fields["mode"]
  if ($mode -ne "msix") {
    $msgs = "?"
    if ($fields.ContainsKey("messages")) { $msgs = [string]$fields["messages"] }
    return @{ Ok = $false; Reason = "mode=$mode (expected msix) messages=$msgs" }
  }

  return @{ Ok = $true; Reason = "ok" }
}

function Get-AeroVirtioBlkRecoveryCounters {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    # Optional: if provided, fall back to parsing the full serial log when the rolling tail buffer does not
    # contain the virtio-blk marker (e.g. because the tail was truncated).
    [Parameter(Mandatory = $false)] [string]$SerialLogPath = ""
  )

  $parseInt = {
    param([string]$Raw)
    $v = 0L
    $ok = [int64]::TryParse($Raw, [ref]$v)
    if (-not $ok) {
      if ($Raw -match "^0x[0-9a-fA-F]+$") {
        try {
          $v = [Convert]::ToInt64($Raw.Substring(2), 16)
          $ok = $true
        } catch {
          $ok = $false
        }
      }
    }
    if (-not $ok) { return $null }
    return $v
  }

  $prefix = "AERO_VIRTIO_SELFTEST|TEST|virtio-blk|"
  $line = Try-ExtractLastAeroMarkerLine -Tail $Tail -Prefix $prefix -SerialLogPath $SerialLogPath
  $fields = @{}
  if ($null -ne $line) {
    foreach ($tok in $line.Split("|")) {
      $idx = $tok.IndexOf("=")
      if ($idx -le 0) { continue }
      $k = $tok.Substring(0, $idx)
      $v = $tok.Substring($idx + 1)
      if (-not [string]::IsNullOrEmpty($k)) {
        $fields[$k] = $v
      }
    }
  }

  $keys = @("abort_srb", "reset_device_srb", "reset_bus_srb", "pnp_srb", "ioctl_reset")
  $haveAll = $true
  foreach ($k in $keys) {
    if (-not $fields.ContainsKey($k)) { $haveAll = $false; break }
  }

  if ($haveAll) {
    $out = @{}
    foreach ($k in $keys) {
      $raw = [string]$fields[$k]
      $v = & $parseInt $raw
      if ($null -eq $v) { return $null }
      $out[$k] = $v
    }
    return $out
  }

  # Backward/robustness: if the virtio-blk per-test marker does not include the counters fields (or is missing),
  # fall back to the dedicated virtio-blk-counters marker (abort/reset_device/reset_bus/pnp/ioctl_reset).
  $countersPrefix = "AERO_VIRTIO_SELFTEST|TEST|virtio-blk-counters|"
  $countersLine = Try-ExtractLastAeroMarkerLine -Tail $Tail -Prefix $countersPrefix -SerialLogPath $SerialLogPath
  if ($null -eq $countersLine) { return $null }

  $cfields = @{}
  foreach ($tok in $countersLine.Split("|")) {
    $idx = $tok.IndexOf("=")
    if ($idx -le 0) { continue }
    $k = $tok.Substring(0, $idx).Trim()
    $v = $tok.Substring($idx + 1).Trim()
    if (-not [string]::IsNullOrEmpty($k)) {
      $cfields[$k] = $v
    }
  }

  $mapping = @{
    abort        = "abort_srb"
    reset_device = "reset_device_srb"
    reset_bus    = "reset_bus_srb"
    pnp          = "pnp_srb"
    ioctl_reset  = "ioctl_reset"
  }

  $out2 = @{}
  foreach ($src in $mapping.Keys) {
    $dst = [string]$mapping[$src]
    if (-not $cfields.ContainsKey($src)) { return $null }
    $v = & $parseInt ([string]$cfields[$src])
    if ($null -eq $v) { return $null }
    $out2[$dst] = $v
  }
  return $out2
}

function Get-AeroVirtioBlkResetRecoveryCounters {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    # Optional: if provided, fall back to parsing the full serial log when the rolling tail buffer does not
    # contain the virtio-blk-reset-recovery marker (e.g. because the tail was truncated).
    [Parameter(Mandatory = $false)] [string]$SerialLogPath = ""
  )

  $parseInt = {
    param([string]$Raw)
    $v = 0L
    $ok = [int64]::TryParse($Raw, [ref]$v)
    if (-not $ok) {
      if ($Raw -match "^0x[0-9a-fA-F]+$") {
        try {
          $v = [Convert]::ToInt64($Raw.Substring(2), 16)
          $ok = $true
        } catch {
          $ok = $false
        }
      }
    }
    if (-not $ok) { return $null }
    return $v
  }

  $aeroPrefix = "AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset-recovery|"
  $line = Try-ExtractLastAeroMarkerLine -Tail $Tail -Prefix $aeroPrefix -SerialLogPath $SerialLogPath
  $fromAeroMarker = $true
  if ($null -eq $line) {
    # Backward compatible fallback: older guest selftests did not emit the dedicated AERO marker, but
    # did emit the miniport diagnostic line.
    $fromAeroMarker = $false
    $miniportPrefix = "virtio-blk-miniport-reset-recovery|"
    $line = Try-ExtractLastAeroMarkerLine -Tail $Tail -Prefix $miniportPrefix -SerialLogPath $SerialLogPath
    if ($null -eq $line) { return $null }
  }

  $toks = $line.Split("|")
  if ($fromAeroMarker) {
    if ($toks.Count -ge 4) {
      $s = $toks[3].Trim().ToUpperInvariant()
      if ($s -eq "SKIP") { return $null }
    }
  } else {
    if ($toks.Count -ge 2) {
      $s = $toks[1].Trim().ToUpperInvariant()
      if ($s -ne "INFO") { return $null }
    } else {
      return $null
    }
  }

  $fields = @{}
  foreach ($tok in $toks) {
    $idx = $tok.IndexOf("=")
    if ($idx -le 0) { continue }
    $k = $tok.Substring(0, $idx).Trim()
    $v = $tok.Substring($idx + 1).Trim()
    if (-not [string]::IsNullOrEmpty($k)) {
      $fields[$k] = $v
    }
  }

  $keys = @("reset_detected", "hw_reset_bus")
  $out = @{}
  foreach ($k in $keys) {
    if (-not $fields.ContainsKey($k)) { return $null }
    $raw = [string]$fields[$k]
    $v = & $parseInt $raw
    if ($null -eq $v) { return $null }
    $out[$k] = $v
  }
  return $out
}

function Get-AeroVirtioBlkMiniportFlags {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    # Optional: if provided, fall back to parsing the full serial log when the rolling tail buffer does not
    # contain the virtio-blk-miniport-flags marker (e.g. because the tail was truncated).
    [Parameter(Mandatory = $false)] [string]$SerialLogPath = ""
  )

  $parseInt = {
    param([string]$Raw)
    $v = 0L
    $ok = [int64]::TryParse($Raw, [ref]$v)
    if (-not $ok) {
      if ($Raw -match "^0x[0-9a-fA-F]+$") {
        try {
          $v = [Convert]::ToInt64($Raw.Substring(2), 16)
          $ok = $true
        } catch {
          $ok = $false
        }
      }
    }
    if (-not $ok) { return $null }
    return $v
  }

  $prefix = "virtio-blk-miniport-flags|"
  $line = Try-ExtractLastAeroMarkerLine -Tail $Tail -Prefix $prefix -SerialLogPath $SerialLogPath
  if ($null -eq $line) { return $null }

  $toks = $line.Split("|")
  if ($toks.Count -ge 2) {
    $s = $toks[1].Trim().ToUpperInvariant()
    if ($s -ne "INFO") { return $null }
  } else {
    return $null
  }

  $fields = @{}
  foreach ($tok in $toks) {
    $idx = $tok.IndexOf("=")
    if ($idx -le 0) { continue }
    $k = $tok.Substring(0, $idx).Trim()
    $v = $tok.Substring($idx + 1).Trim()
    if (-not [string]::IsNullOrEmpty($k)) {
      $fields[$k] = $v
    }
  }

  $keys = @("raw", "removed", "surprise_removed", "reset_in_progress", "reset_pending")
  $out = @{}
  foreach ($k in $keys) {
    if (-not $fields.ContainsKey($k)) { return $null }
    $v = & $parseInt ([string]$fields[$k])
    if ($null -eq $v) { return $null }
    $out[$k] = $v
  }
  return $out
}

function Try-EmitAeroVirtioBlkRecoveryMarker {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    # Optional: if provided, fall back to parsing the full serial log when the rolling tail buffer does not
    # contain the virtio-blk marker (e.g. because the tail was truncated).
    [Parameter(Mandatory = $false)] [string]$SerialLogPath = ""
  )

  $counters = Get-AeroVirtioBlkRecoveryCounters -Tail $Tail -SerialLogPath $SerialLogPath
  if ($null -eq $counters) { return }

  $out = "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RECOVERY|INFO"
  foreach ($k in @("abort_srb", "reset_device_srb", "reset_bus_srb", "pnp_srb", "ioctl_reset")) {
    if ($counters.ContainsKey($k)) {
      $out += "|$k=$(Sanitize-AeroMarkerValue ([string]$counters[$k]))"
    }
  }
  Write-Host $out
}

function Try-EmitAeroVirtioBlkCountersMarker {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    # Optional: if provided, fall back to parsing the full serial log when the rolling tail buffer does not
    # contain the virtio-blk-counters marker (e.g. because the tail was truncated).
    [Parameter(Mandatory = $false)] [string]$SerialLogPath = ""
  )

  $prefix = "AERO_VIRTIO_SELFTEST|TEST|virtio-blk-counters|"
  $line = Try-ExtractLastAeroMarkerLine -Tail $Tail -Prefix $prefix -SerialLogPath $SerialLogPath
  if ($null -eq $line) {
    # Backward compatible fallback: older guest selftests emitted the counters on the virtio-blk per-test marker
    # rather than the dedicated virtio-blk-counters marker.
    $blkPrefix = "AERO_VIRTIO_SELFTEST|TEST|virtio-blk|"
    $blkLine = Try-ExtractLastAeroMarkerLine -Tail $Tail -Prefix $blkPrefix -SerialLogPath $SerialLogPath
    if ($null -eq $blkLine) { return }

    $blkFields = @{}
    foreach ($tok in $blkLine.Split("|")) {
      $idx = $tok.IndexOf("=")
      if ($idx -le 0) { continue }
      $k = $tok.Substring(0, $idx).Trim()
      $v = $tok.Substring($idx + 1).Trim()
      if (-not [string]::IsNullOrEmpty($k)) {
        $blkFields[$k] = $v
      }
    }

    $mapping = @{
      abort_srb        = "abort"
      reset_device_srb = "reset_device"
      reset_bus_srb    = "reset_bus"
      pnp_srb          = "pnp"
      ioctl_reset      = "ioctl_reset"
    }

    $out = "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_COUNTERS|INFO"
    $sawAny = $false
    foreach ($src in @("abort_srb", "reset_device_srb", "reset_bus_srb", "pnp_srb", "ioctl_reset")) {
      if ($blkFields.ContainsKey($src)) {
        $dst = [string]$mapping[$src]
        $out += "|$dst=$(Sanitize-AeroMarkerValue $blkFields[$src])"
        $sawAny = $true
      }
    }
    if ($sawAny) {
      Write-Host $out
    }
    return
  }

  $toks = $line.Split("|")
  $status = "INFO"
  if ($toks.Count -ge 4) {
    $s = $toks[3].Trim().ToUpperInvariant()
    # Keep the host marker stable: treat any non-SKIP guest status as INFO.
    if ($s -eq "SKIP") { $status = "SKIP" }
  }

  $fields = @{}
  foreach ($tok in $toks) {
    $idx = $tok.IndexOf("=")
    if ($idx -le 0) { continue }
    $k = $tok.Substring(0, $idx).Trim()
    $v = $tok.Substring($idx + 1).Trim()
    if (-not [string]::IsNullOrEmpty($k)) {
      $fields[$k] = $v
    }
  }

  $ordered = @("abort", "reset_device", "reset_bus", "pnp", "ioctl_reset", "capacity_change_events", "reason", "returned_len")
  $orderedSet = @{}
  foreach ($k in $ordered) { $orderedSet[$k] = $true }

  $out = "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_COUNTERS|$status"
  foreach ($k in $ordered) {
    if ($fields.ContainsKey($k)) {
      $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
    }
  }
  foreach ($k in ($fields.Keys | Where-Object { -not $orderedSet.ContainsKey($_) } | Sort-Object)) {
    $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
  }
  Write-Host $out
}

function Try-EmitAeroVirtioBlkResetRecoveryMarker {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    # Optional: if provided, fall back to parsing the full serial log when the rolling tail buffer does not
    # contain the virtio-blk-reset-recovery marker (e.g. because the tail was truncated).
    [Parameter(Mandatory = $false)] [string]$SerialLogPath = ""
  )

  $aeroPrefix = "AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset-recovery|"
  $line = Try-ExtractLastAeroMarkerLine -Tail $Tail -Prefix $aeroPrefix -SerialLogPath $SerialLogPath
  $fromAeroMarker = $true
  if ($null -eq $line) {
    # Backward compatible fallback: older guest selftests did not emit the dedicated AERO marker, but
    # did emit the miniport diagnostic line.
    $fromAeroMarker = $false
    $miniportPrefix = "virtio-blk-miniport-reset-recovery|"
    $line = Try-ExtractLastAeroMarkerLine -Tail $Tail -Prefix $miniportPrefix -SerialLogPath $SerialLogPath
    if ($null -eq $line) { return }
  }

  $toks = $line.Split("|")
  $status = "INFO"
  if ($fromAeroMarker) {
    if ($toks.Count -ge 4) {
      $s = $toks[3].Trim().ToUpperInvariant()
      # Keep the host marker stable: treat any non-SKIP guest status as INFO.
      if ($s -eq "SKIP") { $status = "SKIP" }
    }
  } else {
    if ($toks.Count -ge 2) {
      $s = $toks[1].Trim().ToUpperInvariant()
      if ($s -eq "WARN") { $status = "SKIP" }
      elseif ($s -ne "INFO") { return }
    } else {
      return
    }
  }

  $fields = @{}
  foreach ($tok in $toks) {
    $idx = $tok.IndexOf("=")
    if ($idx -le 0) { continue }
    $k = $tok.Substring(0, $idx).Trim()
    $v = $tok.Substring($idx + 1).Trim()
    if (-not [string]::IsNullOrEmpty($k)) {
      $fields[$k] = $v
    }
  }

  $ordered = @("reset_detected", "hw_reset_bus", "reason", "returned_len", "expected_min")
  $orderedSet = @{}
  foreach ($k in $ordered) { $orderedSet[$k] = $true }

  $out = "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RESET_RECOVERY|$status"
  foreach ($k in $ordered) {
    if ($fields.ContainsKey($k)) {
      $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
    }
  }
  foreach ($k in ($fields.Keys | Where-Object { -not $orderedSet.ContainsKey($_) } | Sort-Object)) {
    $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
  }
  Write-Host $out
}

function Try-EmitAeroVirtioBlkMiniportFlagsMarker {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    # Optional: if provided, fall back to parsing the full serial log when the rolling tail buffer does not
    # contain the virtio-blk-miniport-flags marker (e.g. because the tail was truncated).
    [Parameter(Mandatory = $false)] [string]$SerialLogPath = ""
  )

  $prefix = "virtio-blk-miniport-flags|"
  $line = Try-ExtractLastAeroMarkerLine -Tail $Tail -Prefix $prefix -SerialLogPath $SerialLogPath
  if ($null -eq $line) { return }

  $toks = $line.Split("|")
  $status = "INFO"
  if ($toks.Count -ge 2) {
    $s = $toks[1].Trim().ToUpperInvariant()
    if ($s -eq "WARN") { $status = "WARN" }
  }

  $fields = @{}
  foreach ($tok in $toks) {
    $idx = $tok.IndexOf("=")
    if ($idx -le 0) { continue }
    $k = $tok.Substring(0, $idx).Trim()
    $v = $tok.Substring($idx + 1).Trim()
    if (-not [string]::IsNullOrEmpty($k)) {
      $fields[$k] = $v
    }
  }

  $ordered = @("raw", "removed", "surprise_removed", "reset_in_progress", "reset_pending", "reason", "returned_len", "expected_min")
  $orderedSet = @{}
  foreach ($k in $ordered) { $orderedSet[$k] = $true }

  $out = "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_MINIPORT_FLAGS|$status"
  foreach ($k in $ordered) {
    if ($fields.ContainsKey($k)) {
      $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
    }
  }
  foreach ($k in ($fields.Keys | Where-Object { -not $orderedSet.ContainsKey($_) } | Sort-Object)) {
    $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
  }
  Write-Host $out
}

function Try-EmitAeroVirtioBlkMiniportResetRecoveryMarker {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    # Optional: if provided, fall back to parsing the full serial log when the rolling tail buffer does not
    # contain the virtio-blk-miniport-reset-recovery marker (e.g. because the tail was truncated).
    [Parameter(Mandatory = $false)] [string]$SerialLogPath = ""
  )

  $prefix = "virtio-blk-miniport-reset-recovery|"
  $line = Try-ExtractLastAeroMarkerLine -Tail $Tail -Prefix $prefix -SerialLogPath $SerialLogPath
  if ($null -eq $line) { return }

  $toks = $line.Split("|")
  $status = "INFO"
  if ($toks.Count -ge 2) {
    $s = $toks[1].Trim().ToUpperInvariant()
    if ($s -eq "WARN") { $status = "WARN" }
  }

  $fields = @{}
  foreach ($tok in $toks) {
    $idx = $tok.IndexOf("=")
    if ($idx -le 0) { continue }
    $k = $tok.Substring(0, $idx).Trim()
    $v = $tok.Substring($idx + 1).Trim()
    if (-not [string]::IsNullOrEmpty($k)) {
      $fields[$k] = $v
    }
  }

  $ordered = @("reset_detected", "hw_reset_bus", "reason", "returned_len", "expected_min")
  $orderedSet = @{}
  foreach ($k in $ordered) { $orderedSet[$k] = $true }

  $out = "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_MINIPORT_RESET_RECOVERY|$status"
  foreach ($k in $ordered) {
    if ($fields.ContainsKey($k)) {
      $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
    }
  }
  foreach ($k in ($fields.Keys | Where-Object { -not $orderedSet.ContainsKey($_) } | Sort-Object)) {
    $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
  }
  Write-Host $out
}

function Try-EmitAeroVirtioBlkResizeMarker {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    # Optional: if provided, fall back to parsing the full serial log when the rolling tail buffer does not
    # contain the virtio-blk-resize marker (e.g. because the tail was truncated).
    [Parameter(Mandatory = $false)] [string]$SerialLogPath = ""
  )

  $prefix = "AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|"
  $line = Try-ExtractLastAeroMarkerLine -Tail $Tail -Prefix $prefix -SerialLogPath $SerialLogPath
  if ($null -eq $line) { return }

  $toks = $line.Split("|")
  $status = "INFO"
  if ($toks.Count -ge 4) {
    $s = $toks[3].Trim().ToUpperInvariant()
    if ($s -eq "PASS" -or $s -eq "FAIL" -or $s -eq "SKIP" -or $s -eq "READY" -or $s -eq "INFO") {
      $status = $s
    }
  }

  $fields = @{}
  foreach ($tok in $toks) {
    $idx = $tok.IndexOf("=")
    if ($idx -le 0) { continue }
    $k = $tok.Substring(0, $idx).Trim()
    $v = $tok.Substring($idx + 1).Trim()
    if (-not [string]::IsNullOrEmpty($k)) {
      $fields[$k] = $v
    }
  }

  $out = "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RESIZE|$status"

  # The guest SKIP marker uses a plain token (e.g. `...|SKIP|flag_not_set`) rather than a
  # `reason=...` field. Mirror it as `reason=` so log scraping can treat it uniformly.
  if ($status -eq "SKIP" -and (-not $fields.ContainsKey("reason"))) {
    for ($i = 0; $i -lt $toks.Count; $i++) {
      if ($toks[$i].Trim().ToUpperInvariant() -eq "SKIP") {
        if ($i + 1 -lt $toks.Count) {
          $reasonTok = $toks[$i + 1].Trim()
          if (-not [string]::IsNullOrEmpty($reasonTok) -and ($reasonTok.IndexOf("=") -lt 0)) {
            $out += "|reason=$(Sanitize-AeroMarkerValue $reasonTok)"
          }
        }
        break
      }
    }
  }

  # Keep ordering stable for log scraping.
  $ordered = @("disk", "old_bytes", "new_bytes", "elapsed_ms", "last_bytes", "err", "reason")
  $orderedSet = @{}
  foreach ($k in $ordered) { $orderedSet[$k] = $true }

  foreach ($k in $ordered) {
    if ($fields.ContainsKey($k)) {
      $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
    }
  }
  foreach ($k in ($fields.Keys | Where-Object { -not $orderedSet.ContainsKey($_) } | Sort-Object)) {
    $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
  }

  Write-Host $out
}

function Try-EmitAeroVirtioBlkResetMarker {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    # Optional: if provided, fall back to parsing the full serial log when the rolling tail buffer does not
    # contain the virtio-blk-reset marker (e.g. because the tail was truncated).
    [Parameter(Mandatory = $false)] [string]$SerialLogPath = ""
  )
  
  $prefix = "AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|"
  $line = Try-ExtractLastAeroMarkerLine -Tail $Tail -Prefix $prefix -SerialLogPath $SerialLogPath
  if ($null -eq $line) { return }
  
  $toks = $line.Split("|")
  $status = "INFO"
  if ($toks.Count -ge 4) {
    $s = $toks[3].Trim().ToUpperInvariant()
    if ($s -eq "PASS" -or $s -eq "FAIL" -or $s -eq "SKIP" -or $s -eq "INFO") {
      $status = $s
    }
  }
  
  $fields = @{}
  foreach ($tok in $toks) {
    $idx = $tok.IndexOf("=")
    if ($idx -le 0) { continue }
    $k = $tok.Substring(0, $idx).Trim()
    $v = $tok.Substring($idx + 1).Trim()
    if (-not [string]::IsNullOrEmpty($k)) {
      $fields[$k] = $v
    }
  }
  
  $out = "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RESET|$status"

  # Backcompat: older selftests may emit `...|SKIP|flag_not_set` or `...|FAIL|post_reset_io_failed` (no `reason=` field).
  # Mirror the trailing token as `reason=...` so log scraping can treat it uniformly.
  if (($status -eq "SKIP" -or $status -eq "FAIL") -and (-not $fields.ContainsKey("reason"))) {
    for ($i = 0; $i -lt $toks.Count; $i++) {
      if ($toks[$i].Trim().ToUpperInvariant() -eq $status) {
        if ($i + 1 -lt $toks.Count) {
          $reasonTok = $toks[$i + 1].Trim()
          if (-not [string]::IsNullOrEmpty($reasonTok) -and ($reasonTok.IndexOf("=") -lt 0)) {
            $fields["reason"] = $reasonTok
          }
        }
        break
      }
    }
  }
  
  # Keep ordering stable for log scraping.
  $ordered = @("performed", "counter_before", "counter_after", "err", "reason")
  $orderedSet = @{}
  foreach ($k in $ordered) { $orderedSet[$k] = $true }
  
  foreach ($k in $ordered) {
    if ($fields.ContainsKey($k)) {
      $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
    }
  }
  foreach ($k in ($fields.Keys | Where-Object { -not $orderedSet.ContainsKey($_) } | Sort-Object)) {
    $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
  }
  
  Write-Host $out
}

function Try-EmitAeroVirtioNetLargeMarker {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    # Optional: if provided, fall back to parsing the full serial log when the rolling tail buffer does not
    # contain the virtio-net marker (e.g. because the tail was truncated).
    [Parameter(Mandatory = $false)] [string]$SerialLogPath = ""
  )

  $prefix = "AERO_VIRTIO_SELFTEST|TEST|virtio-net|"
  $line = Try-ExtractLastAeroMarkerLine -Tail $Tail -Prefix $prefix -SerialLogPath $SerialLogPath
  if ($null -eq $line) { return }
  $fields = @{}
  foreach ($tok in $line.Split("|")) {
    $idx = $tok.IndexOf("=")
    if ($idx -le 0) { continue }
    $k = $tok.Substring(0, $idx)
    $v = $tok.Substring($idx + 1)
    if (-not [string]::IsNullOrEmpty($k)) {
      $fields[$k] = $v
    }
  }

  if (-not (
      $fields.ContainsKey("large_ok") -or
      $fields.ContainsKey("large_bytes") -or
      $fields.ContainsKey("large_mbps") -or
      $fields.ContainsKey("large_fnv1a64") -or
      $fields.ContainsKey("upload_ok") -or
      $fields.ContainsKey("upload_bytes") -or
      $fields.ContainsKey("upload_mbps") -or
      $fields.ContainsKey("msi") -or
      $fields.ContainsKey("msi_messages")
    )) {
    return
  }

  $status = "INFO"
  # Prefer the overall marker PASS/FAIL token so this stays correct even when the
  # large download passes but the optional large upload fails (TX vs RX stress).
  if ($line -match "\|FAIL(\||$)") { $status = "FAIL" }
  elseif ($line -match "\|PASS(\||$)") { $status = "PASS" }
  elseif ($fields.ContainsKey("large_ok") -and $fields["large_ok"] -eq "0") { $status = "FAIL" }
  elseif ($fields.ContainsKey("upload_ok") -and $fields["upload_ok"] -eq "0") { $status = "FAIL" }
  elseif ($fields.ContainsKey("large_ok") -and $fields["large_ok"] -eq "1" -and (-not $fields.ContainsKey("upload_ok") -or $fields["upload_ok"] -eq "1")) { $status = "PASS" }

  $out = "AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_LARGE|$status"
  foreach ($k in @("large_ok", "large_bytes", "large_fnv1a64", "large_mbps", "upload_ok", "upload_bytes", "upload_mbps", "msi", "msi_messages")) {
    if ($fields.ContainsKey($k)) {
      $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
    }
  }
  Write-Host $out
}

function Try-EmitAeroVirtioNetUdpMarker {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    # Optional: if provided, fall back to parsing the full serial log when the rolling tail buffer does not
    # contain the virtio-net-udp marker (e.g. because the tail was truncated).
    [Parameter(Mandatory = $false)] [string]$SerialLogPath = ""
  )

  $prefix = "AERO_VIRTIO_SELFTEST|TEST|virtio-net-udp|"
  $line = Try-ExtractLastAeroMarkerLine -Tail $Tail -Prefix $prefix -SerialLogPath $SerialLogPath
  if ($null -eq $line) { return }

  $toks = $line.Split("|")
  $status = "INFO"
  if ($toks.Count -ge 4) {
    $s = $toks[3].Trim().ToUpperInvariant()
    if ($s -eq "PASS" -or $s -eq "FAIL" -or $s -eq "SKIP" -or $s -eq "INFO") {
      $status = $s
    }
  }

  $fields = @{}
  foreach ($tok in $toks) {
    $idx = $tok.IndexOf("=")
    if ($idx -le 0) { continue }
    $k = $tok.Substring(0, $idx).Trim()
    $v = $tok.Substring($idx + 1).Trim()
    if (-not [string]::IsNullOrEmpty($k)) {
      $fields[$k] = $v
    }
  }

  $out = "AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_UDP|$status"
  foreach ($k in @("bytes", "small_bytes", "mtu_bytes", "reason", "wsa")) {
    if ($fields.ContainsKey($k)) {
      $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
    }
  }
  Write-Host $out
}

function Try-EmitAeroVirtioNetUdpDnsMarker {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    # Optional: if provided, fall back to parsing the full serial log when the rolling tail buffer does not
    # contain the virtio-net-udp-dns marker (e.g. because the tail was truncated).
    [Parameter(Mandatory = $false)] [string]$SerialLogPath = ""
  )

  $prefix = "AERO_VIRTIO_SELFTEST|TEST|virtio-net-udp-dns|"
  $line = Try-ExtractLastAeroMarkerLine -Tail $Tail -Prefix $prefix -SerialLogPath $SerialLogPath
  if ($null -eq $line) { return }

  $toks = $line.Split("|")
  $status = "INFO"
  if ($toks.Count -ge 4) {
    $s = $toks[3].Trim().ToUpperInvariant()
    if ($s -eq "PASS" -or $s -eq "FAIL" -or $s -eq "SKIP" -or $s -eq "INFO") {
      $status = $s
    }
  }

  $fields = @{}
  foreach ($tok in $toks) {
    $idx = $tok.IndexOf("=")
    if ($idx -le 0) { continue }
    $k = $tok.Substring(0, $idx).Trim()
    $v = $tok.Substring($idx + 1).Trim()
    if (-not [string]::IsNullOrEmpty($k)) {
      $fields[$k] = $v
    }
  }

  $out = "AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_UDP_DNS|$status"
  foreach ($k in @("server", "query", "sent", "recv", "rcode")) {
    if ($fields.ContainsKey($k)) {
      $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
    }
  }

  # If the guest marker included a trailing reason token (no '='), preserve it for SKIP/FAIL.
  if (($status -eq "SKIP" -or $status -eq "FAIL") -and (-not $fields.ContainsKey("reason")) -and $toks.Count -ge 5) {
    $reasonTok = $toks[4].Trim()
    if (-not [string]::IsNullOrEmpty($reasonTok) -and ($reasonTok.IndexOf("=") -lt 0)) {
      $out += "|reason=$(Sanitize-AeroMarkerValue $reasonTok)"
    }
  }

  Write-Host $out
}

function Try-EmitAeroVirtioNetOffloadCsumMarker {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    # Optional: if provided, fall back to parsing the full serial log when the rolling tail buffer does not
    # contain the virtio-net-offload-csum marker (e.g. because the tail was truncated).
    [Parameter(Mandatory = $false)] [string]$SerialLogPath = ""
  )

  $prefix = "AERO_VIRTIO_SELFTEST|TEST|virtio-net-offload-csum|"
  $line = Try-ExtractLastAeroMarkerLine -Tail $Tail -Prefix $prefix -SerialLogPath $SerialLogPath
  if ($null -eq $line) { return }

  $toks = $line.Split("|")
  $status = "INFO"
  if ($toks.Count -ge 4) {
    $s = $toks[3].Trim().ToUpperInvariant()
    if ($s -eq "PASS" -or $s -eq "FAIL" -or $s -eq "SKIP" -or $s -eq "INFO") {
      $status = $s
    }
  }

  $fields = @{}
  foreach ($tok in $toks) {
    $idx = $tok.IndexOf("=")
    if ($idx -le 0) { continue }
    $k = $tok.Substring(0, $idx).Trim()
    $v = $tok.Substring($idx + 1).Trim()
    if (-not [string]::IsNullOrEmpty($k)) {
      $fields[$k] = $v
    }
  }

  $out = "AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_OFFLOAD_CSUM|$status"

  # Keep ordering stable for log scraping.
  $ordered = @(
    "tx_csum",
    "rx_csum",
    "fallback",
    "tx_tcp",
    "tx_udp",
    "rx_tcp",
    "rx_udp",
    "tx_tcp4",
    "tx_tcp6",
    "tx_udp4",
    "tx_udp6",
    "rx_tcp4",
    "rx_tcp6",
    "rx_udp4",
    "rx_udp6"
  )
  $orderedSet = @{}
  foreach ($k in $ordered) { $orderedSet[$k] = $true }

  foreach ($k in $ordered) {
    if ($fields.ContainsKey($k)) {
      $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
    }
  }

  foreach ($k in ($fields.Keys | Where-Object { (-not $orderedSet.ContainsKey($_)) } | Sort-Object)) {
    $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
  }

  Write-Host $out
}

function Try-EmitAeroVirtioNetDiagMarker {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    # Optional: if provided, fall back to parsing the full serial log when the rolling tail buffer does not
    # contain the virtio-net-diag marker (e.g. because the tail was truncated).
    [Parameter(Mandatory = $false)] [string]$SerialLogPath = ""
  )

  $prefix = "virtio-net-diag|"
  $line = Try-ExtractLastAeroMarkerLine -Tail $Tail -Prefix $prefix -SerialLogPath $SerialLogPath
  if ($null -eq $line) { return }

  $toks = $line.Split("|")
  $level = "INFO"
  if ($toks.Count -ge 2) {
    $lvl = $toks[1].Trim().ToUpperInvariant()
    if ($lvl -eq "WARN") { $level = "WARN" }
    elseif ($lvl -eq "INFO") { $level = "INFO" }
  }

  $fields = @{}
  $extras = @()
  # Parse fields after `virtio-net-diag|<LEVEL>|...`.
  for ($i = 2; $i -lt $toks.Count; $i++) {
    $tok = $toks[$i].Trim()
    if ([string]::IsNullOrEmpty($tok)) { continue }
    $idx = $tok.IndexOf("=")
    if ($idx -gt 0) {
      $k = $tok.Substring(0, $idx).Trim()
      $v = $tok.Substring($idx + 1).Trim()
      if (-not [string]::IsNullOrEmpty($k)) {
        $fields[$k] = $v
      }
    } else {
      $extras += $tok
    }
  }
  if ($extras.Count -gt 0) {
    $fields["msg"] = ($extras -join "|")
  }

  $out = "AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_DIAG|$level"

  $ordered = @(
    "reason",
    "host_features",
    "guest_features",
    "irq_mode",
    "irq_message_count",
    "msix_config_vector",
    "msix_rx_vector",
    "msix_tx_vector",
    "rx_queue_size",
    "tx_queue_size",
    "rx_avail_idx",
    "rx_used_idx",
    "tx_avail_idx",
    "tx_used_idx",
    "rx_vq_error_flags",
    "tx_vq_error_flags",
    "tx_csum_v4",
    "tx_csum_v6",
    "tx_udp_csum_v4",
    "tx_udp_csum_v6",
    "tx_tcp_csum_offload_pkts",
    "tx_tcp_csum_fallback_pkts",
    "tx_udp_csum_offload_pkts",
    "tx_udp_csum_fallback_pkts",
    "tx_tso_v4",
    "tx_tso_v6",
    "tx_tso_max_size",
    "ctrl_vq",
    "ctrl_rx",
    "ctrl_vlan",
    "ctrl_mac_addr",
    "ctrl_queue_index",
    "ctrl_queue_size",
    "ctrl_error_flags",
    "ctrl_cmd_sent",
    "ctrl_cmd_ok",
    "ctrl_cmd_err",
    "ctrl_cmd_timeout",
    "perm_mac",
    "cur_mac",
    "link_up",
    "stat_tx_err",
    "stat_rx_err",
    "stat_rx_no_buf",
    "msg"
  )

  $orderedSet = @{}
  foreach ($k in $ordered) { $orderedSet[$k] = $true }

  foreach ($k in $ordered) {
    if ($fields.ContainsKey($k)) {
      $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
    }
  }

  foreach ($k in ($fields.Keys | Where-Object { -not $orderedSet.ContainsKey($_) } | Sort-Object)) {
    $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
  }

  Write-Host $out
}

function Try-EmitAeroVirtioNetMsixMarker {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    # Optional: if provided, fall back to parsing the full serial log when the rolling tail buffer does not
    # contain the marker (e.g. because the tail was truncated).
    [Parameter(Mandatory = $false)] [string]$SerialLogPath = ""
  )

  $prefix = "AERO_VIRTIO_SELFTEST|TEST|virtio-net-msix|"
  $line = Try-ExtractLastAeroMarkerLine -Tail $Tail -Prefix $prefix -SerialLogPath $SerialLogPath
  if ($null -eq $line) { return }

  $toks = $line.Split("|")

  $status = "INFO"
  foreach ($t in $toks) {
    $tt = $t.Trim()
    if ($tt -eq "FAIL") { $status = "FAIL"; break }
    if ($tt -eq "PASS") { $status = "PASS"; break }
    if ($tt -eq "SKIP") { $status = "SKIP"; break }
    if ($tt -eq "INFO") { $status = "INFO" }
  }

  $fields = @{}
  foreach ($tok in $toks) {
    $idx = $tok.IndexOf("=")
    if ($idx -le 0) { continue }
    $k = $tok.Substring(0, $idx).Trim()
    $v = $tok.Substring($idx + 1).Trim()
    if (-not [string]::IsNullOrEmpty($k)) {
      $fields[$k] = $v
    }
  }

  $out = "AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_MSIX|$status"

  # Keep ordering stable for log scraping.
  $ordered = @(
    "mode",
    "messages",
    "config_vector",
    "rx_vector",
    "tx_vector",
    "bytes",
    "reason",
    "err"
  )
  $orderedSet = @{}
  foreach ($k in $ordered) { $orderedSet[$k] = $true }

  foreach ($k in $ordered) {
    if ($fields.ContainsKey($k)) {
      $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
    }
  }

  foreach ($k in ($fields.Keys | Where-Object { (-not $orderedSet.ContainsKey($_)) } | Sort-Object)) {
    $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
  }

  Write-Host $out
}

function Try-EmitAeroVirtioBlkIoMarker {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    # Optional: if provided, fall back to parsing the full serial log when the rolling tail buffer does not
    # contain the virtio-blk marker (e.g. because the tail was truncated).
    [Parameter(Mandatory = $false)] [string]$SerialLogPath = ""
  )

  $prefix = "AERO_VIRTIO_SELFTEST|TEST|virtio-blk|"
  $line = Try-ExtractLastAeroMarkerLine -Tail $Tail -Prefix $prefix -SerialLogPath $SerialLogPath
  if ($null -eq $line) { return }

  $fields = @{}
  foreach ($tok in $line.Split("|")) {
    $idx = $tok.IndexOf("=")
    if ($idx -le 0) { continue }
    $k = $tok.Substring(0, $idx).Trim()
    $v = $tok.Substring($idx + 1).Trim()
    if (-not [string]::IsNullOrEmpty($k)) {
      $fields[$k] = $v
    }
  }

  # Backward compatible: older guest selftests will not include the I/O perf fields. Emit nothing
  # unless we see at least one throughput/byte count key.
  if (-not (
      $fields.ContainsKey("write_bytes") -or
      $fields.ContainsKey("write_mbps") -or
      $fields.ContainsKey("read_bytes") -or
      $fields.ContainsKey("read_mbps")
    )) {
    return
  }

  $status = "INFO"
  if ($line -match "\|FAIL(\||$)") { $status = "FAIL" }
  elseif ($line -match "\|PASS(\||$)") { $status = "PASS" }
  elseif (($fields.ContainsKey("write_ok") -and $fields["write_ok"] -eq "0") -or ($fields.ContainsKey("flush_ok") -and $fields["flush_ok"] -eq "0") -or ($fields.ContainsKey("read_ok") -and $fields["read_ok"] -eq "0")) {
    $status = "FAIL"
  } elseif (($fields.ContainsKey("write_ok") -and $fields["write_ok"] -eq "1") -and ($fields.ContainsKey("flush_ok") -and $fields["flush_ok"] -eq "1") -and ($fields.ContainsKey("read_ok") -and $fields["read_ok"] -eq "1")) {
    $status = "PASS"
  }

  $out = "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_IO|$status"
  foreach ($k in @("write_ok", "write_bytes", "write_mbps", "flush_ok", "read_ok", "read_bytes", "read_mbps")) {
    if ($fields.ContainsKey($k)) {
      $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
    }
  }
  Write-Host $out
}

function Try-EmitAeroVirtioIrqMarkerFromTestMarker {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    # Guest per-test marker name (e.g. virtio-net, virtio-snd, virtio-input).
    [Parameter(Mandatory = $true)] [string]$Device,
    # Host marker token (e.g. VIRTIO_NET_IRQ).
    [Parameter(Mandatory = $true)] [string]$HostMarker,
    # Optional: if provided, fall back to parsing the full serial log when the rolling tail buffer does not
    # contain the marker (e.g. because the tail was truncated).
    [Parameter(Mandatory = $false)] [string]$SerialLogPath = ""
  )

  $prefix = "AERO_VIRTIO_SELFTEST|TEST|$Device|"
  $line = Try-ExtractLastAeroMarkerLine -Tail $Tail -Prefix $prefix -SerialLogPath $SerialLogPath
  if ($null -eq $line) { return }

  $fields = @{}
  foreach ($tok in $line.Split("|")) {
    $idx = $tok.IndexOf("=")
    if ($idx -le 0) { continue }
    $k = $tok.Substring(0, $idx).Trim()
    $v = $tok.Substring($idx + 1).Trim()
    if (-not [string]::IsNullOrEmpty($k)) {
      $fields[$k] = $v
    }
  }

  $irqKeys = @($fields.Keys | Where-Object { $_.StartsWith("irq_") })
  if ($irqKeys.Count -eq 0) { return }

  $status = "INFO"
  if ($line -match "\|FAIL(\||$)") { $status = "FAIL" }
  elseif ($line -match "\|PASS(\||$)") { $status = "PASS" }

  $out = "AERO_VIRTIO_WIN7_HOST|$HostMarker|$status"

  # Keep ordering stable for log scraping.
  foreach ($k in @("irq_mode", "irq_message_count")) {
    if ($fields.ContainsKey($k)) {
      $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
    }
  }

  foreach ($k in ($irqKeys | Where-Object { $_ -ne "irq_mode" -and $_ -ne "irq_message_count" } | Sort-Object)) {
    $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
  }

  Write-Host $out
}

function Try-EmitAeroVirtioInputMsixMarker {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    # Optional: if provided, fall back to parsing the full serial log when the rolling tail buffer does not
    # contain the marker (e.g. because the tail was truncated).
    [Parameter(Mandatory = $false)] [string]$SerialLogPath = ""
  )

  $prefix = "AERO_VIRTIO_SELFTEST|TEST|virtio-input-msix|"
  $line = Try-ExtractLastAeroMarkerLine -Tail $Tail -Prefix $prefix -SerialLogPath $SerialLogPath
  if ($null -eq $line) { return }

  $toks = $line.Split("|")

  $status = "INFO"
  foreach ($t in $toks) {
    $tt = $t.Trim()
    if ($tt -eq "FAIL") { $status = "FAIL"; break }
    if ($tt -eq "PASS") { $status = "PASS"; break }
    if ($tt -eq "SKIP") { $status = "SKIP"; break }
    if ($tt -eq "INFO") { $status = "INFO" }
  }

  $fields = @{}
  foreach ($tok in $toks) {
    $idx = $tok.IndexOf("=")
    if ($idx -le 0) { continue }
    $k = $tok.Substring(0, $idx).Trim()
    $v = $tok.Substring($idx + 1).Trim()
    if (-not [string]::IsNullOrEmpty($k)) {
      $fields[$k] = $v
    }
  }

  $out = "AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_MSIX|$status"

  # Keep ordering stable for log scraping.
  $ordered = @(
    "mode",
    "messages",
    "mapping",
    "used_vectors",
    "config_vector",
    "queue0_vector",
    "queue1_vector",
    "msix_devices",
    "intx_devices",
    "unknown_devices",
    "intx_spurious",
    "total_interrupts",
    "total_dpcs",
    "config_irqs",
    "queue0_irqs",
    "queue1_irqs",
    "reason",
    "err"
  )
  $orderedSet = @{}
  foreach ($k in $ordered) { $orderedSet[$k] = $true }

  foreach ($k in $ordered) {
    if ($fields.ContainsKey($k)) {
      $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
    }
  }

  foreach ($k in ($fields.Keys | Where-Object { (-not $orderedSet.ContainsKey($_)) } | Sort-Object)) {
    $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
  }

  Write-Host $out
}

function Try-EmitAeroVirtioInputBindMarker {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    # Optional: if provided, fall back to parsing the full serial log when the rolling tail buffer does not
    # contain the marker (e.g. because the tail was truncated).
    [Parameter(Mandatory = $false)] [string]$SerialLogPath = ""
  )

  $prefix = "AERO_VIRTIO_SELFTEST|TEST|virtio-input-bind|"
  $line = Try-ExtractLastAeroMarkerLine -Tail $Tail -Prefix $prefix -SerialLogPath $SerialLogPath
  if ($null -eq $line) { return }

  $toks = $line.Split("|")

  $status = "INFO"
  foreach ($t in $toks) {
    $tt = $t.Trim()
    if ($tt -eq "FAIL") { $status = "FAIL"; break }
    if ($tt -eq "PASS") { $status = "PASS"; break }
    if ($tt -eq "SKIP") { $status = "SKIP"; break }
    if ($tt -eq "INFO") { $status = "INFO" }
  }

  $fields = @{}
  foreach ($tok in $toks) {
    $idx = $tok.IndexOf("=")
    if ($idx -le 0) { continue }
    $k = $tok.Substring(0, $idx).Trim()
    $v = $tok.Substring($idx + 1).Trim()
    if (-not [string]::IsNullOrEmpty($k)) {
      $fields[$k] = $v
    }
  }

  $out = "AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_BIND|$status"

  # Keep ordering stable for log scraping.
  $ordered = @(
    "reason",
    "service",
    "expected",
    "actual",
    "pnp_id",
    "devices",
    "wrong_service",
    "missing_service",
    "problem"
  )
  $orderedSet = @{}
  foreach ($k in $ordered) { $orderedSet[$k] = $true }

  foreach ($k in $ordered) {
    if ($fields.ContainsKey($k)) {
      $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
    }
  }

  foreach ($k in ($fields.Keys | Where-Object { (-not $orderedSet.ContainsKey($_)) } | Sort-Object)) {
    $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
  }

  Write-Host $out
}

function Try-EmitAeroVirtioInputBindingMarker {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    # Optional: if provided, fall back to parsing the full serial log when the rolling tail buffer does not
    # contain the marker (e.g. because the tail was truncated).
    [Parameter(Mandatory = $false)] [string]$SerialLogPath = ""
  )
 
  $prefix = "AERO_VIRTIO_SELFTEST|TEST|virtio-input-binding|"
  $line = Try-ExtractLastAeroMarkerLine -Tail $Tail -Prefix $prefix -SerialLogPath $SerialLogPath
  if ($null -eq $line) { return }
 
  $toks = $line.Split("|")
 
  $status = "INFO"
  foreach ($t in $toks) {
    $tt = $t.Trim()
    if ($tt -eq "FAIL") { $status = "FAIL"; break }
    if ($tt -eq "PASS") { $status = "PASS"; break }
    if ($tt -eq "SKIP") { $status = "SKIP"; break }
    if ($tt -eq "INFO") { $status = "INFO" }
  }
 
  $fields = @{}
  foreach ($tok in $toks) {
    $idx = $tok.IndexOf("=")
    if ($idx -le 0) { continue }
    $k = $tok.Substring(0, $idx).Trim()
    $v = $tok.Substring($idx + 1).Trim()
    if (-not [string]::IsNullOrEmpty($k)) {
      $fields[$k] = $v
    }
  }
 
  $out = "AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_BINDING|$status"
 
  # Keep ordering stable for log scraping.
  $ordered = @(
    "reason",
    "expected",
    "actual",
    "service",
    "pnp_id",
    "hwid0",
    "cm_problem",
    "cm_status"
  )
  $orderedSet = @{}
  foreach ($k in $ordered) { $orderedSet[$k] = $true }
 
  foreach ($k in $ordered) {
    if ($fields.ContainsKey($k)) {
      $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
    }
  }
 
  foreach ($k in ($fields.Keys | Where-Object { (-not $orderedSet.ContainsKey($_)) } | Sort-Object)) {
    $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
  }
 
  Write-Host $out
}
 
function Try-EmitAeroVirtioBlkMsixMarker {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    # Optional: if provided, fall back to parsing the full serial log when the rolling tail buffer does not
    # contain the marker (e.g. because the tail was truncated).
    [Parameter(Mandatory = $false)] [string]$SerialLogPath = ""
  )

  $prefix = "AERO_VIRTIO_SELFTEST|TEST|virtio-blk-msix|"
  $line = Try-ExtractLastAeroMarkerLine -Tail $Tail -Prefix $prefix -SerialLogPath $SerialLogPath
  if ($null -eq $line) { return }

  $toks = $line.Split("|")

  $status = "INFO"
  foreach ($t in $toks) {
    $tt = $t.Trim()
    if ($tt -eq "FAIL") { $status = "FAIL"; break }
    if ($tt -eq "PASS") { $status = "PASS"; break }
    if ($tt -eq "SKIP") { $status = "SKIP"; break }
    if ($tt -eq "INFO") { $status = "INFO" }
  }

  $fields = @{}
  foreach ($tok in $toks) {
    $idx = $tok.IndexOf("=")
    if ($idx -le 0) { continue }
    $k = $tok.Substring(0, $idx).Trim()
    $v = $tok.Substring($idx + 1).Trim()
    if (-not [string]::IsNullOrEmpty($k)) {
      $fields[$k] = $v
    }
  }

  $out = "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_MSIX|$status"

  # Keep ordering stable for log scraping.
  $ordered = @(
    "mode",
    "messages",
    "config_vector",
    "queue_vector",
    "returned_len",
    "reason",
    "err"
  )
  $orderedSet = @{}
  foreach ($k in $ordered) { $orderedSet[$k] = $true }

  foreach ($k in $ordered) {
    if ($fields.ContainsKey($k)) {
      $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
    }
  }

  foreach ($k in ($fields.Keys | Where-Object { (-not $orderedSet.ContainsKey($_)) } | Sort-Object)) {
    $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
  }

  Write-Host $out
}

function Try-EmitAeroVirtioSndMsixMarker {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    # Optional: if provided, fall back to parsing the full serial log when the rolling tail buffer does not
    # contain the marker (e.g. because the tail was truncated).
    [Parameter(Mandatory = $false)] [string]$SerialLogPath = ""
  )

  $prefix = "AERO_VIRTIO_SELFTEST|TEST|virtio-snd-msix|"
  $line = Try-ExtractLastAeroMarkerLine -Tail $Tail -Prefix $prefix -SerialLogPath $SerialLogPath
  if ($null -eq $line) { return }

  $toks = $line.Split("|")

  $status = "INFO"
  foreach ($t in $toks) {
    $tt = $t.Trim()
    if ($tt -eq "FAIL") { $status = "FAIL"; break }
    if ($tt -eq "PASS") { $status = "PASS"; break }
    if ($tt -eq "SKIP") { $status = "SKIP"; break }
    if ($tt -eq "INFO") { $status = "INFO" }
  }

  $fields = @{}
  foreach ($tok in $toks) {
    $idx = $tok.IndexOf("=")
    if ($idx -le 0) { continue }
    $k = $tok.Substring(0, $idx).Trim()
    $v = $tok.Substring($idx + 1).Trim()
    if (-not [string]::IsNullOrEmpty($k)) {
      $fields[$k] = $v
    }
  }

  $out = "AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_MSIX|$status"

  # Keep ordering stable for log scraping.
  $ordered = @(
    "mode",
    "messages",
    "config_vector",
    "queue0_vector",
    "queue1_vector",
    "queue2_vector",
    "queue3_vector",
    "interrupts",
    "dpcs",
    "drain0",
    "drain1",
    "drain2",
    "drain3",
    "reason",
    "err"
  )
  $orderedSet = @{}
  foreach ($k in $ordered) { $orderedSet[$k] = $true }

  foreach ($k in $ordered) {
    if ($fields.ContainsKey($k)) {
      $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
    }
  }

  foreach ($k in ($fields.Keys | Where-Object { (-not $orderedSet.ContainsKey($_)) } | Sort-Object)) {
    $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
  }

  Write-Host $out
}

function Try-EmitAeroVirtioSndMarker {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    # Optional: if provided, fall back to parsing the full serial log when the rolling tail buffer does not
    # contain the virtio-snd marker (e.g. because the tail was truncated).
    [Parameter(Mandatory = $false)] [string]$SerialLogPath = ""
  )

  $prefix = "AERO_VIRTIO_SELFTEST|TEST|virtio-snd|"
  $line = Try-ExtractLastAeroMarkerLine -Tail $Tail -Prefix $prefix -SerialLogPath $SerialLogPath
  if ($null -eq $line) { return }

  $toks = $line.Split("|")
  $status = "INFO"
  foreach ($t in $toks) {
    $tt = $t.Trim()
    if ($tt -eq "FAIL") { $status = "FAIL"; break }
    if ($tt -eq "PASS") { $status = "PASS"; break }
    if ($tt -eq "SKIP") { $status = "SKIP"; break }
    if ($tt -eq "INFO") { $status = "INFO" }
  }

  $fields = @{}
  foreach ($tok in $toks) {
    $idx = $tok.IndexOf("=")
    if ($idx -le 0) { continue }
    $k = $tok.Substring(0, $idx).Trim()
    $v = $tok.Substring($idx + 1).Trim()
    if (-not [string]::IsNullOrEmpty($k)) {
      $fields[$k] = $v
    }
  }

  # For FAIL/SKIP markers that use a plain token (e.g. `...|FAIL|force_null_backend|...`),
  # mirror it into reason=... so log scraping can treat it uniformly.
  if (($status -eq "FAIL" -or $status -eq "SKIP") -and (-not $fields.ContainsKey("reason"))) {
    for ($i = 0; $i -lt $toks.Count; $i++) {
      if ($toks[$i].Trim() -eq $status) {
        if ($i + 1 -lt $toks.Count) {
          $reasonTok = $toks[$i + 1].Trim()
          if (-not [string]::IsNullOrEmpty($reasonTok) -and ($reasonTok.IndexOf("=") -lt 0)) {
            $fields["reason"] = $reasonTok
          }
        }
        break
      }
    }
  }

  $out = "AERO_VIRTIO_WIN7_HOST|VIRTIO_SND|$status"
  # Keep ordering stable for log scraping.
  foreach ($k in @("reason")) {
    if ($fields.ContainsKey($k)) {
      $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
    }
  }
  foreach ($k in ($fields.Keys | Where-Object { $_ -ne "reason" } | Sort-Object)) {
    $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
  }
  Write-Host $out
}

function Try-EmitAeroVirtioSndCaptureMarker {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    # Optional: if provided, fall back to parsing the full serial log when the rolling tail buffer does not
    # contain the virtio-snd-capture marker (e.g. because the tail was truncated).
    [Parameter(Mandatory = $false)] [string]$SerialLogPath = ""
  )

  $prefix = "AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|"
  $line = Try-ExtractLastAeroMarkerLine -Tail $Tail -Prefix $prefix -SerialLogPath $SerialLogPath
  if ($null -eq $line) { return }

  $toks = $line.Split("|")
  $status = "INFO"
  foreach ($t in $toks) {
    $tt = $t.Trim()
    if ($tt -eq "FAIL") { $status = "FAIL"; break }
    if ($tt -eq "PASS") { $status = "PASS"; break }
    if ($tt -eq "SKIP") { $status = "SKIP"; break }
    if ($tt -eq "INFO") { $status = "INFO" }
  }

  $fields = @{}
  foreach ($tok in $toks) {
    $idx = $tok.IndexOf("=")
    if ($idx -le 0) { continue }
    $k = $tok.Substring(0, $idx).Trim()
    $v = $tok.Substring($idx + 1).Trim()
    if (-not [string]::IsNullOrEmpty($k)) {
      $fields[$k] = $v
    }
  }

  $out = "AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_CAPTURE|$status"

  # The guest SKIP marker often uses a plain token (e.g. `...|SKIP|endpoint_missing`) rather than
  # a `reason=...` field. Mirror it as `reason=` so log scraping can treat it uniformly.
  if (($status -eq "SKIP" -or $status -eq "FAIL") -and (-not $fields.ContainsKey("reason"))) {
    for ($i = 0; $i -lt $toks.Count; $i++) {
      if ($toks[$i].Trim() -eq $status) {
        if ($i + 1 -lt $toks.Count) {
          $reasonTok = $toks[$i + 1].Trim()
          if (-not [string]::IsNullOrEmpty($reasonTok) -and ($reasonTok.IndexOf("=") -lt 0)) {
            $fields["reason"] = $reasonTok
          }
        }
        break
      }
    }
  }

  foreach ($k in @("method", "frames", "non_silence", "silence_only", "reason", "hr")) {
    if ($fields.ContainsKey($k)) {
      $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
    }
  }

  Write-Host $out
}

function Try-EmitAeroVirtioSndDuplexMarker {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    # Optional: if provided, fall back to parsing the full serial log when the rolling tail buffer does not
    # contain the virtio-snd-duplex marker (e.g. because the tail was truncated).
    [Parameter(Mandatory = $false)] [string]$SerialLogPath = ""
  )

  $prefix = "AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|"
  $line = Try-ExtractLastAeroMarkerLine -Tail $Tail -Prefix $prefix -SerialLogPath $SerialLogPath
  if ($null -eq $line) { return }

  $toks = $line.Split("|")
  $status = "INFO"
  foreach ($t in $toks) {
    $tt = $t.Trim()
    if ($tt -eq "FAIL") { $status = "FAIL"; break }
    if ($tt -eq "PASS") { $status = "PASS"; break }
    if ($tt -eq "SKIP") { $status = "SKIP"; break }
    if ($tt -eq "INFO") { $status = "INFO" }
  }

  $fields = @{}
  foreach ($tok in $toks) {
    $idx = $tok.IndexOf("=")
    if ($idx -le 0) { continue }
    $k = $tok.Substring(0, $idx).Trim()
    $v = $tok.Substring($idx + 1).Trim()
    if (-not [string]::IsNullOrEmpty($k)) {
      $fields[$k] = $v
    }
  }

  $out = "AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_DUPLEX|$status"

  if (($status -eq "SKIP" -or $status -eq "FAIL") -and (-not $fields.ContainsKey("reason"))) {
    for ($i = 0; $i -lt $toks.Count; $i++) {
      if ($toks[$i].Trim() -eq $status) {
        if ($i + 1 -lt $toks.Count) {
          $reasonTok = $toks[$i + 1].Trim()
          if (-not [string]::IsNullOrEmpty($reasonTok) -and ($reasonTok.IndexOf("=") -lt 0)) {
            $fields["reason"] = $reasonTok
          }
        }
        break
      }
    }
  }

  foreach ($k in @("frames", "non_silence", "reason", "hr")) {
    if ($fields.ContainsKey($k)) {
      $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
    }
  }

  Write-Host $out
}

function Try-EmitAeroVirtioSndBufferLimitsMarker {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    # Optional: if provided, fall back to parsing the full serial log when the rolling tail buffer does not
    # contain the virtio-snd-buffer-limits marker (e.g. because the tail was truncated).
    [Parameter(Mandatory = $false)] [string]$SerialLogPath = ""
  )

  $prefix = "AERO_VIRTIO_SELFTEST|TEST|virtio-snd-buffer-limits|"
  $line = Try-ExtractLastAeroMarkerLine -Tail $Tail -Prefix $prefix -SerialLogPath $SerialLogPath
  if ($null -eq $line) { return }

  $toks = $line.Split("|")
  $status = "INFO"
  foreach ($t in $toks) {
    $tt = $t.Trim()
    if ($tt -eq "FAIL") { $status = "FAIL"; break }
    if ($tt -eq "PASS") { $status = "PASS"; break }
    if ($tt -eq "SKIP") { $status = "SKIP"; break }
    if ($tt -eq "INFO") { $status = "INFO" }
  }

  $fields = @{}
  foreach ($tok in $toks) {
    $idx = $tok.IndexOf("=")
    if ($idx -le 0) { continue }
    $k = $tok.Substring(0, $idx).Trim()
    $v = $tok.Substring($idx + 1).Trim()
    if (-not [string]::IsNullOrEmpty($k)) {
      $fields[$k] = $v
    }
  }

  # The guest SKIP/FAIL marker may use a plain token (e.g. `...|SKIP|flag_not_set`) rather than
  # a `reason=...` field. Mirror it as `reason=` so log scraping can treat it uniformly.
  if (($status -eq "SKIP" -or $status -eq "FAIL") -and (-not $fields.ContainsKey("reason"))) {
    for ($i = 0; $i -lt $toks.Count; $i++) {
      if ($toks[$i].Trim() -eq $status) {
        if ($i + 1 -lt $toks.Count) {
          $reasonTok = $toks[$i + 1].Trim()
          if (-not [string]::IsNullOrEmpty($reasonTok) -and ($reasonTok.IndexOf("=") -lt 0)) {
            $fields["reason"] = $reasonTok
          }
        }
        break
      }
    }
  }

  $out = "AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_BUFFER_LIMITS|$status"
  foreach ($k in @("mode", "expected_failure", "buffer_bytes", "init_hr", "hr", "reason")) {
    if ($fields.ContainsKey($k)) {
      $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
    }
  }
  Write-Host $out
}

function Try-EmitAeroVirtioSndEventqMarker {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    # Optional: if provided, fall back to parsing the full serial log when the rolling tail buffer does not
    # contain the virtio-snd-eventq marker (e.g. because the tail was truncated).
    [Parameter(Mandatory = $false)] [string]$SerialLogPath = ""
  )

  $prefix = "AERO_VIRTIO_SELFTEST|TEST|virtio-snd-eventq|"
  $line = Try-ExtractLastAeroMarkerLine -Tail $Tail -Prefix $prefix -SerialLogPath $SerialLogPath
  if ($null -eq $line) { return }
  $toks = $line.Split("|")

  # The guest marker is informational and typically uses INFO or SKIP, but accept PASS/FAIL
  # if a future guest selftest implementation uses them.
  $status = "INFO"
  foreach ($t in $toks) {
    if ($t.Trim() -eq "FAIL") { $status = "FAIL"; break }
    if ($t.Trim() -eq "PASS") { $status = "PASS"; break }
    if ($t.Trim() -eq "SKIP") { $status = "SKIP"; break }
    if ($t.Trim() -eq "INFO") { $status = "INFO" }
  }

  $fields = @{}
  foreach ($tok in $toks) {
    $idx = $tok.IndexOf("=")
    if ($idx -le 0) { continue }
    $k = $tok.Substring(0, $idx).Trim()
    $v = $tok.Substring($idx + 1).Trim()
    if (-not [string]::IsNullOrEmpty($k)) {
      $fields[$k] = $v
    }
  }

  $out = "AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_EVENTQ|$status"

  # The guest SKIP marker uses a plain token (e.g. `...|SKIP|device_missing`) rather than a
  # `reason=...` field. Mirror it as `reason=` so log scraping can treat it uniformly.
  if ($status -eq "SKIP" -and (-not $fields.ContainsKey("reason"))) {
    for ($i = 0; $i -lt $toks.Count; $i++) {
      if ($toks[$i].Trim() -eq "SKIP") {
        if ($i + 1 -lt $toks.Count) {
          $reasonTok = $toks[$i + 1].Trim()
          if (-not [string]::IsNullOrEmpty($reasonTok) -and ($reasonTok.IndexOf("=") -lt 0)) {
            $out += "|reason=$(Sanitize-AeroMarkerValue $reasonTok)"
          }
        }
        break
      }
    }
  }

  # Keep ordering stable for log scraping.
  $ordered = @(
    "completions",
    "parsed",
    "short",
    "unknown",
    "jack_connected",
    "jack_disconnected",
    "pcm_period",
    "xrun",
    "ctl_notify"
  )
  $orderedSet = @{}
  foreach ($k in $ordered) { $orderedSet[$k] = $true }

  foreach ($k in $ordered) {
    if ($fields.ContainsKey($k)) {
      $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
    }
  }

  foreach ($k in ($fields.Keys | Where-Object { (-not $orderedSet.ContainsKey($_)) -and $_ -ne "reason" } | Sort-Object)) {
    $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
  }

  Write-Host $out
}

function Try-EmitAeroVirtioSndFormatMarker {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    # Optional: if provided, fall back to parsing the full serial log when the rolling tail buffer does not
    # contain the virtio-snd-format marker (e.g. because the tail was truncated).
    [Parameter(Mandatory = $false)] [string]$SerialLogPath = ""
  )

  $prefix = "AERO_VIRTIO_SELFTEST|TEST|virtio-snd-format|"
  $line = Try-ExtractLastAeroMarkerLine -Tail $Tail -Prefix $prefix -SerialLogPath $SerialLogPath
  if ($null -eq $line) { return }
  $toks = $line.Split("|")

  $status = "INFO"
  foreach ($t in $toks) {
    $tt = $t.Trim()
    if ($tt -eq "FAIL") { $status = "FAIL"; break }
    if ($tt -eq "PASS") { $status = "PASS"; break }
    if ($tt -eq "SKIP") { $status = "SKIP"; break }
    if ($tt -eq "INFO") { $status = "INFO" }
  }

  $fields = @{}
  foreach ($tok in $toks) {
    $idx = $tok.IndexOf("=")
    if ($idx -le 0) { continue }
    $k = $tok.Substring(0, $idx).Trim()
    $v = $tok.Substring($idx + 1).Trim()
    if (-not [string]::IsNullOrEmpty($k)) {
      $fields[$k] = $v
    }
  }

  $out = "AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_FORMAT|$status"
  foreach ($k in @("render", "capture")) {
    if ($fields.ContainsKey($k)) {
      $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
    }
  }
  Write-Host $out
}

function Try-EmitAeroVirtioIrqDiagnosticsMarkers {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    # Optional: if provided, fall back to parsing the full serial log when the rolling tail buffer does not
    # contain any virtio-*-irq markers (e.g. because the tail was truncated).
    [Parameter(Mandatory = $false)] [string]$SerialLogPath = ""
  )

  # Guest selftest may emit interrupt mode diagnostics markers:
  #   virtio-<dev>-irq|INFO|mode=intx
  #   virtio-<dev>-irq|INFO|mode=msi|messages=<n>      # message interrupts (MSI or MSI-X)
  #   virtio-<dev>-irq|INFO|mode=msix|messages=<n>|...  # richer MSI-X diagnostics when available (e.g. virtio-snd)
  #   virtio-<dev>-irq|INFO|mode=none|...               # polling-only (virtio-snd)
  #   virtio-<dev>-irq|WARN|reason=...|...
  #
  # These are informational by default and do not affect PASS/FAIL; the harness
  # re-emits them as host-side markers for log scraping/diagnostics.
  $parseText = {
    param([Parameter(Mandatory = $true)] [string]$Text)
    $map = @{}
    foreach ($line in ($Text -split "`r`n|`n|`r")) {
      if ($line -match "^\s*virtio-(.+)-irq\|(INFO|WARN)(?:\|(.*))?$") {
        $dev = $Matches[1]
        $level = $Matches[2]
        $rest = $Matches[3]
        $fields = @{}
        $extras = @()
        if (-not [string]::IsNullOrEmpty($rest)) {
          foreach ($tok in $rest.Split("|")) {
            if ([string]::IsNullOrEmpty($tok)) { continue }
            $idx = $tok.IndexOf("=")
            if ($idx -gt 0) {
              $k = $tok.Substring(0, $idx).Trim()
              $v = $tok.Substring($idx + 1).Trim()
              if (-not [string]::IsNullOrEmpty($k)) {
                $fields[$k] = $v
              }
            } else {
              $extras += $tok.Trim()
            }
          }
        }
        if ($extras.Count -gt 0) {
          $fields["msg"] = ($extras -join "|")
        }
        $map[$dev] = @{
          Level = $level
          Fields = $fields
        }
      }
    }
    return $map
  }

  $byDev = & $parseText $Tail

  # If a serial log path is provided, optionally merge markers from the full file so early
  # virtio-*-irq lines are not lost when the rolling tail buffer is truncated.
  #
  # We only do this when:
  # - no markers were found in the tail (e.g. guest printed them early), OR
  # - the tail buffer hit the cap (likely truncated).
  $tailWasTruncated = $Tail.Length -ge 131072
  if ((-not [string]::IsNullOrEmpty($SerialLogPath)) -and (Test-Path -LiteralPath $SerialLogPath) -and ($byDev.Count -eq 0 -or $tailWasTruncated)) {
    try {
      # Avoid reading the entire file into memory; scan line-by-line and keep the last marker per device.
      $fs = [System.IO.File]::Open($SerialLogPath, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, [System.IO.FileShare]::ReadWrite)
      try {
        $sr = [System.IO.StreamReader]::new($fs, [System.Text.Encoding]::UTF8, $true, 4096, $true)
        try {
          while ($true) {
            $line = $sr.ReadLine()
            if ($null -eq $line) { break }
            if ($line -match "^\s*virtio-(.+)-irq\|(INFO|WARN)(?:\|(.*))?$") {
              $dev = $Matches[1]
              $level = $Matches[2]
              $rest = $Matches[3]
              $fields = @{}
              $extras = @()
              if (-not [string]::IsNullOrEmpty($rest)) {
                foreach ($tok in $rest.Split("|")) {
                  if ([string]::IsNullOrEmpty($tok)) { continue }
                  $idx = $tok.IndexOf("=")
                  if ($idx -gt 0) {
                    $k = $tok.Substring(0, $idx).Trim()
                    $v = $tok.Substring($idx + 1).Trim()
                    if (-not [string]::IsNullOrEmpty($k)) {
                      $fields[$k] = $v
                    }
                  } else {
                    $extras += $tok.Trim()
                  }
                }
              }
              if ($extras.Count -gt 0) {
                $fields["msg"] = ($extras -join "|")
              }
              $byDev[$dev] = @{
                Level = $level
                Fields = $fields
              }
            }
          }
        } finally {
          $sr.Dispose()
        }
      } finally {
        $fs.Dispose()
      }
    } catch { }
  }

  foreach ($dev in ($byDev.Keys | Sort-Object)) {
    $info = $byDev[$dev]
    $level = $info.Level
    $fields = $info.Fields
    $name = "VIRTIO_$($dev.ToUpperInvariant().Replace('-', '_'))_IRQ_DIAG"
    $out = "AERO_VIRTIO_WIN7_HOST|$name|$level"
    foreach ($k in ($fields.Keys | Sort-Object)) {
      $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
    }
    Write-Host $out
  }
}

function Normalize-AeroVirtioIrqMode {
  param(
    [Parameter(Mandatory = $true)] [string]$Mode
  )

  $m = ([string]$Mode).Trim().ToLowerInvariant().Replace("_", "-")
  if ($m -eq "msi-x") { return "msix" }
  return $m
}

function Get-AeroVirtioIrqModeFamily {
  param(
    [Parameter(Mandatory = $true)] [string]$Mode
  )

  $m = Normalize-AeroVirtioIrqMode $Mode
  if ($m -eq "intx") { return "intx" }
  if ($m -eq "msi" -or $m -eq "msix") { return "msi" }
  return ""
}

function Get-AeroIrqModeFromAeroMarkerLine {
  param(
    [Parameter(Mandatory = $true)] [string]$Line
  )

  $fields = @{}
  foreach ($tok in $Line.Split("|")) {
    $idx = $tok.IndexOf("=")
    if ($idx -le 0) { continue }
    $k = $tok.Substring(0, $idx)
    $v = $tok.Substring($idx + 1)
    if (-not [string]::IsNullOrEmpty($k)) {
      $fields[$k] = $v
    }
  }

  if ($fields.ContainsKey("irq_mode")) { return $fields["irq_mode"] }
  if ($fields.ContainsKey("mode")) {
    $m = $fields["mode"]
    if (-not [string]::IsNullOrEmpty((Get-AeroVirtioIrqModeFamily $m))) { return $m }
  }
  if ($fields.ContainsKey("interrupt_mode")) {
    $m = $fields["interrupt_mode"]
    if (-not [string]::IsNullOrEmpty((Get-AeroVirtioIrqModeFamily $m))) { return $m }
  }

  return $null
}

function Get-AeroVirtioIrqMode {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    [Parameter(Mandatory = $true)] [string]$Device
  )

  $dev = $Device
  if ($dev.StartsWith("virtio-")) { $dev = $dev.Substring(7) }

  # Prefer standalone guest IRQ diagnostics markers:
  #   virtio-<dev>-irq|INFO/WARN|mode=intx/msi/msix|...
  $re = [regex]::new("(?m)^\s*virtio-" + [regex]::Escape($dev) + "-irq\|(INFO|WARN)(?:\|(?<rest>.*))?$")
  $matches = $re.Matches($Tail)
  if ($matches.Count -gt 0) {
    $rest = $matches[$matches.Count - 1].Groups["rest"].Value
    if (-not [string]::IsNullOrEmpty($rest)) {
      foreach ($tok in $rest.Split("|")) {
        if ($tok -match "^mode=(.+)$") {
          return $Matches[1]
        }
      }
    }
  }

  # virtio-blk has additional marker variants in some guest builds.
  if ($Device -eq "virtio-blk") {
    foreach ($prefix in @(
        "AERO_VIRTIO_SELFTEST|TEST|virtio-blk-irq|",
        "AERO_VIRTIO_SELFTEST|MARKER|virtio-blk-irq|",
        "AERO_VIRTIO_SELFTEST|TEST|virtio-blk|IRQ|",
        "AERO_VIRTIO_SELFTEST|TEST|virtio-blk|INFO|"
      )) {
      $mm = [regex]::Matches($Tail, [regex]::Escape($prefix) + "[^`r`n]*")
      if ($mm.Count -gt 0) {
        $line = $mm[$mm.Count - 1].Value
        $mode = Get-AeroIrqModeFromAeroMarkerLine $line
        if (-not [string]::IsNullOrEmpty($mode)) { return $mode }
      }
    }
  }

  # Fall back to per-test marker fields when present.
  $prefix = "AERO_VIRTIO_SELFTEST|TEST|$Device|"
  $mm = [regex]::Matches($Tail, [regex]::Escape($prefix) + "[^`r`n]*")
  if ($mm.Count -gt 0) {
    $line = $mm[$mm.Count - 1].Value
    $mode = Get-AeroIrqModeFromAeroMarkerLine $line
    if (-not [string]::IsNullOrEmpty($mode)) { return $mode }
  }

  return $null
}

function Test-AeroVirtioIrqModeEnforcement {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    [Parameter(Mandatory = $true)] [string[]]$Devices,
    [Parameter(Mandatory = $true)] [ValidateSet("intx", "msi")] [string]$Expected
  )

  foreach ($dev in $Devices) {
    $mode = Get-AeroVirtioIrqMode -Tail $Tail -Device $dev
    $got = ""
    if (-not [string]::IsNullOrEmpty($mode)) {
      $got = Normalize-AeroVirtioIrqMode $mode
    }
    $family = ""
    if (-not [string]::IsNullOrEmpty($got)) {
      $family = Get-AeroVirtioIrqModeFamily $got
    }

    if ([string]::IsNullOrEmpty($family)) {
      return @{ Ok = $false; Device = $dev; Expected = $Expected; Got = $(if ([string]::IsNullOrEmpty($got)) { "unknown" } else { $got }) }
    }
    if ($Expected -eq "intx" -and $family -ne "intx") {
      return @{ Ok = $false; Device = $dev; Expected = $Expected; Got = $got }
    }
    if ($Expected -eq "msi" -and $family -ne "msi") {
      return @{ Ok = $false; Device = $dev; Expected = $Expected; Got = $got }
    }
  }

  return @{ Ok = $true }
}

function Get-AeroVirtioNetOffloadCsumStatsFromTail {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    # Optional: if provided, fall back to parsing the full serial log when the rolling tail buffer does not
    # contain the marker (e.g. because the tail was truncated).
    [Parameter(Mandatory = $false)] [string]$SerialLogPath = ""
  )

  $prefix = "AERO_VIRTIO_SELFTEST|TEST|virtio-net-offload-csum|"
  $line = Try-ExtractLastAeroMarkerLine -Tail $Tail -Prefix $prefix -SerialLogPath $SerialLogPath
  if ($null -eq $line) { return $null }
  $fields = @{}
  foreach ($tok in $line.Split("|")) {
    $idx = $tok.IndexOf("=")
    if ($idx -le 0) { continue }
    $k = $tok.Substring(0, $idx)
    $v = $tok.Substring($idx + 1)
    if (-not [string]::IsNullOrEmpty($k)) {
      $fields[$k] = $v
    }
  }

  $status = "INFO"
  if ($line -match "\|FAIL(\||$)") { $status = "FAIL" }
  elseif ($line -match "\|PASS(\||$)") { $status = "PASS" }

  function Try-ParseU64([string]$v) {
    if ([string]::IsNullOrEmpty($v)) { return $null }
    $v = $v.Trim()
    try {
      if ($v -match "^0[xX]([0-9a-fA-F]+)$") {
        return [UInt64]::Parse($Matches[1], [System.Globalization.NumberStyles]::AllowHexSpecifier)
      }
      return [UInt64]$v
    } catch {
      return $null
    }
  }

  $tx = $null
  $rx = $null
  $fallback = $null
  if ($fields.ContainsKey("tx_csum")) { $tx = Try-ParseU64 $fields["tx_csum"] }
  if ($fields.ContainsKey("rx_csum")) { $rx = Try-ParseU64 $fields["rx_csum"] }
  if ($fields.ContainsKey("fallback")) { $fallback = Try-ParseU64 $fields["fallback"] }
 
  # Best-effort protocol breakdown fields (newer guest drivers/selftest builds).
  $txTcp = $null
  $txUdp = $null
  $rxTcp = $null
  $rxUdp = $null
  $txTcp4 = $null
  $txTcp6 = $null
  $txUdp4 = $null
  $txUdp6 = $null
  $rxTcp4 = $null
  $rxTcp6 = $null
  $rxUdp4 = $null
  $rxUdp6 = $null
 
  if ($fields.ContainsKey("tx_tcp")) { $txTcp = Try-ParseU64 $fields["tx_tcp"] }
  if ($fields.ContainsKey("tx_udp")) { $txUdp = Try-ParseU64 $fields["tx_udp"] }
  if ($fields.ContainsKey("rx_tcp")) { $rxTcp = Try-ParseU64 $fields["rx_tcp"] }
  if ($fields.ContainsKey("rx_udp")) { $rxUdp = Try-ParseU64 $fields["rx_udp"] }
  if ($fields.ContainsKey("tx_tcp4")) { $txTcp4 = Try-ParseU64 $fields["tx_tcp4"] }
  if ($fields.ContainsKey("tx_tcp6")) { $txTcp6 = Try-ParseU64 $fields["tx_tcp6"] }
  if ($fields.ContainsKey("tx_udp4")) { $txUdp4 = Try-ParseU64 $fields["tx_udp4"] }
  if ($fields.ContainsKey("tx_udp6")) { $txUdp6 = Try-ParseU64 $fields["tx_udp6"] }
  if ($fields.ContainsKey("rx_tcp4")) { $rxTcp4 = Try-ParseU64 $fields["rx_tcp4"] }
  if ($fields.ContainsKey("rx_tcp6")) { $rxTcp6 = Try-ParseU64 $fields["rx_tcp6"] }
  if ($fields.ContainsKey("rx_udp4")) { $rxUdp4 = Try-ParseU64 $fields["rx_udp4"] }
  if ($fields.ContainsKey("rx_udp6")) { $rxUdp6 = Try-ParseU64 $fields["rx_udp6"] }

  return [PSCustomObject]@{
    Line     = $line
    Status   = $status
    TxCsum   = $tx
    RxCsum   = $rx
    Fallback = $fallback
    TxTcp    = $txTcp
    TxUdp    = $txUdp
    RxTcp    = $rxTcp
    RxUdp    = $rxUdp
    TxTcp4   = $txTcp4
    TxTcp6   = $txTcp6
    TxUdp4   = $txUdp4
    TxUdp6   = $txUdp6
    RxTcp4   = $rxTcp4
    RxTcp6   = $rxTcp6
    RxUdp4   = $rxUdp4
    RxUdp6   = $rxUdp6
    Fields   = $fields
  }
}

function Invoke-AeroVirtioSndWavVerification {
  param(
    [Parameter(Mandatory = $true)] [string]$WavPath,
    [Parameter(Mandatory = $true)] [int]$PeakThreshold,
    [Parameter(Mandatory = $true)] [int]$RmsThreshold
  )

  $markerPrefix = "AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_WAV"

  try {
    if (-not (Test-Path -LiteralPath $WavPath)) {
      Write-Host "$markerPrefix|FAIL|reason=missing_wav_file|path=$(Sanitize-AeroMarkerValue $WavPath)"
      return $false
    }

    $fi = Get-Item -LiteralPath $WavPath
    if ($fi.Length -le 0) {
      Write-Host "$markerPrefix|FAIL|reason=empty_wav_file|path=$(Sanitize-AeroMarkerValue $WavPath)"
      return $false
    }

    $fs = [System.IO.File]::Open($WavPath, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, [System.IO.FileShare]::ReadWrite)
    $br = [System.IO.BinaryReader]::new($fs, [System.Text.Encoding]::ASCII, $true)

    try {
      $riffBytes = $br.ReadBytes(4)
      if ($riffBytes.Length -lt 4) { throw "wav file too small for RIFF header" }
      $riff = [System.Text.Encoding]::ASCII.GetString($riffBytes)
      if ($riff -ne "RIFF") { throw "missing RIFF header" }
      $null = $br.ReadUInt32() # file size (ignored)
      $waveBytes = $br.ReadBytes(4)
      if ($waveBytes.Length -lt 4) { throw "wav file too small for WAVE header" }
      $wave = [System.Text.Encoding]::ASCII.GetString($waveBytes)
      if ($wave -ne "WAVE") { throw "missing WAVE form type" }

      $fmtFound = $false
      $dataFound = $false
      $formatTag = 0
      $channels = 0
      $sampleRate = 0
      $blockAlign = 0
      $bitsPerSample = 0
      $subformatHex = ""
      $dataOffset = 0L
      $dataSize = 0L

      while (($fs.Position + 8) -le $fs.Length) {
        $chunkIdBytes = $br.ReadBytes(4)
        if ($chunkIdBytes.Length -lt 4) { break }
        $chunkId = [System.Text.Encoding]::ASCII.GetString($chunkIdBytes)
        $chunkSize = [long]$br.ReadUInt32()
        $chunkDataStart = $fs.Position
        $remaining = $fs.Length - $chunkDataStart
        if ($chunkSize -gt $remaining) { $chunkSize = $remaining }

        if ($chunkId -eq "fmt ") {
          if ($chunkSize -lt 16) { throw "fmt chunk too small" }
          $formatTag = [int]$br.ReadUInt16()
          $channels = [int]$br.ReadUInt16()
          $sampleRate = [int]$br.ReadUInt32()
          $null = $br.ReadUInt32() # avg bytes/sec (ignored)
          $blockAlign = [int]$br.ReadUInt16()
          $bitsPerSample = [int]$br.ReadUInt16()
          $fmtFound = $true
          # WAVE_FORMAT_EXTENSIBLE appends an extension that contains the SubFormat GUID.
          if ($formatTag -eq 0xFFFE -and $chunkSize -ge 40) {
            $null = $br.ReadUInt16() # cbSize
            $null = $br.ReadUInt16() # valid bits per sample
            $null = $br.ReadUInt32() # channel mask
            $sub = $br.ReadBytes(16)
            if ($sub.Length -eq 16) {
              $subformatHex = ([System.BitConverter]::ToString($sub).Replace("-", "").ToLowerInvariant())
            }
            $skip = $chunkSize - 40
            if ($skip -gt 0) { $fs.Seek($skip, [System.IO.SeekOrigin]::Current) | Out-Null }
          } else {
            $skip = $chunkSize - 16
            if ($skip -gt 0) { $fs.Seek($skip, [System.IO.SeekOrigin]::Current) | Out-Null }
          }
        } elseif ($chunkId -eq "data") {
          # If QEMU is killed hard, it may never rewrite the data chunk size (placeholder 0).
          # Recover by treating the rest of the file as audio data when it doesn't look like another chunk header.
          $effectiveSize = $chunkSize
          if ($chunkSize -eq 0 -and $remaining -gt 0) {
            $peekLen = 8
            if ($remaining -lt 8) { $peekLen = [int]$remaining }
            $peek = $br.ReadBytes($peekLen)
            $fs.Seek(-$peek.Length, [System.IO.SeekOrigin]::Current) | Out-Null

            $looksLikeChunk = $false
            if ($peek.Length -ge 8) {
              $printable = $true
              for ($i = 0; $i -lt 4; $i++) {
                $b = $peek[$i]
                if ($b -lt 0x20 -or $b -gt 0x7E) { $printable = $false; break }
              }
              if ($printable) {
                $nextSize = [System.BitConverter]::ToUInt32($peek, 4)
                if ($nextSize -le ($remaining - 8)) { $looksLikeChunk = $true }
              }
            }

            if (-not $looksLikeChunk) { $effectiveSize = $remaining }
          }

          if (-not $dataFound -or ($dataSize -eq 0 -and $effectiveSize -gt 0)) {
            $dataOffset = [long]$chunkDataStart
            $dataSize = [long]$effectiveSize
            $dataFound = $true
          }
          $fs.Seek($effectiveSize, [System.IO.SeekOrigin]::Current) | Out-Null
        } else {
          $fs.Seek($chunkSize, [System.IO.SeekOrigin]::Current) | Out-Null
        }

        if (($chunkSize % 2) -eq 1 -and $fs.Position -lt $fs.Length) {
          $fs.Seek(1, [System.IO.SeekOrigin]::Current) | Out-Null
        }
      }

      if (-not $fmtFound) { throw "missing fmt chunk" }
      if (-not $dataFound) { throw "missing data chunk" }

      $pcmGuidHex = "0100000000001000800000aa00389b71"
      $floatGuidHex = "0300000000001000800000aa00389b71"

      $kind = ""
      if ($formatTag -eq 1) {
        $kind = "pcm"
      } elseif ($formatTag -eq 3) {
        $kind = "float"
      } elseif ($formatTag -eq 0xFFFE) {
        if ([string]::IsNullOrEmpty($subformatHex)) {
          Write-Host "$markerPrefix|FAIL|reason=unsupported_extensible_missing_subformat"
          return $false
        }
        if ($subformatHex -eq $pcmGuidHex) {
          $kind = "pcm"
        } elseif ($subformatHex -eq $floatGuidHex) {
          $kind = "float"
        } else {
          Write-Host "$markerPrefix|FAIL|reason=unsupported_extensible_subformat_$(Sanitize-AeroMarkerValue $subformatHex)"
          return $false
        }
      } else {
        Write-Host "$markerPrefix|FAIL|reason=unsupported_format_tag_$formatTag"
        return $false
      }

      if ($kind -eq "pcm" -and ($bitsPerSample -ne 8 -and $bitsPerSample -ne 16 -and $bitsPerSample -ne 24 -and $bitsPerSample -ne 32)) {
        Write-Host "$markerPrefix|FAIL|reason=unsupported_bits_per_sample_$bitsPerSample"
        return $false
      }
      if ($kind -eq "float" -and $bitsPerSample -ne 32) {
        Write-Host "$markerPrefix|FAIL|reason=unsupported_bits_per_sample_$bitsPerSample"
        return $false
      }
      if ($dataSize -le 0) {
        Write-Host "$markerPrefix|FAIL|reason=missing_or_empty_data_chunk"
        return $false
      }

      $fs.Seek($dataOffset, [System.IO.SeekOrigin]::Begin) | Out-Null
      $peakF = 0.0
      $sumSq = 0.0
      $sampleValues = 0L
      $remainingData = $dataSize
      $buf = New-Object byte[] 65536
      $carry = $null
      $sampleBytes = [int]($bitsPerSample / 8)

      while ($remainingData -gt 0) {
        $toRead = [int][Math]::Min($buf.Length, $remainingData)
        $n = $fs.Read($buf, 0, $toRead)
        if ($n -le 0) { break }
        $remainingData -= $n

        # Build a contiguous byte[] for the current chunk (plus any carry from the previous read).
        if ($null -ne $carry) {
          $data = New-Object byte[] ($carry.Length + $n)
          [System.Buffer]::BlockCopy($carry, 0, $data, 0, $carry.Length)
          [System.Buffer]::BlockCopy($buf, 0, $data, $carry.Length, $n)
          $carry = $null
        } else {
          $data = New-Object byte[] $n
          [System.Buffer]::BlockCopy($buf, 0, $data, 0, $n)
        }

        $dataLen = $data.Length
        if ($dataLen -le 0) { continue }

        $limit = ($dataLen / $sampleBytes) * $sampleBytes
        if ($limit -le 0) {
          $carry = $data
          continue
        }

        $rem = $dataLen - $limit
        if ($rem -gt 0) {
          $carry = New-Object byte[] $rem
          [System.Buffer]::BlockCopy($data, $limit, $carry, 0, $rem)
        }

        for ($i = 0; $i -lt $limit; $i += $sampleBytes) {
          $v = 0.0
          if ($kind -eq "pcm") {
            if ($bitsPerSample -eq 8) {
              $raw = ([int]$data[$i]) - 128
              $v = ([double]$raw / 128.0) * 32767.0
            } elseif ($bitsPerSample -eq 16) {
              $val = ([int]$data[$i]) -bor (([int]$data[$i + 1]) -shl 8)
              if ($val -ge 0x8000) { $val -= 0x10000 }
              $v = [double]$val
            } elseif ($bitsPerSample -eq 24) {
              $val = ([int]$data[$i]) -bor (([int]$data[$i + 1]) -shl 8) -bor (([int]$data[$i + 2]) -shl 16)
              if (($val -band 0x800000) -ne 0) { $val -= 0x1000000 }
              $v = ([double]$val / 8388608.0) * 32767.0
            } elseif ($bitsPerSample -eq 32) {
              $val = [System.BitConverter]::ToInt32($data, $i)
              $v = ([double]$val / 2147483648.0) * 32767.0
            }
          } elseif ($kind -eq "float") {
            $f = [System.BitConverter]::ToSingle($data, $i)
            if ([double]::IsNaN($f) -or [double]::IsInfinity($f)) { continue }
            $v = ([double]$f) * 32767.0
          }

          $absVal = if ($v -lt 0.0) { -$v } else { $v }
          if ($absVal -gt $peakF) { $peakF = $absVal }
          $sumSq += ($v * $v)
          $sampleValues++
        }
      }

      $rms = if ($sampleValues -gt 0) { [Math]::Sqrt($sumSq / [double]$sampleValues) } else { 0.0 }
      $rmsI = [int][Math]::Round($rms)
      $frames = if ($channels -gt 0) { [long]($sampleValues / $channels) } else { [long]$sampleValues }

      $peak = [int][Math]::Round($peakF)
      if ($peak -gt $PeakThreshold -or $rms -gt $RmsThreshold) {
        Write-Host "$markerPrefix|PASS|peak=$peak|rms=$rmsI|samples=$frames|sr=$sampleRate|ch=$channels"
        return $true
      }

      Write-Host "$markerPrefix|FAIL|reason=silent_pcm|peak=$peak|rms=$rmsI|samples=$frames|sr=$sampleRate|ch=$channels"
      return $false
    } finally {
      if ($br) { $br.Dispose() }
      if ($fs) { $fs.Dispose() }
    }
  } catch {
    $reason = Sanitize-AeroMarkerValue ($_.Exception.Message)
    if ([string]::IsNullOrEmpty($reason)) { $reason = "exception" }
    Write-Host "$markerPrefix|FAIL|reason=$reason"
    return $false
  }
}

function Write-AeroQemuStderrTail {
  param(
    [Parameter(Mandatory = $true)] [string]$Path
  )

  if (-not (Test-Path -LiteralPath $Path)) { return }

  $lines = @()
  try {
    $lines = Get-Content -LiteralPath $Path -Tail 200 -ErrorAction SilentlyContinue
  } catch {
    return
  }
  if ($lines.Count -eq 0) { return }

  Write-Host "`n--- QEMU stderr tail ---"
  $lines | ForEach-Object { Write-Host $_ }
}

function Write-AeroVirtioInputBindDiagnostics {
  param(
    [Parameter(Mandatory = $true)] [string]$SerialLogPath
  )

  if ([string]::IsNullOrEmpty($SerialLogPath)) { return }
  if (-not (Test-Path -LiteralPath $SerialLogPath)) { return }

  # These lines are emitted by the guest selftest to make virtio-input driver binding issues actionable:
  # - per-device bound service name (`SPDRP_SERVICE`)
  # - missing REV_01 hint (contract-v1 requires x-pci-revision=0x01)
  # - ConfigManagerErrorCode / DN_HAS_PROBLEM
  $matches = @()
  try {
    $matches = Select-String -Path $SerialLogPath -SimpleMatch -Pattern @(
      "virtio-input-bind:",
      "AERO_VIRTIO_SELFTEST|TEST|virtio-input-bind|"
    ) -ErrorAction SilentlyContinue
  } catch {
    return
  }

  if ($matches.Count -eq 0) { return }

  Write-Host "`n--- virtio-input-bind diagnostics ---"
  $matches | Select-Object -Last 50 | ForEach-Object { Write-Host $_.Line }
}

function Get-AeroFreeTcpPort {
  # Reserve an ephemeral port by binding to 0, then immediately releasing it. This is inherently
  # racy, but good enough for a best-effort QMP channel used only for graceful shutdown.
  $listener = $null
  try {
    $listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 0)
    $listener.Start()
    return $listener.LocalEndpoint.Port
  } finally {
    if ($listener) { $listener.Stop() }
  }
}

function Try-AeroQmpQuit {
  param(
    [Parameter(Mandatory = $true)] [string]$Host,
    [Parameter(Mandatory = $true)] [int]$Port
  )

  $deadline = [DateTime]::UtcNow.AddSeconds(2)
  while ([DateTime]::UtcNow -lt $deadline) {
    $client = $null
    try {
      $client = [System.Net.Sockets.TcpClient]::new()
      $client.ReceiveTimeout = 2000
      $client.SendTimeout = 2000
      $client.Connect($Host, $Port)

      $stream = $client.GetStream()
      $reader = [System.IO.StreamReader]::new($stream, [System.Text.Encoding]::UTF8, $false, 4096, $true)
      $writer = [System.IO.StreamWriter]::new($stream, [System.Text.Encoding]::UTF8, 4096, $true)
      $writer.NewLine = "`n"
      $writer.AutoFlush = $true

      # Greeting.
      $null = $reader.ReadLine()
      $writer.WriteLine('{"execute":"qmp_capabilities"}')
      $null = $reader.ReadLine()
      $writer.WriteLine('{"execute":"quit"}')
      return $true
    } catch {
      Start-Sleep -Milliseconds 100
      continue
    } finally {
      if ($client) { $client.Close() }
    }
  }

  return $false
}

function Read-AeroQmpResponse {
  param(
    [Parameter(Mandatory = $true)] [System.IO.StreamReader]$Reader
  )

  while ($true) {
    $line = $Reader.ReadLine()
    if ($null -eq $line) {
      throw "EOF while waiting for QMP response"
    }
    if ([string]::IsNullOrWhiteSpace($line)) { continue }

    $obj = $null
    try {
      $obj = $line | ConvertFrom-Json -ErrorAction Stop
    } catch {
      continue
    }

    if ($obj.PSObject.Properties.Name -contains "return") { return $obj }
    if ($obj.PSObject.Properties.Name -contains "error") { return $obj }
    # Otherwise, ignore async events and keep reading.
  }
}

function Invoke-AeroQmpCommand {
  param(
    [Parameter(Mandatory = $true)] [System.IO.StreamWriter]$Writer,
    [Parameter(Mandatory = $true)] [System.IO.StreamReader]$Reader,
    [Parameter(Mandatory = $true)] $Command
  )

  $Writer.WriteLine(($Command | ConvertTo-Json -Compress -Depth 10))
  $resp = Read-AeroQmpResponse -Reader $Reader
  if ($resp.PSObject.Properties.Name -contains "error") {
    $execName = ""
    try { $execName = [string]$Command.execute } catch { }
    $cls = ""
    $desc = ""
    try { $cls = [string]$resp.error.class } catch { }
    try { $desc = [string]$resp.error.desc } catch { }
    if ([string]::IsNullOrEmpty($desc)) { $desc = "unknown" }
    if (-not [string]::IsNullOrEmpty($cls)) {
      if (-not [string]::IsNullOrEmpty($execName)) {
        throw "QMP command '$execName' failed ($cls): $desc"
      }
      throw "QMP command failed ($cls): $desc"
    }
    if (-not [string]::IsNullOrEmpty($execName)) {
      throw "QMP command '$execName' failed: $desc"
    }
    throw "QMP command failed: $desc"
  }
  return $resp
}

function Test-AeroQmpCommandNotFound {
  param(
    [Parameter(Mandatory = $true)] [string]$Message,
    [Parameter(Mandatory = $true)] [string]$Command
  )

  $m = $Message.ToLowerInvariant()
  $cmd = $Command.ToLowerInvariant()
  if (-not $m.Contains($cmd)) { return $false }

  # Avoid misclassifying "Device ... has not been found" (DeviceNotFound) as a missing QMP command just because the
  # error string also includes "QMP command '<name>' ...".
  if ($m.Contains("devicenotfound")) { return $false }

  if ($m.Contains("commandnotfound")) { return $true }
  if ($m.Contains("unknown command")) { return $true }
  if ($m.Contains("command not found")) { return $true }

  # Match QEMU phrasing:
  #   "The command input-send-event has not been found"
  # and variants with quotes around the command name.
  $cmdEsc = [regex]::Escape($cmd)
  # PowerShell string escaping: `"` inside a double-quoted string must be escaped with backtick (not backslash).
  if ($m -match "\bcommand\s+['`"]?$cmdEsc['`"]?\s+has\s+not\s+been\s+found\b") { return $true }
  return $false
}

function Invoke-AeroQmpHumanMonitorCommand {
  param(
    [Parameter(Mandatory = $true)] [System.IO.StreamWriter]$Writer,
    [Parameter(Mandatory = $true)] [System.IO.StreamReader]$Reader,
    [Parameter(Mandatory = $true)] [string]$CommandLine
  )

  $cmd = @{
    execute   = "human-monitor-command"
    arguments = @{
      "command-line" = $CommandLine
    }
  }
  try {
    $null = Invoke-AeroQmpCommand -Writer $Writer -Reader $Reader -Command $cmd
  } catch {
    $msg = ""
    try { $msg = [string]$_.Exception.Message } catch { }
    if (-not [string]::IsNullOrEmpty($msg) -and (Test-AeroQmpCommandNotFound -Message $msg -Command "human-monitor-command")) {
      throw "FAIL: QMP_HMP_FALLBACK_UNSUPPORTED: QEMU QMP does not support human-monitor-command (required for HMP input fallback)"
    }
    throw
  }
}

function Invoke-AeroQmpSendKey {
  param(
    [Parameter(Mandatory = $true)] [System.IO.StreamWriter]$Writer,
    [Parameter(Mandatory = $true)] [System.IO.StreamReader]$Reader,
    [Parameter(Mandatory = $true)] [string[]]$Qcodes,
    [Parameter(Mandatory = $false)] [int]$HoldTimeMs = 50
  )

  $keys = @()
  foreach ($q in $Qcodes) {
    $keys += @{ type = "qcode"; data = $q }
  }

  $cmd = @{
    execute   = "send-key"
    arguments = @{
      keys        = $keys
      "hold-time" = $HoldTimeMs
    }
  }
  $null = Invoke-AeroQmpCommand -Writer $Writer -Reader $Reader -Command $cmd
}

function Compute-AeroVirtioBlkResizeNewBytes {
  param(
    [Parameter(Mandatory = $true)] [UInt64]$OldBytes,
    [Parameter(Mandatory = $true)] [UInt64]$DeltaBytes
  )

  if ($DeltaBytes -le 0) { throw "delta_bytes must be > 0" }
  $newBytes = [UInt64]($OldBytes + $DeltaBytes)

  # Align up to 512 bytes (QEMU typically requires sector alignment).
  $align = 512.0
  if (($newBytes % 512) -ne 0) {
    $newBytes = [UInt64]([math]::Ceiling(([double]$newBytes) / $align) * $align)
  }
  if ($newBytes -le $OldBytes) {
    $newBytes = [UInt64]($OldBytes + 512)
  }
  return $newBytes
}

function Try-AeroQmpResizeVirtioBlk {
  param(
    [Parameter(Mandatory = $true)] [string]$Host,
    [Parameter(Mandatory = $true)] [int]$Port,
    [Parameter(Mandatory = $true)] [UInt64]$OldBytes,
    [Parameter(Mandatory = $true)] [UInt64]$NewBytes,
    [Parameter(Mandatory = $false)] [string]$DriveId = "drive0"
  )

  $deadline = [DateTime]::UtcNow.AddSeconds(5)
  $lastErr = ""
  $backend = "qmp_input_send_event"
  while ([DateTime]::UtcNow -lt $deadline) {
    $client = $null
    try {
      $client = [System.Net.Sockets.TcpClient]::new()
      $client.ReceiveTimeout = 2000
      $client.SendTimeout = 2000
      $client.Connect($Host, $Port)

      $stream = $client.GetStream()
      $reader = [System.IO.StreamReader]::new($stream, [System.Text.Encoding]::UTF8, $false, 4096, $true)
      $writer = [System.IO.StreamWriter]::new($stream, [System.Text.Encoding]::UTF8, 4096, $true)
      $writer.NewLine = "`n"
      $writer.AutoFlush = $true

      # Greeting.
      $null = $reader.ReadLine()
      $null = Invoke-AeroQmpCommand -Writer $writer -Reader $reader -Command @{ execute = "qmp_capabilities" }

      $cmdUsed = ""
      $errBlockdev = ""
      try {
        $null = Invoke-AeroQmpCommand -Writer $writer -Reader $reader -Command @{
          execute   = "blockdev-resize"
          arguments = @{ "node-name" = $DriveId; size = $NewBytes }
        }
        $cmdUsed = "blockdev-resize"
      } catch {
        try { $errBlockdev = [string]$_.Exception.Message } catch { }
        if ([string]::IsNullOrEmpty($errBlockdev)) { $errBlockdev = "unknown" }
        try {
          $null = Invoke-AeroQmpCommand -Writer $writer -Reader $reader -Command @{
            execute   = "block_resize"
            arguments = @{ device = $DriveId; size = $NewBytes }
          }
          $cmdUsed = "block_resize"
          Write-Warning "QMP blockdev-resize failed; falling back to block_resize: $errBlockdev"
        } catch {
          $errLegacy = ""
          try { $errLegacy = [string]$_.Exception.Message } catch { }
          if ([string]::IsNullOrEmpty($errLegacy)) { $errLegacy = "unknown" }
          throw "QMP resize failed: blockdev-resize error=$errBlockdev; block_resize error=$errLegacy"
        }
      }

      Write-Host "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RESIZE|REQUEST|old_bytes=$OldBytes|new_bytes=$NewBytes|qmp_cmd=$cmdUsed"
      return $true
    } catch {
      $lastErr = ""
      try { $lastErr = [string]$_.Exception.Message } catch { }
      Start-Sleep -Milliseconds 100
      continue
    } finally {
      if ($client) { $client.Close() }
    }
  }

  $reason = ""
  try { $reason = Sanitize-AeroMarkerValue $lastErr } catch { }
  if ([string]::IsNullOrEmpty($reason)) { $reason = "qmp_resize_failed" }
  $driveTok = $DriveId
  try { $driveTok = Sanitize-AeroMarkerValue $DriveId } catch { }
  Write-Host "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RESIZE|FAIL|reason=$reason|old_bytes=$OldBytes|new_bytes=$NewBytes|drive_id=$driveTok"
  return $false
}

function Invoke-AeroQmpInputSendEvent {
  param(
    [Parameter(Mandatory = $true)] [System.IO.StreamWriter]$Writer,
    [Parameter(Mandatory = $true)] [System.IO.StreamReader]$Reader,
    [Parameter(Mandatory = $false)] [string]$Device = "",
    [Parameter(Mandatory = $true)] $Events
  )

  # Prefer targeting the virtio input devices by QOM id (so we don't accidentally exercise PS/2),
  # but fall back to broadcasting the event when QEMU rejects the `device` argument.
  $cmd = @{
    execute   = "input-send-event"
    arguments = @{
      events = $Events
    }
  }
  if (-not [string]::IsNullOrEmpty($Device)) {
    $cmd.arguments.device = $Device
  }

  try {
    $null = Invoke-AeroQmpCommand -Writer $Writer -Reader $Reader -Command $cmd
    return $Device
  } catch {
    if ([string]::IsNullOrEmpty($Device)) {
      throw
    }

    # Retry without device routing.
    try {
      $cmd.arguments.Remove("device")
      $null = Invoke-AeroQmpCommand -Writer $Writer -Reader $Reader -Command $cmd
      Write-Warning "QMP input-send-event rejected device='$Device'; falling back to broadcast: $($_.Exception.Message)"
      return $null
    } catch {
      throw "QMP input-send-event failed with device='$Device' and without device: $_"
    }
  }
}

function Try-AeroQmpInjectVirtioInputEvents {
  param(
    [Parameter(Mandatory = $true)] [string]$Host,
    [Parameter(Mandatory = $true)] [int]$Port,
    # Outer retry attempt number (1-based). Included in the emitted host marker so log scraping can
    # correlate guest READY/PASS timing with host injection attempts.
    [Parameter(Mandatory = $true)] [int]$Attempt,
    # If set, inject wheel + horizontal wheel (AC Pan) events in addition to motion/click.
    [Parameter(Mandatory = $false)] [switch]$WithWheel,
    # If set, also inject extended virtio-input events (modifiers/buttons/wheel).
    [Parameter(Mandatory = $false)] [bool]$Extended = $false
  )

  $deadline = [DateTime]::UtcNow.AddSeconds(5)
  $lastErr = ""
  while ([DateTime]::UtcNow -lt $deadline) {
    $client = $null
    try {
      $client = [System.Net.Sockets.TcpClient]::new()
      $client.ReceiveTimeout = 2000
      $client.SendTimeout = 2000
      $client.Connect($Host, $Port)

      $stream = $client.GetStream()
      $reader = [System.IO.StreamReader]::new($stream, [System.Text.Encoding]::UTF8, $false, 4096, $true)
      $writer = [System.IO.StreamWriter]::new($stream, [System.Text.Encoding]::UTF8, 4096, $true)
      $writer.NewLine = "`n"
      $writer.AutoFlush = $true

      # Greeting.
      $null = $reader.ReadLine()
      $null = Invoke-AeroQmpCommand -Writer $writer -Reader $reader -Command @{ execute = "qmp_capabilities" }

      $kbdDevice = $script:VirtioInputKeyboardQmpId
      $mouseDevice = $script:VirtioInputMouseQmpId

      try {
        # Preferred path: QMP `input-send-event` (supports virtio `device=` routing on newer QEMU).
        #
        # Keyboard: 'a' down/up.
        $kbdDevice = Invoke-AeroQmpInputSendEvent -Writer $writer -Reader $reader -Device $kbdDevice -Events @(
          @{ type = "key"; data = @{ down = $true; key = @{ type = "qcode"; data = "a" } } }
        )

        Start-Sleep -Milliseconds 50

        $kbdDevice = Invoke-AeroQmpInputSendEvent -Writer $writer -Reader $reader -Device $kbdDevice -Events @(
          @{ type = "key"; data = @{ down = $false; key = @{ type = "qcode"; data = "a" } } }
        )

        Start-Sleep -Milliseconds 50

      # Mouse: move + left click.
      $wantWheel = ([bool]$WithWheel) -or $Extended
      $mouseRelEvents = @(
        @{ type = "rel"; data = @{ axis = "x"; value = 10 } },
        @{ type = "rel"; data = @{ axis = "y"; value = 5 } }
      )
      if ($wantWheel) {
        $mouseRelEvents += @(
          @{ type = "rel"; data = @{ axis = "wheel"; value = 1 } },
          @{ type = "rel"; data = @{ axis = "hscroll"; value = -2 } }
        )
      }
      try {
        $mouseDevice = Invoke-AeroQmpInputSendEvent -Writer $writer -Reader $reader -Device $mouseDevice -Events $mouseRelEvents
      } catch {
        if (-not $wantWheel) { throw }
        $errWheelHscroll = ""
        try { $errWheelHscroll = [string]$_.Exception.Message } catch { }

        # Some QEMU builds use alternate axis names for scroll wheels. Try a best-effort matrix of
        # axis pairs before failing.
        $errors = @{}
        $errors["wheel+hscroll"] = $errWheelHscroll
        $attempts = @(
          @{ name = "wheel+hwheel"; axisMap = @{ "hscroll" = "hwheel" } },
          @{ name = "vscroll+hscroll"; axisMap = @{ "wheel" = "vscroll" } },
          @{ name = "vscroll+hwheel"; axisMap = @{ "wheel" = "vscroll"; "hscroll" = "hwheel" } }
        )

        $ok = $false
        foreach ($att in $attempts) {
          $evs = @()
          foreach ($ev in $mouseRelEvents) {
            if ($ev.type -eq "rel" -and $att.axisMap.ContainsKey($ev.data.axis)) {
              $evs += @{ type = "rel"; data = @{ axis = $att.axisMap[$ev.data.axis]; value = $ev.data.value } }
            } else {
              $evs += $ev
            }
          }

          try {
            $mouseDevice = Invoke-AeroQmpInputSendEvent -Writer $writer -Reader $reader -Device $mouseDevice -Events $evs
            $ok = $true
            break
          } catch {
            $msg = ""
            try { $msg = [string]$_.Exception.Message } catch { }
            $errors[$att.name] = $msg
            continue
          }
        }

        if (-not $ok) {
          $errorsText = ""
          try {
            $errorsText = ($errors.GetEnumerator() | ForEach-Object { "$($_.Key)=$($_.Value)" }) -join "; "
          } catch {
            try { $errorsText = [string]$errors } catch { $errorsText = "<unavailable>" }
          }
          throw "QMP input-send-event failed while injecting scroll for -WithInputWheel/-WithVirtioInputWheel/-RequireVirtioInputWheel/-EnableVirtioInputWheel or -WithInputEventsExtended/-WithInputEventsExtra. Upgrade QEMU or omit those flags. errors=[$errorsText]"
        }
      }

        Start-Sleep -Milliseconds 50

        $mouseDevice = Invoke-AeroQmpInputSendEvent -Writer $writer -Reader $reader -Device $mouseDevice -Events @(
          @{ type = "btn"; data = @{ down = $true; button = "left" } }
        )

        Start-Sleep -Milliseconds 50

        $mouseDevice = Invoke-AeroQmpInputSendEvent -Writer $writer -Reader $reader -Device $mouseDevice -Events @(
          @{ type = "btn"; data = @{ down = $false; button = "left" } }
        )
      } catch {
        # Backcompat: older QEMU builds lack `input-send-event`.
        $msg = ""
        try { $msg = [string]$_.Exception.Message } catch { }
        if (-not (Test-AeroQmpCommandNotFound -Message $msg -Command "input-send-event")) {
          throw
        }

        $backend = "hmp_fallback"
        $wantWheel = ([bool]$WithWheel) -or $Extended
        if ($wantWheel) {
          throw "QMP input-send-event is required for -WithInputWheel/-WithVirtioInputWheel/-RequireVirtioInputWheel/-EnableVirtioInputWheel or -WithInputEventsExtended/-WithInputEventsExtra, but this QEMU build does not support it. Upgrade QEMU or omit those flags."
        }

        Write-Warning "QMP input-send-event is unavailable; falling back to legacy input injection (send-key / HMP mouse_move/mouse_button)."

        # Keyboard fallback:
        # - Prefer QMP `send-key` (qcodes)
        # - Otherwise HMP `sendkey` via `human-monitor-command`
        try {
          Invoke-AeroQmpSendKey -Writer $writer -Reader $reader -Qcodes @("a") -HoldTimeMs 50
        } catch {
          $msg2 = ""
          try { $msg2 = [string]$_.Exception.Message } catch { }
          if (-not (Test-AeroQmpCommandNotFound -Message $msg2 -Command "send-key")) {
            throw
          }
          Invoke-AeroQmpHumanMonitorCommand -Writer $writer -Reader $reader -CommandLine "sendkey a"
        }

        # Mouse fallback: HMP `mouse_move` + `mouse_button`.
        Invoke-AeroQmpHumanMonitorCommand -Writer $writer -Reader $reader -CommandLine "mouse_move 10 5"
        Invoke-AeroQmpHumanMonitorCommand -Writer $writer -Reader $reader -CommandLine "mouse_button 1"
        Invoke-AeroQmpHumanMonitorCommand -Writer $writer -Reader $reader -CommandLine "mouse_button 0"

        # Legacy fallbacks are broadcast-only.
        $kbdDevice = $null
        $mouseDevice = $null
      }

      if ($Extended) {
        Start-Sleep -Milliseconds 50

        # Keyboard: modifiers + function key.
        $kbdExtra = @(
          # Shift + b.
          @{ type = "key"; data = @{ down = $true; key = @{ type = "qcode"; data = "shift" } } },
          @{ type = "key"; data = @{ down = $true; key = @{ type = "qcode"; data = "b" } } },
          @{ type = "key"; data = @{ down = $false; key = @{ type = "qcode"; data = "b" } } },
          @{ type = "key"; data = @{ down = $false; key = @{ type = "qcode"; data = "shift" } } },
          # Ctrl.
          @{ type = "key"; data = @{ down = $true; key = @{ type = "qcode"; data = "ctrl" } } },
          @{ type = "key"; data = @{ down = $false; key = @{ type = "qcode"; data = "ctrl" } } },
          # Alt.
          @{ type = "key"; data = @{ down = $true; key = @{ type = "qcode"; data = "alt" } } },
          @{ type = "key"; data = @{ down = $false; key = @{ type = "qcode"; data = "alt" } } },
          # Function key.
          @{ type = "key"; data = @{ down = $true; key = @{ type = "qcode"; data = "f1" } } },
          @{ type = "key"; data = @{ down = $false; key = @{ type = "qcode"; data = "f1" } } }
        )
        foreach ($evt in $kbdExtra) {
          $kbdDevice = Invoke-AeroQmpInputSendEvent -Writer $writer -Reader $reader -Device $kbdDevice -Events @($evt)
          Start-Sleep -Milliseconds 50
        }

        # Mouse: side/extra buttons.
        $mouseExtra = @(
          @{ type = "btn"; data = @{ down = $true; button = "side" } },
          @{ type = "btn"; data = @{ down = $false; button = "side" } },
          @{ type = "btn"; data = @{ down = $true; button = "extra" } },
          @{ type = "btn"; data = @{ down = $false; button = "extra" } }
        )
        foreach ($evt in $mouseExtra) {
          $mouseDevice = Invoke-AeroQmpInputSendEvent -Writer $writer -Reader $reader -Device $mouseDevice -Events @($evt)
          Start-Sleep -Milliseconds 50
        }
      }

      $kbdMode = if ([string]::IsNullOrEmpty($kbdDevice)) { "broadcast" } else { "device" }
      $mouseMode = if ([string]::IsNullOrEmpty($mouseDevice)) { "broadcast" } else { "device" }
      Write-Host "AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_EVENTS_INJECT|PASS|attempt=$Attempt|backend=$backend|kbd_mode=$kbdMode|mouse_mode=$mouseMode"
      return $true
    } catch {
      try { $lastErr = [string]$_.Exception.Message } catch { }
      if (-not [string]::IsNullOrEmpty($lastErr)) {
        if (Test-AeroQmpCommandNotFound -Message $lastErr -Command "input-send-event") {
          # No backcompat path for absolute pointer (tablet) injection.
          $lastErr = "input-send-event is unavailable; no fallback is available for absolute tablet injection (--with-input-tablet-events)"
          break
        }
      }
      Start-Sleep -Milliseconds 100
      continue
    } finally {
      if ($client) { $client.Close() }
    }
  }

  $reason = "timeout"
  if (-not [string]::IsNullOrEmpty($lastErr)) {
    $reason = Sanitize-AeroMarkerValue $lastErr
  }
  Write-Host "AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_EVENTS_INJECT|FAIL|attempt=$Attempt|backend=$backend|reason=$reason"
  return $false
}

function Try-AeroQmpInjectVirtioInputMediaKeys {
  param(
    [Parameter(Mandatory = $true)] [string]$Host,
    [Parameter(Mandatory = $true)] [int]$Port,
    # Outer retry attempt number (1-based). Included in the emitted host marker so log scraping can
    # correlate guest READY/PASS timing with host injection attempts.
    [Parameter(Mandatory = $true)] [int]$Attempt,
    # QMP QKeyCode qcode to send (default: volumeup). The guest selftest currently validates VolumeUp.
    [Parameter(Mandatory = $false)] [string]$Qcode = "volumeup"
  )

  $deadline = [DateTime]::UtcNow.AddSeconds(5)
  $lastErr = ""
  $backend = "qmp_input_send_event"
  while ([DateTime]::UtcNow -lt $deadline) {
    $client = $null
    try {
      $client = [System.Net.Sockets.TcpClient]::new()
      $client.ReceiveTimeout = 2000
      $client.SendTimeout = 2000
      $client.Connect($Host, $Port)

      $stream = $client.GetStream()
      $reader = [System.IO.StreamReader]::new($stream, [System.Text.Encoding]::UTF8, $false, 4096, $true)
      $writer = [System.IO.StreamWriter]::new($stream, [System.Text.Encoding]::UTF8, 4096, $true)
      $writer.NewLine = "`n"
      $writer.AutoFlush = $true

      # Greeting.
      $null = $reader.ReadLine()
      $null = Invoke-AeroQmpCommand -Writer $writer -Reader $reader -Command @{ execute = "qmp_capabilities" }

      $kbdDevice = $script:VirtioInputKeyboardQmpId

      try {
        # Preferred path: QMP `input-send-event`.
        # Media key: press + release.
        $kbdDevice = Invoke-AeroQmpInputSendEvent -Writer $writer -Reader $reader -Device $kbdDevice -Events @(
          @{ type = "key"; data = @{ down = $true; key = @{ type = "qcode"; data = $Qcode } } }
        )

        Start-Sleep -Milliseconds 50

        $kbdDevice = Invoke-AeroQmpInputSendEvent -Writer $writer -Reader $reader -Device $kbdDevice -Events @(
          @{ type = "key"; data = @{ down = $false; key = @{ type = "qcode"; data = $Qcode } } }
        )
      } catch {
        # Backcompat: older QEMU builds lack `input-send-event`.
        $msg = ""
        try { $msg = [string]$_.Exception.Message } catch { }
        if (-not (Test-AeroQmpCommandNotFound -Message $msg -Command "input-send-event")) {
          throw
        }

        $backend = "hmp_fallback"

        # Keyboard fallback:
        # - Prefer QMP `send-key` (qcodes)
        # - Otherwise HMP `sendkey` via `human-monitor-command`
        try {
          Invoke-AeroQmpSendKey -Writer $writer -Reader $reader -Qcodes @($Qcode) -HoldTimeMs 50
        } catch {
          $msg2 = ""
          try { $msg2 = [string]$_.Exception.Message } catch { }
          if (-not (Test-AeroQmpCommandNotFound -Message $msg2 -Command "send-key")) {
            throw
          }
          Invoke-AeroQmpHumanMonitorCommand -Writer $writer -Reader $reader -CommandLine ("sendkey " + $Qcode)
        }

        # Legacy fallbacks are broadcast-only.
        $kbdDevice = $null
      }

      $kbdMode = if ([string]::IsNullOrEmpty($kbdDevice)) { "broadcast" } else { "device" }
      Write-Host "AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_MEDIA_KEYS_INJECT|PASS|attempt=$Attempt|backend=$backend|kbd_mode=$kbdMode"
      return $true
    } catch {
      try { $lastErr = [string]$_.Exception.Message } catch { }
      Start-Sleep -Milliseconds 100
      continue
    } finally {
      if ($client) { $client.Close() }
    }
  }

  $reason = "timeout"
  if (-not [string]::IsNullOrEmpty($lastErr)) {
    $reason = Sanitize-AeroMarkerValue $lastErr
  }
  Write-Host "AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_MEDIA_KEYS_INJECT|FAIL|attempt=$Attempt|backend=$backend|reason=$reason"
  return $false
}

function Try-AeroQmpInjectVirtioInputTabletEvents {
  param(
    [Parameter(Mandatory = $true)] [string]$Host,
    [Parameter(Mandatory = $true)] [int]$Port,
    # Outer retry attempt number (1-based). Included in the emitted host marker so log scraping can
    # correlate guest READY/PASS timing with host injection attempts.
    [Parameter(Mandatory = $true)] [int]$Attempt
  )

  $deadline = [DateTime]::UtcNow.AddSeconds(5)
  $lastErr = ""
  $backend = "qmp_input_send_event"
  while ([DateTime]::UtcNow -lt $deadline) {
    $client = $null
    try {
      $client = [System.Net.Sockets.TcpClient]::new()
      $client.ReceiveTimeout = 2000
      $client.SendTimeout = 2000
      $client.Connect($Host, $Port)

      $stream = $client.GetStream()
      $reader = [System.IO.StreamReader]::new($stream, [System.Text.Encoding]::UTF8, $false, 4096, $true)
      $writer = [System.IO.StreamWriter]::new($stream, [System.Text.Encoding]::UTF8, 4096, $true)
      $writer.NewLine = "`n"
      $writer.AutoFlush = $true

      # Greeting.
      $null = $reader.ReadLine()
      $null = Invoke-AeroQmpCommand -Writer $writer -Reader $reader -Command @{ execute = "qmp_capabilities" }

      $tabletDevice = $script:VirtioInputTabletQmpId

      if ($backend -eq "hmp_fallback") {
        $lastErr = "FAIL: HMP_INPUT_UNSUPPORTED: HMP fallback does not support tablet/absolute-pointer injection (requires QMP input-send-event)"
        $reason = Sanitize-AeroMarkerValue $lastErr
        Write-Host "AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_TABLET_EVENTS_INJECT|FAIL|attempt=$Attempt|backend=$backend|reason=$reason"
        return $false
      }

      try {
        # Deterministic absolute-pointer move + click sequence.
        #
        # This must match the guest selftest expectations in aero-virtio-selftest.exe.
        # Reset move (0,0) to avoid "no-op" repeats.
        $tabletDevice = Invoke-AeroQmpInputSendEvent -Writer $writer -Reader $reader -Device $tabletDevice -Events @(
          @{ type = "abs"; data = @{ axis = "x"; value = 0 } },
          @{ type = "abs"; data = @{ axis = "y"; value = 0 } }
        )

        Start-Sleep -Milliseconds 50

        # Target move.
        $tabletDevice = Invoke-AeroQmpInputSendEvent -Writer $writer -Reader $reader -Device $tabletDevice -Events @(
          @{ type = "abs"; data = @{ axis = "x"; value = 10000 } },
          @{ type = "abs"; data = @{ axis = "y"; value = 20000 } }
        )

        Start-Sleep -Milliseconds 50

        # Left click down/up.
        $tabletDevice = Invoke-AeroQmpInputSendEvent -Writer $writer -Reader $reader -Device $tabletDevice -Events @(
          @{ type = "btn"; data = @{ down = $true; button = "left" } }
        )

        Start-Sleep -Milliseconds 50

        $tabletDevice = Invoke-AeroQmpInputSendEvent -Writer $writer -Reader $reader -Device $tabletDevice -Events @(
          @{ type = "btn"; data = @{ down = $false; button = "left" } }
        )
      } catch {
        $msg = ""
        try { $msg = [string]$_.Exception.Message } catch { }
        if (Test-AeroQmpCommandNotFound -Message $msg -Command "input-send-event") {
          $backend = "hmp_fallback"
          $lastErr = "FAIL: HMP_INPUT_UNSUPPORTED: HMP fallback does not support tablet/absolute-pointer injection (requires QMP input-send-event)"
          $reason = Sanitize-AeroMarkerValue $lastErr
          Write-Host "AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_TABLET_EVENTS_INJECT|FAIL|attempt=$Attempt|backend=$backend|reason=$reason"
          return $false
        }
        throw
      }

      $tabletMode = if ([string]::IsNullOrEmpty($tabletDevice)) { "broadcast" } else { "device" }
      Write-Host "AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_TABLET_EVENTS_INJECT|PASS|attempt=$Attempt|backend=$backend|tablet_mode=$tabletMode"
      return $true
    } catch {
      try { $lastErr = [string]$_.Exception.Message } catch { }
      Start-Sleep -Milliseconds 100
      continue
    } finally {
      if ($client) { $client.Close() }
    }
  }

  $reason = "timeout"
  if (-not [string]::IsNullOrEmpty($lastErr)) {
    $reason = Sanitize-AeroMarkerValue $lastErr
  }
  Write-Host "AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_TABLET_EVENTS_INJECT|FAIL|attempt=$Attempt|backend=$backend|reason=$reason"
  return $false
}

function Try-AeroQmpSetLink {
  param(
    [Parameter(Mandatory = $true)] [string]$Host,
    [Parameter(Mandatory = $true)] [int]$Port,
    # Name(s) to try for QMP `set_link` targeting (device id / netdev id).
    [Parameter(Mandatory = $true)] [string[]]$Names,
    [Parameter(Mandatory = $true)] [bool]$Up
  )

  $deadline = [DateTime]::UtcNow.AddSeconds(5)
  $lastErr = ""
  while ([DateTime]::UtcNow -lt $deadline) {
    $client = $null
    try {
      $client = [System.Net.Sockets.TcpClient]::new()
      $client.ReceiveTimeout = 2000
      $client.SendTimeout = 2000
      $client.Connect($Host, $Port)

      $stream = $client.GetStream()
      $reader = [System.IO.StreamReader]::new($stream, [System.Text.Encoding]::UTF8, $false, 4096, $true)
      $writer = [System.IO.StreamWriter]::new($stream, [System.Text.Encoding]::UTF8, 4096, $true)
      $writer.NewLine = "`n"
      $writer.AutoFlush = $true

      # Greeting.
      $null = $reader.ReadLine()
      $null = Invoke-AeroQmpCommand -Writer $writer -Reader $reader -Command @{ execute = "qmp_capabilities" }

      foreach ($name in $Names) {
        if ([string]::IsNullOrEmpty($name)) { continue }
        $cmd = $null
        if ($name -eq $script:VirtioNetQmpId) {
          # Prefer the stable QOM id we assign (`$script:VirtioNetQmpId`) so QMP set_link targets the
          # intended virtio-net device deterministically.
          $cmd = @{
            execute = "set_link"
            arguments = @{
              name = $script:VirtioNetQmpId
              up   = [bool]$Up
            }
          }
        } else {
          $cmd = @{
            execute = "set_link"
            arguments = @{
              name = $name
              up   = [bool]$Up
            }
          }
        }
        $writer.WriteLine(($cmd | ConvertTo-Json -Compress -Depth 10))
        $resp = Read-AeroQmpResponse -Reader $reader
        if ($resp.PSObject.Properties.Name -contains "return") {
          return @{ Ok = $true; Name = $name; Unsupported = $false; Reason = "" }
        }
        if ($resp.PSObject.Properties.Name -contains "error") {
          $klass = ""
          $desc = ""
          try { $klass = [string]$resp.error.class } catch { }
          try { $desc = [string]$resp.error.desc } catch { }
          if ($klass -eq "CommandNotFound") {
            return @{ Ok = $false; Name = ""; Unsupported = $true; Reason = "CommandNotFound" }
          }
          if (-not [string]::IsNullOrEmpty($desc)) {
            # Some QEMU builds report unknown QMP commands as GenericError with a descriptive message
            # (instead of the structured CommandNotFound class). Treat those as "unsupported" so the
            # caller can emit the stable QMP_SET_LINK_UNSUPPORTED token.
            $msg = "QMP command 'set_link' failed: $desc"
            if (Test-AeroQmpCommandNotFound -Message $msg -Command "set_link") {
              return @{ Ok = $false; Name = ""; Unsupported = $true; Reason = "CommandNotFound" }
            }
            $lastErr = $desc
          }
          continue
        }
      }

      if ([string]::IsNullOrEmpty($lastErr)) { $lastErr = "unknown" }
      return @{ Ok = $false; Name = ""; Unsupported = $false; Reason = $lastErr }
    } catch {
      try { $lastErr = [string]$_.Exception.Message } catch { }
      Start-Sleep -Milliseconds 100
      continue
    } finally {
      if ($client) { $client.Close() }
    }
  }

  if ([string]::IsNullOrEmpty($lastErr)) { $lastErr = "timeout" }
  return @{ Ok = $false; Name = ""; Unsupported = $false; Reason = $lastErr }
}

function Convert-AeroPciInt {
  param(
    [Parameter(Mandatory = $false)] $Value
  )
  if ($null -eq $Value) { return $null }
  if ($Value -is [int] -or $Value -is [long]) { return [int]$Value }

  $s = ([string]$Value).Trim()
  if ([string]::IsNullOrEmpty($s)) { return $null }
  if ($s.StartsWith("0x") -or $s.StartsWith("0X")) {
    try { return [int]([Convert]::ToInt32($s.Substring(2), 16)) } catch { return $null }
  }

  $n = 0
  if ([int]::TryParse($s, [ref]$n)) { return $n }

  # Fallback: bare hex without a prefix (e.g. "1af4").
  try { return [int]([Convert]::ToInt32($s, 16)) } catch { return $null }
}

function Get-AeroPciVendorDeviceFromQueryPciDevice {
  param(
    [Parameter(Mandatory = $true)] $Device
  )

  $vendor = Convert-AeroPciInt $Device.vendor_id
  $device = Convert-AeroPciInt $Device.device_id
  if (($null -ne $vendor) -and ($null -ne $device)) {
    return [pscustomobject]@{ VendorId = $vendor; DeviceId = $device }
  }

  # Some QEMU builds may nest the IDs under an `id` object.
  $idObj = $null
  try { $idObj = $Device.id } catch { }
  if ($null -ne $idObj) {
    if ($idObj -is [System.Collections.IDictionary]) {
      if ($null -eq $vendor) {
        $vendor = Convert-AeroPciInt $idObj["vendor_id"]
        if ($null -eq $vendor) { $vendor = Convert-AeroPciInt $idObj["vendor"] }
      }
      if ($null -eq $device) {
        $device = Convert-AeroPciInt $idObj["device_id"]
        if ($null -eq $device) { $device = Convert-AeroPciInt $idObj["device"] }
      }
    } else {
      if ($null -eq $vendor) {
        $vendor = Convert-AeroPciInt $idObj.vendor_id
        if ($null -eq $vendor) { $vendor = Convert-AeroPciInt $idObj.vendor }
      }
      if ($null -eq $device) {
        $device = Convert-AeroPciInt $idObj.device_id
        if ($null -eq $device) { $device = Convert-AeroPciInt $idObj.device }
      }
    }
  }

  return [pscustomobject]@{ VendorId = $vendor; DeviceId = $device }
}

function Format-AeroPciBdf {
  param(
    [Parameter(Mandatory = $false)] [Nullable[int]]$Bus,
    [Parameter(Mandatory = $false)] [Nullable[int]]$Slot,
    [Parameter(Mandatory = $false)] [Nullable[int]]$Function
  )
  if (($null -eq $Bus) -or ($null -eq $Slot) -or ($null -eq $Function)) { return "?:?.?" }
  return ("{0:x2}:{1:x2}.{2}" -f $Bus, $Slot, $Function)
}

function Get-AeroPciIdsFromQueryPci {
  param(
    [Parameter(Mandatory = $true)] $QueryPciReturn
  )

  if ($null -eq $QueryPciReturn) { return @() }
  $ids = [System.Collections.ArrayList]::new()

  $seenBuses = @{}

  function Visit-AeroQueryPciBus {
    param(
      [Parameter(Mandatory = $true)] $BusObj,
      [Parameter(Mandatory = $false)] [Nullable[int]]$BusFallback
    )

    if ($null -eq $BusObj) { return }

    $busNum = Convert-AeroPciInt $BusObj.bus
    if ($null -eq $busNum) { $busNum = Convert-AeroPciInt $BusObj.number }
    if ($null -eq $busNum) { $busNum = $BusFallback }

    if ($null -ne $busNum) {
      $k = [string]$busNum
      if ($seenBuses.ContainsKey($k)) { return }
      $seenBuses[$k] = $true
    }

    $devs = $BusObj.devices
    if ($null -eq $devs) { return }
    foreach ($dev in $devs) {
      # Recurse into bridge bus, if present (do this even if we can't parse vendor/device IDs for the
      # bridge itself; some QEMU builds may omit them).
      $childBus = $null
      try { $childBus = $dev.pci_bridge.bus } catch { }
      if ($null -ne $childBus) {
        Visit-AeroQueryPciBus -BusObj $childBus -BusFallback $busNum
      }

      $vd = Get-AeroPciVendorDeviceFromQueryPciDevice -Device $dev
      $vendor = $vd.VendorId
      $device = $vd.DeviceId
      if (($null -eq $vendor) -or ($null -eq $device)) { continue }

      $rev = Convert-AeroPciInt $dev.revision
      $subVendor = Convert-AeroPciInt $dev.subsystem_vendor_id
      $subId = Convert-AeroPciInt $dev.subsystem_id
      if (($null -eq $rev) -or ($null -eq $subVendor) -or ($null -eq $subId)) {
        # Some QEMU builds nest PCI identity fields under an `id` object (query-pci schema).
        $idObj = $null
        try { $idObj = $dev.id } catch { }
        if ($null -ne $idObj) {
          if ($idObj -is [System.Collections.IDictionary]) {
            if ($null -eq $rev) { $rev = Convert-AeroPciInt $idObj["revision"] }
            if ($null -eq $subVendor) {
              $subVendor = Convert-AeroPciInt $idObj["subsystem_vendor_id"]
              if ($null -eq $subVendor) { $subVendor = Convert-AeroPciInt $idObj["subsystem_vendor"] }
            }
            if ($null -eq $subId) {
              $subId = Convert-AeroPciInt $idObj["subsystem_id"]
              if ($null -eq $subId) { $subId = Convert-AeroPciInt $idObj["subsystem"] }
            }
          } else {
            if ($null -eq $rev) { $rev = Convert-AeroPciInt $idObj.revision }
            if ($null -eq $subVendor) {
              $subVendor = Convert-AeroPciInt $idObj.subsystem_vendor_id
              if ($null -eq $subVendor) { $subVendor = Convert-AeroPciInt $idObj.subsystem_vendor }
            }
            if ($null -eq $subId) {
              $subId = Convert-AeroPciInt $idObj.subsystem_id
              if ($null -eq $subId) { $subId = Convert-AeroPciInt $idObj.subsystem }
            }
          }
        }
      }

      $devBus = Convert-AeroPciInt $dev.bus
      if ($null -eq $devBus) { $devBus = $busNum }
      $slot = Convert-AeroPciInt $dev.slot
      $function = Convert-AeroPciInt $dev.function

      $null = $ids.Add([pscustomobject]@{
        VendorId          = $vendor
        DeviceId          = $device
        Revision          = $rev
        SubsystemVendorId = $subVendor
        SubsystemId       = $subId
        Bus               = $devBus
        Slot              = $slot
        Function          = $function
      })
    }
  }

  foreach ($bus in $QueryPciReturn) {
    Visit-AeroQueryPciBus -BusObj $bus -BusFallback $null
  }
  return $ids.ToArray()
}

function Format-AeroPciIdSummary {
  param(
    [Parameter(Mandatory = $true)] $Ids
  )

  $keys = @{}
  foreach ($d in $Ids) {
    $ven = Convert-AeroPciInt $d.VendorId
    $dev = Convert-AeroPciInt $d.DeviceId
    $rev = Convert-AeroPciInt $d.Revision
    if (($null -eq $ven) -or ($null -eq $dev)) { continue }
    $revText = if ($null -eq $rev) { "??" } else { "{0:x2}" -f $rev }
    $k = "{0:x4}:{1:x4}@{2}" -f $ven, $dev, $revText
    $keys[$k] = $true
  }

  # Keys are formatted with fixed-width hex fields (e.g. "1af4:1041@01"), so lexicographic sorting
  # is stable enough and keeps this compatible with Windows PowerShell 5.1 (no Sort-Object -Stable).
  return (($keys.Keys | Sort-Object) -join ",")
}

function Format-AeroPciIdDump {
  param(
    [Parameter(Mandatory = $true)] $Ids,
    [Parameter(Mandatory = $false)] [int]$MaxLines = 32
  )

  $lines = @()
  foreach ($d in ($Ids | Sort-Object VendorId, DeviceId, Bus, Slot, Function)) {
    $ven = Convert-AeroPciInt $d.VendorId
    $dev = Convert-AeroPciInt $d.DeviceId
    if (($null -eq $ven) -or ($null -eq $dev)) { continue }

    $bdf = Format-AeroPciBdf -Bus $d.Bus -Slot $d.Slot -Function $d.Function
    $revText = if ($null -eq $d.Revision) { "?" } else { "0x{0:x2}" -f ([int]$d.Revision) }
    $subText = "?:?"
    if (($null -ne $d.SubsystemVendorId) -and ($null -ne $d.SubsystemId)) {
      $subText = "0x{0:x4}:0x{1:x4}" -f ([int]$d.SubsystemVendorId), ([int]$d.SubsystemId)
    }
    $lines += ("$bdf 0x{0:x4}:0x{1:x4} subsys=$subText rev=$revText" -f $ven, $dev)
  }

  if ($lines.Count -eq 0) { return "  (no devices parsed from query-pci output)" }

  if ($lines.Count -gt $MaxLines) {
    $extra = $lines.Count - $MaxLines
    $lines = @($lines[0..($MaxLines - 1)]) + "... ($extra more)"
  }

  return (($lines | ForEach-Object { "  $_" }) -join "`n")
}

function Test-AeroQmpVirtioPciPreflight {
  param(
    [Parameter(Mandatory = $true)] [string]$Host,
    [Parameter(Mandatory = $true)] [int]$Port,
    [Parameter(Mandatory = $false)] [bool]$VirtioTransitional = $false,
    [Parameter(Mandatory = $false)] [bool]$WithVirtioSnd = $false,
    [Parameter(Mandatory = $false)] [bool]$WithVirtioTablet = $false
  )

  $deadline = [DateTime]::UtcNow.AddSeconds(5)
  $lastErr = ""
  while ([DateTime]::UtcNow -lt $deadline) {
    $client = $null
    try {
      $client = [System.Net.Sockets.TcpClient]::new()
      $client.ReceiveTimeout = 2000
      $client.SendTimeout = 2000
      $client.Connect($Host, $Port)

      $stream = $client.GetStream()
      $reader = [System.IO.StreamReader]::new($stream, [System.Text.Encoding]::UTF8, $false, 4096, $true)
      $writer = [System.IO.StreamWriter]::new($stream, [System.Text.Encoding]::UTF8, 4096, $true)
      $writer.NewLine = "`n"
      $writer.AutoFlush = $true

      # Greeting.
      $null = $reader.ReadLine()
      $null = Invoke-AeroQmpCommand -Writer $writer -Reader $reader -Command @{ execute = "qmp_capabilities" }

      $query = (Invoke-AeroQmpCommand -Writer $writer -Reader $reader -Command @{ execute = "query-pci" }).return
      $ids = Get-AeroPciIdsFromQueryPci -QueryPciReturn $query
      $virtio = @($ids | Where-Object { $_.VendorId -eq 0x1AF4 })

      if ($virtio.Count -eq 0) {
        $dump = Format-AeroPciIdDump -Ids $ids
        return @{
          Ok     = $false
          Reason = "QMP query-pci did not report any virtio PCI devices (expected vendor_id=0x1AF4).`nquery-pci devices:`n$dump"
        }
      }

      if ($VirtioTransitional) {
        $summary = Format-AeroPciIdSummary -Ids $virtio
        Write-Host "AERO_VIRTIO_WIN7_HOST|QEMU_PCI_PREFLIGHT|PASS|mode=transitional|vendor=1af4|devices=$(Sanitize-AeroMarkerValue $summary)"
        return @{ Ok = $true }
      }

      $expectedCounts = @{}
      $expectedCounts[0x1041] = 1
      $expectedCounts[0x1042] = 1
      $expectedCounts[0x1052] = $(if ($WithVirtioTablet) { 3 } else { 2 })
      if ($WithVirtioSnd) { $expectedCounts[0x1059] = 1 }

      $missing = @()
      foreach ($devId in ($expectedCounts.Keys | Sort-Object)) {
        $want = [int]$expectedCounts[$devId]
        $have = @($virtio | Where-Object { $_.DeviceId -eq $devId }).Count
        if ($have -lt $want) {
          $missing += ("DEV_{0:X4} (need>={1}, got={2})" -f $devId, $want, $have)
        }
      }

      $badRev = @(
        $virtio | Where-Object { $expectedCounts.ContainsKey($_.DeviceId) -and ($null -ne $_.Revision) -and $_.Revision -ne 0x01 }
      )
      $missingRev = @(
        $virtio | Where-Object { $expectedCounts.ContainsKey($_.DeviceId) -and $null -eq $_.Revision }
      )
      $revProblems = @($badRev + $missingRev)

      if (($missing.Count -gt 0) -or ($revProblems.Count -gt 0)) {
        $summary = Format-AeroPciIdSummary -Ids $virtio
        $dump = Format-AeroPciIdDump -Ids $virtio
        $expectedStr = ($expectedCounts.Keys | Sort-Object | ForEach-Object { "DEV_{0:X4}" -f $_ }) -join "/"
        $reason = "QEMU PCI preflight failed (expected Aero contract v1 virtio PCI IDs).`nExpected (vendor/device/rev): VEN_1AF4 with $expectedStr and REV_01."
        if ($missing.Count -gt 0) {
          $reason += "`nMissing expected device IDs: " + ($missing -join ", ")
        }
        if ($revProblems.Count -gt 0) {
          $revStr = (
            $revProblems |
              Sort-Object VendorId, DeviceId, Bus, Slot, Function |
              ForEach-Object {
                $r = if ($null -eq $_.Revision) { "??" } else { "{0:X2}" -f ([int]$_.Revision) }
                "{0:X4}:{1:X4}@{2}" -f ([int]$_.VendorId), ([int]$_.DeviceId), $r
              }
          ) -join ", "
          $reason += "`nUnexpected revision IDs (expected REV_01): $revStr"
        }
        $reason += "`nDetected virtio devices (from query-pci):`n$dump`nCompact summary: $summary"
        $reason += "`nHint: in contract-v1 mode the harness expects modern-only virtio-pci devices with disable-legacy=on,x-pci-revision=0x01."
        return @{ Ok = $false; Reason = $reason }
      }

      $matched = @($virtio | Where-Object { $expectedCounts.ContainsKey($_.DeviceId) })
      $summary = Format-AeroPciIdSummary -Ids $matched
      Write-Host "AERO_VIRTIO_WIN7_HOST|QEMU_PCI_PREFLIGHT|PASS|mode=contract-v1|vendor=1af4|devices=$(Sanitize-AeroMarkerValue $summary)"
      return @{ Ok = $true }
    } catch {
      try { $lastErr = [string]$_.Exception.Message } catch { }
      Start-Sleep -Milliseconds 100
      continue
    } finally {
      if ($client) { $client.Close() }
    }
  }

  if ([string]::IsNullOrEmpty($lastErr)) { $lastErr = "timeout" }
  return @{ Ok = $false; Reason = "failed to connect to QMP for PCI preflight: $(Sanitize-AeroMarkerValue $lastErr)" }
}

function Get-AeroPciMsixInfoFromQueryPci {
  param(
    [Parameter(Mandatory = $true)] $QueryPciReturn
  )

  if ($null -eq $QueryPciReturn) { return @() }
  $infos = [System.Collections.ArrayList]::new()

  $seenBuses = @{}

  function Visit-AeroQueryPciBusForMsix {
    param(
      [Parameter(Mandatory = $true)] $BusObj,
      [Parameter(Mandatory = $false)] [Nullable[int]]$BusFallback
    )

    if ($null -eq $BusObj) { return }

    $busNum = Convert-AeroPciInt $BusObj.bus
    if ($null -eq $busNum) { $busNum = Convert-AeroPciInt $BusObj.number }
    if ($null -eq $busNum) { $busNum = $BusFallback }

    if ($null -ne $busNum) {
      $k = [string]$busNum
      if ($seenBuses.ContainsKey($k)) { return }
      $seenBuses[$k] = $true
    }

    $devs = $BusObj.devices
    if ($null -eq $devs) { return }
    foreach ($dev in $devs) {
      # Recurse into bridge bus, if present (do this even if we can't parse vendor/device IDs for the
      # bridge itself; some QEMU builds may omit them).
      $childBus = $null
      try { $childBus = $dev.pci_bridge.bus } catch { }
      if ($null -ne $childBus) {
        Visit-AeroQueryPciBusForMsix -BusObj $childBus -BusFallback $busNum
      }

      $vd = Get-AeroPciVendorDeviceFromQueryPciDevice -Device $dev
      $vendor = $vd.VendorId
      $device = $vd.DeviceId
      if (($null -eq $vendor) -or ($null -eq $device)) { continue }

      $msixEnabled = $null
      try {
        foreach ($cap in $dev.capabilities) {
          $capId = ""
          try { $capId = [string]$cap.id } catch { }
          if (-not [string]::IsNullOrEmpty($capId) -and $capId.ToLowerInvariant() -eq "msix") {
            try { $msixEnabled = [bool]$cap.msix.enabled } catch { }
            break
          }
          # Some QEMU builds may not provide `id` but still include a `msix` object.
          try {
            if ($cap.PSObject.Properties.Name -contains "msix") {
              try { $msixEnabled = [bool]$cap.msix.enabled } catch { }
              break
            }
          } catch { }
        }
      } catch { }

      $devBus = Convert-AeroPciInt $dev.bus
      if ($null -eq $devBus) { $devBus = $busNum }
      $slot = Convert-AeroPciInt $dev.slot
      $function = Convert-AeroPciInt $dev.function

      $null = $infos.Add([pscustomobject]@{
        VendorId    = $vendor
        DeviceId    = $device
        Bus         = $devBus
        Slot        = $slot
        Function    = $function
        MsixEnabled = $msixEnabled
        Source      = "query-pci"
      })
    }
  }

  foreach ($bus in $QueryPciReturn) {
    Visit-AeroQueryPciBusForMsix -BusObj $bus -BusFallback $null
  }
  return $infos.ToArray()
}

function Get-AeroPciMsixInfoFromInfoPci {
  param(
    [Parameter(Mandatory = $true)] [string]$InfoPciText
  )

  $infos = @()
  $bus = $null
  $slot = $null
  $function = $null
  $vendor = $null
  $device = $null
  $msix = $null

  foreach ($raw in ($InfoPciText -split "`n")) {
    $line = $raw.TrimEnd("`r")
    if ($line -match "^Bus\s+(\d+),\s*device\s+(\d+),\s*function\s+(\d+):") {
      if (($null -ne $vendor) -and ($null -ne $device)) {
        $infos += [pscustomobject]@{
          VendorId    = $vendor
          DeviceId    = $device
          Bus         = $bus
          Slot        = $slot
          Function    = $function
          MsixEnabled = $msix
          Source      = "info pci"
        }
      }
      $bus = Convert-AeroPciInt $Matches[1]
      $slot = Convert-AeroPciInt $Matches[2]
      $function = Convert-AeroPciInt $Matches[3]
      $vendor = $null
      $device = $null
      $msix = $null
      continue
    }

    if ($line -match "\bVendor\s+ID:\s*([0-9a-fA-Fx]+)\s+Device\s+ID:\s*([0-9a-fA-Fx]+)\b") {
      $vendor = Convert-AeroPciInt $Matches[1]
      $device = Convert-AeroPciInt $Matches[2]
      continue
    }

    if (($null -eq $vendor) -or ($null -eq $device)) {
      if ($line -match "\b([0-9a-fA-F]{4}):([0-9a-fA-F]{4})\b") {
        $vendor = Convert-AeroPciInt ("0x" + $Matches[1])
        $device = Convert-AeroPciInt ("0x" + $Matches[2])
      }
    }

    if ($line -match "(?i)msi-x|msix") {
      $low = $line.ToLowerInvariant()
      if ($low.Contains("disabled") -or $low.Contains("enable-") -or $low.Contains("off")) {
        $msix = $false
      } elseif ($low.Contains("enabled") -or $low.Contains("enable+")) {
        $msix = $true
      }
    }
  }
  if (($null -ne $vendor) -and ($null -ne $device)) {
    $infos += [pscustomobject]@{
      VendorId    = $vendor
      DeviceId    = $device
      Bus         = $bus
      Slot        = $slot
      Function    = $function
      MsixEnabled = $msix
      Source      = "info pci"
    }
  }
  return $infos
}

function Test-AeroQmpRequiredVirtioMsix {
  param(
    [Parameter(Mandatory = $true)] [string]$Host,
    [Parameter(Mandatory = $true)] [int]$Port,
    [Parameter(Mandatory = $false)] [bool]$RequireVirtioNetMsix = $false,
    [Parameter(Mandatory = $false)] [bool]$RequireVirtioBlkMsix = $false,
    [Parameter(Mandatory = $false)] [bool]$RequireVirtioSndMsix = $false
  )

  if (-not ($RequireVirtioNetMsix -or $RequireVirtioBlkMsix -or $RequireVirtioSndMsix)) {
    return @{ Ok = $true }
  }

  $vendorId = 0x1AF4
  $reqs = @()
  if ($RequireVirtioNetMsix) { $reqs += @{ Name = "virtio-net"; Token = "VIRTIO_NET_MSIX_NOT_ENABLED"; DeviceIds = @(0x1041, 0x1000) } }
  if ($RequireVirtioBlkMsix) { $reqs += @{ Name = "virtio-blk"; Token = "VIRTIO_BLK_MSIX_NOT_ENABLED"; DeviceIds = @(0x1042, 0x1001) } }
  if ($RequireVirtioSndMsix) { $reqs += @{ Name = "virtio-snd"; Token = "VIRTIO_SND_MSIX_NOT_ENABLED"; DeviceIds = @(0x1059) } }

  $client = $null
  try {
    $client = [System.Net.Sockets.TcpClient]::new()
    $client.ReceiveTimeout = 2000
    $client.SendTimeout = 2000
    $client.Connect($Host, $Port)

    $stream = $client.GetStream()
    $reader = [System.IO.StreamReader]::new($stream, [System.Text.Encoding]::UTF8, $false, 4096, $true)
    $writer = [System.IO.StreamWriter]::new($stream, [System.Text.Encoding]::UTF8, 4096, $true)
    $writer.NewLine = "`n"
    $writer.AutoFlush = $true

    # Greeting.
    $null = $reader.ReadLine()
    $null = Invoke-AeroQmpCommand -Writer $writer -Reader $reader -Command @{ execute = "qmp_capabilities" }

    $queryInfos = @()
    $infoInfos = @()
    $querySupported = $false
    $infoSupported = $false

    try {
      $query = (Invoke-AeroQmpCommand -Writer $writer -Reader $reader -Command @{ execute = "query-pci" }).return
      $queryInfos = Get-AeroPciMsixInfoFromQueryPci -QueryPciReturn $query
      $querySupported = $true
    } catch { }

    try {
      $txt = (Invoke-AeroQmpCommand -Writer $writer -Reader $reader -Command @{ execute = "human-monitor-command"; arguments = @{ "command-line" = "info pci" } }).return
      if ($null -ne $txt) {
        $infoInfos = Get-AeroPciMsixInfoFromInfoPci -InfoPciText ([string]$txt)
      }
      $infoSupported = $true
    } catch { }

    if ((-not $querySupported) -and (-not $infoSupported)) {
      return @{ Ok = $false; Result = "QMP_MSIX_CHECK_UNSUPPORTED"; Reason = "QEMU QMP does not support query-pci or human-monitor-command (required for MSI-X verification)" }
    }

    foreach ($r in $reqs) {
      $deviceName = [string]$r.Name
      $token = [string]$r.Token
      $deviceIds = [int[]]$r.DeviceIds

      $q = @($queryInfos | Where-Object { $_.VendorId -eq $vendorId -and ($deviceIds -contains $_.DeviceId) })
      $h = @($infoInfos | Where-Object { $_.VendorId -eq $vendorId -and ($deviceIds -contains $_.DeviceId) })
      $any = @($q + $h)

      if ($any.Count -eq 0) {
        $idsStr = ($deviceIds | ForEach-Object { "{0:x4}:{1:x4}" -f $vendorId, $_ }) -join ","
        return @{ Ok = $false; Result = $token; Reason = "did not find $deviceName PCI function(s) ($idsStr) in QEMU PCI introspection output" }
      }

      $matches = $null
      if (($q.Count -gt 0) -and (@($q | Where-Object { $null -eq $_.MsixEnabled }).Count -eq 0)) {
        $matches = $q
      } elseif (($h.Count -gt 0) -and (@($h | Where-Object { $null -eq $_.MsixEnabled }).Count -eq 0)) {
        $matches = $h
      }

      if ($null -eq $matches) {
        $idsStr = ($deviceIds | ForEach-Object { "{0:x4}:{1:x4}" -f $vendorId, $_ }) -join ","
        $bdf = Format-AeroPciBdf -Bus ($any[0].Bus) -Slot ($any[0].Slot) -Function ($any[0].Function)
        return @{ Ok = $false; Result = "QMP_MSIX_CHECK_UNSUPPORTED"; Reason = "could not determine MSI-X enabled state for $deviceName PCI function(s) ($idsStr) (example_bdf=$bdf)" }
      }

      $disabled = @($matches | Where-Object { -not $_.MsixEnabled })
      if ($disabled.Count -gt 0) {
        $d = $disabled[0]
        $bdf = Format-AeroPciBdf -Bus ($d.Bus) -Slot ($d.Slot) -Function ($d.Function)
        $idStr = ("{0:x4}:{1:x4}" -f $d.VendorId, $d.DeviceId)
        return @{ Ok = $false; Result = $token; Reason = "$deviceName PCI function $idStr at $bdf reported MSI-X disabled (source=$($d.Source))" }
      }
    }

    return @{ Ok = $true }
  } catch {
    $msg = ""
    try { $msg = [string]$_.Exception.Message } catch { }
    if ([string]::IsNullOrEmpty($msg)) { $msg = "unknown" }
    return @{ Ok = $false; Result = "QMP_MSIX_CHECK_FAILED"; Reason = "failed to query PCI MSI-X state via QMP: $msg" }
  } finally {
    if ($client) { $client.Close() }
  }
}

if ($DryRun) {
  # Dry-run should not require that the disk image exists; compute a best-effort absolute path so the
  # printed argv is still useful in CI/debugging environments.
  try { $DiskImagePath = [System.IO.Path]::GetFullPath($DiskImagePath) } catch { }
  try { $SerialLogPath = [System.IO.Path]::GetFullPath($SerialLogPath) } catch { }
  if (Test-Path -LiteralPath $DiskImagePath -PathType Container) {
    throw "-DiskImagePath must be a disk image file path (got a directory): $DiskImagePath"
  }
  if (Test-Path -LiteralPath $SerialLogPath -PathType Container) {
    throw "-SerialLogPath must be a file path (got a directory): $SerialLogPath"
  }
} else {
  $DiskImagePath = (Resolve-Path -LiteralPath $DiskImagePath).Path
  if (Test-Path -LiteralPath $DiskImagePath -PathType Container) {
    throw "-DiskImagePath must be a disk image file path (got a directory): $DiskImagePath"
  }

  $serialParent = Split-Path -Parent $SerialLogPath
  if ([string]::IsNullOrEmpty($serialParent)) { $serialParent = "." }
  if (-not (Test-Path -LiteralPath $serialParent)) {
    New-Item -ItemType Directory -Path $serialParent -Force | Out-Null
  }
  $SerialLogPath = Join-Path (Resolve-Path -LiteralPath $serialParent).Path (Split-Path -Leaf $SerialLogPath)

  if (Test-Path -LiteralPath $SerialLogPath -PathType Container) {
    throw "-SerialLogPath must be a file path (got a directory): $SerialLogPath"
  }
  if (Test-Path -LiteralPath $SerialLogPath) {
    Remove-Item -LiteralPath $SerialLogPath -Force
  }
}

if ($DryRun) {
  function Quote-AeroPsArg {
    param(
      [Parameter(Mandatory = $true)] [string]$Value
    )
    # Single-quote for PowerShell; escape embedded single quotes by doubling.
    return "'" + $Value.Replace("'", "''") + "'"
  }

  $qemuSystemResolved = $QemuSystem
  try {
    $app = Get-Command -Name $QemuSystem -CommandType Application -ErrorAction Stop
    if ($app -and (-not [string]::IsNullOrEmpty($app.Path))) {
      $qemuSystemResolved = $app.Path
    }
  } catch { }

  $needInputWheel = [bool]$WithInputWheel
  $needInputEventsExtended = [bool]$WithInputEventsExtended
  $needInputEvents = ([bool]$WithInputEvents) -or $needInputWheel -or $needInputEventsExtended
  $needInputLeds = [bool]$WithInputLeds
  $needInputMediaKeys = [bool]$WithInputMediaKeys
  $needInputTabletEvents = [bool]$WithInputTabletEvents
  $needNetLinkFlap = [bool]$WithNetLinkFlap
  $needVirtioTablet = [bool]$WithVirtioTablet -or $needInputTabletEvents
  $needBlkResize = [bool]$WithBlkResize

  if ($needBlkResize) {
    if ($VirtioTransitional) {
      throw "-WithBlkResize/-WithVirtioBlkResize/-RequireVirtioBlkResize/-EnableVirtioBlkResize is incompatible with -VirtioTransitional (blk resize uses the contract-v1 drive layout with id=drive0)"
    }
    if ($BlkResizeDeltaMiB -le 0) {
      throw "-BlkResizeDeltaMiB must be a positive integer when -WithBlkResize/-WithVirtioBlkResize/-RequireVirtioBlkResize/-EnableVirtioBlkResize is enabled"
    }
  }

  $requestedVirtioNetVectors = $(if ($VirtioNetVectors -gt 0) { $VirtioNetVectors } else { $VirtioMsixVectors })
  $requestedVirtioBlkVectors = $(if ($VirtioBlkVectors -gt 0) { $VirtioBlkVectors } else { $VirtioMsixVectors })
  $requestedVirtioSndVectors = $(if ($VirtioSndVectors -gt 0) { $VirtioSndVectors } else { $VirtioMsixVectors })
  $requestedVirtioInputVectors = $(if ($VirtioInputVectors -gt 0) { $VirtioInputVectors } else { $VirtioMsixVectors })
  $needMsixCheck = [bool]$RequireVirtioNetMsix -or [bool]$RequireVirtioBlkMsix -or [bool]$RequireVirtioSndMsix

  $qmpPort = $null
  $qmpArgs = @()
  $needQmp = ($WithVirtioSnd -and $VirtioSndAudioBackend -eq "wav") -or $needInputEvents -or $needInputMediaKeys -or $needInputTabletEvents -or $needNetLinkFlap -or $needBlkResize -or $needMsixCheck -or [bool]$QemuPreflightPci
  if ($needQmp) {
    try {
      $qmpPort = Get-AeroFreeTcpPort
      $qmpArgs = @(
        "-qmp", "tcp:127.0.0.1:$qmpPort,server,nowait"
      )
    } catch {
      if ($needInputEvents -or $needInputMediaKeys -or $needInputTabletEvents -or $needNetLinkFlap -or $needBlkResize -or $needMsixCheck -or [bool]$QemuPreflightPci) {
        throw "Failed to allocate QMP port required for QMP-dependent flags (input injection / net link flap / blk resize / msix check / QemuPreflightPci): $_"
      }
      Write-Warning "Failed to allocate QMP port for graceful shutdown: $_"
      $qmpPort = $null
      $qmpArgs = @()
    }
  }

  $serialChardev = "file,id=charserial0,path=$(Quote-AeroWin7QemuKeyvalValue $SerialLogPath)"
  $netdev = "user,id=net0"

  $netVectors = $(if ($requestedVirtioNetVectors -gt 0) { $requestedVirtioNetVectors } else { 0 })
  $blkVectors = $(if ($requestedVirtioBlkVectors -gt 0) { $requestedVirtioBlkVectors } else { 0 })
  $inputVectors = $(if ($requestedVirtioInputVectors -gt 0) { $requestedVirtioInputVectors } else { 0 })

  if ($VirtioTransitional) {
    # Include a stable device id so QMP `set_link` can target the virtio-net device when requested
    # (parity with non-dry-run mode).
    $nic = "virtio-net-pci,id=$($script:VirtioNetQmpId),netdev=net0"
    if ($VirtioDisableMsix) {
      $nic += ",vectors=0"
    } elseif ($netVectors -gt 0) {
      $nic += ",vectors=$netVectors"
    }

    $virtioBlkArgs = @()
    if ($VirtioDisableMsix -or $blkVectors -gt 0) {
      # Use an explicit virtio-blk-pci device so we can apply `vectors=` / `vectors=0`.
      $driveId = "drive0"
      $drive = "file=$(Quote-AeroWin7QemuKeyvalValue $DiskImagePath),if=none,id=$driveId,cache=writeback"
      if ($Snapshot) { $drive += ",snapshot=on" }
      $blk = "virtio-blk-pci,drive=$driveId"
      if ($VirtioDisableMsix) {
        $blk += ",vectors=0"
      } else {
        $blk += ",vectors=$blkVectors"
      }
      $virtioBlkArgs = @(
        "-drive", $drive,
        "-device", $blk
      )
    } else {
      $drive = "file=$(Quote-AeroWin7QemuKeyvalValue $DiskImagePath),if=virtio,cache=writeback"
      if ($Snapshot) { $drive += ",snapshot=on" }
      $virtioBlkArgs = @(
        "-drive", $drive
      )
    }

    $kbdArg = "virtio-keyboard-pci,id=$($script:VirtioInputKeyboardQmpId)"
    $mouseArg = "virtio-mouse-pci,id=$($script:VirtioInputMouseQmpId)"
    if ($VirtioDisableMsix) {
      $kbdArg += ",vectors=0"
      $mouseArg += ",vectors=0"
    } elseif ($inputVectors -gt 0) {
      $kbdArg += ",vectors=$inputVectors"
      $mouseArg += ",vectors=$inputVectors"
    }

    $virtioInputArgs = @(
      "-device", $kbdArg,
      "-device", $mouseArg
    )
    if ($needVirtioTablet) {
      $tabletArg = "virtio-tablet-pci,id=$($script:VirtioInputTabletQmpId)"
      if ($VirtioDisableMsix) {
        $tabletArg += ",vectors=0"
      } elseif ($inputVectors -gt 0) {
        $tabletArg += ",vectors=$inputVectors"
      }
      $virtioInputArgs += @(
        "-device", $tabletArg
      )
    }

    $qemuArgs = @(
      "-m", "$MemoryMB",
      "-smp", "$Smp",
      "-display", "none",
      "-no-reboot"
    ) + $qmpArgs + @(
      "-chardev", $serialChardev,
      "-serial", "chardev:charserial0",
      "-netdev", $netdev,
      "-device", $nic
    ) + $virtioInputArgs + $virtioBlkArgs + $QemuExtraArgs
  } else {
    $nic = New-AeroWin7VirtioNetDeviceArg -NetdevId "net0" -MsixVectors $netVectors -DisableMsix:$VirtioDisableMsix
    $driveId = "drive0"
    $drive = New-AeroWin7VirtioBlkDriveArg -DiskImagePath $DiskImagePath -DriveId $driveId -Snapshot:$Snapshot
    $blk = New-AeroWin7VirtioBlkDeviceArg -DriveId $driveId -MsixVectors $blkVectors -DisableMsix:$VirtioDisableMsix

    $kbd = "$(New-AeroWin7VirtioKeyboardDeviceArg -MsixVectors $inputVectors -DisableMsix:$VirtioDisableMsix),id=$($script:VirtioInputKeyboardQmpId)"
    $mouse = "$(New-AeroWin7VirtioMouseDeviceArg -MsixVectors $inputVectors -DisableMsix:$VirtioDisableMsix),id=$($script:VirtioInputMouseQmpId)"
    $virtioTabletArgs = @()
    if ($needVirtioTablet) {
      $tablet = "$(New-AeroWin7VirtioTabletDeviceArg -MsixVectors $inputVectors -DisableMsix:$VirtioDisableMsix),id=$($script:VirtioInputTabletQmpId)"
      $virtioTabletArgs = @(
        "-device", $tablet
      )
    }

    $virtioSndArgs = @()
    if ($WithVirtioSnd) {
      $audiodev = ""
      switch ($VirtioSndAudioBackend) {
        "none" {
          $audiodev = "none,id=snd0"
        }
        "wav" {
          if ([string]::IsNullOrEmpty($VirtioSndWavPath)) {
            throw "VirtioSndWavPath is required when VirtioSndAudioBackend is 'wav'."
          }
          $wavPath = $VirtioSndWavPath
          try { $wavPath = [System.IO.Path]::GetFullPath($wavPath) } catch { }
          $audiodev = "wav,id=snd0,path=$(Quote-AeroWin7QemuKeyvalValue $wavPath)"
        }
        default {
          throw "Unexpected VirtioSndAudioBackend: $VirtioSndAudioBackend"
        }
      }

      $sndDeviceName = $virtioSndPciDeviceName
      if ([string]::IsNullOrEmpty($sndDeviceName)) { $sndDeviceName = "virtio-sound-pci" }
      $virtioSndDevice = "$sndDeviceName,disable-legacy=on,x-pci-revision=0x01,audiodev=snd0"
      if ($VirtioDisableMsix) {
        $virtioSndDevice += ",vectors=0"
      } elseif ($requestedVirtioSndVectors -gt 0) {
        $virtioSndDevice += ",vectors=$requestedVirtioSndVectors"
      }
      $virtioSndArgs = @(
        "-audiodev", $audiodev,
        "-device", $virtioSndDevice
      )
    }

    $qemuArgs = @(
      "-m", "$MemoryMB",
      "-smp", "$Smp",
      "-display", "none",
      "-no-reboot"
    ) + $qmpArgs + @(
      "-chardev", $serialChardev,
      "-serial", "chardev:charserial0",
      "-netdev", $netdev,
      "-device", $nic,
      "-device", $kbd,
      "-device", $mouse
    ) + $virtioTabletArgs + @(
      "-drive", $drive,
      "-device", $blk
    ) + $virtioSndArgs + $QemuExtraArgs
  }

  # First line: machine-readable JSON argv array (parity with the Python harness dry-run mode).
  $argvJson = ConvertTo-Json -Compress -InputObject (@($qemuSystemResolved) + $qemuArgs)
  Write-Host $argvJson
  Write-Host ""

  Write-Host "DryRun: QEMU argv (one per line):"
  Write-Host "  $qemuSystemResolved"
  $qemuArgs | ForEach-Object { Write-Host "  $_" }
  Write-Host ""
  Write-Host "DryRun: QEMU command (PowerShell quoted):"
  $quoted = @()
  $quoted += (Quote-AeroPsArg $qemuSystemResolved)
  $quoted += ($qemuArgs | ForEach-Object { Quote-AeroPsArg $_ })
  Write-Host ("  & " + ($quoted -join " "))
  exit 0
}

if (-not [string]::IsNullOrEmpty($HttpLogPath)) {
  # Best-effort: never fail the harness due to HTTP log path issues.
  try {
    $httpLogParent = Split-Path -Parent $HttpLogPath
    if ([string]::IsNullOrEmpty($httpLogParent)) { $httpLogParent = "." }
    if (-not (Test-Path -LiteralPath $httpLogParent)) {
      New-Item -ItemType Directory -Path $httpLogParent -Force | Out-Null
    }
    $HttpLogPath = Join-Path (Resolve-Path -LiteralPath $httpLogParent).Path (Split-Path -Leaf $HttpLogPath)
    if (Test-Path -LiteralPath $HttpLogPath) {
      if (Test-Path -LiteralPath $HttpLogPath -PathType Container) {
        throw "HTTP request log path is a directory: $HttpLogPath"
      }
      Remove-Item -LiteralPath $HttpLogPath -Force
    }
    # Create an empty file so CI artifacts include it even if the guest never makes requests.
    [System.IO.File]::WriteAllText($HttpLogPath, "", [System.Text.Encoding]::UTF8)
    Write-Host "HTTP request log enabled: $HttpLogPath"
  } catch {
    Write-Warning "Failed to prepare HTTP request log at '$HttpLogPath' (disabling HTTP log): $_"
    $HttpLogPath = ""
  }
}

$httpListener = $null
$udpSocket = $null

try {
  $qmpPort = $null
  $qmpArgs = @()
  $needInputWheel = [bool]$WithInputWheel
  $needInputEventsExtended = [bool]$WithInputEventsExtended
  $needInputEvents = ([bool]$WithInputEvents) -or $needInputWheel -or $needInputEventsExtended
  $needInputMediaKeys = [bool]$WithInputMediaKeys
  $needInputLed = [bool]$WithInputLed
  $needInputTabletEvents = [bool]$WithInputTabletEvents
  $needNetLinkFlap = [bool]$WithNetLinkFlap
  $needVirtioTablet = [bool]$WithVirtioTablet -or $needInputTabletEvents
  $needBlkResize = [bool]$WithBlkResize

  if ($needBlkResize) {
    if ($VirtioTransitional) {
      throw "-WithBlkResize/-WithVirtioBlkResize/-RequireVirtioBlkResize/-EnableVirtioBlkResize is incompatible with -VirtioTransitional (blk resize uses the contract-v1 drive layout with id=drive0)"
    }
    if ($BlkResizeDeltaMiB -le 0) {
      throw "-BlkResizeDeltaMiB must be a positive integer when -WithBlkResize/-WithVirtioBlkResize/-RequireVirtioBlkResize/-EnableVirtioBlkResize is enabled"
    }
  }
  $requestedVirtioNetVectors = $(if ($VirtioNetVectors -gt 0) { $VirtioNetVectors } else { $VirtioMsixVectors })
  $requestedVirtioBlkVectors = $(if ($VirtioBlkVectors -gt 0) { $VirtioBlkVectors } else { $VirtioMsixVectors })
  $requestedVirtioSndVectors = $(if ($VirtioSndVectors -gt 0) { $VirtioSndVectors } else { $VirtioMsixVectors })
  $requestedVirtioInputVectors = $(if ($VirtioInputVectors -gt 0) { $VirtioInputVectors } else { $VirtioMsixVectors })
  $virtioNetVectorsFlag = $(if ($VirtioNetVectors -gt 0) { "-VirtioNetVectors" } else { "-VirtioMsixVectors" })
  $virtioBlkVectorsFlag = $(if ($VirtioBlkVectors -gt 0) { "-VirtioBlkVectors" } else { "-VirtioMsixVectors" })
  $virtioSndVectorsFlag = $(if ($VirtioSndVectors -gt 0) { "-VirtioSndVectors" } else { "-VirtioMsixVectors" })
  $virtioInputVectorsFlag = $(if ($VirtioInputVectors -gt 0) { "-VirtioInputVectors" } else { "-VirtioMsixVectors" })
  if ($VirtioDisableMsix) {
    $requestedVirtioNetVectors = 0
    $requestedVirtioBlkVectors = 0
    $requestedVirtioSndVectors = 0
    $requestedVirtioInputVectors = 0
    $virtioNetVectorsFlag = "-VirtioDisableMsix"
    $virtioBlkVectorsFlag = "-VirtioDisableMsix"
    $virtioSndVectorsFlag = "-VirtioDisableMsix"
    $virtioInputVectorsFlag = "-VirtioDisableMsix"
  }
  $needMsixCheck = [bool]$RequireVirtioNetMsix -or [bool]$RequireVirtioBlkMsix -or [bool]$RequireVirtioSndMsix
  $needQmp = ($WithVirtioSnd -and $VirtioSndAudioBackend -eq "wav") -or $needInputEvents -or $needInputMediaKeys -or $needInputTabletEvents -or $needNetLinkFlap -or $needBlkResize -or $needMsixCheck -or [bool]$QemuPreflightPci
  if ($needQmp) {
    # QMP channel:
    # - Used for graceful shutdown when using the `wav` audiodev backend (so the RIFF header is finalized).
    # - Also used for virtio-input event injection when input injection flags are enabled
    #   (prefers QMP `input-send-event`, with backcompat fallbacks for keyboard/mouse when unavailable).
    # - Also used for virtio PCI MSI-X enable verification (query-pci / info pci) when -RequireVirtio*Msix is set.
    # - Also used for virtio-blk runtime resize (-WithBlkResize).
    # - Also used for the optional virtio PCI ID preflight (-QemuPreflightPci/-QmpPreflightPci).
    try {
      $qmpPort = Get-AeroFreeTcpPort
      $qmpArgs = @(
        "-qmp", "tcp:127.0.0.1:$qmpPort,server,nowait"
      )
    } catch {
      if ($needInputEvents -or $needInputMediaKeys -or $needInputTabletEvents -or $needNetLinkFlap -or $needBlkResize -or $needMsixCheck -or [bool]$QemuPreflightPci) {
        throw "Failed to allocate QMP port required for QMP-dependent flags (-WithInputEvents/-WithVirtioInputEvents/-RequireVirtioInputEvents/-EnableVirtioInputEvents, -WithInputWheel/-WithVirtioInputWheel/-RequireVirtioInputWheel/-EnableVirtioInputWheel, -WithInputEventsExtended/-WithInputEventsExtra, -WithInputMediaKeys/-WithVirtioInputMediaKeys/-RequireVirtioInputMediaKeys/-EnableVirtioInputMediaKeys, -WithInputTabletEvents/-WithVirtioInputTabletEvents/-RequireVirtioInputTabletEvents/-WithTabletEvents/-EnableTabletEvents, -WithNetLinkFlap/-WithVirtioNetLinkFlap/-RequireVirtioNetLinkFlap/-EnableVirtioNetLinkFlap, -WithBlkResize/-WithVirtioBlkResize/-RequireVirtioBlkResize/-EnableVirtioBlkResize, -RequireNetMsix/-RequireVirtioNetMsix, -RequireBlkMsix/-RequireVirtioBlkMsix, -RequireSndMsix/-RequireVirtioSndMsix) or -QemuPreflightPci/-QmpPreflightPci: $_"
      }
      Write-Warning "Failed to allocate QMP port for graceful shutdown: $_"
      $qmpPort = $null
      $qmpArgs = @()
    }
  }

  $serialChardev = "file,id=charserial0,path=$(Quote-AeroWin7QemuKeyvalValue $SerialLogPath)"
  $netdev = "user,id=net0"
  $serialBase = [System.IO.Path]::GetFileNameWithoutExtension((Split-Path -Leaf $SerialLogPath))
  $qemuStderrPath = Join-Path (Split-Path -Parent $SerialLogPath) "$serialBase.qemu.stderr.log"
  if (Test-Path -LiteralPath $qemuStderrPath) {
    if (Test-Path -LiteralPath $qemuStderrPath -PathType Container) {
      throw "QEMU stderr log path is a directory: $qemuStderrPath"
    }
    Remove-Item -LiteralPath $qemuStderrPath -Force
  }

  if ($VirtioDisableMsix) {
    # Fail fast if the running QEMU build rejects `vectors=0` (used to disable MSI-X and force INTx).
    Assert-AeroWin7QemuAcceptsVectorsZero -QemuSystem $QemuSystem -DeviceName "virtio-net-pci"
    Assert-AeroWin7QemuAcceptsVectorsZero -QemuSystem $QemuSystem -DeviceName "virtio-blk-pci"
    if (-not $VirtioTransitional) {
      Assert-AeroWin7QemuAcceptsVectorsZero -QemuSystem $QemuSystem -DeviceName "virtio-keyboard-pci"
      Assert-AeroWin7QemuAcceptsVectorsZero -QemuSystem $QemuSystem -DeviceName "virtio-mouse-pci"
      if ($needVirtioTablet) {
        Assert-AeroWin7QemuAcceptsVectorsZero -QemuSystem $QemuSystem -DeviceName "virtio-tablet-pci"
      }
    }
  }
  if ($VirtioTransitional) {
    $netVectors = Resolve-AeroWin7QemuMsixVectors -QemuSystem $QemuSystem -DeviceName "virtio-net-pci" -Vectors $requestedVirtioNetVectors -ParamName $virtioNetVectorsFlag -ForceCheck:$VirtioDisableMsix
    $nic = "virtio-net-pci,id=$($script:VirtioNetQmpId),netdev=net0"
    if ($VirtioDisableMsix) {
      $nic += ",vectors=0"
    } elseif ($netVectors -gt 0) {
      $nic += ",vectors=$netVectors"
    }

    $virtioBlkArgs = @()
    $blkVectors = Resolve-AeroWin7QemuMsixVectors -QemuSystem $QemuSystem -DeviceName "virtio-blk-pci" -Vectors $requestedVirtioBlkVectors -ParamName $virtioBlkVectorsFlag -ForceCheck:$VirtioDisableMsix
    if ($VirtioDisableMsix -or $blkVectors -gt 0) {
      # Use an explicit virtio-blk-pci device so we can apply `vectors=` / `vectors=0`.
      $driveId = "drive0"
      $drive = "file=$(Quote-AeroWin7QemuKeyvalValue $DiskImagePath),if=none,id=$driveId,cache=writeback"
      if ($Snapshot) { $drive += ",snapshot=on" }
      $blk = "virtio-blk-pci,drive=$driveId"
      if ($VirtioDisableMsix) {
        $blk += ",vectors=0"
      } else {
        $blk += ",vectors=$blkVectors"
      }
      $virtioBlkArgs = @(
        "-drive", $drive,
        "-device", $blk
      )
    } else {
      $drive = "file=$(Quote-AeroWin7QemuKeyvalValue $DiskImagePath),if=virtio,cache=writeback"
      if ($Snapshot) { $drive += ",snapshot=on" }
      $virtioBlkArgs = @(
        "-drive", $drive
      )
    }

    # Transitional mode is primarily an escape hatch for older QEMU builds (or intentionally
    # testing legacy driver packages). The guest selftest still expects virtio-input devices,
    # so attach virtio-keyboard-pci + virtio-mouse-pci when the QEMU binary supports them.
    $virtioInputArgs = @()
    $haveVirtioKbd = $false
    $haveVirtioMouse = $false
    $haveVirtioTablet = $false
    try {
      $null = Get-AeroWin7QemuDeviceHelpText -QemuSystem $QemuSystem -DeviceName "virtio-keyboard-pci"
      $haveVirtioKbd = $true
    } catch { }
    try {
      $null = Get-AeroWin7QemuDeviceHelpText -QemuSystem $QemuSystem -DeviceName "virtio-mouse-pci"
      $haveVirtioMouse = $true
    } catch { }
    try {
      $null = Get-AeroWin7QemuDeviceHelpText -QemuSystem $QemuSystem -DeviceName "virtio-tablet-pci"
      $haveVirtioTablet = $true
    } catch { }

    if ($needInputEvents -and (-not ($haveVirtioKbd -and $haveVirtioMouse))) {
      throw "QEMU does not advertise virtio-keyboard-pci/virtio-mouse-pci but input injection flags were enabled (-WithInputEvents/-WithVirtioInputEvents/-RequireVirtioInputEvents/-EnableVirtioInputEvents, -WithInputWheel/-WithVirtioInputWheel/-RequireVirtioInputWheel/-EnableVirtioInputWheel, -WithInputEventsExtended/-WithInputEventsExtra). Upgrade QEMU or omit input event injection."
    }
    if ($needInputLeds -and (-not ($haveVirtioKbd -and $haveVirtioMouse))) {
      throw "QEMU does not advertise virtio-keyboard-pci/virtio-mouse-pci but -WithInputLeds/-WithVirtioInputLeds/-RequireVirtioInputLeds/-EnableVirtioInputLeds was enabled. Upgrade QEMU or omit input LED/statusq testing."
    }
    if ($needInputMediaKeys -and (-not $haveVirtioKbd)) {
      throw "QEMU does not advertise virtio-keyboard-pci but -WithInputMediaKeys/-WithVirtioInputMediaKeys/-RequireVirtioInputMediaKeys/-EnableVirtioInputMediaKeys was enabled. Upgrade QEMU or omit media key injection."
    }
    if ($needInputLed -and (-not ($haveVirtioKbd -and $haveVirtioMouse))) {
      throw "QEMU does not advertise virtio-keyboard-pci/virtio-mouse-pci but -WithInputLed/-WithVirtioInputLed/-RequireVirtioInputLed/-EnableVirtioInputLed was enabled. Upgrade QEMU or omit LED/statusq testing."
    }
    if (-not ($haveVirtioKbd -and $haveVirtioMouse)) {
      Write-Warning "QEMU does not advertise virtio-keyboard-pci/virtio-mouse-pci. The guest virtio-input selftest will likely FAIL. Upgrade QEMU or adjust the guest image/selftest expectations."
    }

    if ($haveVirtioKbd) {
      $kbdArg = "virtio-keyboard-pci,id=$($script:VirtioInputKeyboardQmpId)"
      $kbdVectors = Resolve-AeroWin7QemuMsixVectors -QemuSystem $QemuSystem -DeviceName "virtio-keyboard-pci" -Vectors $requestedVirtioInputVectors -ParamName $virtioInputVectorsFlag -ForceCheck:$VirtioDisableMsix
      if ($VirtioDisableMsix) {
        Assert-AeroWin7QemuAcceptsVectorsZero -QemuSystem $QemuSystem -DeviceName "virtio-keyboard-pci"
        $kbdArg += ",vectors=0"
      } elseif ($kbdVectors -gt 0) {
        $kbdArg += ",vectors=$kbdVectors"
      }
      $virtioInputArgs += @(
        "-device", $kbdArg
      )
    }
    if ($haveVirtioMouse) {
      $mouseArg = "virtio-mouse-pci,id=$($script:VirtioInputMouseQmpId)"
      $mouseVectors = Resolve-AeroWin7QemuMsixVectors -QemuSystem $QemuSystem -DeviceName "virtio-mouse-pci" -Vectors $requestedVirtioInputVectors -ParamName $virtioInputVectorsFlag -ForceCheck:$VirtioDisableMsix
      if ($VirtioDisableMsix) {
        Assert-AeroWin7QemuAcceptsVectorsZero -QemuSystem $QemuSystem -DeviceName "virtio-mouse-pci"
        $mouseArg += ",vectors=0"
      } elseif ($mouseVectors -gt 0) {
        $mouseArg += ",vectors=$mouseVectors"
      }
      $virtioInputArgs += @(
        "-device", $mouseArg
      )
    }
    if ($needVirtioTablet) {
      if (-not $haveVirtioTablet) {
        throw "QEMU does not advertise virtio-tablet-pci but -WithVirtioTablet/-WithInputTabletEvents/-WithVirtioInputTabletEvents/-RequireVirtioInputTabletEvents/-WithTabletEvents/-EnableTabletEvents was enabled. Upgrade QEMU or omit tablet support."
      }
      $tabletArg = "virtio-tablet-pci,id=$($script:VirtioInputTabletQmpId)"
      $tabletVectors = Resolve-AeroWin7QemuMsixVectors -QemuSystem $QemuSystem -DeviceName "virtio-tablet-pci" -Vectors $requestedVirtioInputVectors -ParamName $virtioInputVectorsFlag -ForceCheck:$VirtioDisableMsix
      if ($VirtioDisableMsix) {
        Assert-AeroWin7QemuAcceptsVectorsZero -QemuSystem $QemuSystem -DeviceName "virtio-tablet-pci"
        $tabletArg += ",vectors=0"
      } elseif ($tabletVectors -gt 0) {
        $tabletArg += ",vectors=$tabletVectors"
      }
      $virtioInputArgs += @(
        "-device", $tabletArg
      )
    }
    $attachedVirtioInput = ($virtioInputArgs.Count -gt 0)

    $virtioSndArgs = @()
    if ($WithVirtioSnd) {
      $audiodev = ""
      switch ($VirtioSndAudioBackend) {
        "none" {
          $audiodev = "none,id=snd0"
        }
        "wav" {
          if ([string]::IsNullOrEmpty($VirtioSndWavPath)) {
            throw "VirtioSndWavPath is required when VirtioSndAudioBackend is 'wav'."
          }

          $wavParent = Split-Path -Parent $VirtioSndWavPath
          if ([string]::IsNullOrEmpty($wavParent)) { $wavParent = "." }
          if (-not (Test-Path -LiteralPath $wavParent)) {
            New-Item -ItemType Directory -Path $wavParent -Force | Out-Null
          }
          $VirtioSndWavPath = Join-Path (Resolve-Path -LiteralPath $wavParent).Path (Split-Path -Leaf $VirtioSndWavPath)

          if (Test-Path -LiteralPath $VirtioSndWavPath) {
            if (Test-Path -LiteralPath $VirtioSndWavPath -PathType Container) {
              throw "VirtioSndWavPath is a directory: $VirtioSndWavPath"
            }
            Remove-Item -LiteralPath $VirtioSndWavPath -Force
          }

          $audiodev = "wav,id=snd0,path=$(Quote-AeroWin7QemuKeyvalValue $VirtioSndWavPath)"
        }
        default {
          throw "Unexpected VirtioSndAudioBackend: $VirtioSndAudioBackend"
        }
      }

      $virtioSndDevice = Get-AeroVirtioSoundDeviceArg -QemuSystem $QemuSystem -ModernOnly $false -MsixVectors $requestedVirtioSndVectors -DisableMsix:$VirtioDisableMsix -VectorsParamName $virtioSndVectorsFlag -DeviceName $virtioSndPciDeviceName
      $virtioSndArgs = @(
        "-audiodev", $audiodev,
        "-device", $virtioSndDevice
      )
    } elseif (-not [string]::IsNullOrEmpty($VirtioSndWavPath) -or $VirtioSndAudioBackend -ne "none") {
      throw "-VirtioSndAudioBackend/-VirtioSndWavPath require -WithVirtioSnd/-RequireVirtioSnd/-EnableVirtioSnd."
    }

    $qemuArgs = @(
      "-m", "$MemoryMB",
      "-smp", "$Smp",
      "-display", "none",
      "-no-reboot"
    ) + $qmpArgs + @(
      "-chardev", $serialChardev,
      "-serial", "chardev:charserial0",
      "-netdev", $netdev,
      "-device", $nic
    ) + $virtioInputArgs + $virtioBlkArgs + $virtioSndArgs + $QemuExtraArgs
  } else {
    # Ensure the QEMU binary supports the modern-only + contract revision properties we rely on.
    Assert-AeroWin7QemuSupportsAeroW7VirtioContractV1 -QemuSystem $QemuSystem -WithVirtioInput -WithVirtioTablet:$needVirtioTablet
    # Force modern-only virtio-pci IDs (DEV_1041/DEV_1042/DEV_1052) per AERO-W7-VIRTIO v1.
    # The shared QEMU arg helpers also set PCI Revision ID = 0x01 so strict contract-v1
    # drivers bind under QEMU.
    $netVectors = Resolve-AeroWin7QemuMsixVectors -QemuSystem $QemuSystem -DeviceName "virtio-net-pci" -Vectors $requestedVirtioNetVectors -ParamName $virtioNetVectorsFlag -ForceCheck:$VirtioDisableMsix
    $blkVectors = Resolve-AeroWin7QemuMsixVectors -QemuSystem $QemuSystem -DeviceName "virtio-blk-pci" -Vectors $requestedVirtioBlkVectors -ParamName $virtioBlkVectorsFlag -ForceCheck:$VirtioDisableMsix
    $kbdVectors = Resolve-AeroWin7QemuMsixVectors -QemuSystem $QemuSystem -DeviceName "virtio-keyboard-pci" -Vectors $requestedVirtioInputVectors -ParamName $virtioInputVectorsFlag -ForceCheck:$VirtioDisableMsix
    $mouseVectors = Resolve-AeroWin7QemuMsixVectors -QemuSystem $QemuSystem -DeviceName "virtio-mouse-pci" -Vectors $requestedVirtioInputVectors -ParamName $virtioInputVectorsFlag -ForceCheck:$VirtioDisableMsix
    $tabletVectors = 0
    if ($needVirtioTablet) {
      $tabletVectors = Resolve-AeroWin7QemuMsixVectors -QemuSystem $QemuSystem -DeviceName "virtio-tablet-pci" -Vectors $requestedVirtioInputVectors -ParamName $virtioInputVectorsFlag -ForceCheck:$VirtioDisableMsix
    }

    $nic = New-AeroWin7VirtioNetDeviceArg -NetdevId "net0" -QomId $($script:VirtioNetQmpId) -MsixVectors $netVectors -DisableMsix:$VirtioDisableMsix
    $driveId = "drive0"
    $drive = New-AeroWin7VirtioBlkDriveArg -DiskImagePath $DiskImagePath -DriveId $driveId -Snapshot:$Snapshot
    $blk = New-AeroWin7VirtioBlkDeviceArg -DriveId $driveId -MsixVectors $blkVectors -DisableMsix:$VirtioDisableMsix

    $kbd = "$(New-AeroWin7VirtioKeyboardDeviceArg -MsixVectors $kbdVectors -DisableMsix:$VirtioDisableMsix),id=$($script:VirtioInputKeyboardQmpId)"
    $mouse = "$(New-AeroWin7VirtioMouseDeviceArg -MsixVectors $mouseVectors -DisableMsix:$VirtioDisableMsix),id=$($script:VirtioInputMouseQmpId)"
    $attachedVirtioInput = $true
    $virtioTabletArgs = @()
    if ($needVirtioTablet) {
      $tablet = "$(New-AeroWin7VirtioTabletDeviceArg -MsixVectors $tabletVectors -DisableMsix:$VirtioDisableMsix),id=$($script:VirtioInputTabletQmpId)"
      $virtioTabletArgs = @(
        "-device", $tablet
      )
    }

    $virtioSndArgs = @()
    if ($WithVirtioSnd) {
      $audiodev = ""
      switch ($VirtioSndAudioBackend) {
        "none" {
          $audiodev = "none,id=snd0"
        }
        "wav" {
          if ([string]::IsNullOrEmpty($VirtioSndWavPath)) {
            throw "VirtioSndWavPath is required when VirtioSndAudioBackend is 'wav'."
          }

          $wavParent = Split-Path -Parent $VirtioSndWavPath
          if ([string]::IsNullOrEmpty($wavParent)) { $wavParent = "." }
          if (-not (Test-Path -LiteralPath $wavParent)) {
            New-Item -ItemType Directory -Path $wavParent -Force | Out-Null
          }
          $VirtioSndWavPath = Join-Path (Resolve-Path -LiteralPath $wavParent).Path (Split-Path -Leaf $VirtioSndWavPath)

          if (Test-Path -LiteralPath $VirtioSndWavPath) {
            if (Test-Path -LiteralPath $VirtioSndWavPath -PathType Container) {
              throw "VirtioSndWavPath is a directory: $VirtioSndWavPath"
            }
            Remove-Item -LiteralPath $VirtioSndWavPath -Force
          }

          $audiodev = "wav,id=snd0,path=$(Quote-AeroWin7QemuKeyvalValue $VirtioSndWavPath)"
        }
        default {
          throw "Unexpected VirtioSndAudioBackend: $VirtioSndAudioBackend"
        }
      }

      $virtioSndDevice = Get-AeroVirtioSoundDeviceArg -QemuSystem $QemuSystem -ModernOnly $true -MsixVectors $requestedVirtioSndVectors -DisableMsix:$VirtioDisableMsix -VectorsParamName $virtioSndVectorsFlag -DeviceName $virtioSndPciDeviceName
      $virtioSndArgs = @(
        "-audiodev", $audiodev,
        "-device", $virtioSndDevice
      )
    } elseif (-not [string]::IsNullOrEmpty($VirtioSndWavPath) -or $VirtioSndAudioBackend -ne "none") {
      throw "-VirtioSndAudioBackend/-VirtioSndWavPath require -WithVirtioSnd/-RequireVirtioSnd/-EnableVirtioSnd."
    }

    $qemuArgs = @(
      "-m", "$MemoryMB",
      "-smp", "$Smp",
      "-display", "none",
      "-no-reboot"
    ) + $qmpArgs + @(
      "-chardev", $serialChardev,
      "-serial", "chardev:charserial0",
      "-netdev", $netdev,
      "-device", $nic,
      "-device", $kbd,
      "-device", $mouse
    ) + $virtioTabletArgs + @(
      "-drive", $drive,
      "-device", $blk
  ) + $virtioSndArgs + $QemuExtraArgs
  }

  if ($VirtioDisableMsix) {
    Write-Host "AERO_VIRTIO_WIN7_HOST|CONFIG|force_intx=1"
  }

  Write-Host "Starting HTTP server on 127.0.0.1:$HttpPort$HttpPath ..."
  $httpLargePath = Get-AeroSelftestLargePath -Path $HttpPath
  Write-Host "  (large payload at 127.0.0.1:$HttpPort$httpLargePath, 1 MiB deterministic bytes)"
  Write-Host "  (guest: http://10.0.2.2:$HttpPort$HttpPath and http://10.0.2.2:$HttpPort$httpLargePath)"
  $httpListener = Start-AeroSelftestHttpServer -Port $HttpPort -Path $HttpPath

  if ($DisableUdp) {
    Write-Host "UDP echo server disabled (-DisableUdp)"
  } else {
    Write-Host "Starting UDP echo server on 127.0.0.1:$UdpPort (guest: 10.0.2.2:$UdpPort) ..."
    try {
      $udpSocket = Start-AeroSelftestUdpEchoServer -Port $UdpPort
    } catch {
      throw "Failed to bind UDP echo server on 127.0.0.1:$UdpPort (port in use?): $_"
    }
  }

  Write-Host "Launching QEMU:"
  Write-Host "  $QemuSystem $($qemuArgs -join ' ')"
 
  $proc = $null
  $scriptExitCode = 0
  
  $result = $null
  try {
    $proc = Start-Process -FilePath $QemuSystem -ArgumentList $qemuArgs -PassThru -RedirectStandardError $qemuStderrPath
    if ([bool]$QemuPreflightPci) {
      if (($null -eq $qmpPort) -or ($qmpPort -le 0)) {
        $result = @{
          Result = "QEMU_PCI_PREFLIGHT_FAILED"
          Tail   = ""
          Reason = "QMP endpoint not configured"
        }
      } else {
        $pref = Test-AeroQmpVirtioPciPreflight `
          -Host "127.0.0.1" `
          -Port ([int]$qmpPort) `
          -VirtioTransitional ([bool]$VirtioTransitional) `
          -WithVirtioSnd ([bool]$WithVirtioSnd) `
          -WithVirtioTablet ([bool]$needVirtioTablet)
        if (-not $pref.Ok) {
          $result = @{
            Result = "QEMU_PCI_PREFLIGHT_FAILED"
            Tail   = ""
            Reason = $pref.Reason
          }
        }
      }
    }

    if ($null -eq $result) {
      $result = Wait-AeroSelftestResult `
        -SerialLogPath $SerialLogPath `
        -QemuProcess $proc `
        -TimeoutSeconds $TimeoutSeconds `
        -HttpListener $httpListener `
        -HttpPath $HttpPath `
        -HttpLogPath $HttpLogPath `
        -UdpSocket $udpSocket `
        -FollowSerial ([bool]$FollowSerial) `
        -RequirePerTestMarkers (-not $VirtioTransitional) `
        -RequireVirtioNetUdpPass (-not $DisableUdp) `
        -RequireVirtioNetMsix ([bool]$RequireVirtioNetMsix) `
        -RequireVirtioSndPass ([bool]$WithVirtioSnd) `
        -RequireVirtioBlkResetPass ([bool]$WithBlkReset) `
        -RequireVirtioSndBufferLimitsPass ([bool]$WithSndBufferLimits) `
        -RequireVirtioBlkResizePass ([bool]$needBlkResize) `
        -VirtioBlkResizeDeltaMiB ([int]$BlkResizeDeltaMiB) `
        -RequireNetCsumOffload ([bool]$RequireNetCsumOffload) `
        -RequireNetUdpCsumOffload ([bool]$RequireNetUdpCsumOffload) `
        -RequireVirtioInputLedsPass ([bool]$WithInputLeds) `
        -RequireVirtioInputEventsPass ([bool]$needInputEvents) `
        -RequireVirtioInputMediaKeysPass ([bool]$needInputMediaKeys) `
        -RequireVirtioInputLedPass ([bool]$needInputLed) `
        -RequireVirtioInputWheelPass ([bool]$needInputWheel) `
        -RequireVirtioInputEventsExtendedPass ([bool]$needInputEventsExtended) `
        -RequireVirtioInputMsixPass ([bool]$RequireVirtioInputMsix) `
        -RequireVirtioInputBindingPass ([bool]$RequireVirtioInputBinding) `
        -RequireVirtioInputTabletEventsPass ([bool]$needInputTabletEvents) `
        -RequireExpectBlkMsi ([bool]$RequireExpectBlkMsi) `
        -RequireVirtioNetLinkFlapPass ([bool]$needNetLinkFlap) `
        -QmpHost "127.0.0.1" `
        -QmpPort $qmpPort
    }
    if ($RequireVirtioNetMsix -and $result.Result -eq "PASS") {
      # In addition to the host-side PCI MSI-X enable check (QMP), require the guest to report
      # virtio-net running in MSI-X mode via the dedicated marker:
      #   AERO_VIRTIO_SELFTEST|TEST|virtio-net-msix|PASS|mode=msix|...
      $chk = Test-AeroVirtioNetMsixMarker -Tail $result.Tail -SerialLogPath $SerialLogPath
      if (-not $chk.Ok) {
        $result = @{
          Result     = "VIRTIO_NET_MSIX_REQUIRED"
          Tail       = $result.Tail
          MsixReason = $chk.Reason
        }
      }
    }
    if ($RequireVirtioBlkMsix -and $result.Result -eq "PASS") {
      # In addition to the host-side PCI MSI-X enable check (QMP), require the guest to report
      # virtio-blk running in MSI-X mode via the dedicated marker:
      #   AERO_VIRTIO_SELFTEST|TEST|virtio-blk-msix|PASS|mode=msix|...
      $chk = Test-AeroVirtioBlkMsixMarker -Tail $result.Tail -SerialLogPath $SerialLogPath
      if (-not $chk.Ok) {
        $result = @{
          Result     = "VIRTIO_BLK_MSIX_REQUIRED"
          Tail       = $result.Tail
          MsixReason = $chk.Reason
        }
      }
    }
    if ($RequireVirtioSndMsix -and $result.Result -eq "PASS") {
      # In addition to the host-side PCI MSI-X enable check (QMP), require the guest to report
      # virtio-snd running in MSI-X mode via the dedicated marker:
      #   AERO_VIRTIO_SELFTEST|TEST|virtio-snd-msix|PASS|mode=msix|...
      $chk = Test-AeroVirtioSndMsixMarker -Tail $result.Tail -SerialLogPath $SerialLogPath
      if (-not $chk.Ok) {
        $result = @{
          Result     = "VIRTIO_SND_MSIX_REQUIRED"
          Tail       = $result.Tail
          MsixReason = $chk.Reason
        }
      }
    }
    if ($needMsixCheck -and $result.Result -eq "PASS") {
      if (($null -eq $qmpPort) -or ($qmpPort -le 0)) {
        $result = @{
          Result     = "QMP_MSIX_CHECK_UNSUPPORTED"
          Tail       = $result.Tail
          MsixReason = "QMP endpoint not configured"
        }
      } else {
        $msix = Test-AeroQmpRequiredVirtioMsix -Host "127.0.0.1" -Port ([int]$qmpPort) -RequireVirtioNetMsix ([bool]$RequireVirtioNetMsix) -RequireVirtioBlkMsix ([bool]$RequireVirtioBlkMsix) -RequireVirtioSndMsix ([bool]$RequireVirtioSndMsix)
        if (-not $msix.Ok) {
          $result = @{
            Result     = $msix.Result
            Tail       = $result.Tail
            MsixReason = $msix.Reason
          }
        }
      }
    }
  } finally {
    if (-not $proc.HasExited) {
      $quitOk = $false
      if ($null -ne $qmpPort) {
        $quitOk = Try-AeroQmpQuit -Host "127.0.0.1" -Port $qmpPort
      }
      if ($quitOk) {
        try { $proc.WaitForExit(10000) } catch { }
      }

      # Only fall back to hard termination if QEMU did not exit after the QMP quit request.
      if (-not $proc.HasExited) {
        Stop-Process -Id $proc.Id -ErrorAction SilentlyContinue
        try { $proc.WaitForExit(5000) } catch { }
        if (-not $proc.HasExited) {
          Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
          try { $proc.WaitForExit(5000) } catch { }
        }
      }
  }
  }

  Try-EmitAeroVirtioBlkIrqMarker -Tail $result.Tail -SerialLogPath $SerialLogPath
  Try-EmitAeroVirtioBlkMsixMarker -Tail $result.Tail -SerialLogPath $SerialLogPath
  Try-EmitAeroVirtioBlkIoMarker -Tail $result.Tail -SerialLogPath $SerialLogPath
  Try-EmitAeroVirtioBlkRecoveryMarker -Tail $result.Tail -SerialLogPath $SerialLogPath
  Try-EmitAeroVirtioBlkCountersMarker -Tail $result.Tail -SerialLogPath $SerialLogPath
  Try-EmitAeroVirtioBlkResetRecoveryMarker -Tail $result.Tail -SerialLogPath $SerialLogPath
  Try-EmitAeroVirtioBlkMiniportFlagsMarker -Tail $result.Tail -SerialLogPath $SerialLogPath
  Try-EmitAeroVirtioBlkMiniportResetRecoveryMarker -Tail $result.Tail -SerialLogPath $SerialLogPath
  Try-EmitAeroVirtioBlkResizeMarker -Tail $result.Tail -SerialLogPath $SerialLogPath
  Try-EmitAeroVirtioBlkResetMarker -Tail $result.Tail -SerialLogPath $SerialLogPath
  Try-EmitAeroVirtioNetLargeMarker -Tail $result.Tail -SerialLogPath $SerialLogPath
  Try-EmitAeroVirtioNetUdpMarker -Tail $result.Tail -SerialLogPath $SerialLogPath
  Try-EmitAeroVirtioNetUdpDnsMarker -Tail $result.Tail -SerialLogPath $SerialLogPath
  Try-EmitAeroVirtioNetOffloadCsumMarker -Tail $result.Tail -SerialLogPath $SerialLogPath
  Try-EmitAeroVirtioNetDiagMarker -Tail $result.Tail -SerialLogPath $SerialLogPath
  Try-EmitAeroVirtioNetMsixMarker -Tail $result.Tail -SerialLogPath $SerialLogPath
  Try-EmitAeroVirtioIrqMarkerFromTestMarker -Tail $result.Tail -Device "virtio-net" -HostMarker "VIRTIO_NET_IRQ" -SerialLogPath $SerialLogPath
  Try-EmitAeroVirtioIrqMarkerFromTestMarker -Tail $result.Tail -Device "virtio-snd" -HostMarker "VIRTIO_SND_IRQ" -SerialLogPath $SerialLogPath
  Try-EmitAeroVirtioIrqMarkerFromTestMarker -Tail $result.Tail -Device "virtio-input" -HostMarker "VIRTIO_INPUT_IRQ" -SerialLogPath $SerialLogPath
  Try-EmitAeroVirtioInputBindMarker -Tail $result.Tail -SerialLogPath $SerialLogPath
  Try-EmitAeroVirtioInputBindingMarker -Tail $result.Tail -SerialLogPath $SerialLogPath
  Try-EmitAeroVirtioInputMsixMarker -Tail $result.Tail -SerialLogPath $SerialLogPath
  Try-EmitAeroVirtioSndMsixMarker -Tail $result.Tail -SerialLogPath $SerialLogPath
  Try-EmitAeroVirtioSndMarker -Tail $result.Tail -SerialLogPath $SerialLogPath
  Try-EmitAeroVirtioSndCaptureMarker -Tail $result.Tail -SerialLogPath $SerialLogPath
  Try-EmitAeroVirtioSndEventqMarker -Tail $result.Tail -SerialLogPath $SerialLogPath
  Try-EmitAeroVirtioSndFormatMarker -Tail $result.Tail -SerialLogPath $SerialLogPath
  Try-EmitAeroVirtioSndDuplexMarker -Tail $result.Tail -SerialLogPath $SerialLogPath
  Try-EmitAeroVirtioSndBufferLimitsMarker -Tail $result.Tail -SerialLogPath $SerialLogPath
  Try-EmitAeroVirtioIrqDiagnosticsMarkers -Tail $result.Tail -SerialLogPath $SerialLogPath

  if ($RequireNoBlkRecovery -and $result.Result -eq "PASS") {
    $counters = Get-AeroVirtioBlkRecoveryCounters -Tail $result.Tail -SerialLogPath $SerialLogPath
    if ($null -ne $counters) {
      $nonzero = $false
      foreach ($k in @("abort_srb", "reset_device_srb", "reset_bus_srb", "pnp_srb", "ioctl_reset")) {
        if ($counters.ContainsKey($k) -and [int64]$counters[$k] -gt 0) { $nonzero = $true; break }
      }
      if ($nonzero) {
        $result["Result"] = "VIRTIO_BLK_RECOVERY_NONZERO"
        $result["BlkRecoveryCounters"] = $counters
      }
    }
  }

  if ($FailOnBlkRecovery -and $result.Result -eq "PASS") {
    $prefix = "AERO_VIRTIO_SELFTEST|TEST|virtio-blk-counters|"
    $line = Try-ExtractLastAeroMarkerLine -Tail $result.Tail -Prefix $prefix -SerialLogPath $SerialLogPath
    if ($null -ne $line) {
      $toks = $line.Split("|")
      $status = "INFO"
      if ($toks.Count -ge 4) {
        $s = $toks[3].Trim().ToUpperInvariant()
        if ($s -eq "PASS" -or $s -eq "FAIL" -or $s -eq "SKIP" -or $s -eq "INFO") {
          $status = $s
        }
      }
      if ($status -ne "SKIP") {
        $fields = @{}
        foreach ($tok in $toks) {
          $idx = $tok.IndexOf("=")
          if ($idx -le 0) { continue }
          $k = $tok.Substring(0, $idx).Trim()
          $v = $tok.Substring($idx + 1).Trim()
          if (-not [string]::IsNullOrEmpty($k)) {
            $fields[$k] = $v
          }
        }

        $keys = @("abort", "reset_device", "reset_bus")
        $vals = @{}
        $okAll = $true
        foreach ($k in $keys) {
          if (-not $fields.ContainsKey($k)) { $okAll = $false; break }
          $raw = [string]$fields[$k]
          $v = 0L
          $ok = [int64]::TryParse($raw, [ref]$v)
          if (-not $ok) {
            if ($raw -match "^0x[0-9a-fA-F]+$") {
              try {
                $v = [Convert]::ToInt64($raw.Substring(2), 16)
                $ok = $true
              } catch {
                $ok = $false
              }
            }
          }
          if (-not $ok) { $okAll = $false; break }
          $vals[$k] = $v
        }

        if ($okAll) {
          if ([int64]$vals["abort"] -gt 0 -or [int64]$vals["reset_device"] -gt 0 -or [int64]$vals["reset_bus"] -gt 0) {
            $result["Result"] = "VIRTIO_BLK_RECOVERY_DETECTED"
            $result["BlkCounters"] = $vals
          }
        }
      }
    } else {
      # Backward compatible fallback: older guest selftests emitted the recovery counters on the virtio-blk
      # per-test marker rather than the dedicated virtio-blk-counters marker.
      $blkPrefix = "AERO_VIRTIO_SELFTEST|TEST|virtio-blk|"
      $blkLine = Try-ExtractLastAeroMarkerLine -Tail $result.Tail -Prefix $blkPrefix -SerialLogPath $SerialLogPath
      if ($null -ne $blkLine) {
        $fields2 = @{}
        foreach ($tok in $blkLine.Split("|")) {
          $idx = $tok.IndexOf("=")
          if ($idx -le 0) { continue }
          $k = $tok.Substring(0, $idx).Trim()
          $v = $tok.Substring($idx + 1).Trim()
          if (-not [string]::IsNullOrEmpty($k)) {
            $fields2[$k] = $v
          }
        }
 
        $mapping = @{
          abort_srb        = "abort"
          reset_device_srb = "reset_device"
          reset_bus_srb    = "reset_bus"
        }
        $vals2 = @{}
        $okAll2 = $true
        foreach ($src in $mapping.Keys) {
          if (-not $fields2.ContainsKey($src)) { $okAll2 = $false; break }
          $raw = [string]$fields2[$src]
          $v = 0L
          $ok = [int64]::TryParse($raw, [ref]$v)
          if (-not $ok) {
            if ($raw -match "^0x[0-9a-fA-F]+$") {
              try {
                $v = [Convert]::ToInt64($raw.Substring(2), 16)
                $ok = $true
              } catch {
                $ok = $false
              }
            }
          }
          if (-not $ok) { $okAll2 = $false; break }
          $vals2[[string]$mapping[$src]] = $v
        }
 
        if ($okAll2) {
          if ([int64]$vals2["abort"] -gt 0 -or [int64]$vals2["reset_device"] -gt 0 -or [int64]$vals2["reset_bus"] -gt 0) {
            $result["Result"] = "VIRTIO_BLK_RECOVERY_DETECTED"
            $result["BlkCounters"] = $vals2
          }
        }
      }
    }
  }

  if ($RequireNoBlkResetRecovery -and $result.Result -eq "PASS") {
    $counters = Get-AeroVirtioBlkResetRecoveryCounters -Tail $result.Tail -SerialLogPath $SerialLogPath
    if ($null -ne $counters) {
      $resetDetected = 0L
      $hwResetBus = 0L
      try { $resetDetected = [int64]$counters["reset_detected"] } catch { }
      try { $hwResetBus = [int64]$counters["hw_reset_bus"] } catch { }

      if ($resetDetected -gt 0 -or $hwResetBus -gt 0) {
        $result["Result"] = "VIRTIO_BLK_RESET_RECOVERY_NONZERO"
        $result["BlkResetRecoveryCounters"] = $counters
      }
    }
  }

  if ($FailOnBlkResetRecovery -and $result.Result -eq "PASS") {
    $counters = Get-AeroVirtioBlkResetRecoveryCounters -Tail $result.Tail -SerialLogPath $SerialLogPath
    if ($null -ne $counters) {
      $hwResetBus = 0L
      try { $hwResetBus = [int64]$counters["hw_reset_bus"] } catch { }
      if ($hwResetBus -gt 0) {
        $result["Result"] = "VIRTIO_BLK_RESET_RECOVERY_DETECTED"
        $result["BlkResetRecoveryCounters"] = $counters
      }
    }
  }

  if ($RequireNoBlkMiniportFlags -and $result.Result -eq "PASS") {
    $flags = Get-AeroVirtioBlkMiniportFlags -Tail $result.Tail -SerialLogPath $SerialLogPath
    if ($null -ne $flags) {
      $nonzero = $false
      foreach ($k in @("removed", "surprise_removed", "reset_in_progress", "reset_pending")) {
        if ($flags.ContainsKey($k)) {
          try { if ([int64]$flags[$k] -gt 0) { $nonzero = $true; break } } catch { }
        }
      }
      if ($nonzero) {
        $result["Result"] = "VIRTIO_BLK_MINIPORT_FLAGS_NONZERO"
        $result["BlkMiniportFlags"] = $flags
      }
    }
  }

  if ($FailOnBlkMiniportFlags -and $result.Result -eq "PASS") {
    $flags = Get-AeroVirtioBlkMiniportFlags -Tail $result.Tail -SerialLogPath $SerialLogPath
    if ($null -ne $flags) {
      $removed = 0L
      $surprise = 0L
      try { $removed = [int64]$flags["removed"] } catch { }
      try { $surprise = [int64]$flags["surprise_removed"] } catch { }
      if ($removed -gt 0 -or $surprise -gt 0) {
        $result["Result"] = "VIRTIO_BLK_MINIPORT_FLAGS_REMOVED"
        $result["BlkMiniportFlags"] = $flags
      }
    }
  }

  switch ($result.Result) {
    "PASS" {
      if ($RequireIntx -or $RequireMsi) {
        $expected = if ($RequireIntx) { "intx" } else { "msi" }
        $devices = @("virtio-blk", "virtio-net")
        if ($attachedVirtioInput) { $devices += "virtio-input" }
        if ($WithVirtioSnd) { $devices += "virtio-snd" }
        $chk = Test-AeroVirtioIrqModeEnforcement -Tail $result.Tail -Devices $devices -Expected $expected
        if (-not $chk.Ok) {
          Write-Host "FAIL: IRQ_MODE_MISMATCH: $($chk.Device) expected=$($chk.Expected) got=$($chk.Got)"
          if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
            Write-Host "`n--- Serial tail ---"
            Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
          }
          $scriptExitCode = 1
          break
        }
      }

      Write-Host "PASS: AERO_VIRTIO_SELFTEST|RESULT|PASS"
      $scriptExitCode = 0
    }
    "FAIL" {
      Write-Host "FAIL: SELFTEST_FAILED: AERO_VIRTIO_SELFTEST|RESULT|FAIL"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_SND_FORCE_NULL_BACKEND" {
      $regPath = "HKLM\SYSTEM\CurrentControlSet\Enum\<DeviceInstancePath>\Device Parameters\Parameters\ForceNullBackend"
      $pnpId = $null
      $source = $null
      try {
        if ($result.Tail -match "ForceNullBackend=1 set \(pnp_id=([^\s]+) source=([^\)]+)\)") {
          $pnpId = $Matches[1]
          $source = $Matches[2]
        } elseif ($result.Tail -match "ForceNullBackend=1 set \(source=([^\)]+)\)") {
          $source = $Matches[1]
        }
      } catch { }

      $extra = ""
      if ((-not [string]::IsNullOrEmpty($pnpId)) -or (-not [string]::IsNullOrEmpty($source))) {
        $extra = " ("
        if (-not [string]::IsNullOrEmpty($pnpId)) {
          $extra += "pnp_id=$pnpId"
        }
        if (-not [string]::IsNullOrEmpty($source)) {
          if ($extra -ne " (") { $extra += " " }
          $extra += "source=$source"
        }
        $extra += ")"
      }

      Write-Host "FAIL: VIRTIO_SND_FORCE_NULL_BACKEND: virtio-snd selftest reported force_null_backend$extra; ForceNullBackend=1 disables the virtio-snd transport (host wav capture will be silent). Clear the registry toggle to enable virtio-snd: $regPath (DWORD 0)."
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "QEMU_EXITED" {
      $exitCode = $null
      try { $exitCode = $proc.ExitCode } catch { }
      Write-Host "FAIL: QEMU_EXITED: QEMU exited before selftest result marker (exit code: $exitCode)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      Write-AeroQemuStderrTail -Path $qemuStderrPath
      $scriptExitCode = 3
    }
    "TIMEOUT" {
      Write-Host "FAIL: TIMEOUT: timed out waiting for AERO_VIRTIO_SELFTEST result marker"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 2
    }
    "EXPECT_BLK_MSI_NOT_SET" {
      Write-Host "FAIL: EXPECT_BLK_MSI_NOT_SET: guest selftest was not provisioned with --expect-blk-msi (expect_blk_msi=1 in CONFIG marker). Re-provision with New-AeroWin7TestImage.ps1 -ExpectBlkMsi or set AERO_VIRTIO_SELFTEST_EXPECT_BLK_MSI=1."
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "MISSING_VIRTIO_BLK" {
      Write-Host "FAIL: MISSING_VIRTIO_BLK: selftest RESULT=PASS but did not emit virtio-blk test marker"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_BLK_FAILED" {
      $writeOk = ""
      $flushOk = ""
      $readOk = ""
      $writeBytes = ""
      $readBytes = ""
      $writeMbps = ""
      $readMbps = ""
      $irqMode = ""
      $irqMessageCount = ""
      $irqReason = ""
      $msixConfigVector = ""
      $msixQueueVector = ""
      $line = Try-ExtractLastAeroMarkerLine `
        -Tail $result.Tail `
        -Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-blk|FAIL|" `
        -SerialLogPath $SerialLogPath
      if ($null -ne $line) {
        if ($line -match "(?:^|\|)write_ok=([^|\r\n]+)") { $writeOk = $Matches[1] }
        if ($line -match "(?:^|\|)flush_ok=([^|\r\n]+)") { $flushOk = $Matches[1] }
        if ($line -match "(?:^|\|)read_ok=([^|\r\n]+)") { $readOk = $Matches[1] }
        if ($line -match "(?:^|\|)write_bytes=([^|\r\n]+)") { $writeBytes = $Matches[1] }
        if ($line -match "(?:^|\|)read_bytes=([^|\r\n]+)") { $readBytes = $Matches[1] }
        if ($line -match "(?:^|\|)write_mbps=([^|\r\n]+)") { $writeMbps = $Matches[1] }
        if ($line -match "(?:^|\|)read_mbps=([^|\r\n]+)") { $readMbps = $Matches[1] }
        if ($line -match "(?:^|\|)irq_mode=([^|\r\n]+)") { $irqMode = $Matches[1] }
        if ($line -match "(?:^|\|)irq_message_count=([^|\r\n]+)") { $irqMessageCount = $Matches[1] }
        if ($line -match "(?:^|\|)irq_reason=([^|\r\n]+)") { $irqReason = $Matches[1] }
        if ($line -match "(?:^|\|)msix_config_vector=([^|\r\n]+)") { $msixConfigVector = $Matches[1] }
        if ($line -match "(?:^|\|)msix_queue_vector=([^|\r\n]+)") { $msixQueueVector = $Matches[1] }
      }
      $detailsParts = @()
      if (-not [string]::IsNullOrEmpty($writeOk)) { $detailsParts += "write_ok=$writeOk" }
      if (-not [string]::IsNullOrEmpty($flushOk)) { $detailsParts += "flush_ok=$flushOk" }
      if (-not [string]::IsNullOrEmpty($readOk)) { $detailsParts += "read_ok=$readOk" }
      if (-not [string]::IsNullOrEmpty($writeBytes)) { $detailsParts += "write_bytes=$writeBytes" }
      if (-not [string]::IsNullOrEmpty($readBytes)) { $detailsParts += "read_bytes=$readBytes" }
      if (-not [string]::IsNullOrEmpty($writeMbps)) { $detailsParts += "write_mbps=$writeMbps" }
      if (-not [string]::IsNullOrEmpty($readMbps)) { $detailsParts += "read_mbps=$readMbps" }
      if (-not [string]::IsNullOrEmpty($irqMode)) { $detailsParts += "irq_mode=$irqMode" }
      if (-not [string]::IsNullOrEmpty($irqMessageCount)) { $detailsParts += "irq_message_count=$irqMessageCount" }
      if (-not [string]::IsNullOrEmpty($irqReason)) { $detailsParts += "irq_reason=$irqReason" }
      if (-not [string]::IsNullOrEmpty($msixConfigVector)) { $detailsParts += "msix_config_vector=$msixConfigVector" }
      if (-not [string]::IsNullOrEmpty($msixQueueVector)) { $detailsParts += "msix_queue_vector=$msixQueueVector" }
      $details = ""
      if ($detailsParts.Count -gt 0) { $details = " (" + ($detailsParts -join " ") + ")" }
      Write-Host "FAIL: VIRTIO_BLK_FAILED: selftest RESULT=PASS but virtio-blk test reported FAIL$details"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_BLK_RECOVERY_NONZERO" {
      $counters = $null
      if ($result.ContainsKey("BlkRecoveryCounters")) {
        $counters = $result["BlkRecoveryCounters"]
      } else {
        $counters = Get-AeroVirtioBlkRecoveryCounters -Tail $result.Tail -SerialLogPath $SerialLogPath
      }

      $msg = "FAIL: VIRTIO_BLK_RECOVERY_NONZERO:"
      if ($null -ne $counters) {
        $msg += " abort_srb=$($counters['abort_srb']) reset_device_srb=$($counters['reset_device_srb']) reset_bus_srb=$($counters['reset_bus_srb']) pnp_srb=$($counters['pnp_srb']) ioctl_reset=$($counters['ioctl_reset'])"
      }
      Write-Host $msg
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_BLK_RECOVERY_DETECTED" {
      $counters = $null
      if ($result.ContainsKey("BlkCounters")) {
        $counters = $result["BlkCounters"]
      }

      $msg = "FAIL: VIRTIO_BLK_RECOVERY_DETECTED:"
      if ($null -ne $counters) {
        $msg += " abort=$($counters['abort']) reset_device=$($counters['reset_device']) reset_bus=$($counters['reset_bus'])"
      }
      Write-Host $msg
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_BLK_RESET_RECOVERY_NONZERO" {
      $counters = $null
      if ($result.ContainsKey("BlkResetRecoveryCounters")) {
        $counters = $result["BlkResetRecoveryCounters"]
      } else {
        $counters = Get-AeroVirtioBlkResetRecoveryCounters -Tail $result.Tail -SerialLogPath $SerialLogPath
      }

      $msg = "FAIL: VIRTIO_BLK_RESET_RECOVERY_NONZERO:"
      if ($null -ne $counters) {
        $msg += " reset_detected=$($counters['reset_detected']) hw_reset_bus=$($counters['hw_reset_bus'])"
      }
      Write-Host $msg
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_BLK_RESET_RECOVERY_DETECTED" {
      $counters = $null
      if ($result.ContainsKey("BlkResetRecoveryCounters")) {
        $counters = $result["BlkResetRecoveryCounters"]
      } else {
        $counters = Get-AeroVirtioBlkResetRecoveryCounters -Tail $result.Tail -SerialLogPath $SerialLogPath
      }

      $msg = "FAIL: VIRTIO_BLK_RESET_RECOVERY_DETECTED:"
      if ($null -ne $counters) {
        $msg += " hw_reset_bus=$($counters['hw_reset_bus']) reset_detected=$($counters['reset_detected'])"
      }
      Write-Host $msg
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_BLK_MINIPORT_FLAGS_NONZERO" {
      $flags = $null
      if ($result.ContainsKey("BlkMiniportFlags")) {
        $flags = $result["BlkMiniportFlags"]
      } else {
        $flags = Get-AeroVirtioBlkMiniportFlags -Tail $result.Tail -SerialLogPath $SerialLogPath
      }

      $msg = "FAIL: VIRTIO_BLK_MINIPORT_FLAGS_NONZERO:"
      if ($null -ne $flags) {
        $rawHex = $null
        try { $rawHex = ("0x{0:x8}" -f ([uint32]$flags["raw"])) } catch { }
        if ($null -ne $rawHex) {
          $msg += " raw=$rawHex"
        }
        $msg += " removed=$($flags['removed']) surprise_removed=$($flags['surprise_removed']) reset_in_progress=$($flags['reset_in_progress']) reset_pending=$($flags['reset_pending'])"
      }
      Write-Host $msg
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_BLK_MINIPORT_FLAGS_REMOVED" {
      $flags = $null
      if ($result.ContainsKey("BlkMiniportFlags")) {
        $flags = $result["BlkMiniportFlags"]
      } else {
        $flags = Get-AeroVirtioBlkMiniportFlags -Tail $result.Tail -SerialLogPath $SerialLogPath
      }

      $msg = "FAIL: VIRTIO_BLK_MINIPORT_FLAGS_REMOVED:"
      if ($null -ne $flags) {
        $rawHex = $null
        try { $rawHex = ("0x{0:x8}" -f ([uint32]$flags["raw"])) } catch { }
        if ($null -ne $rawHex) {
          $msg += " raw=$rawHex"
        }
        $msg += " removed=$($flags['removed']) surprise_removed=$($flags['surprise_removed'])"
      }
      Write-Host $msg
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "MISSING_VIRTIO_BLK_RESIZE" {
      Write-Host "FAIL: MISSING_VIRTIO_BLK_RESIZE: did not observe virtio-blk-resize marker (READY/SKIP/PASS/FAIL) after virtio-blk completed while -WithBlkResize/-WithVirtioBlkResize/-RequireVirtioBlkResize/-EnableVirtioBlkResize was enabled (guest selftest too old or missing --test-blk-resize)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_BLK_RESIZE_SKIPPED" {
      $reason = "unknown"
      $line = Try-ExtractLastAeroMarkerLine `
        -Tail $result.Tail `
        -Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|SKIP|" `
        -SerialLogPath $SerialLogPath
      if ($null -ne $line) {
        if ($line -match "reason=([^|\r\n]+)") { $reason = $Matches[1] }
        elseif ($line -match "\|SKIP\|([^|\r\n=]+)(?:\||$)") { $reason = $Matches[1] }
      }

      if ($reason -eq "flag_not_set") {
        Write-Host "FAIL: VIRTIO_BLK_RESIZE_SKIPPED: virtio-blk-resize test was skipped (flag_not_set) but -WithBlkResize/-WithVirtioBlkResize/-RequireVirtioBlkResize/-EnableVirtioBlkResize was enabled (provision the guest with --test-blk-resize)"
      } else {
        Write-Host "FAIL: VIRTIO_BLK_RESIZE_SKIPPED: virtio-blk-resize test was skipped ($reason) but -WithBlkResize/-WithVirtioBlkResize/-RequireVirtioBlkResize/-EnableVirtioBlkResize was enabled"
      }
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_BLK_RESIZE_FAILED" {
      $reason = "unknown"
      $err = "unknown"
      $disk = ""
      $oldBytes = ""
      $lastBytes = ""
      $line = Try-ExtractLastAeroMarkerLine `
        -Tail $result.Tail `
        -Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|FAIL|" `
        -SerialLogPath $SerialLogPath
      if ($null -ne $line) {
        if ($line -match "reason=([^|\r\n]+)") { $reason = $Matches[1] }
        elseif ($line -match "\|FAIL\|([^|\r\n=]+)(?:\||$)") { $reason = $Matches[1] }
        if ($line -match "(?:^|\|)err=([^|\r\n]+)") { $err = $Matches[1] }
        if ($line -match "(?:^|\|)disk=([^|\r\n]+)") { $disk = $Matches[1] }
        if ($line -match "(?:^|\|)old_bytes=([^|\r\n]+)") { $oldBytes = $Matches[1] }
        if ($line -match "(?:^|\|)last_bytes=([^|\r\n]+)") { $lastBytes = $Matches[1] }
      }
      $details = "(reason=$reason err=$err"
      if (-not [string]::IsNullOrEmpty($disk)) { $details += " disk=$disk" }
      if (-not [string]::IsNullOrEmpty($oldBytes)) { $details += " old_bytes=$oldBytes" }
      if (-not [string]::IsNullOrEmpty($lastBytes)) { $details += " last_bytes=$lastBytes" }
      $details += ")"
      Write-Host "FAIL: VIRTIO_BLK_RESIZE_FAILED: virtio-blk-resize test reported FAIL while -WithBlkResize/-WithVirtioBlkResize/-RequireVirtioBlkResize/-EnableVirtioBlkResize was enabled $details"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "QMP_BLK_RESIZE_FAILED" {
      Write-Host "FAIL: QMP_BLK_RESIZE_FAILED: failed to resize virtio-blk device via QMP (ensure QMP is reachable and QEMU supports blockdev-resize or block_resize)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "MISSING_VIRTIO_BLK_RESET" {
      Write-Host "FAIL: MISSING_VIRTIO_BLK_RESET: did not observe virtio-blk-reset PASS marker while -WithBlkReset/-WithVirtioBlkReset/-RequireVirtioBlkReset/-EnableVirtioBlkReset was enabled (guest selftest too old or missing --test-blk-reset)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_BLK_RESET_SKIPPED" {
      $reason = "unknown"
      $line = Try-ExtractLastAeroMarkerLine `
        -Tail $result.Tail `
        -Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|SKIP|" `
        -SerialLogPath $SerialLogPath
      if ($null -ne $line) {
        if ($line -match "reason=([^|\r\n]+)") { $reason = $Matches[1] }
        elseif ($line -match "\|SKIP\|([^|\r\n=]+)(?:\||$)") { $reason = $Matches[1] }
      }
      if ($reason -eq "flag_not_set") {
        Write-Host "FAIL: VIRTIO_BLK_RESET_SKIPPED: virtio-blk-reset test was skipped (flag_not_set) but -WithBlkReset/-WithVirtioBlkReset/-RequireVirtioBlkReset/-EnableVirtioBlkReset was enabled (provision the guest with --test-blk-reset)"
      } else {
        Write-Host "FAIL: VIRTIO_BLK_RESET_SKIPPED: virtio-blk-reset test was skipped ($reason) but -WithBlkReset/-WithVirtioBlkReset/-RequireVirtioBlkReset/-EnableVirtioBlkReset was enabled"
      }
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_BLK_RESET_FAILED" {
      $reason = "unknown"
      $err = "unknown"
      $line = Try-ExtractLastAeroMarkerLine `
        -Tail $result.Tail `
        -Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|FAIL|" `
        -SerialLogPath $SerialLogPath
      if ($null -ne $line) {
        if ($line -match "reason=([^|\r\n]+)") { $reason = $Matches[1] }
        elseif ($line -match "\|FAIL\|([^|\r\n=]+)(?:\||$)") { $reason = $Matches[1] }
        if ($line -match "(?:^|\|)err=([^|\r\n]+)") { $err = $Matches[1] }
      }
      Write-Host "FAIL: VIRTIO_BLK_RESET_FAILED: virtio-blk-reset test reported FAIL while -WithBlkReset/-WithVirtioBlkReset/-RequireVirtioBlkReset/-EnableVirtioBlkReset was enabled (reason=$reason err=$err)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "MISSING_VIRTIO_INPUT" {
      Write-Host "FAIL: MISSING_VIRTIO_INPUT: selftest RESULT=PASS but did not emit virtio-input test marker"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_INPUT_FAILED" {
      $reason = ""
      $devices = ""
      $keyboardDevices = ""
      $consumerDevices = ""
      $mouseDevices = ""
      $ambiguousDevices = ""
      $unknownDevices = ""
      $keyboardCollections = ""
      $consumerCollections = ""
      $mouseCollections = ""
      $tabletDevices = ""
      $tabletCollections = ""
      $irqMode = ""
      $irqMessageCount = ""
      $irqReason = ""
      $line = Try-ExtractLastAeroMarkerLine `
        -Tail $result.Tail `
        -Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-input|FAIL|" `
        -SerialLogPath $SerialLogPath
      if ($null -ne $line) {
        if ($line -match "(?:^|\|)reason=([^|\r\n]+)") { $reason = $Matches[1] }
        if ($line -match "(?:^|\|)devices=([^|\r\n]+)") { $devices = $Matches[1] }
        if ($line -match "(?:^|\|)keyboard_devices=([^|\r\n]+)") { $keyboardDevices = $Matches[1] }
        if ($line -match "(?:^|\|)consumer_devices=([^|\r\n]+)") { $consumerDevices = $Matches[1] }
        if ($line -match "(?:^|\|)mouse_devices=([^|\r\n]+)") { $mouseDevices = $Matches[1] }
        if ($line -match "(?:^|\|)ambiguous_devices=([^|\r\n]+)") { $ambiguousDevices = $Matches[1] }
        if ($line -match "(?:^|\|)unknown_devices=([^|\r\n]+)") { $unknownDevices = $Matches[1] }
        if ($line -match "(?:^|\|)tablet_devices=([^|\r\n]+)") { $tabletDevices = $Matches[1] }
        if ($line -match "(?:^|\|)keyboard_collections=([^|\r\n]+)") { $keyboardCollections = $Matches[1] }
        if ($line -match "(?:^|\|)consumer_collections=([^|\r\n]+)") { $consumerCollections = $Matches[1] }
        if ($line -match "(?:^|\|)mouse_collections=([^|\r\n]+)") { $mouseCollections = $Matches[1] }
        if ($line -match "(?:^|\|)tablet_collections=([^|\r\n]+)") { $tabletCollections = $Matches[1] }
        if ($line -match "(?:^|\|)irq_mode=([^|\r\n]+)") { $irqMode = $Matches[1] }
        if ($line -match "(?:^|\|)irq_message_count=([^|\r\n]+)") { $irqMessageCount = $Matches[1] }
        if ($line -match "(?:^|\|)irq_reason=([^|\r\n]+)") { $irqReason = $Matches[1] }
      }
      $detailsParts = @()
      if (-not [string]::IsNullOrEmpty($reason)) { $detailsParts += "reason=$reason" }
      if (-not [string]::IsNullOrEmpty($devices)) { $detailsParts += "devices=$devices" }
      if (-not [string]::IsNullOrEmpty($keyboardDevices)) { $detailsParts += "keyboard_devices=$keyboardDevices" }
      if (-not [string]::IsNullOrEmpty($consumerDevices)) { $detailsParts += "consumer_devices=$consumerDevices" }
      if (-not [string]::IsNullOrEmpty($mouseDevices)) { $detailsParts += "mouse_devices=$mouseDevices" }
      if (-not [string]::IsNullOrEmpty($ambiguousDevices)) { $detailsParts += "ambiguous_devices=$ambiguousDevices" }
      if (-not [string]::IsNullOrEmpty($unknownDevices)) { $detailsParts += "unknown_devices=$unknownDevices" }
      if (-not [string]::IsNullOrEmpty($tabletDevices)) { $detailsParts += "tablet_devices=$tabletDevices" }
      if (-not [string]::IsNullOrEmpty($keyboardCollections)) { $detailsParts += "keyboard_collections=$keyboardCollections" }
      if (-not [string]::IsNullOrEmpty($consumerCollections)) { $detailsParts += "consumer_collections=$consumerCollections" }
      if (-not [string]::IsNullOrEmpty($mouseCollections)) { $detailsParts += "mouse_collections=$mouseCollections" }
      if (-not [string]::IsNullOrEmpty($tabletCollections)) { $detailsParts += "tablet_collections=$tabletCollections" }
      if (-not [string]::IsNullOrEmpty($irqMode)) { $detailsParts += "irq_mode=$irqMode" }
      if (-not [string]::IsNullOrEmpty($irqMessageCount)) { $detailsParts += "irq_message_count=$irqMessageCount" }
      if (-not [string]::IsNullOrEmpty($irqReason)) { $detailsParts += "irq_reason=$irqReason" }
      $details = ""
      if ($detailsParts.Count -gt 0) { $details = " (" + ($detailsParts -join " ") + ")" }
      Write-Host "FAIL: VIRTIO_INPUT_FAILED: selftest RESULT=PASS but virtio-input test reported FAIL$details"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "MISSING_VIRTIO_INPUT_MSIX" {
      Write-Host "FAIL: MISSING_VIRTIO_INPUT_MSIX: did not observe virtio-input-msix marker while -RequireVirtioInputMsix/-RequireInputMsix was enabled (guest selftest too old?)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "MISSING_VIRTIO_INPUT_BINDING" {
      Write-Host "FAIL: MISSING_VIRTIO_INPUT_BINDING: did not observe virtio-input-binding PASS marker while -RequireVirtioInputBinding was enabled (guest selftest too old?)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_INPUT_BINDING_SKIPPED" {
      Write-Host "FAIL: VIRTIO_INPUT_BINDING_SKIPPED: virtio-input-binding marker reported SKIP while -RequireVirtioInputBinding was enabled (guest selftest too old?)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_INPUT_BINDING_FAILED" {
      $reason = "unknown"
      $expected = ""
      $actual = ""
      $line = Try-ExtractLastAeroMarkerLine `
        -Tail $result.Tail `
        -Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-input-binding|" `
        -SerialLogPath $SerialLogPath
      if ($null -ne $line) {
        if ($line -match "reason=([^|\r\n]+)") { $reason = $Matches[1] }
        if ($line -match "expected=([^|\r\n]+)") { $expected = $Matches[1] }
        if ($line -match "actual=([^|\r\n]+)") { $actual = $Matches[1] }
      }
      $details = "reason=$reason"
      if (-not [string]::IsNullOrEmpty($expected)) { $details += " expected=$expected" }
      if (-not [string]::IsNullOrEmpty($actual)) { $details += " actual=$actual" }
      Write-Host "FAIL: VIRTIO_INPUT_BINDING_FAILED: virtio-input-binding marker reported FAIL while -RequireVirtioInputBinding was enabled ($details)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "MISSING_VIRTIO_INPUT_BIND" {
      Write-Host "FAIL: MISSING_VIRTIO_INPUT_BIND: selftest RESULT=PASS but did not emit virtio-input-bind test marker (guest selftest too old; update the image/selftest binary)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "MISSING_VIRTIO_INPUT_LEDS" {
      Write-Host "FAIL: MISSING_VIRTIO_INPUT_LEDS: did not observe virtio-input-leds marker (PASS/FAIL/SKIP) after virtio-input completed while -WithInputLeds/-WithVirtioInputLeds/-RequireVirtioInputLeds/-EnableVirtioInputLeds was enabled (guest selftest too old or missing --test-input-leds/--test-input-led)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_INPUT_MSIX_REQUIRED" {
      $reason = ""
      try { $reason = [string]$result.MsixReason } catch { }
      if ([string]::IsNullOrEmpty($reason)) { $reason = "virtio-input-msix marker did not report mode=msix while -RequireVirtioInputMsix/-RequireInputMsix was enabled" }
      Write-Host "FAIL: VIRTIO_INPUT_MSIX_REQUIRED: $reason"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_INPUT_BIND_FAILED" {
      $reason = ""
      $expected = ""
      $actual = ""
      $pnpId = ""
      $devices = ""
      $wrongService = ""
      $missingService = ""
      $problem = ""
      $line = Try-ExtractLastAeroMarkerLine `
        -Tail $result.Tail `
        -Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-input-bind|FAIL|" `
        -SerialLogPath $SerialLogPath
      if ($null -ne $line) {
        if ($line -match "(?:^|\|)reason=([^|\r\n]+)") { $reason = $Matches[1] }
        if ($line -match "(?:^|\|)expected=([^|\r\n]+)") { $expected = $Matches[1] }
        if ($line -match "(?:^|\|)actual=([^|\r\n]+)") { $actual = $Matches[1] }
        if ($line -match "(?:^|\|)pnp_id=([^|\r\n]+)") { $pnpId = $Matches[1] }
        if ($line -match "(?:^|\|)devices=([^|\r\n]+)") { $devices = $Matches[1] }
        if ($line -match "(?:^|\|)wrong_service=([^|\r\n]+)") { $wrongService = $Matches[1] }
        if ($line -match "(?:^|\|)missing_service=([^|\r\n]+)") { $missingService = $Matches[1] }
        if ($line -match "(?:^|\|)problem=([^|\r\n]+)") { $problem = $Matches[1] }
      }
      $detailsParts = @()
      if (-not [string]::IsNullOrEmpty($reason)) { $detailsParts += "reason=$reason" }
      if (-not [string]::IsNullOrEmpty($expected)) { $detailsParts += "expected=$expected" }
      if (-not [string]::IsNullOrEmpty($actual)) { $detailsParts += "actual=$actual" }
      if (-not [string]::IsNullOrEmpty($pnpId)) { $detailsParts += "pnp_id=$pnpId" }
      if (-not [string]::IsNullOrEmpty($devices)) { $detailsParts += "devices=$devices" }
      if (-not [string]::IsNullOrEmpty($wrongService)) { $detailsParts += "wrong_service=$wrongService" }
      if (-not [string]::IsNullOrEmpty($missingService)) { $detailsParts += "missing_service=$missingService" }
      if (-not [string]::IsNullOrEmpty($problem)) { $detailsParts += "problem=$problem" }
      $details = ""
      if ($detailsParts.Count -gt 0) { $details = " (" + ($detailsParts -join " ") + ")" }

      Write-Host "FAIL: VIRTIO_INPUT_BIND_FAILED: selftest RESULT=PASS but virtio-input-bind test reported FAIL$details (see serial log for bound service name / ConfigManager error details)"
      Write-AeroVirtioInputBindDiagnostics -SerialLogPath $SerialLogPath
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_INPUT_LEDS_SKIPPED" {
      Write-Host "FAIL: VIRTIO_INPUT_LEDS_SKIPPED: virtio-input-leds test was skipped (flag_not_set) but -WithInputLeds/-WithVirtioInputLeds/-RequireVirtioInputLeds/-EnableVirtioInputLeds was enabled (provision the guest with --test-input-leds; newer guest selftests also accept --test-input-led)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_INPUT_LEDS_FAILED" {
      $reason = "unknown"
      $err = "unknown"
      $writes = ""
      $line = Try-ExtractLastAeroMarkerLine `
        -Tail $result.Tail `
        -Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-input-leds|FAIL|" `
        -SerialLogPath $SerialLogPath
      if ($null -ne $line) {
        if ($line -match "reason=([^|\r\n]+)") { $reason = $Matches[1] }
        elseif ($line -match "\|FAIL\|([^|\r\n=]+)(?:\||$)") { $reason = $Matches[1] }
        if ($line -match "(?:^|\|)err=([^|\r\n]+)") { $err = $Matches[1] }
        if ($line -match "(?:^|\|)writes=([^|\r\n]+)") { $writes = $Matches[1] }
      }
      $details = "(reason=$reason err=$err"
      if (-not [string]::IsNullOrEmpty($writes)) { $details += " writes=$writes" }
      $details += ")"
      Write-Host "FAIL: VIRTIO_INPUT_LEDS_FAILED: virtio-input-leds test reported FAIL while -WithInputLeds/-WithVirtioInputLeds/-RequireVirtioInputLeds/-EnableVirtioInputLeds was enabled $details"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "MISSING_VIRTIO_INPUT_EVENTS" {
      Write-Host "FAIL: MISSING_VIRTIO_INPUT_EVENTS: did not observe virtio-input-events marker (READY/SKIP/PASS/FAIL) after virtio-input completed while input injection flags were enabled (-WithInputEvents/-WithVirtioInputEvents/-RequireVirtioInputEvents/-EnableVirtioInputEvents, -WithInputWheel/-WithVirtioInputWheel/-RequireVirtioInputWheel/-EnableVirtioInputWheel, -WithInputEventsExtended/-WithInputEventsExtra) (guest selftest too old or missing --test-input-events)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "MISSING_VIRTIO_INPUT_EVENTS_EXTENDED" {
      Write-Host "FAIL: MISSING_VIRTIO_INPUT_EVENTS_EXTENDED: did not observe virtio-input-events-modifiers/buttons/wheel markers while -WithInputEventsExtended/-WithInputEventsExtra was enabled (guest selftest too old or missing --test-input-events-extended)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_INPUT_EVENTS_SKIPPED" {
      Write-Host "FAIL: VIRTIO_INPUT_EVENTS_SKIPPED: virtio-input-events test was skipped (flag_not_set) but input injection flags were enabled (-WithInputEvents/-WithVirtioInputEvents/-RequireVirtioInputEvents/-EnableVirtioInputEvents, -WithInputWheel/-WithVirtioInputWheel/-RequireVirtioInputWheel/-EnableVirtioInputWheel, -WithInputEventsExtended/-WithInputEventsExtra) (provision the guest with --test-input-events)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_INPUT_EVENTS_EXTENDED_SKIPPED" {
      $subtest = ""
      $reason = "unknown"
      $line = $null

      $line = Try-ExtractLastAeroMarkerLine `
        -Tail $result.Tail `
        -Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-modifiers|SKIP|" `
        -SerialLogPath $SerialLogPath
      if ($null -ne $line) {
        $subtest = "virtio-input-events-modifiers"
      } else {
        $line = Try-ExtractLastAeroMarkerLine `
          -Tail $result.Tail `
          -Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-buttons|SKIP|" `
          -SerialLogPath $SerialLogPath
        if ($null -ne $line) {
          $subtest = "virtio-input-events-buttons"
        } else {
          $line = Try-ExtractLastAeroMarkerLine `
            -Tail $result.Tail `
            -Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-wheel|SKIP|" `
            -SerialLogPath $SerialLogPath
          if ($null -ne $line) {
            $subtest = "virtio-input-events-wheel"
          }
        }
      }

      if ($null -ne $line) {
        if ($line -match "(?:^|\|)reason=([^|\r\n]+)") { $reason = $Matches[1] }
        elseif ($line -match "\|SKIP\|([^|\r\n=]+)(?:\||$)") { $reason = $Matches[1] }
      }

      $subtestDesc = $subtest
      if ([string]::IsNullOrEmpty($subtestDesc)) { $subtestDesc = "virtio-input-events-*" }

      if ($reason -eq "flag_not_set") {
        Write-Host "FAIL: VIRTIO_INPUT_EVENTS_EXTENDED_SKIPPED: $subtestDesc was skipped (flag_not_set) but -WithInputEventsExtended/-WithInputEventsExtra was enabled (provision the guest with --test-input-events-extended)"
      } else {
        Write-Host "FAIL: VIRTIO_INPUT_EVENTS_EXTENDED_SKIPPED: $subtestDesc was skipped ($reason) but -WithInputEventsExtended/-WithInputEventsExtra was enabled"
      }
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_INPUT_EVENTS_FAILED" {
      $reason = "unknown"
      $err = "unknown"
      $kbdReports = ""
      $mouseReports = ""
      $kbdBadReports = ""
      $mouseBadReports = ""
      $kbdADown = ""
      $kbdAUp = ""
      $mouseMove = ""
      $mouseLeftDown = ""
      $mouseLeftUp = ""
      $line = Try-ExtractLastAeroMarkerLine `
        -Tail $result.Tail `
        -Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|FAIL|" `
        -SerialLogPath $SerialLogPath
      if ($null -ne $line) {
        if ($line -match "reason=([^|\r\n]+)") { $reason = $Matches[1] }
        elseif ($line -match "\|FAIL\|([^|\r\n=]+)(?:\||$)") { $reason = $Matches[1] }
        if ($line -match "(?:^|\|)err=([^|\r\n]+)") { $err = $Matches[1] }
        if ($line -match "(?:^|\|)kbd_reports=([^|\r\n]+)") { $kbdReports = $Matches[1] }
        if ($line -match "(?:^|\|)mouse_reports=([^|\r\n]+)") { $mouseReports = $Matches[1] }
        if ($line -match "(?:^|\|)kbd_bad_reports=([^|\r\n]+)") { $kbdBadReports = $Matches[1] }
        if ($line -match "(?:^|\|)mouse_bad_reports=([^|\r\n]+)") { $mouseBadReports = $Matches[1] }
        if ($line -match "(?:^|\|)kbd_a_down=([^|\r\n]+)") { $kbdADown = $Matches[1] }
        if ($line -match "(?:^|\|)kbd_a_up=([^|\r\n]+)") { $kbdAUp = $Matches[1] }
        if ($line -match "(?:^|\|)mouse_move=([^|\r\n]+)") { $mouseMove = $Matches[1] }
        if ($line -match "(?:^|\|)mouse_left_down=([^|\r\n]+)") { $mouseLeftDown = $Matches[1] }
        if ($line -match "(?:^|\|)mouse_left_up=([^|\r\n]+)") { $mouseLeftUp = $Matches[1] }
      }
      $details = "(reason=$reason err=$err"
      if (-not [string]::IsNullOrEmpty($kbdReports)) { $details += " kbd_reports=$kbdReports" }
      if (-not [string]::IsNullOrEmpty($mouseReports)) { $details += " mouse_reports=$mouseReports" }
      if (-not [string]::IsNullOrEmpty($kbdBadReports)) { $details += " kbd_bad_reports=$kbdBadReports" }
      if (-not [string]::IsNullOrEmpty($mouseBadReports)) { $details += " mouse_bad_reports=$mouseBadReports" }
      if (-not [string]::IsNullOrEmpty($kbdADown)) { $details += " kbd_a_down=$kbdADown" }
      if (-not [string]::IsNullOrEmpty($kbdAUp)) { $details += " kbd_a_up=$kbdAUp" }
      if (-not [string]::IsNullOrEmpty($mouseMove)) { $details += " mouse_move=$mouseMove" }
      if (-not [string]::IsNullOrEmpty($mouseLeftDown)) { $details += " mouse_left_down=$mouseLeftDown" }
      if (-not [string]::IsNullOrEmpty($mouseLeftUp)) { $details += " mouse_left_up=$mouseLeftUp" }
      $details += ")"
      Write-Host "FAIL: VIRTIO_INPUT_EVENTS_FAILED: virtio-input-events test reported FAIL while input injection flags were enabled (-WithInputEvents/-WithVirtioInputEvents/-RequireVirtioInputEvents/-EnableVirtioInputEvents, -WithInputWheel/-WithVirtioInputWheel/-RequireVirtioInputWheel/-EnableVirtioInputWheel, -WithInputEventsExtended/-WithInputEventsExtra) $details"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "MISSING_VIRTIO_INPUT_MEDIA_KEYS" {
      Write-Host "FAIL: MISSING_VIRTIO_INPUT_MEDIA_KEYS: did not observe virtio-input-media-keys marker (READY/SKIP/PASS/FAIL) after virtio-input completed while -WithInputMediaKeys/-WithVirtioInputMediaKeys/-RequireVirtioInputMediaKeys/-EnableVirtioInputMediaKeys was enabled (guest selftest too old or missing --test-input-media-keys)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_INPUT_MEDIA_KEYS_SKIPPED" {
      Write-Host "FAIL: VIRTIO_INPUT_MEDIA_KEYS_SKIPPED: virtio-input-media-keys test was skipped (flag_not_set) but -WithInputMediaKeys/-WithVirtioInputMediaKeys/-RequireVirtioInputMediaKeys/-EnableVirtioInputMediaKeys was enabled (provision the guest with --test-input-media-keys)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_INPUT_MEDIA_KEYS_FAILED" {
      $reason = "unknown"
      $err = "unknown"
      $reports = ""
      $volumeUpDown = ""
      $volumeUpUp = ""
      $line = Try-ExtractLastAeroMarkerLine `
        -Tail $result.Tail `
        -Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-input-media-keys|FAIL|" `
        -SerialLogPath $SerialLogPath
      if ($null -ne $line) {
        if ($line -match "reason=([^|\r\n]+)") { $reason = $Matches[1] }
        elseif ($line -match "\|FAIL\|([^|\r\n=]+)(?:\||$)") { $reason = $Matches[1] }
        if ($line -match "(?:^|\|)err=([^|\r\n]+)") { $err = $Matches[1] }
        if ($line -match "(?:^|\|)reports=([^|\r\n]+)") { $reports = $Matches[1] }
        if ($line -match "(?:^|\|)volume_up_down=([^|\r\n]+)") { $volumeUpDown = $Matches[1] }
        if ($line -match "(?:^|\|)volume_up_up=([^|\r\n]+)") { $volumeUpUp = $Matches[1] }
      }
      $details = "(reason=$reason err=$err"
      if (-not [string]::IsNullOrEmpty($reports)) { $details += " reports=$reports" }
      if (-not [string]::IsNullOrEmpty($volumeUpDown)) { $details += " volume_up_down=$volumeUpDown" }
      if (-not [string]::IsNullOrEmpty($volumeUpUp)) { $details += " volume_up_up=$volumeUpUp" }
      $details += ")"
      Write-Host "FAIL: VIRTIO_INPUT_MEDIA_KEYS_FAILED: virtio-input-media-keys test reported FAIL while -WithInputMediaKeys/-WithVirtioInputMediaKeys/-RequireVirtioInputMediaKeys/-EnableVirtioInputMediaKeys was enabled $details"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "MISSING_VIRTIO_INPUT_LED" {
      Write-Host "FAIL: MISSING_VIRTIO_INPUT_LED: did not observe virtio-input-led marker (SKIP/PASS/FAIL) after virtio-input completed while -WithInputLed/-WithVirtioInputLed/-RequireVirtioInputLed/-EnableVirtioInputLed was enabled (guest selftest too old or missing --test-input-led)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_INPUT_LED_SKIPPED" {
      $reason = "unknown"
      $line = Try-ExtractLastAeroMarkerLine `
        -Tail $result.Tail `
        -Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-input-led|SKIP|" `
        -SerialLogPath $SerialLogPath
      if ($null -ne $line) {
        if ($line -match "(?:^|\|)reason=([^|\r\n]+)") { $reason = $Matches[1] }
        elseif ($line -match "\|SKIP\|([^|\r\n=]+)(?:\||$)") { $reason = $Matches[1] }
      }

      if ($reason -eq "flag_not_set") {
        Write-Host "FAIL: VIRTIO_INPUT_LED_SKIPPED: virtio-input-led test was skipped (flag_not_set) but -WithInputLed/-WithVirtioInputLed/-RequireVirtioInputLed/-EnableVirtioInputLed was enabled (provision the guest with --test-input-led)"
      } else {
        Write-Host "FAIL: VIRTIO_INPUT_LED_SKIPPED: virtio-input-led test was skipped ($reason) but -WithInputLed/-WithVirtioInputLed/-RequireVirtioInputLed/-EnableVirtioInputLed was enabled"
      }
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_INPUT_LED_FAILED" {
      $reason = "unknown"
      $err = "unknown"
      $sent = ""
      $format = ""
      $led = ""
      $line = Try-ExtractLastAeroMarkerLine `
        -Tail $result.Tail `
        -Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-input-led|FAIL|" `
        -SerialLogPath $SerialLogPath
      if ($null -ne $line) {
        if ($line -match "reason=([^|\r\n]+)") { $reason = $Matches[1] }
        elseif ($line -match "\|FAIL\|([^|\r\n=]+)(?:\||$)") { $reason = $Matches[1] }
        if ($line -match "(?:^|\|)err=([^|\r\n]+)") { $err = $Matches[1] }
        if ($line -match "(?:^|\|)sent=([^|\r\n]+)") { $sent = $Matches[1] }
        if ($line -match "(?:^|\|)format=([^|\r\n]+)") { $format = $Matches[1] }
        if ($line -match "(?:^|\|)led=([^|\r\n]+)") { $led = $Matches[1] }
      }
      $details = "(reason=$reason err=$err"
      if (-not [string]::IsNullOrEmpty($sent)) { $details += " sent=$sent" }
      if (-not [string]::IsNullOrEmpty($format)) { $details += " format=$format" }
      if (-not [string]::IsNullOrEmpty($led)) { $details += " led=$led" }
      $details += ")"
      Write-Host "FAIL: VIRTIO_INPUT_LED_FAILED: virtio-input-led test reported FAIL while -WithInputLed/-WithVirtioInputLed/-RequireVirtioInputLed/-EnableVirtioInputLed was enabled $details"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "QMP_MEDIA_KEYS_UNSUPPORTED" {
      Write-Host "FAIL: QMP_MEDIA_KEYS_UNSUPPORTED: failed to inject virtio-input media keys via QMP (ensure QMP is reachable and QEMU supports input-send-event or send-key / human-monitor-command sendkey fallback)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "QEMU_PCI_PREFLIGHT_FAILED" {
      $reason = ""
      try { $reason = [string]$result.Reason } catch { }
      if ([string]::IsNullOrEmpty($reason)) { $reason = "QMP query-pci preflight failed" }
      Write-Host "FAIL: QEMU_PCI_PREFLIGHT_FAILED: $reason"
      Write-AeroQemuStderrTail -Path $qemuStderrPath
      $scriptExitCode = 2
    }
    "MISSING_VIRTIO_INPUT_WHEEL" {
      Write-Host "FAIL: MISSING_VIRTIO_INPUT_WHEEL: did not observe virtio-input-wheel marker while -WithInputWheel/-WithVirtioInputWheel/-RequireVirtioInputWheel/-EnableVirtioInputWheel was enabled (guest selftest too old or missing wheel coverage)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_INPUT_WHEEL_SKIPPED" {
      $code = "unknown"
      $reason = ""
      $err = ""
      $wheelTotal = ""
      $hwheelTotal = ""
      $line = Try-ExtractLastAeroMarkerLine `
        -Tail $result.Tail `
        -Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-input-wheel|SKIP|" `
        -SerialLogPath $SerialLogPath
      if ($null -ne $line) {
        if ($line -match "\|SKIP\|([^|\r\n=]+)(?:\||$)") { $code = $Matches[1] }
        if ($line -match "(?:^|\|)reason=([^|\r\n]+)") { $reason = $Matches[1] }
        if ($line -match "(?:^|\|)err=([^|\r\n]+)") { $err = $Matches[1] }
        if ($line -match "(?:^|\|)wheel_total=([^|\r\n]+)") { $wheelTotal = $Matches[1] }
        if ($line -match "(?:^|\|)hwheel_total=([^|\r\n]+)") { $hwheelTotal = $Matches[1] }
      }
      if ($code -eq "flag_not_set") {
        Write-Host "FAIL: VIRTIO_INPUT_WHEEL_SKIPPED: virtio-input-wheel test was skipped (flag_not_set) but -WithInputWheel/-WithVirtioInputWheel/-RequireVirtioInputWheel/-EnableVirtioInputWheel was enabled (provision the guest with --test-input-events)"
      } else {
        $parts = @()
        if ($code -ne "unknown") { $parts += $code }
        if (-not [string]::IsNullOrEmpty($reason)) { $parts += "reason=$reason" }
        if (-not [string]::IsNullOrEmpty($err)) { $parts += "err=$err" }
        if (-not [string]::IsNullOrEmpty($wheelTotal)) { $parts += "wheel_total=$wheelTotal" }
        if (-not [string]::IsNullOrEmpty($hwheelTotal)) { $parts += "hwheel_total=$hwheelTotal" }
        $details = if ($parts.Count -gt 0) { " (" + ($parts -join " ") + ")" } else { "" }
        Write-Host "FAIL: VIRTIO_INPUT_WHEEL_SKIPPED: virtio-input-wheel test was skipped$details but -WithInputWheel/-WithVirtioInputWheel/-RequireVirtioInputWheel/-EnableVirtioInputWheel was enabled"
      }
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_INPUT_WHEEL_FAILED" {
      $reason = "unknown"
      $wheelTotal = ""
      $hwheelTotal = ""
      $expectedWheel = ""
      $expectedHwheel = ""
      $wheelEvents = ""
      $hwheelEvents = ""
      $sawWheel = ""
      $sawHwheel = ""
      $sawWheelExpected = ""
      $sawHwheelExpected = ""
      $wheelUnexpectedLast = ""
      $hwheelUnexpectedLast = ""
      $line = Try-ExtractLastAeroMarkerLine `
        -Tail $result.Tail `
        -Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-input-wheel|FAIL|" `
        -SerialLogPath $SerialLogPath
      if ($null -ne $line) {
        if ($line -match "reason=([^|\r\n]+)") { $reason = $Matches[1] }
        elseif ($line -match "\|FAIL\|([^|\r\n=]+)(?:\||$)") { $reason = $Matches[1] }
        if ($line -match "(?:^|\|)wheel_total=([^|\r\n]+)") { $wheelTotal = $Matches[1] }
        if ($line -match "(?:^|\|)hwheel_total=([^|\r\n]+)") { $hwheelTotal = $Matches[1] }
        if ($line -match "(?:^|\|)expected_wheel=([^|\r\n]+)") { $expectedWheel = $Matches[1] }
        if ($line -match "(?:^|\|)expected_hwheel=([^|\r\n]+)") { $expectedHwheel = $Matches[1] }
        if ($line -match "(?:^|\|)wheel_events=([^|\r\n]+)") { $wheelEvents = $Matches[1] }
        if ($line -match "(?:^|\|)hwheel_events=([^|\r\n]+)") { $hwheelEvents = $Matches[1] }
        if ($line -match "(?:^|\|)saw_wheel=([^|\r\n]+)") { $sawWheel = $Matches[1] }
        if ($line -match "(?:^|\|)saw_hwheel=([^|\r\n]+)") { $sawHwheel = $Matches[1] }
        if ($line -match "(?:^|\|)saw_wheel_expected=([^|\r\n]+)") { $sawWheelExpected = $Matches[1] }
        if ($line -match "(?:^|\|)saw_hwheel_expected=([^|\r\n]+)") { $sawHwheelExpected = $Matches[1] }
        if ($line -match "(?:^|\|)wheel_unexpected_last=([^|\r\n]+)") { $wheelUnexpectedLast = $Matches[1] }
        if ($line -match "(?:^|\|)hwheel_unexpected_last=([^|\r\n]+)") { $hwheelUnexpectedLast = $Matches[1] }
      }
      $details = "(reason=$reason"
      if (-not [string]::IsNullOrEmpty($wheelTotal)) { $details += " wheel_total=$wheelTotal" }
      if (-not [string]::IsNullOrEmpty($hwheelTotal)) { $details += " hwheel_total=$hwheelTotal" }
      if (-not [string]::IsNullOrEmpty($expectedWheel)) { $details += " expected_wheel=$expectedWheel" }
      if (-not [string]::IsNullOrEmpty($expectedHwheel)) { $details += " expected_hwheel=$expectedHwheel" }
      if (-not [string]::IsNullOrEmpty($wheelEvents)) { $details += " wheel_events=$wheelEvents" }
      if (-not [string]::IsNullOrEmpty($hwheelEvents)) { $details += " hwheel_events=$hwheelEvents" }
      if (-not [string]::IsNullOrEmpty($sawWheel)) { $details += " saw_wheel=$sawWheel" }
      if (-not [string]::IsNullOrEmpty($sawHwheel)) { $details += " saw_hwheel=$sawHwheel" }
      if (-not [string]::IsNullOrEmpty($sawWheelExpected)) { $details += " saw_wheel_expected=$sawWheelExpected" }
      if (-not [string]::IsNullOrEmpty($sawHwheelExpected)) { $details += " saw_hwheel_expected=$sawHwheelExpected" }
      if (-not [string]::IsNullOrEmpty($wheelUnexpectedLast)) { $details += " wheel_unexpected_last=$wheelUnexpectedLast" }
      if (-not [string]::IsNullOrEmpty($hwheelUnexpectedLast)) { $details += " hwheel_unexpected_last=$hwheelUnexpectedLast" }
      $details += ")"
      Write-Host "FAIL: VIRTIO_INPUT_WHEEL_FAILED: virtio-input-wheel test reported FAIL while -WithInputWheel/-WithVirtioInputWheel/-RequireVirtioInputWheel/-EnableVirtioInputWheel was enabled $details"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_INPUT_EVENTS_EXTENDED_FAILED" {
      $subtest = ""
      $line = $null
      $line = Try-ExtractLastAeroMarkerLine `
        -Tail $result.Tail `
        -Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-modifiers|FAIL|" `
        -SerialLogPath $SerialLogPath
      if ($null -ne $line) {
        $subtest = "virtio-input-events-modifiers"
      } else {
        $line = Try-ExtractLastAeroMarkerLine `
          -Tail $result.Tail `
          -Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-buttons|FAIL|" `
          -SerialLogPath $SerialLogPath
        if ($null -ne $line) {
          $subtest = "virtio-input-events-buttons"
        } else {
          $line = Try-ExtractLastAeroMarkerLine `
            -Tail $result.Tail `
            -Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-wheel|FAIL|" `
            -SerialLogPath $SerialLogPath
          if ($null -ne $line) {
            $subtest = "virtio-input-events-wheel"
          }
        }
      }

      $reason = ""
      $err = ""
      $kbdReports = ""
      $kbdBadReports = ""
      $shiftB = ""
      $ctrlDown = ""
      $ctrlUp = ""
      $altDown = ""
      $altUp = ""
      $f1Down = ""
      $f1Up = ""
      $mouseReports = ""
      $mouseBadReports = ""
      $sideDown = ""
      $sideUp = ""
      $extraDown = ""
      $extraUp = ""
      $wheelTotal = ""
      $hwheelTotal = ""
      $expectedWheel = ""
      $expectedHwheel = ""
      $sawWheel = ""
      $sawHwheel = ""

      if ($null -ne $line) {
        if ($line -match "(?:^|\|)reason=([^|\r\n]+)") { $reason = $Matches[1] }
        if ($line -match "(?:^|\|)err=([^|\r\n]+)") { $err = $Matches[1] }
        if ($line -match "(?:^|\|)kbd_reports=([^|\r\n]+)") { $kbdReports = $Matches[1] }
        if ($line -match "(?:^|\|)kbd_bad_reports=([^|\r\n]+)") { $kbdBadReports = $Matches[1] }
        if ($line -match "(?:^|\|)shift_b=([^|\r\n]+)") { $shiftB = $Matches[1] }
        if ($line -match "(?:^|\|)ctrl_down=([^|\r\n]+)") { $ctrlDown = $Matches[1] }
        if ($line -match "(?:^|\|)ctrl_up=([^|\r\n]+)") { $ctrlUp = $Matches[1] }
        if ($line -match "(?:^|\|)alt_down=([^|\r\n]+)") { $altDown = $Matches[1] }
        if ($line -match "(?:^|\|)alt_up=([^|\r\n]+)") { $altUp = $Matches[1] }
        if ($line -match "(?:^|\|)f1_down=([^|\r\n]+)") { $f1Down = $Matches[1] }
        if ($line -match "(?:^|\|)f1_up=([^|\r\n]+)") { $f1Up = $Matches[1] }
        if ($line -match "(?:^|\|)mouse_reports=([^|\r\n]+)") { $mouseReports = $Matches[1] }
        if ($line -match "(?:^|\|)mouse_bad_reports=([^|\r\n]+)") { $mouseBadReports = $Matches[1] }
        if ($line -match "(?:^|\|)side_down=([^|\r\n]+)") { $sideDown = $Matches[1] }
        if ($line -match "(?:^|\|)side_up=([^|\r\n]+)") { $sideUp = $Matches[1] }
        if ($line -match "(?:^|\|)extra_down=([^|\r\n]+)") { $extraDown = $Matches[1] }
        if ($line -match "(?:^|\|)extra_up=([^|\r\n]+)") { $extraUp = $Matches[1] }
        if ($line -match "(?:^|\|)wheel_total=([^|\r\n]+)") { $wheelTotal = $Matches[1] }
        if ($line -match "(?:^|\|)hwheel_total=([^|\r\n]+)") { $hwheelTotal = $Matches[1] }
        if ($line -match "(?:^|\|)expected_wheel=([^|\r\n]+)") { $expectedWheel = $Matches[1] }
        if ($line -match "(?:^|\|)expected_hwheel=([^|\r\n]+)") { $expectedHwheel = $Matches[1] }
        if ($line -match "(?:^|\|)saw_wheel=([^|\r\n]+)") { $sawWheel = $Matches[1] }
        if ($line -match "(?:^|\|)saw_hwheel=([^|\r\n]+)") { $sawHwheel = $Matches[1] }
      }

      $detailsParts = @()
      if (-not [string]::IsNullOrEmpty($reason)) { $detailsParts += "reason=$reason" }
      if (-not [string]::IsNullOrEmpty($err)) { $detailsParts += "err=$err" }
      if (-not [string]::IsNullOrEmpty($kbdReports)) { $detailsParts += "kbd_reports=$kbdReports" }
      if (-not [string]::IsNullOrEmpty($kbdBadReports)) { $detailsParts += "kbd_bad_reports=$kbdBadReports" }
      if (-not [string]::IsNullOrEmpty($shiftB)) { $detailsParts += "shift_b=$shiftB" }
      if (-not [string]::IsNullOrEmpty($ctrlDown)) { $detailsParts += "ctrl_down=$ctrlDown" }
      if (-not [string]::IsNullOrEmpty($ctrlUp)) { $detailsParts += "ctrl_up=$ctrlUp" }
      if (-not [string]::IsNullOrEmpty($altDown)) { $detailsParts += "alt_down=$altDown" }
      if (-not [string]::IsNullOrEmpty($altUp)) { $detailsParts += "alt_up=$altUp" }
      if (-not [string]::IsNullOrEmpty($f1Down)) { $detailsParts += "f1_down=$f1Down" }
      if (-not [string]::IsNullOrEmpty($f1Up)) { $detailsParts += "f1_up=$f1Up" }
      if (-not [string]::IsNullOrEmpty($mouseReports)) { $detailsParts += "mouse_reports=$mouseReports" }
      if (-not [string]::IsNullOrEmpty($mouseBadReports)) { $detailsParts += "mouse_bad_reports=$mouseBadReports" }
      if (-not [string]::IsNullOrEmpty($sideDown)) { $detailsParts += "side_down=$sideDown" }
      if (-not [string]::IsNullOrEmpty($sideUp)) { $detailsParts += "side_up=$sideUp" }
      if (-not [string]::IsNullOrEmpty($extraDown)) { $detailsParts += "extra_down=$extraDown" }
      if (-not [string]::IsNullOrEmpty($extraUp)) { $detailsParts += "extra_up=$extraUp" }
      if (-not [string]::IsNullOrEmpty($wheelTotal)) { $detailsParts += "wheel_total=$wheelTotal" }
      if (-not [string]::IsNullOrEmpty($hwheelTotal)) { $detailsParts += "hwheel_total=$hwheelTotal" }
      if (-not [string]::IsNullOrEmpty($expectedWheel)) { $detailsParts += "expected_wheel=$expectedWheel" }
      if (-not [string]::IsNullOrEmpty($expectedHwheel)) { $detailsParts += "expected_hwheel=$expectedHwheel" }
      if (-not [string]::IsNullOrEmpty($sawWheel)) { $detailsParts += "saw_wheel=$sawWheel" }
      if (-not [string]::IsNullOrEmpty($sawHwheel)) { $detailsParts += "saw_hwheel=$sawHwheel" }
      $details = ""
      if ($detailsParts.Count -gt 0) { $details = " (" + ($detailsParts -join " ") + ")" }

      $subtestDesc = $subtest
      if ([string]::IsNullOrEmpty($subtestDesc)) { $subtestDesc = "virtio-input-events-*" }

      Write-Host "FAIL: VIRTIO_INPUT_EVENTS_EXTENDED_FAILED: $subtestDesc reported FAIL while -WithInputEventsExtended/-WithInputEventsExtra was enabled$details"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "QMP_INPUT_INJECT_FAILED" {
      Write-Host "FAIL: QMP_INPUT_INJECT_FAILED: failed to inject virtio-input events via QMP (ensure QMP is reachable and QEMU supports an input injection mechanism: input-send-event, send-key, or human-monitor-command)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "QMP_SET_LINK_UNSUPPORTED" {
      Write-Host "FAIL: QMP_SET_LINK_UNSUPPORTED: unsupported QEMU: QMP does not support set_link (required for -WithNetLinkFlap/-WithVirtioNetLinkFlap/-RequireVirtioNetLinkFlap/-EnableVirtioNetLinkFlap). Upgrade QEMU or omit net link flap testing."
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "QMP_SET_LINK_FAILED" {
      Write-Host "FAIL: QMP_SET_LINK_FAILED: failed to toggle virtio-net link via QMP set_link (ensure QMP is reachable and the virtio-net device uses a stable id)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "MISSING_VIRTIO_INPUT_TABLET_EVENTS" {
      Write-Host "FAIL: MISSING_VIRTIO_INPUT_TABLET_EVENTS: did not observe virtio-input-tablet-events marker (READY/SKIP/PASS/FAIL) after virtio-input completed while -WithInputTabletEvents/-WithVirtioInputTabletEvents/-RequireVirtioInputTabletEvents/-WithTabletEvents/-EnableTabletEvents was enabled (guest selftest too old or missing --test-input-tablet-events/--test-tablet-events)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_INPUT_TABLET_EVENTS_SKIPPED" {
      $reason = "unknown"
      $line = Try-ExtractLastAeroMarkerLine `
        -Tail $result.Tail `
        -Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|SKIP|" `
        -SerialLogPath $SerialLogPath
      if ($null -ne $line) {
        if ($line -match "reason=([^|\r\n]+)") { $reason = $Matches[1] }
        elseif ($line -match "\|SKIP\|([^|\r\n=]+)(?:\||$)") { $reason = $Matches[1] }
      }
      if ($reason -eq "flag_not_set") {
        Write-Host "FAIL: VIRTIO_INPUT_TABLET_EVENTS_SKIPPED: virtio-input-tablet-events test was skipped (flag_not_set) but -WithInputTabletEvents/-WithVirtioInputTabletEvents/-RequireVirtioInputTabletEvents/-WithTabletEvents/-EnableTabletEvents was enabled (provision the guest with --test-input-tablet-events/--test-tablet-events)"
      } elseif ($reason -eq "no_tablet_device") {
        Write-Host "FAIL: VIRTIO_INPUT_TABLET_EVENTS_SKIPPED: virtio-input-tablet-events test was skipped (no_tablet_device) but -WithInputTabletEvents/-WithVirtioInputTabletEvents/-RequireVirtioInputTabletEvents/-WithTabletEvents/-EnableTabletEvents was enabled (attach a virtio-tablet device with -WithVirtioTablet and ensure the guest tablet driver is installed)"
      } else {
        Write-Host "FAIL: VIRTIO_INPUT_TABLET_EVENTS_SKIPPED: virtio-input-tablet-events test was skipped ($reason) but -WithInputTabletEvents/-WithVirtioInputTabletEvents/-RequireVirtioInputTabletEvents/-WithTabletEvents/-EnableTabletEvents was enabled"
      }
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_INPUT_TABLET_EVENTS_FAILED" {
      $reason = "unknown"
      $err = "unknown"
      $tabletReports = ""
      $moveTarget = ""
      $leftDown = ""
      $leftUp = ""
      $lastX = ""
      $lastY = ""
      $lastLeft = ""
      $line = Try-ExtractLastAeroMarkerLine `
        -Tail $result.Tail `
        -Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|FAIL|" `
        -SerialLogPath $SerialLogPath
      if ($null -ne $line) {
        if ($line -match "reason=([^|\r\n]+)") { $reason = $Matches[1] }
        elseif ($line -match "\|FAIL\|([^|\r\n=]+)(?:\||$)") { $reason = $Matches[1] }
        if ($line -match "(?:^|\|)err=([^|\r\n]+)") { $err = $Matches[1] }
        if ($line -match "(?:^|\|)tablet_reports=([^|\r\n]+)") { $tabletReports = $Matches[1] }
        if ($line -match "(?:^|\|)move_target=([^|\r\n]+)") { $moveTarget = $Matches[1] }
        if ($line -match "(?:^|\|)left_down=([^|\r\n]+)") { $leftDown = $Matches[1] }
        if ($line -match "(?:^|\|)left_up=([^|\r\n]+)") { $leftUp = $Matches[1] }
        if ($line -match "(?:^|\|)last_x=([^|\r\n]+)") { $lastX = $Matches[1] }
        if ($line -match "(?:^|\|)last_y=([^|\r\n]+)") { $lastY = $Matches[1] }
        if ($line -match "(?:^|\|)last_left=([^|\r\n]+)") { $lastLeft = $Matches[1] }
      }
      $details = "(reason=$reason err=$err"
      if (-not [string]::IsNullOrEmpty($tabletReports)) { $details += " tablet_reports=$tabletReports" }
      if (-not [string]::IsNullOrEmpty($moveTarget)) { $details += " move_target=$moveTarget" }
      if (-not [string]::IsNullOrEmpty($leftDown)) { $details += " left_down=$leftDown" }
      if (-not [string]::IsNullOrEmpty($leftUp)) { $details += " left_up=$leftUp" }
      if (-not [string]::IsNullOrEmpty($lastX)) { $details += " last_x=$lastX" }
      if (-not [string]::IsNullOrEmpty($lastY)) { $details += " last_y=$lastY" }
      if (-not [string]::IsNullOrEmpty($lastLeft)) { $details += " last_left=$lastLeft" }
      $details += ")"
      Write-Host "FAIL: VIRTIO_INPUT_TABLET_EVENTS_FAILED: virtio-input-tablet-events test reported FAIL while -WithInputTabletEvents/-WithVirtioInputTabletEvents/-RequireVirtioInputTabletEvents/-WithTabletEvents/-EnableTabletEvents was enabled $details"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "QMP_INPUT_TABLET_INJECT_FAILED" {
      Write-Host "FAIL: QMP_INPUT_TABLET_INJECT_FAILED: failed to inject virtio-input tablet events via QMP (ensure QMP is reachable and QEMU supports input-send-event; no backcompat path is available for absolute tablet injection)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_SND_FAILED" {
      $reason = ""
      $irqMode = ""
      $irqMessageCount = ""
      $irqReason = ""
      $line = Try-ExtractLastAeroMarkerLine `
        -Tail $result.Tail `
        -Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-snd|FAIL|" `
        -SerialLogPath $SerialLogPath
      if ($null -ne $line) {
        if ($line -match "(?:^|\|)reason=([^|\r\n]+)") { $reason = $Matches[1] }
        elseif ($line -match "\|FAIL\|([^|\r\n]+)(?:\||$)") {
          $tok = $Matches[1]
          if (-not [string]::IsNullOrEmpty($tok) -and (-not ($tok -match "="))) { $reason = $tok }
        }
        if ($line -match "(?:^|\|)irq_mode=([^|\r\n]+)") { $irqMode = $Matches[1] }
        if ($line -match "(?:^|\|)irq_message_count=([^|\r\n]+)") { $irqMessageCount = $Matches[1] }
        if ($line -match "(?:^|\|)irq_reason=([^|\r\n]+)") { $irqReason = $Matches[1] }
      }
      $detailsParts = @()
      if (-not [string]::IsNullOrEmpty($reason)) { $detailsParts += "reason=$reason" }
      if (-not [string]::IsNullOrEmpty($irqMode)) { $detailsParts += "irq_mode=$irqMode" }
      if (-not [string]::IsNullOrEmpty($irqMessageCount)) { $detailsParts += "irq_message_count=$irqMessageCount" }
      if (-not [string]::IsNullOrEmpty($irqReason)) { $detailsParts += "irq_reason=$irqReason" }
      $details = ""
      if ($detailsParts.Count -gt 0) { $details = " (" + ($detailsParts -join " ") + ")" }
      Write-Host "FAIL: VIRTIO_SND_FAILED: selftest RESULT=PASS but virtio-snd test reported FAIL$details"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_SND_CAPTURE_FAILED" {
      $reason = ""
      $hr = ""
      $line = Try-ExtractLastAeroMarkerLine `
        -Tail $result.Tail `
        -Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|FAIL|" `
        -SerialLogPath $SerialLogPath
      if ($null -ne $line) {
        if ($line -match "(?:^|\|)reason=([^|\r\n]+)") { $reason = $Matches[1] }
        elseif ($line -match "\|FAIL\|([^|\r\n]+)(?:\||$)") {
          $tok = $Matches[1]
          if (-not [string]::IsNullOrEmpty($tok) -and (-not ($tok -match "="))) { $reason = $tok }
        }
        if ($line -match "(?:^|\|)hr=([^|\r\n]+)") { $hr = $Matches[1] }
      }
      $detailsParts = @()
      if (-not [string]::IsNullOrEmpty($reason)) { $detailsParts += "reason=$reason" }
      if (-not [string]::IsNullOrEmpty($hr)) { $detailsParts += "hr=$hr" }
      $details = ""
      if ($detailsParts.Count -gt 0) { $details = " (" + ($detailsParts -join " ") + ")" }
      Write-Host "FAIL: VIRTIO_SND_CAPTURE_FAILED: selftest RESULT=PASS but virtio-snd-capture test reported FAIL$details"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_SND_DUPLEX_FAILED" {
      $reason = ""
      $hr = ""
      $line = Try-ExtractLastAeroMarkerLine `
        -Tail $result.Tail `
        -Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|FAIL|" `
        -SerialLogPath $SerialLogPath
      if ($null -ne $line) {
        if ($line -match "(?:^|\|)reason=([^|\r\n]+)") { $reason = $Matches[1] }
        elseif ($line -match "\|FAIL\|([^|\r\n]+)(?:\||$)") {
          $tok = $Matches[1]
          if (-not [string]::IsNullOrEmpty($tok) -and (-not ($tok -match "="))) { $reason = $tok }
        }
        if ($line -match "(?:^|\|)hr=([^|\r\n]+)") { $hr = $Matches[1] }
      }
      $detailsParts = @()
      if (-not [string]::IsNullOrEmpty($reason)) { $detailsParts += "reason=$reason" }
      if (-not [string]::IsNullOrEmpty($hr)) { $detailsParts += "hr=$hr" }
      $details = ""
      if ($detailsParts.Count -gt 0) { $details = " (" + ($detailsParts -join " ") + ")" }
      Write-Host "FAIL: VIRTIO_SND_DUPLEX_FAILED: selftest RESULT=PASS but virtio-snd-duplex test reported FAIL$details"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "MISSING_VIRTIO_SND" {
      Write-Host "FAIL: MISSING_VIRTIO_SND: selftest RESULT=PASS but did not emit virtio-snd test marker"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "MISSING_VIRTIO_SND_CAPTURE" {
      Write-Host "FAIL: MISSING_VIRTIO_SND_CAPTURE: selftest RESULT=PASS but did not emit virtio-snd-capture test marker"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "MISSING_VIRTIO_SND_DUPLEX" {
      Write-Host "FAIL: MISSING_VIRTIO_SND_DUPLEX: selftest RESULT=PASS but did not emit virtio-snd-duplex test marker"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_SND_BUFFER_LIMITS_FAILED" {
      $reason = ""
      $hr = ""
      $line = Try-ExtractLastAeroMarkerLine `
        -Tail $result.Tail `
        -Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-snd-buffer-limits|FAIL|" `
        -SerialLogPath $SerialLogPath
      if ($null -ne $line) {
        if ($line -match "(?:^|\|)reason=([^|\r\n]+)") { $reason = $Matches[1] }
        elseif ($line -match "\|FAIL\|([^|\r\n]+)(?:\||$)") {
          $tok = $Matches[1]
          if (-not [string]::IsNullOrEmpty($tok) -and (-not ($tok -match "="))) { $reason = $tok }
        }
        if ($line -match "(?:^|\|)hr=([^|\r\n]+)") { $hr = $Matches[1] }
      }
      $detailsParts = @()
      if (-not [string]::IsNullOrEmpty($reason)) { $detailsParts += "reason=$reason" }
      if (-not [string]::IsNullOrEmpty($hr)) { $detailsParts += "hr=$hr" }
      $details = ""
      if ($detailsParts.Count -gt 0) { $details = " (" + ($detailsParts -join " ") + ")" }
      Write-Host "FAIL: VIRTIO_SND_BUFFER_LIMITS_FAILED: selftest RESULT=PASS but virtio-snd-buffer-limits test reported FAIL$details while -WithSndBufferLimits/-WithVirtioSndBufferLimits/-RequireVirtioSndBufferLimits/-EnableSndBufferLimits/-EnableVirtioSndBufferLimits was enabled"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "MISSING_VIRTIO_SND_BUFFER_LIMITS" {
      Write-Host "FAIL: MISSING_VIRTIO_SND_BUFFER_LIMITS: selftest RESULT=PASS but did not emit virtio-snd-buffer-limits test marker while -WithSndBufferLimits/-WithVirtioSndBufferLimits/-RequireVirtioSndBufferLimits/-EnableSndBufferLimits/-EnableVirtioSndBufferLimits was enabled (provision the guest with --test-snd-buffer-limits or set env var AERO_VIRTIO_SELFTEST_TEST_SND_BUFFER_LIMITS=1)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "MISSING_VIRTIO_NET" {
      Write-Host "FAIL: MISSING_VIRTIO_NET: selftest RESULT=PASS but did not emit virtio-net test marker"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "UDP_PORT_MISMATCH" {
      $guestPort = "?"
      $hostPort = "?"
      try { $guestPort = [string]$result.GuestPort } catch { }
      try { $hostPort = [string]$result.HostPort } catch { }
      Write-Host "FAIL: UDP_PORT_MISMATCH: guest selftest CONFIG udp_port=$guestPort but host harness UDP echo server is on $hostPort. Run Invoke-AeroVirtioWin7Tests.ps1 -UdpPort $guestPort (or provision the guest scheduled task with --udp-port $hostPort / New-AeroWin7TestImage.ps1 -UdpPort $hostPort)."
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "MISSING_VIRTIO_NET_UDP" {
      Write-Host "FAIL: MISSING_VIRTIO_NET_UDP: selftest RESULT=PASS but did not emit virtio-net-udp test marker"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "MISSING_VIRTIO_NET_LINK_FLAP" {
      Write-Host "FAIL: MISSING_VIRTIO_NET_LINK_FLAP: did not observe virtio-net-link-flap PASS marker while -WithNetLinkFlap/-WithVirtioNetLinkFlap/-RequireVirtioNetLinkFlap/-EnableVirtioNetLinkFlap was enabled (provision the guest with --test-net-link-flap)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_NET_FAILED" {
      $largeOk = ""
      $largeBytes = ""
      $largeFnv = ""
      $largeMbps = ""
      $uploadOk = ""
      $uploadBytes = ""
      $uploadMbps = ""
      $msi = ""
      $msiMessages = ""
      $irqMode = ""
      $irqMessageCount = ""
      $irqReason = ""
      $line = Try-ExtractLastAeroMarkerLine `
        -Tail $result.Tail `
        -Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-net|FAIL|" `
        -SerialLogPath $SerialLogPath
      if ($null -ne $line) {
        if ($line -match "(?:^|\|)large_ok=([^|\r\n]+)") { $largeOk = $Matches[1] }
        if ($line -match "(?:^|\|)large_bytes=([^|\r\n]+)") { $largeBytes = $Matches[1] }
        if ($line -match "(?:^|\|)large_fnv1a64=([^|\r\n]+)") { $largeFnv = $Matches[1] }
        if ($line -match "(?:^|\|)large_mbps=([^|\r\n]+)") { $largeMbps = $Matches[1] }
        if ($line -match "(?:^|\|)upload_ok=([^|\r\n]+)") { $uploadOk = $Matches[1] }
        if ($line -match "(?:^|\|)upload_bytes=([^|\r\n]+)") { $uploadBytes = $Matches[1] }
        if ($line -match "(?:^|\|)upload_mbps=([^|\r\n]+)") { $uploadMbps = $Matches[1] }
        if ($line -match "(?:^|\|)msi=([^|\r\n]+)") { $msi = $Matches[1] }
        if ($line -match "(?:^|\|)msi_messages=([^|\r\n]+)") { $msiMessages = $Matches[1] }
        if ($line -match "(?:^|\|)irq_mode=([^|\r\n]+)") { $irqMode = $Matches[1] }
        if ($line -match "(?:^|\|)irq_message_count=([^|\r\n]+)") { $irqMessageCount = $Matches[1] }
        if ($line -match "(?:^|\|)irq_reason=([^|\r\n]+)") { $irqReason = $Matches[1] }
      }
      $detailsParts = @()
      if (-not [string]::IsNullOrEmpty($largeOk)) { $detailsParts += "large_ok=$largeOk" }
      if (-not [string]::IsNullOrEmpty($largeBytes)) { $detailsParts += "large_bytes=$largeBytes" }
      if (-not [string]::IsNullOrEmpty($largeFnv)) { $detailsParts += "large_fnv1a64=$largeFnv" }
      if (-not [string]::IsNullOrEmpty($largeMbps)) { $detailsParts += "large_mbps=$largeMbps" }
      if (-not [string]::IsNullOrEmpty($uploadOk)) { $detailsParts += "upload_ok=$uploadOk" }
      if (-not [string]::IsNullOrEmpty($uploadBytes)) { $detailsParts += "upload_bytes=$uploadBytes" }
      if (-not [string]::IsNullOrEmpty($uploadMbps)) { $detailsParts += "upload_mbps=$uploadMbps" }
      if (-not [string]::IsNullOrEmpty($msi)) { $detailsParts += "msi=$msi" }
      if (-not [string]::IsNullOrEmpty($msiMessages)) { $detailsParts += "msi_messages=$msiMessages" }
      if (-not [string]::IsNullOrEmpty($irqMode)) { $detailsParts += "irq_mode=$irqMode" }
      if (-not [string]::IsNullOrEmpty($irqMessageCount)) { $detailsParts += "irq_message_count=$irqMessageCount" }
      if (-not [string]::IsNullOrEmpty($irqReason)) { $detailsParts += "irq_reason=$irqReason" }
      $details = ""
      if ($detailsParts.Count -gt 0) { $details = " (" + ($detailsParts -join " ") + ")" }
      Write-Host "FAIL: VIRTIO_NET_FAILED: selftest RESULT=PASS but virtio-net test reported FAIL$details"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_NET_LINK_FLAP_FAILED" {
      $reason = ""
      $downSec = ""
      $upSec = ""
      $httpAttempts = ""
      $cfgVector = ""
      $cfgDownDelta = ""
      $cfgUpDelta = ""
      $line = Try-ExtractLastAeroMarkerLine `
        -Tail $result.Tail `
        -Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-net-link-flap|FAIL|" `
        -SerialLogPath $SerialLogPath
      if ($null -ne $line) {
        if ($line -match "(?:^|\|)reason=([^|\r\n]+)") { $reason = $Matches[1] }
        if ($line -match "(?:^|\|)down_sec=([^|\r\n]+)") { $downSec = $Matches[1] }
        if ($line -match "(?:^|\|)up_sec=([^|\r\n]+)") { $upSec = $Matches[1] }
        if ($line -match "(?:^|\|)http_attempts=([^|\r\n]+)") { $httpAttempts = $Matches[1] }
        if ($line -match "(?:^|\|)cfg_vector=([^|\r\n]+)") { $cfgVector = $Matches[1] }
        if ($line -match "(?:^|\|)cfg_intr_down_delta=([^|\r\n]+)") { $cfgDownDelta = $Matches[1] }
        if ($line -match "(?:^|\|)cfg_intr_up_delta=([^|\r\n]+)") { $cfgUpDelta = $Matches[1] }
      }
      $detailsParts = @()
      if (-not [string]::IsNullOrEmpty($reason)) { $detailsParts += "reason=$reason" }
      if (-not [string]::IsNullOrEmpty($downSec)) { $detailsParts += "down_sec=$downSec" }
      if (-not [string]::IsNullOrEmpty($upSec)) { $detailsParts += "up_sec=$upSec" }
      if (-not [string]::IsNullOrEmpty($httpAttempts)) { $detailsParts += "http_attempts=$httpAttempts" }
      if (-not [string]::IsNullOrEmpty($cfgVector)) { $detailsParts += "cfg_vector=$cfgVector" }
      if (-not [string]::IsNullOrEmpty($cfgDownDelta)) { $detailsParts += "cfg_intr_down_delta=$cfgDownDelta" }
      if (-not [string]::IsNullOrEmpty($cfgUpDelta)) { $detailsParts += "cfg_intr_up_delta=$cfgUpDelta" }
      $details = ""
      if ($detailsParts.Count -gt 0) { $details = " (" + ($detailsParts -join " ") + ")" }
      Write-Host "FAIL: VIRTIO_NET_LINK_FLAP_FAILED: virtio-net-link-flap test reported FAIL while -WithNetLinkFlap/-WithVirtioNetLinkFlap/-RequireVirtioNetLinkFlap/-EnableVirtioNetLinkFlap was enabled$details"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_NET_LINK_FLAP_SKIPPED" {
      $reason = "unknown"
      $line = Try-ExtractLastAeroMarkerLine `
        -Tail $result.Tail `
        -Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-net-link-flap|SKIP|" `
        -SerialLogPath $SerialLogPath
      if ($null -ne $line) {
        if ($line -match "(?:^|\|)reason=([^|\r\n]+)") { $reason = $Matches[1] }
        elseif ($line -match "\|SKIP\|([^|\r\n=]+)(?:\||$)") { $reason = $Matches[1] }
      }
      if ($reason -eq "flag_not_set") {
        Write-Host "FAIL: VIRTIO_NET_LINK_FLAP_SKIPPED: virtio-net-link-flap test was skipped (flag_not_set) but -WithNetLinkFlap/-WithVirtioNetLinkFlap/-RequireVirtioNetLinkFlap/-EnableVirtioNetLinkFlap was enabled (provision the guest with --test-net-link-flap)"
      } else {
        Write-Host "FAIL: VIRTIO_NET_LINK_FLAP_SKIPPED: virtio-net-link-flap test was skipped ($reason) but -WithNetLinkFlap/-WithVirtioNetLinkFlap/-RequireVirtioNetLinkFlap/-EnableVirtioNetLinkFlap was enabled"
      }
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_NET_UDP_FAILED" {
      $reason = ""
      $wsa = ""
      $bytes = ""
      $smallBytes = ""
      $mtuBytes = ""
      $line = Try-ExtractLastAeroMarkerLine `
        -Tail $result.Tail `
        -Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-net-udp|FAIL|" `
        -SerialLogPath $SerialLogPath
      if ($null -ne $line) {
        if ($line -match "(?:^|\|)reason=([^|\r\n]+)") { $reason = $Matches[1] }
        if ($line -match "(?:^|\|)wsa=([^|\r\n]+)") { $wsa = $Matches[1] }
        if ($line -match "(?:^|\|)bytes=([^|\r\n]+)") { $bytes = $Matches[1] }
        if ($line -match "(?:^|\|)small_bytes=([^|\r\n]+)") { $smallBytes = $Matches[1] }
        if ($line -match "(?:^|\|)mtu_bytes=([^|\r\n]+)") { $mtuBytes = $Matches[1] }
      }
      $detailsParts = @()
      if (-not [string]::IsNullOrEmpty($reason)) { $detailsParts += "reason=$reason" }
      if (-not [string]::IsNullOrEmpty($wsa)) { $detailsParts += "wsa=$wsa" }
      if (-not [string]::IsNullOrEmpty($bytes)) { $detailsParts += "bytes=$bytes" }
      if (-not [string]::IsNullOrEmpty($smallBytes)) { $detailsParts += "small_bytes=$smallBytes" }
      if (-not [string]::IsNullOrEmpty($mtuBytes)) { $detailsParts += "mtu_bytes=$mtuBytes" }
      $details = ""
      if ($detailsParts.Count -gt 0) { $details = " (" + ($detailsParts -join " ") + ")" }
      Write-Host "FAIL: VIRTIO_NET_UDP_FAILED: virtio-net-udp test reported FAIL$details"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_NET_UDP_SKIPPED" {
      $reason = "unknown"
      $line = Try-ExtractLastAeroMarkerLine `
        -Tail $result.Tail `
        -Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-net-udp|SKIP|" `
        -SerialLogPath $SerialLogPath
      if ($null -ne $line) {
        if ($line -match "(?:^|\|)reason=([^|\r\n]+)") { $reason = $Matches[1] }
        elseif ($line -match "\|SKIP\|([^|\r\n=]+)(?:\||$)") { $reason = $Matches[1] }
      }

      if ($reason -eq "unknown") {
        Write-Host "FAIL: VIRTIO_NET_UDP_SKIPPED: virtio-net-udp test was skipped but UDP testing is enabled (update/provision the guest selftest)"
      } else {
        Write-Host "FAIL: VIRTIO_NET_UDP_SKIPPED: virtio-net-udp test was skipped ($reason) but UDP testing is enabled (update/provision the guest selftest)"
      }
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "QMP_NET_LINK_FLAP_FAILED" {
      Write-Host "FAIL: QMP_NET_LINK_FLAP_FAILED: failed to flap virtio-net link via QMP (ensure QMP is reachable and QEMU supports set_link)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_NET_MSIX_NOT_ENABLED" {
      $reason = ""
      try { $reason = [string]$result.MsixReason } catch { }
      if ([string]::IsNullOrEmpty($reason)) { $reason = "virtio-net MSI-X was not enabled" }
      Write-Host "FAIL: VIRTIO_NET_MSIX_NOT_ENABLED: $reason (while -RequireVirtioNetMsix/-RequireNetMsix was enabled)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_NET_MSIX_REQUIRED" {
      $reason = ""
      try { $reason = [string]$result.MsixReason } catch { }
      if ([string]::IsNullOrEmpty($reason)) { $reason = "guest did not report virtio-net running in MSI-X mode" }
      Write-Host "FAIL: VIRTIO_NET_MSIX_REQUIRED: $reason (while -RequireVirtioNetMsix/-RequireNetMsix was enabled)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_BLK_MSIX_NOT_ENABLED" {
      $reason = ""
      try { $reason = [string]$result.MsixReason } catch { }
      if ([string]::IsNullOrEmpty($reason)) { $reason = "virtio-blk MSI-X was not enabled" }
      Write-Host "FAIL: VIRTIO_BLK_MSIX_NOT_ENABLED: $reason (while -RequireVirtioBlkMsix/-RequireBlkMsix was enabled)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_BLK_MSIX_REQUIRED" {
      $reason = ""
      try { $reason = [string]$result.MsixReason } catch { }
      if ([string]::IsNullOrEmpty($reason)) { $reason = "guest did not report virtio-blk running in MSI-X mode" }
      Write-Host "FAIL: VIRTIO_BLK_MSIX_REQUIRED: $reason (while -RequireVirtioBlkMsix/-RequireBlkMsix was enabled)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_SND_MSIX_NOT_ENABLED" {
      $reason = ""
      try { $reason = [string]$result.MsixReason } catch { }
      if ([string]::IsNullOrEmpty($reason)) { $reason = "virtio-snd MSI-X was not enabled" }
      Write-Host "FAIL: VIRTIO_SND_MSIX_NOT_ENABLED: $reason (while -RequireVirtioSndMsix/-RequireSndMsix was enabled)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_SND_MSIX_REQUIRED" {
      $reason = ""
      try { $reason = [string]$result.MsixReason } catch { }
      if ([string]::IsNullOrEmpty($reason)) { $reason = "guest did not report virtio-snd running in MSI-X mode" }
      Write-Host "FAIL: VIRTIO_SND_MSIX_REQUIRED: $reason (while -RequireVirtioSndMsix/-RequireSndMsix was enabled)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "QMP_MSIX_CHECK_UNSUPPORTED" {
      $reason = ""
      try { $reason = [string]$result.MsixReason } catch { }
      if ([string]::IsNullOrEmpty($reason)) { $reason = "QEMU QMP did not expose query-pci or human-monitor-command info pci" }
      Write-Host "FAIL: QMP_MSIX_CHECK_UNSUPPORTED: $reason"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "QMP_MSIX_CHECK_FAILED" {
      $reason = ""
      try { $reason = [string]$result.MsixReason } catch { }
      if ([string]::IsNullOrEmpty($reason)) { $reason = "failed to query QEMU PCI MSI-X state via QMP" }
      Write-Host "FAIL: QMP_MSIX_CHECK_FAILED: $reason"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "MISSING_VIRTIO_NET_CSUM_OFFLOAD" {
      Write-Host "FAIL: MISSING_VIRTIO_NET_CSUM_OFFLOAD: missing virtio-net-offload-csum marker while -RequireNetCsumOffload/-RequireVirtioNetCsumOffload was enabled"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_NET_CSUM_OFFLOAD_FAILED" {
      $csum = Get-AeroVirtioNetOffloadCsumStatsFromTail -Tail $result.Tail -SerialLogPath $SerialLogPath
      $detailsParts = @()
      if ($null -ne $csum) {
        if (-not [string]::IsNullOrEmpty($csum.Status)) { $detailsParts += "status=$($csum.Status)" }
        if ($null -ne $csum.TxCsum) { $detailsParts += "tx_csum=$($csum.TxCsum)" }
        if ($null -ne $csum.RxCsum) { $detailsParts += "rx_csum=$($csum.RxCsum)" }
        if ($null -ne $csum.Fallback) { $detailsParts += "fallback=$($csum.Fallback)" }
      }
      $details = ""
      if ($detailsParts.Count -gt 0) { $details = " (" + ($detailsParts -join " ") + ")" }
      Write-Host "FAIL: VIRTIO_NET_CSUM_OFFLOAD_FAILED: virtio-net-offload-csum marker did not PASS while -RequireNetCsumOffload/-RequireVirtioNetCsumOffload was enabled$details"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_NET_CSUM_OFFLOAD_MISSING_FIELDS" {
      Write-Host "FAIL: VIRTIO_NET_CSUM_OFFLOAD_MISSING_FIELDS: virtio-net-offload-csum marker missing tx_csum field"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_NET_CSUM_OFFLOAD_ZERO" {
      $csum = Get-AeroVirtioNetOffloadCsumStatsFromTail -Tail $result.Tail -SerialLogPath $SerialLogPath
      $detailsParts = @()
      if ($null -ne $csum) {
        if ($null -ne $csum.TxCsum) { $detailsParts += "tx_csum=$($csum.TxCsum)" }
        if ($null -ne $csum.RxCsum) { $detailsParts += "rx_csum=$($csum.RxCsum)" }
        if ($null -ne $csum.Fallback) { $detailsParts += "fallback=$($csum.Fallback)" }
      }
      $details = ""
      if ($detailsParts.Count -gt 0) { $details = " (" + ($detailsParts -join " ") + ")" }
      Write-Host "FAIL: VIRTIO_NET_CSUM_OFFLOAD_ZERO: checksum offload requirement not met$details"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "MISSING_VIRTIO_NET_UDP_CSUM_OFFLOAD" {
      Write-Host "FAIL: MISSING_VIRTIO_NET_UDP_CSUM_OFFLOAD: missing virtio-net-offload-csum marker while -RequireNetUdpCsumOffload/-RequireVirtioNetUdpCsumOffload was enabled"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_NET_UDP_CSUM_OFFLOAD_FAILED" {
      $csum = Get-AeroVirtioNetOffloadCsumStatsFromTail -Tail $result.Tail -SerialLogPath $SerialLogPath
      $detailsParts = @()
      if ($null -ne $csum) {
        if (-not [string]::IsNullOrEmpty($csum.Status)) { $detailsParts += "status=$($csum.Status)" }
        $txUdp = $csum.TxUdp
        if ($null -eq $txUdp) {
          $txUdp = [UInt64]0
          if ($null -ne $csum.TxUdp4) { $txUdp += $csum.TxUdp4 }
          if ($null -ne $csum.TxUdp6) { $txUdp += $csum.TxUdp6 }
        }
        if ($null -ne $txUdp) { $detailsParts += "tx_udp=$txUdp" }
        if ($null -ne $csum.TxUdp4) { $detailsParts += "tx_udp4=$($csum.TxUdp4)" }
        if ($null -ne $csum.TxUdp6) { $detailsParts += "tx_udp6=$($csum.TxUdp6)" }
        if ($null -ne $csum.Fallback) { $detailsParts += "fallback=$($csum.Fallback)" }
      }
      $details = ""
      if ($detailsParts.Count -gt 0) { $details = " (" + ($detailsParts -join " ") + ")" }
      Write-Host "FAIL: VIRTIO_NET_UDP_CSUM_OFFLOAD_FAILED: virtio-net-offload-csum marker did not PASS while -RequireNetUdpCsumOffload/-RequireVirtioNetUdpCsumOffload was enabled$details"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_NET_UDP_CSUM_OFFLOAD_MISSING_FIELDS" {
      Write-Host "FAIL: VIRTIO_NET_UDP_CSUM_OFFLOAD_MISSING_FIELDS: virtio-net-offload-csum marker missing tx_udp/tx_udp4/tx_udp6 fields"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_NET_UDP_CSUM_OFFLOAD_ZERO" {
      $csum = Get-AeroVirtioNetOffloadCsumStatsFromTail -Tail $result.Tail -SerialLogPath $SerialLogPath
      $txUdp = $null
      $txUdp4 = $null
      $txUdp6 = $null
      $fallback = $null
      if ($null -ne $csum) {
        $txUdp = $csum.TxUdp
        $txUdp4 = $csum.TxUdp4
        $txUdp6 = $csum.TxUdp6
        if ($null -eq $txUdp) {
          $txUdp = [UInt64]0
          if ($null -ne $txUdp4) { $txUdp += $txUdp4 }
          if ($null -ne $txUdp6) { $txUdp += $txUdp6 }
        }
        $fallback = $csum.Fallback
      }
      $detailsParts = @()
      if ($null -ne $txUdp) { $detailsParts += "tx_udp=$txUdp" }
      if ($null -ne $txUdp4) { $detailsParts += "tx_udp4=$txUdp4" }
      if ($null -ne $txUdp6) { $detailsParts += "tx_udp6=$txUdp6" }
      if ($null -ne $fallback) { $detailsParts += "fallback=$fallback" }
      $details = ""
      if ($detailsParts.Count -gt 0) { $details = " (" + ($detailsParts -join " ") + ")" }
      Write-Host "FAIL: VIRTIO_NET_UDP_CSUM_OFFLOAD_ZERO: UDP checksum offload requirement not met$details"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_SND_SKIPPED" {
      $reason = "unknown"
      $detectedReason = Try-ExtractVirtioSndSkipReason -Tail $result.Tail -SerialLogPath $SerialLogPath
      if ($null -ne $detectedReason) {
        $reason = $detectedReason
      }

      $irqMode = ""
      $irqMessageCount = ""
      $irqReason = ""
      $line = Try-ExtractLastAeroMarkerLine `
        -Tail $result.Tail `
        -Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-snd|SKIP" `
        -SerialLogPath $SerialLogPath
      if ($null -ne $line) {
        if ($line -match "(?:^|\|)irq_mode=([^|\r\n]+)") { $irqMode = $Matches[1] }
        if ($line -match "(?:^|\|)irq_message_count=([^|\r\n]+)") { $irqMessageCount = $Matches[1] }
        if ($line -match "(?:^|\|)irq_reason=([^|\r\n]+)") { $irqReason = $Matches[1] }
      }
      $detailsParts = @()
      if (-not [string]::IsNullOrEmpty($irqMode)) { $detailsParts += "irq_mode=$irqMode" }
      if (-not [string]::IsNullOrEmpty($irqMessageCount)) { $detailsParts += "irq_message_count=$irqMessageCount" }
      if (-not [string]::IsNullOrEmpty($irqReason)) { $detailsParts += "irq_reason=$irqReason" }
      $details = ""
      if ($detailsParts.Count -gt 0) { $details = " (" + ($detailsParts -join " ") + ")" }

      Write-Host "FAIL: VIRTIO_SND_SKIPPED: virtio-snd test was skipped ($reason) but -WithVirtioSnd/-RequireVirtioSnd/-EnableVirtioSnd was enabled$details"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      exit 1
    }
    "VIRTIO_SND_CAPTURE_SKIPPED" {
      $reason = "unknown"
      $line = Try-ExtractLastAeroMarkerLine `
        -Tail $result.Tail `
        -Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|" `
        -SerialLogPath $SerialLogPath
      if ($null -ne $line) {
        if ($line -match "(?:^|\|)reason=([^|\r\n]+)") { $reason = $Matches[1] }
        elseif ($line -match "\|SKIP\|([^|\r\n=]+)(?:\||$)") { $reason = $Matches[1] }
      }

      if ($reason -eq "flag_not_set") {
        Write-Host "FAIL: VIRTIO_SND_CAPTURE_SKIPPED: virtio-snd capture test was skipped (flag_not_set) but -WithVirtioSnd/-RequireVirtioSnd/-EnableVirtioSnd was enabled (provision the guest with --test-snd-capture or set env var AERO_VIRTIO_SELFTEST_TEST_SND_CAPTURE=1)"
      } else {
        Write-Host "FAIL: VIRTIO_SND_CAPTURE_SKIPPED: virtio-snd capture test was skipped ($reason) but -WithVirtioSnd/-RequireVirtioSnd/-EnableVirtioSnd was enabled"
      }
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      exit 1
    }
    "VIRTIO_SND_DUPLEX_SKIPPED" {
      $reason = "unknown"
      $line = Try-ExtractLastAeroMarkerLine `
        -Tail $result.Tail `
        -Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|SKIP|" `
        -SerialLogPath $SerialLogPath
      if ($null -ne $line) {
        if ($line -match "(?:^|\|)reason=([^|\r\n]+)") { $reason = $Matches[1] }
        elseif ($line -match "\|SKIP\|([^|\r\n=]+)(?:\||$)") { $reason = $Matches[1] }
      }

      if ($reason -eq "flag_not_set") {
        Write-Host "FAIL: VIRTIO_SND_DUPLEX_SKIPPED: virtio-snd duplex test was skipped (flag_not_set) but -WithVirtioSnd/-RequireVirtioSnd/-EnableVirtioSnd was enabled (provision the guest with --test-snd-capture or set env var AERO_VIRTIO_SELFTEST_TEST_SND_CAPTURE=1)"
      } else {
        Write-Host "FAIL: VIRTIO_SND_DUPLEX_SKIPPED: virtio-snd duplex test was skipped ($reason) but -WithVirtioSnd/-RequireVirtioSnd/-EnableVirtioSnd was enabled"
      }
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      exit 1
    }
    "VIRTIO_SND_BUFFER_LIMITS_SKIPPED" {
      $reason = "unknown"
      $line = Try-ExtractLastAeroMarkerLine `
        -Tail $result.Tail `
        -Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-snd-buffer-limits|SKIP|" `
        -SerialLogPath $SerialLogPath
      if ($null -ne $line) {
        if ($line -match "(?:^|\|)reason=([^|\r\n]+)") { $reason = $Matches[1] }
        elseif ($line -match "\|SKIP\|([^|\r\n=]+)(?:\||$)") { $reason = $Matches[1] }
      }

      if ($reason -eq "flag_not_set") {
        Write-Host "FAIL: VIRTIO_SND_BUFFER_LIMITS_SKIPPED: virtio-snd-buffer-limits test was skipped (flag_not_set) but -WithSndBufferLimits/-WithVirtioSndBufferLimits/-RequireVirtioSndBufferLimits/-EnableSndBufferLimits/-EnableVirtioSndBufferLimits was enabled (provision the guest with --test-snd-buffer-limits or set env var AERO_VIRTIO_SELFTEST_TEST_SND_BUFFER_LIMITS=1)"
      } else {
        Write-Host "FAIL: VIRTIO_SND_BUFFER_LIMITS_SKIPPED: virtio-snd-buffer-limits test was skipped ($reason) but -WithSndBufferLimits/-WithVirtioSndBufferLimits/-RequireVirtioSndBufferLimits/-EnableSndBufferLimits/-EnableVirtioSndBufferLimits was enabled"
      }
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      exit 1
    }
    default {
      Write-Host "FAIL: unexpected harness result: $($result.Result)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 4
    }
  }

  if ($VerifyVirtioSndWav) {
    $wavOk = Invoke-AeroVirtioSndWavVerification -WavPath $VirtioSndWavPath -PeakThreshold $VirtioSndWavPeakThreshold -RmsThreshold $VirtioSndWavRmsThreshold
    if (-not $wavOk -and $scriptExitCode -eq 0) {
      $scriptExitCode = 5
    }
  }

} finally {
  if ($httpListener) {
    try { $httpListener.Stop() } catch { }
  }
  if ($udpSocket) {
    try { $udpSocket.Close() } catch { }
    try { $udpSocket.Dispose() } catch { }
  }
}

exit $scriptExitCode
