# `aero-virtio-selftest.exe` (Windows 7 guest tool)

This is a small **Windows 7 user-mode console tool** intended to run inside the guest at boot and report
virtio driver health via **COM1 serial** (host-captured), stdout, and a log file on `C:\`.

## What it tests

- **virtio-blk**
  - Detect a virtio disk device (SetupAPI hardware IDs).
  - Create a temporary file on a **virtio-backed volume** and perform:
    - sequential write + readback verification
    - `FlushFileBuffers` success check
    - sequential read pass
- **virtio-net**
  - Detect a virtio network adapter (SetupAPI hardware IDs).
  - Wait for link + DHCP IPv4 address (non-APIPA).
  - DNS resolution (`getaddrinfo`)
  - HTTP GET to a configurable URL (WinHTTP)
- **virtio-input**
  - Enumerate HID devices (SetupAPI via `GUID_DEVINTERFACE_HID`).
  - Detect virtio-input devices by matching virtio-input PCI/HID IDs:
    - `VEN_1AF4&DEV_1052` (modern) and `VEN_1AF4&DEV_1011` (transitional)
    - or HID-style `VID_1AF4&PID_1052` / `VID_1AF4&PID_1011`
  - Aero contract note:
    - `AERO-W7-VIRTIO` v1 expects the modern virtio-input PCI ID (`DEV_1052`) with `REV_01`.
    - The in-tree Aero Win7 virtio-input INF is revision-gated, so QEMU-style `REV_00` virtio-input devices will not bind unless you override the revision (for example `x-pci-revision=0x01`).
  - Read the HID report descriptor (`IOCTL_HID_GET_REPORT_DESCRIPTOR`) and sanity-check that:
    - at least one **keyboard-only** HID device exists
    - at least one **mouse-only** HID device exists
    - no matched HID device advertises both keyboard and mouse application collections (contract v1 expects two separate PCI functions).
- **virtio-snd** (optional; playback runs automatically when a supported virtio-snd device is detected)
  - Detect the virtio-snd PCI function via SetupAPI hardware IDs:
    - `PCI\VEN_1AF4&DEV_1059` (modern; strict INF matches `PCI\VEN_1AF4&DEV_1059&REV_01`)
      - If the VM/device does not report `REV_01`, the Aero contract driver will not bind and the selftest will report binding diagnostics (for example `driver_not_bound` / `wrong_service`) and log that `REV_01` is missing.
      - For QEMU-based testing with the strict contract-v1 package, you typically need `disable-legacy=on,x-pci-revision=0x01` for the virtio-snd device so Windows enumerates `PCI\VEN_1AF4&DEV_1059&REV_01`.
    - If QEMU is not launched with `disable-legacy=on`, virtio-snd may enumerate as the transitional PCI ID `PCI\VEN_1AF4&DEV_1018` (often `REV_00`).
      - The Aero contract INF is strict and will not bind; install the opt-in transitional package (`aero-virtio-snd-legacy.inf` + `virtiosnd_legacy.sys`).
      - Pass `--allow-virtio-snd-transitional` to accept the transitional ID (intended for QEMU bring-up/regression).
  - Validate that the PCI device is bound to the expected in-tree driver service and emit
    actionable diagnostics (PNP instance ID, ConfigManagerErrorCode / Device Manager “Code X”, driver INF name
    when queryable).
    - Contract v1 (`aero-virtio-snd.inf`): expects service `aeroviosnd`
    - QEMU compatibility package (`aero-virtio-snd-legacy.inf`): expects service `aeroviosnd_legacy`
  - Enumerate audio render endpoints via MMDevice API and start a shared-mode WASAPI render stream.
  - Render a short deterministic tone (440Hz) at 48kHz/16-bit/stereo.
  - If WASAPI fails, a WinMM `waveOut` fallback is attempted.
  - By default, if a supported virtio-snd PCI function is detected, the selftest exercises playback automatically.
    - If no supported device is detected, virtio-snd is reported as **SKIP**.
    - Use `--require-snd` (alias: `--test-snd`) to make missing virtio-snd fail the overall selftest.
  - Playback failures cause the overall selftest to **FAIL**.
  - Also emits a separate `virtio-snd-capture` marker by attempting to detect a virtio-snd **capture** endpoint
    (MMDevice `eCapture`).
    - Missing capture is reported as **SKIP** by default; use `--require-snd-capture` to make it **FAIL**.
    - Use `--test-snd-capture` to run a shared-mode WASAPI capture smoke test when a capture endpoint exists
      (otherwise only endpoint detection is performed).
      - By default, the smoke test **PASS**es even if the captured audio is only silence.
      - Use `--require-non-silence` to fail the capture smoke test if no non-silent buffers are observed.
      - `--test-snd-capture` can also be enabled via env var: `AERO_VIRTIO_SELFTEST_TEST_SND_CAPTURE=1`.
  - Use `--disable-snd` to force **SKIP** for both playback and capture.
  - Use `--disable-snd-capture` to force **SKIP** for capture only (while still exercising playback).

Note: For deterministic DNS testing under QEMU slirp, the default `--dns-host` is `host.lan`
(with fallbacks like `gateway.lan` / `dns.lan`).

## Output markers

The host harness parses these markers from COM1 serial:

```
AERO_VIRTIO_SELFTEST|START|...
AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS
AERO_VIRTIO_SELFTEST|TEST|virtio-input|PASS|...

# virtio-snd may be SKIP/PASS/FAIL depending on flags and device presence.
# Capture is reported separately as "virtio-snd-capture".
#
# Example: virtio-snd not present (or not required) => skip:
AERO_VIRTIO_SELFTEST|TEST|virtio-snd|SKIP
AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|flag_not_set
AERO_VIRTIO_SELFTEST|TEST|virtio-net|PASS
AERO_VIRTIO_SELFTEST|RESULT|PASS

# Example: virtio-snd present => playback + capture markers:
AERO_VIRTIO_SELFTEST|TEST|virtio-snd|PASS
AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|PASS|endpoint_present
AERO_VIRTIO_SELFTEST|TEST|virtio-net|PASS
AERO_VIRTIO_SELFTEST|RESULT|PASS

# Example: virtio-snd failure => overall FAIL:
AERO_VIRTIO_SELFTEST|TEST|virtio-snd|FAIL|...
AERO_VIRTIO_SELFTEST|RESULT|FAIL
```

Notes:
- If no supported virtio-snd PCI function is detected (and no capture flags are set), the tool emits
  `AERO_VIRTIO_SELFTEST|TEST|virtio-snd|SKIP` and `AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|flag_not_set`.
- If `--require-snd` / `--test-snd` is set and the PCI device is missing, the tool emits
  `AERO_VIRTIO_SELFTEST|TEST|virtio-snd|FAIL`.
  (In this case, the capture marker uses `...|device_missing` and is `SKIP` by default unless `--require-snd-capture` is set.)
- If the virtio-snd PCI device is present but not bound to the expected driver, the tool emits a reason code:
  - `wrong_service` (bound to an unexpected service)
  - `driver_not_bound` (no `SPDRP_SERVICE` / no driver installed)
  - `device_error` (Config Manager “Code X” / `DN_HAS_PROBLEM`)
  (and will `SKIP`/`FAIL` the capture marker similarly, depending on `--require-snd-capture`).
- If the virtio-snd capture endpoint is missing, the tool emits `AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|endpoint_missing`
  (unless `--require-snd-capture` is set).
- When a capture smoke test runs (`--test-snd-capture` or `--require-non-silence`), the `virtio-snd-capture` marker includes
  extra diagnostics such as `method=...`, `frames=...`, and whether any non-silence was observed. If `--require-non-silence`
  is set and only silence is captured, the tool emits `AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|FAIL|silence`.
- If the virtio-snd test is disabled via `--disable-snd`, the tool emits
  `AERO_VIRTIO_SELFTEST|TEST|virtio-snd|SKIP` and `AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|disabled`.
- If capture is disabled via `--disable-snd-capture`, the tool emits
  `AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|disabled` (playback behavior unchanged).

## Building

### Prereqs

- Visual Studio (MSVC) capable of producing Windows 7 compatible binaries
- Windows 7 compatible SDK / toolset (or a newer SDK while still targeting `_WIN32_WINNT=0x0601`)
- CMake (optional, but recommended)

Notes on Win7 compatibility:
- The provided CMake config builds with the **static MSVC runtime** (`/MT`) and sets the subsystem version to **6.01**,
  so the resulting `aero-virtio-selftest.exe` can run on a clean Windows 7 SP1 install without an additional VC++ runtime step.
- The virtio-snd test uses WASAPI/MMDevice (and a WinMM fallback) and requires linking `mmdevapi`, `ole32`, `uuid`, and `winmm`
  (handled by the CMake config).

### Build with CMake (recommended)

From a Developer Command Prompt:

```bat
cd drivers\windows7\tests\guest-selftest
mkdir build
cd build
cmake -G "Visual Studio 17 2022" -A x64 ..
cmake --build . --config Release
```

Output:
- `build\Release\aero-virtio-selftest.exe`

Build x86:

```bat
cmake -G "Visual Studio 17 2022" -A Win32 ..
cmake --build . --config Release
```

## Installing in the guest

Copy `aero-virtio-selftest.exe` to the guest, then configure it to run automatically on boot.

Recommended (runs as SYSTEM at startup):

```bat
mkdir C:\AeroTests
copy aero-virtio-selftest.exe C:\AeroTests\

schtasks /Create /F /TN "AeroVirtioSelftest" /SC ONSTART /RU SYSTEM ^
  /TR "\"C:\AeroTests\aero-virtio-selftest.exe\" --http-url http://10.0.2.2:18080/aero-virtio-selftest --dns-host host.lan"
```

To require virtio-snd (fail the overall run if the virtio-snd PCI device is missing):

```bat
schtasks /Create /F /TN "AeroVirtioSelftest" /SC ONSTART /RU SYSTEM ^
  /TR "\"C:\AeroTests\aero-virtio-selftest.exe\" --require-snd --http-url http://10.0.2.2:18080/aero-virtio-selftest --dns-host host.lan"
```

(Alias: `--test-snd`.)

Note: Aero contract v1 requires `REV_01` and a modern-only virtio-snd PCI function. If the device does not report
`REV_01` (or does not expose the modern virtio-snd PCI ID), the Aero driver will not bind and the selftest will
report virtio-snd as missing.

To explicitly skip virtio-snd:

```bat
schtasks /Create /F /TN "AeroVirtioSelftest" /SC ONSTART /RU SYSTEM ^
  /TR "\"C:\AeroTests\aero-virtio-selftest.exe\" --disable-snd --http-url http://10.0.2.2:18080/aero-virtio-selftest --dns-host host.lan"
```

If the VM has multiple disks (e.g. IDE boot disk + separate virtio data disk), you can force the virtio-blk test location:

```bat
schtasks /Create /F /TN "AeroVirtioSelftest" /SC ONSTART /RU SYSTEM ^
  /TR "\"C:\AeroTests\aero-virtio-selftest.exe\" --blk-root D:\aero-virtio-selftest\ --http-url http://10.0.2.2:18080/aero-virtio-selftest --dns-host host.lan"
```

The host harness expects the tool to run automatically and print a final `AERO_VIRTIO_SELFTEST|RESULT|...` line to COM1.
When the host harness attaches virtio-snd (`-WithVirtioSnd` / `--with-virtio-snd`), it also expects both
`AERO_VIRTIO_SELFTEST|TEST|virtio-snd|PASS` and `AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|PASS` (not `SKIP`).
