# hidtest (Win7)

Minimal user-mode HID probe utility for validating the `virtio-input` HID minidriver without full emulator integration.

For the consolidated end-to-end virtio-input validation plan (device model + driver + web runtime), see:

- [`docs/virtio-input-test-plan.md`](../../../../../docs/virtio-input-test-plan.md)

## What it does

- Enumerates present HID device interfaces (SetupDi APIs, `GUID_DEVINTERFACE_HID`).
- Opens a handle to a selected interface.
- Calls:
  - `HidD_GetAttributes` (VID/PID)
  - `HidD_GetPreparsedData` + `HidP_GetCaps` (report sizes / usage)
  - `IOCTL_HID_GET_REPORT_DESCRIPTOR` (descriptor length sanity check)
  - `IOCTL_HID_GET_DEVICE_DESCRIPTOR` (cross-check reported descriptor length)
  - `IOCTL_HID_GET_COLLECTION_DESCRIPTOR` (when supported by the OS/headers; useful for newer HIDCLASS consumers)
- Supports `--selftest` mode to validate the virtio-input HID descriptor contract and exit non-zero on mismatch.
- Reads input reports via `ReadFile` in a loop and prints raw bytes + best-effort decoding for
  (with optional `--duration`/`--count` auto-exit + summary at end):
  - virtio-input keyboard report (`ReportID=1`)
  - virtio-input mouse report (`ReportID=2`)
- Optionally writes a keyboard LED output report (`ReportID=1`) via:
  - `WriteFile` (exercises `IOCTL_HID_WRITE_REPORT`)
  - `HidD_SetOutputReport` (exercises `IOCTL_HID_SET_OUTPUT_REPORT`)
  - `DeviceIoControl(IOCTL_HID_SET_OUTPUT_REPORT)` (explicit IOCTL test path)
- Includes optional negative tests that pass invalid METHOD_NEITHER pointers (should fail cleanly without crashing the guest).

## Aero virtio-input IDs / expectations

The in-tree Aero virtio-input Win7 driver exposes **separate** HID devices:

- Keyboard: VID:PID `1AF4:0001` (ReportID `1`)
- Mouse: VID:PID `1AF4:0002` (ReportID `2`)

`hidtest` uses these IDs to auto-prefer the virtio-input keyboard/mouse when multiple HID devices are present.

The tool also sanity-checks report descriptor lengths against the in-tree driver:

- Keyboard report descriptor: 65 bytes
- Mouse report descriptor: 48 bytes

Older/alternate builds may use PCI-style virtio IDs (PID `1052`/`1011`); `hidtest` still recognizes these as virtio-input.

## Build (MSVC)

From a Visual Studio / Windows SDK command prompt:

```bat
cl /nologo /W4 /D_CRT_SECURE_NO_WARNINGS main.c /link setupapi.lib hid.lib
```

Or use the helper script (builds with `cl.exe` and optionally copies the binary into
a driver package directory for easy transfer into a guest):

```bat
REM Build into drivers\windows7\virtio-input\tools\hidtest\bin\hidtest.exe
drivers\windows7\virtio-input\tools\hidtest\build_vs2010.cmd

REM Build and copy into a prebuilt driver package directory:
drivers\windows7\virtio-input\tools\hidtest\build_vs2010.cmd out\packages\windows7\virtio-input\x64
```

Or open `hidtest.vcxproj` in Visual Studio (VS2010+).

## Build (MinGW-w64)

```sh
gcc -municode -Wall -Wextra -O2 -o hidtest.exe main.c -lsetupapi -lhid
```

## Usage

List all HID interfaces:

```bat
hidtest.exe --list
```

Run the virtio-input descriptor selftest (prints `PASS`/`FAIL` lines and exits non-zero on mismatch).

Selftest output is pipe-delimited for easy serial log scraping:

```text
HIDTEST|SELFTEST|keyboard|...|PASS|...
HIDTEST|SELFTEST|mouse|...|PASS|...
HIDTEST|SELFTEST|SUMMARY|RESULT|PASS
```

```bat
hidtest.exe --selftest
```

To selftest just one collection:

```bat
hidtest.exe --selftest --keyboard
hidtest.exe --selftest --mouse
```

`--selftest` is intentionally standalone and cannot be combined with `--vid`, `--pid`, `--index`, `--counters`, LED writes, or negative-test options.

Open the virtio keyboard collection by default and read reports:

```bat
hidtest.exe
```

Read reports for a fixed duration (useful for automation):

```bat
hidtest.exe --keyboard --duration 5
```

Read a fixed number of reports and exit:

```bat
hidtest.exe --mouse --count 10
```

Force selecting the keyboard collection:

```bat
hidtest.exe --keyboard
```

Force selecting the mouse collection:

```bat
hidtest.exe --mouse
```

If multiple mice are present, `--mouse` prefers a virtio-input interface (VID `0x1AF4`, PID `0x0002`) when available.

Write keyboard LEDs (NumLock|CapsLock|ScrollLock):

```bat
hidtest.exe --led 0x07
```

Write keyboard LEDs using `HidD_SetOutputReport` (exercises `IOCTL_HID_SET_OUTPUT_REPORT`):

```bat
hidtest.exe --led-hidd 0x07
```

Write keyboard LEDs using an explicit `DeviceIoControl(IOCTL_HID_SET_OUTPUT_REPORT)` call:

```bat
hidtest.exe --led-ioctl-set-output 0x07
```

Negative test (invalid METHOD_NEITHER pointer; should fail cleanly without crashing the guest):

```bat
hidtest.exe --keyboard --ioctl-bad-xfer-packet
hidtest.exe --keyboard --ioctl-bad-write-report
hidtest.exe --keyboard --ioctl-bad-set-output-xfer-packet
hidtest.exe --keyboard --ioctl-bad-set-output-report
hidtest.exe --keyboard --ioctl-bad-get-report-descriptor
hidtest.exe --keyboard --ioctl-bad-get-device-descriptor
hidtest.exe --keyboard --ioctl-bad-get-string
hidtest.exe --keyboard --ioctl-bad-get-indexed-string
hidtest.exe --keyboard --ioctl-bad-get-string-out
hidtest.exe --keyboard --ioctl-bad-get-indexed-string-out
```

Probe the driver-private counters IOCTL with a short output buffer (verifies the driver returns
`STATUS_BUFFER_TOO_SMALL` while still returning `Size`/`Version` so tools can adapt to version bumps):

```bat
hidtest.exe --ioctl-query-counters-short
```

Negative test (invalid `HidD_SetOutputReport` buffer pointer; should fail cleanly without crashing the guest):

```bat
hidtest.exe --keyboard --hidd-bad-set-output-report
```

Cycle LEDs (guaranteed visible changes):

```bat
hidtest.exe --led-cycle
```

Dump the raw HID report descriptor bytes:

```bat
hidtest.exe --dump-desc
```

Dump the raw bytes returned by `IOCTL_HID_GET_COLLECTION_DESCRIPTOR`:

```bat
hidtest.exe --dump-collection-desc
```

Query virtio-input driver diagnostic counters (IOCTL_VIOINPUT_QUERY_COUNTERS):

```bat
hidtest.exe --counters
```

Query virtio-input driver diagnostic counters in JSON form:

```bat
hidtest.exe --counters-json
```

You should see non-zero counts after some HID activity (enumeration, input reports, etc). If you run a non-virtio-input HID
device, the IOCTL will fail with an "invalid function" style error.
