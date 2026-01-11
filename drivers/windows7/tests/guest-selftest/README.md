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
  - Read the HID report descriptor (`IOCTL_HID_GET_REPORT_DESCRIPTOR`) and sanity-check that:
    - at least one **keyboard-only** HID device exists
    - at least one **mouse-only** HID device exists
    - no matched HID device advertises both keyboard and mouse application collections (contract v1 expects two separate PCI functions).
- **virtio-snd** (can be skipped with `--disable-snd`)
  - Detect the virtio-snd PCI function via SetupAPI hardware IDs:
    - `PCI\VEN_1AF4&DEV_1059` (modern; Aero contract v1 expects `REV_01`)
    - `PCI\VEN_1AF4&DEV_1018` (transitional; may appear when QEMU is not launched with `disable-legacy=on`)
  - Enumerate audio render endpoints via MMDevice API and start a shared-mode WASAPI render stream.
  - Render a short deterministic tone (440Hz) at 48kHz/16-bit/stereo.
  - If WASAPI fails, a WinMM `waveOut` fallback is attempted.
  - By default, missing device or playback failure is **FAIL**. Use `--disable-snd` to skip.
  - Also emits a separate `virtio-snd-capture` marker by attempting to detect a virtio-snd **capture** endpoint
    (MMDevice `eCapture`).
    - Missing capture is reported as **SKIP** by default; use `--require-snd-capture` to make it **FAIL**.
    - Use `--test-snd-capture` to run a shared-mode WASAPI capture smoke test when a capture endpoint exists
      (otherwise only endpoint detection is performed).

Note: For deterministic DNS testing under QEMU slirp, the default `--dns-host` is `host.lan`
(with fallbacks like `gateway.lan` / `dns.lan`).

## Output markers

The host harness parses these markers from COM1 serial:

```
AERO_VIRTIO_SELFTEST|START|...
AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS|...
AERO_VIRTIO_SELFTEST|TEST|virtio-input|PASS|...
AERO_VIRTIO_SELFTEST|TEST|virtio-snd|PASS
AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|endpoint_missing
AERO_VIRTIO_SELFTEST|TEST|virtio-net|PASS|...
AERO_VIRTIO_SELFTEST|RESULT|PASS
```

Notes:
- If the virtio-snd capture endpoint is missing, the tool emits `AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|endpoint_missing`
  (unless `--require-snd-capture` is set).
- If the virtio-snd test is disabled via `--disable-snd`, the tool emits
  `AERO_VIRTIO_SELFTEST|TEST|virtio-snd|SKIP|disabled` and `AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|disabled`.

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
