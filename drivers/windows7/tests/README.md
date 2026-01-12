# Windows 7 virtio functional tests (QEMU harness)

This directory contains a **basic, automatable functional test harness** for the Windows 7 virtio drivers.

Goals:
- Run **end-to-end**, **repeatable** tests under **QEMU**.
- Validate **virtio-blk** (disk I/O), **virtio-net** (DHCP + outbound TCP), **virtio-input** (HID enumeration + optional
  end-to-end event delivery), and **virtio-snd** (audio endpoint enumeration + basic playback) without manual UI
  interaction.
- Produce logs over **COM1 serial** so the host can deterministically parse **PASS/FAIL**.
- Keep the structure extensible (more tests later).

Non-goals:
- No Windows images are committed (see [Image strategy](#image-strategy-no-redistribution)).

## Layout

```
drivers/windows7/tests/
  guest-selftest/   # Win7 user-mode console tool: aero-virtio-selftest.exe
  host-harness/     # PowerShell scripts to boot QEMU + parse serial PASS/FAIL
  README.md         # (this file)
```

---

## Guest selftest tool

`guest-selftest/` builds `aero-virtio-selftest.exe`, a Win7 user-mode console tool that:
- Detects virtio devices via SetupAPI (hardware IDs like `VEN_1AF4` / `VIRTIO`).
- Runs a virtio-blk file I/O test (write/readback, sequential read, flush) on a **virtio-backed volume**.
- Runs a virtio-net test (wait for DHCP, DNS resolve, HTTP GET).
- Runs a virtio-input HID sanity test (detect virtio-input HID devices + validate separate keyboard-only + mouse-only HID devices).
- Emits a `virtio-input-events` marker that can be used to validate **end-to-end input report delivery** when the host
  harness injects deterministic keyboard/mouse events via QMP (`input-send-event`).
  - This path reads HID input reports directly from the virtio-input HID interface so it does not depend on UI focus.
  - By default the guest selftest reports `virtio-input-events|SKIP|flag_not_set`; provision the guest to run the
    selftest with `--test-input-events` to enable it.
- Optionally runs a virtio-snd test (PCI detection + endpoint enumeration + short playback) when a supported virtio-snd
  device is detected (or when `--require-snd` / `--test-snd` is set).
  - Detects the virtio-snd PCI function by hardware ID:
    - `PCI\VEN_1AF4&DEV_1059` (modern; Aero contract v1 requires `PCI\VEN_1AF4&DEV_1059&REV_01` for strict INF binding)
    - If the device enumerates as transitional virtio-snd (`PCI\VEN_1AF4&DEV_1018`; stock QEMU defaults), the selftest
      only accepts it when `--allow-virtio-snd-transitional` is set. In that mode, install the opt-in legacy package
      (`drivers/windows7/virtio-snd/inf/aero-virtio-snd-legacy.inf` + `virtiosnd_legacy.sys`).
- Also emits a `virtio-snd-capture` marker (capture endpoint detection + optional WASAPI capture smoke test).
- Logs to:
  - stdout
  - `C:\aero-virtio-selftest.log`
  - `COM1` (serial)

The selftest emits machine-parseable markers:

```
 AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS
 AERO_VIRTIO_SELFTEST|TEST|virtio-input|PASS|...
 AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|SKIP|flag_not_set

 # Optional: end-to-end virtio-input event delivery (requires `--test-input-events` in the guest and host-side QMP injection):
 # AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|READY
 # AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|PASS|...
 # (virtio-snd is emitted as PASS/FAIL/SKIP depending on device/config):
  AERO_VIRTIO_SELFTEST|TEST|virtio-snd|SKIP
  AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|flag_not_set
 # or:
 AERO_VIRTIO_SELFTEST|TEST|virtio-snd|PASS
 AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|PASS|...
 AERO_VIRTIO_SELFTEST|TEST|virtio-net|PASS
AERO_VIRTIO_SELFTEST|RESULT|PASS
```

The host harness waits for the final `AERO_VIRTIO_SELFTEST|RESULT|...` line and also enforces that key per-test markers
(virtio-blk + virtio-input + virtio-snd + virtio-snd-capture + virtio-net) were emitted so older selftest binaries
can’t accidentally pass. When the harness is run with `-WithVirtioSnd` / `--with-virtio-snd`, both `virtio-snd` and
`virtio-snd-capture` must `PASS` (not `SKIP`).

Note:
- virtio-snd is optional. When a supported virtio-snd PCI function is detected, the selftest exercises playback
  automatically (even without `--test-snd`). Use `--require-snd` / `--test-snd` to make missing virtio-snd fail the
  overall selftest, and `--disable-snd` to force skipping playback + capture.
  - For stock QEMU transitional virtio-snd (`PCI\VEN_1AF4&DEV_1018`), also pass `--allow-virtio-snd-transitional` and
    install the legacy virtio-snd package (`aero-virtio-snd-legacy.inf` + `virtiosnd_legacy.sys`).
- Capture is reported separately via the `virtio-snd-capture` marker. Missing capture is `SKIP` by default unless
  `--require-snd-capture` is set. Use `--test-snd-capture` to run the capture smoke test (otherwise only endpoint
  detection is performed). Use `--disable-snd-capture` to skip capture-only checks while still exercising playback.

### Building (Windows)

See `guest-selftest/README.md`.

Note: The virtio-blk test requires a **mounted** virtio-backed volume. If the guest boots from a non-virtio disk,
attach an additional virtio disk with a drive letter (or run the selftest with `--blk-root <path>`).

---

## Host harness (PowerShell + QEMU)

`host-harness/Invoke-AeroVirtioWin7Tests.ps1`:
- Starts a tiny HTTP server on the host (loopback), reachable from the guest as `10.0.2.2`.
- Launches QEMU with:
  - virtio-blk disk (**modern-only** virtio-pci: `disable-legacy=on,x-pci-revision=0x01`)
  - virtio-net NIC (user-mode networking / slirp; **modern-only** virtio-pci: `disable-legacy=on,x-pci-revision=0x01`)
  - virtio-input keyboard + mouse devices (`virtio-keyboard-pci`, `virtio-mouse-pci`; **modern-only** virtio-pci: `disable-legacy=on,x-pci-revision=0x01`)
  - (optional) virtio-snd device (when enabled via `-WithVirtioSnd` / `--with-virtio-snd`; **modern-only** virtio-pci: `disable-legacy=on,x-pci-revision=0x01`)
- COM1 redirected to a host log file
- Parses the serial log for `AERO_VIRTIO_SELFTEST|RESULT|PASS/FAIL` and requires per-test markers for
  virtio-blk + virtio-input + virtio-snd + virtio-snd-capture + virtio-net when RESULT=PASS is seen.
  - When `-WithInputEvents` (alias: `-WithVirtioInputEvents`) / `--with-input-events` (alias: `--with-virtio-input-events`)
    is enabled, the harness also injects a small keyboard + mouse sequence via QMP (`input-send-event`) and requires
    `AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|PASS`.
    - Note: this requires a guest image provisioned with `--test-input-events` so the guest selftest enables the
      `virtio-input-events` read loop (otherwise the guest reports `...|SKIP|flag_not_set`).
    - The harness also emits a host marker for the injection step itself:
      `AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_EVENTS_INJECT|PASS/FAIL|...`
- Exits with `0` on PASS, non-zero on FAIL/timeout.

The harness also sets the PCI **Revision ID** (`x-pci-revision=0x01`) to match the
[`AERO-W7-VIRTIO` v1 contract](../../../docs/windows7-virtio-driver-contract.md). Newer Aero drivers may refuse to bind
if the Revision ID does not match.

For Linux/CI environments, `host-harness/invoke_aero_virtio_win7_tests.py` provides the same behavior without requiring PowerShell.

See `host-harness/README.md` for required prerequisites and usage.

---

## Image strategy (no redistribution)

**Do not commit Windows ISOs or disk images.**

You must provide one of:
1) A user-supplied Windows 7 ISO + license key, or
2) An already-installed Win7 disk image (qcow2/raw/vhdx) you own.

The repository includes scripts + documentation to:
- copy/install the Aero virtio drivers
- install `aero-virtio-selftest.exe`
- configure automatic execution on boot (Task Scheduler recommended)

See `host-harness/README.md` for a recommended provisioning approach.

For a standardized QEMU command line to perform an interactive Windows 7 installation from your own ISO (with an attached provisioning ISO), see:
- `host-harness/Start-AeroWin7Installer.ps1`

---

## Extensibility hooks

The guest tool is structured so adding more tests is straightforward:

### virtio-snd
- Enumerate audio render endpoints via MMDevice API and log them (friendly name + device ID).
- Select the virtio-snd endpoint by friendly name substring and/or hardware ID.
- Start a shared-mode WASAPI render stream and play a short deterministic tone (440Hz), with a waveOut fallback.
- Runs when a supported virtio-snd device is detected. Use `--require-snd` / `--test-snd` to fail if missing, or
  `--disable-snd` to force `SKIP`.

### virtio-input
- Enumerate HID devices via SetupAPI/HIDClass.
- Validate virtio-input HID report descriptors correspond to separate keyboard and mouse devices.

When adding tests:
- Emit `AERO_VIRTIO_SELFTEST|TEST|<name>|PASS/FAIL/SKIP|...` lines.
- Keep each test independently pass/fail/skip so the harness can report granular failures.
