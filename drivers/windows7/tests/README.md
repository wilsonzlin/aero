# Windows 7 virtio functional tests (QEMU harness)

This directory contains a **basic, automatable functional test harness** for the Windows 7 virtio drivers.

Goals:
- Run **end-to-end**, **repeatable** tests under **QEMU**.
- Validate **virtio-blk** (disk I/O), **virtio-net** (DHCP + outbound TCP), **virtio-input** (HID enumeration), and
  **virtio-snd** (audio endpoint enumeration + WASAPI render) without manual UI interaction.
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
- Runs a virtio-snd test (enumerate render endpoints via MMDevice API and start a short WASAPI render stream).
- Logs to:
  - stdout
  - `C:\aero-virtio-selftest.log`
  - `COM1` (serial)

The selftest emits machine-parseable markers:

```
AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS|...
AERO_VIRTIO_SELFTEST|TEST|virtio-input|PASS|...
AERO_VIRTIO_SELFTEST|TEST|virtio-snd|PASS|...    # or SKIP if virtio-snd is disabled/absent
AERO_VIRTIO_SELFTEST|TEST|virtio-net|PASS|...
AERO_VIRTIO_SELFTEST|RESULT|PASS
```

The host harness waits for the final `AERO_VIRTIO_SELFTEST|RESULT|...` line.

Note: The virtio-snd test is enabled by default and will FAIL if no virtio-snd render endpoint is present. If you need
to keep older images passing until virtio-snd is provisioned, run the guest tool with `--disable-snd`.

### Building (Windows)

See `guest-selftest/README.md`.

Note: The virtio-blk test requires a **mounted** virtio-backed volume. If the guest boots from a non-virtio disk,
attach an additional virtio disk with a drive letter (or run the selftest with `--blk-root <path>`).

---

## Host harness (PowerShell + QEMU)

`host-harness/Invoke-AeroVirtioWin7Tests.ps1`:
- Starts a tiny HTTP server on the host (loopback), reachable from the guest as `10.0.2.2`.
- Launches QEMU with:
  - virtio-blk disk
  - virtio-net NIC (user-mode networking / slirp)
  - virtio-input keyboard + mouse devices (`virtio-keyboard-pci`, `virtio-mouse-pci`)
  - (optional) virtio-snd device (when enabled via `-WithVirtioSnd` / `--with-virtio-snd`)
  - COM1 redirected to a host log file
- Parses the serial log for `AERO_VIRTIO_SELFTEST|RESULT|PASS/FAIL`.
- Exits with `0` on PASS, non-zero on FAIL/timeout.

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

## Extensibility hooks (future tests)

The guest tool is structured so adding more tests is straightforward:

### virtio-snd
- Enumerate audio render endpoints via MMDevice API and log them (friendly name + device ID).
- Select the virtio-snd PortCls endpoint by friendly name substring and/or hardware ID.
- Start a shared-mode WASAPI render stream and play a short deterministic tone (440Hz).

### virtio-input
- Enumerate HID devices via SetupAPI/HIDClass.
- Validate virtio-input HID report descriptors correspond to separate keyboard and mouse devices.

When adding tests:
- Emit `AERO_VIRTIO_SELFTEST|TEST|<name>|PASS/FAIL|...` lines.
- Keep each test independently pass/fail so the harness can report granular failures.
