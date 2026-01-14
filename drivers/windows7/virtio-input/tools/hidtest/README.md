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
- Optionally probes the GetInputReport path (`IOCTL_HID_GET_INPUT_REPORT`) via:
  - `DeviceIoControl(IOCTL_HID_GET_INPUT_REPORT)` (`--ioctl-get-input-report`)
  - `HidD_GetInputReport` (`--hidd-get-input-report`)
- Supports `--selftest` mode to validate the virtio-input HID descriptor contract and exit non-zero on mismatch.
- Reads input reports via `ReadFile` in a loop and prints raw bytes + best-effort decoding for
  (with optional `--duration`/`--count` auto-exit + summary at end):
  - virtio-input keyboard report (`ReportID=1`)
  - virtio-input mouse report (`ReportID=2`)
- Can query/reset the virtio-input driver diagnostic counters (`--counters`, `--counters-json`, `--reset-counters`).
- Can get/set the virtio-input driver's diagnostics log mask at runtime (`--get-log-mask`, `--set-log-mask`) when using a DBG/diagnostics build of the driver.
- Optionally writes a keyboard LED output report (`ReportID=1`) via:
  - `WriteFile` (exercises `IOCTL_HID_WRITE_REPORT`)
  - `HidD_SetOutputReport` (exercises `IOCTL_HID_SET_OUTPUT_REPORT`)
  - `DeviceIoControl(IOCTL_HID_SET_OUTPUT_REPORT)` (explicit IOCTL test path)
- Supports stress-testing the LED/statusq write path via `--led-spam N`.
- Optionally queries/resets virtio-input driver diagnostics counters via:
  - `DeviceIoControl(IOCTL_VIOINPUT_QUERY_COUNTERS)`
  - `DeviceIoControl(IOCTL_VIOINPUT_RESET_COUNTERS)`
- Includes optional negative tests that pass invalid METHOD_NEITHER pointers (should fail cleanly without crashing the guest).

## Aero virtio-input IDs / expectations

The in-tree Aero virtio-input Win7 driver exposes **separate** HID devices:

- Keyboard: VID:PID `1AF4:0001` (ReportID `1`)
- Mouse: VID:PID `1AF4:0002` (ReportID `2`)
- Tablet: VID:PID `1AF4:0003` (ReportID `4`)

`hidtest` uses these IDs to auto-prefer the virtio-input keyboard/mouse when multiple HID devices are present.

The tool also sanity-checks report descriptor lengths against the in-tree driver:

- Keyboard report descriptor: 104 bytes (keyboard + LEDs + Consumer Control/media keys)
- Mouse report descriptor: 57 bytes (8 buttons + X/Y/Wheel + Consumer/AC Pan)
- Tablet report descriptor: 47 bytes (8 buttons + absolute X/Y)

Mouse input reports (`ReportID=2`) are 6 bytes:

`[id][buttons][dx][dy][wheel][AC Pan]`

Older/alternate builds may use PCI-style virtio IDs (PID `1052`/`1011`); `hidtest` still recognizes these as virtio-input.

Note: `--selftest` validates the **keyboard + mouse** descriptor contract by default. Pass `--tablet` to validate the
tablet collection contract (when present).

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

List all HID interfaces as JSON (for harnesses/parsing):

```bat
hidtest.exe --list --json
```

The JSON output is a single array on stdout. Each entry has:

- `index`, `path`, `vid`, `pid`, `usagePage`, `usage`, `inputLen`, `outputLen`, `reportDescLen`

Run the virtio-input descriptor selftest (prints `PASS`/`FAIL` lines and exits non-zero on mismatch):

```bat
hidtest.exe --selftest
```

The selftest validates (per selected device):
  - input report length (`HidP_GetCaps`)
  - output report length (`HidP_GetCaps`, keyboard only)
  - report descriptor length (`IOCTL_HID_GET_REPORT_DESCRIPTOR`)
  - HID descriptor-reported report length (`IOCTL_HID_GET_DEVICE_DESCRIPTOR`)
  - collection descriptor length (`IOCTL_HID_GET_COLLECTION_DESCRIPTOR`) **when supported**

On Windows 7, `IOCTL_HID_GET_COLLECTION_DESCRIPTOR` is often not implemented by HIDCLASS; in that case the check is reported
as `SKIP` and does not fail the selftest.

Selftest output is pipe-delimited for easy serial log scraping:

```text
HIDTEST|SELFTEST|keyboard|...|PASS|...
HIDTEST|SELFTEST|mouse|...|PASS|...
HIDTEST|SELFTEST|SUMMARY|RESULT|PASS
```

To selftest just one collection:

```bat
hidtest.exe --selftest --keyboard
hidtest.exe --selftest --mouse
hidtest.exe --selftest --tablet
```

Machine-readable selftest output:

```bat
hidtest.exe --selftest --json
```

The JSON output is an object with:

- `pass` (bool)
- `keyboard`, `mouse`, `tablet` (object or null; `tablet` is null unless `--tablet` is used)
- `failures` (array)

The JSON output includes additional fields for the optional collection descriptor check:
  - `collectionDescLen`, `collectionDescIoctl`, `collectionDescErr`

`--selftest` exits `0` on pass and `1` on fail.

`--selftest` is intentionally standalone and cannot be combined with `--state`, `--list`, descriptor dump options, `--vid`, `--pid`, `--index`, counters, LED writes, or negative-test options.

Tip: pass `--quiet` with `--selftest` to suppress the per-device enumeration output (leaving only the `HIDTEST|SELFTEST|...` lines).

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

Query virtio-input driver diagnostic counters (custom IOCTLs exposed by the in-tree virtio-input minidriver):

```bat
hidtest.exe --counters
```

Reset virtio-input driver diagnostic counters:

```bat
hidtest.exe --reset-counters
```

Note: `--reset-counters` requires opening the HID interface with write access (the tool prefers read/write but may fall back to read-only). If it fails with a “GENERIC_WRITE” message, rerun elevated and/or ensure no other process is holding the device exclusively.

Note: depth gauge counters (e.g. `ReadReportQueueDepth`, `ReportRingDepth`, `PendingRingDepth`) reflect the driver's *current* state and may remain non-zero after a reset if the driver still has queued work.

Tip: combine with `--counters` / `--counters-json` to verify reset immediately (the tool's own enumeration traffic is cleared by the reset):

```bat
hidtest.exe --reset-counters --counters
hidtest.exe --reset-counters --counters-json
```

Toggle the virtio-input driver diagnostics log mask at runtime (**DBG/diagnostics driver builds only**):

```bat
hidtest.exe --get-log-mask
hidtest.exe --set-log-mask 0x8000000F
```

`--get-log-mask` / `--set-log-mask` are intended as standalone actions (similar to `--state` / `--counters`); they are mutually exclusive with other report/IOCTL test modes.

This updates the same `DiagnosticsMask` that is normally read from the registry at `DriverEntry`:

`HKLM\\System\\CurrentControlSet\\Services\\aero_virtio_input\\Parameters\\DiagnosticsMask`

Mask bits (see `drivers/windows7/virtio-input/src/log.h`):

- `0x00000001` `VIOINPUT_LOG_ERROR`
- `0x00000002` `VIOINPUT_LOG_IOCTL`
- `0x00000004` `VIOINPUT_LOG_QUEUE`
- `0x00000008` `VIOINPUT_LOG_VIRTQ`
- `0x80000000` `VIOINPUT_LOG_VERBOSE`

### Counters interpretation

The virtio-input Win7 minidriver maintains a set of best-effort counters that track:

- **HIDCLASS IOCTL traffic** (what Windows is asking the driver to do)
- **READ_REPORT lifecycle** (pended/completed/cancelled)
- **Translation-layer buffering** (`virtio_input_device.report_ring` inside the translation layer)
- **Pending READ_REPORT buffering** (`PendingReportRing[]`, used to satisfy `IOCTL_HID_READ_REPORT`)
- **Virtio event flow** (events arriving from the device model / virtqueue)
- **statusq / keyboard LED writes** (driver -> device output path)

During normal use (typing/mouse movement), you should typically see:

- `VirtioEvents` increase as input events arrive from the virtio eventq.
- `IoctlHidReadReport` increase as HIDCLASS issues read requests (driven by Windows input stacks).
- `ReadReportPended` and `ReadReportCompleted` increase and remain close in value.
- `ReadReportQueueDepth`, `ReportRingDepth`, and `PendingRingDepth` stay low (often 0–1), indicating the consumer is keeping up.

Indicators of drops/overruns:

- `PendingRingDrops` increasing indicates reports were buffered faster than `IOCTL_HID_READ_REPORT` was consuming them, so the driver dropped the **oldest pending report**.
- `ReportRingDrops` or `VirtioEventDrops` increasing indicates the translation layer report ring filled up (the driver could not drain/process translated reports fast enough).
- `ReportRingOverruns` or `VirtioEventOverruns` should remain **0**; any non-zero value indicates reports/events exceeded the expected maximum size.

Write keyboard LEDs (HID boot keyboard LED bits: NumLock|CapsLock|ScrollLock|Compose|Kana):

```bat
# 0x07 toggles the three common lock LEDs (Num/Caps/Scroll).
# 0x1F sets all 5 defined LED bits (adds Compose + Kana).
hidtest.exe --led 0x1F
```

Write keyboard LEDs using `HidD_SetOutputReport` (exercises `IOCTL_HID_SET_OUTPUT_REPORT`):

```bat
hidtest.exe --led-hidd 0x1F
```

Write keyboard LEDs using an explicit `DeviceIoControl(IOCTL_HID_SET_OUTPUT_REPORT)` call:

```bat
hidtest.exe --led-ioctl-set-output 0x1F
```

Negative test (invalid METHOD_NEITHER pointer; should fail cleanly without crashing the guest):

```bat
hidtest.exe --keyboard --ioctl-bad-xfer-packet
hidtest.exe --keyboard --ioctl-bad-write-report
hidtest.exe --keyboard --ioctl-bad-read-xfer-packet
hidtest.exe --keyboard --ioctl-bad-read-report
hidtest.exe --keyboard --ioctl-bad-get-input-xfer-packet
hidtest.exe --keyboard --ioctl-bad-get-input-report
hidtest.exe --keyboard --ioctl-bad-set-output-xfer-packet
hidtest.exe --keyboard --ioctl-bad-set-output-report
hidtest.exe --keyboard --ioctl-bad-get-report-descriptor
hidtest.exe --keyboard --ioctl-bad-get-collection-descriptor
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

Probe the driver-private state IOCTL with a short output buffer (verifies the driver returns
`STATUS_BUFFER_TOO_SMALL` while still returning `Size`/`Version` so tools can adapt to version bumps):

```bat
hidtest.exe --ioctl-query-state-short
```

Test the GetInputReport path (should return a single report of the expected size, then return a no-data error when no new
input is available):

```bat
hidtest.exe --keyboard --ioctl-get-input-report
hidtest.exe --mouse --ioctl-get-input-report

hidtest.exe --keyboard --hidd-get-input-report
hidtest.exe --mouse --hidd-get-input-report
```

Negative test (invalid `HidD_SetOutputReport` buffer pointer; should fail cleanly without crashing the guest):

```bat
hidtest.exe --keyboard --hidd-bad-set-output-report
```

Cycle LEDs (guaranteed visible changes):

```bat
hidtest.exe --led-cycle
```

### Stress-testing StatusQ / LED writes

The driver exposes an optional registry knob to change how the virtio **statusq**
behaves when it is full:

`HKLM\System\CurrentControlSet\Services\aero_virtio_input\Parameters\StatusQDropOnFull` (DWORD)

When set to nonzero, pending LED writes are dropped instead of being held until
the queue drains. You can enable it and then stress the write path with:

```bat
hidtest.exe --keyboard --state
hidtest.exe --keyboard --led-spam 10000
hidtest.exe --keyboard --counters
```

Watch:

- `LedWritesRequested` to see how many keyboard LED output reports HIDCLASS requested.
- `LedWritesSubmitted` / `StatusQSubmits` to see how many LED updates were actually submitted to the device.
  - Due to coalescing, this can be **much lower** than `LedWritesRequested` under heavy write load.
- `StatusQCompletions` to see how many submitted statusq buffers have completed. (`StatusQSubmits - StatusQCompletions` is the rough outstanding count.)
- `StatusQFull` to see how often the statusq hit backpressure.
- With `StatusQDropOnFull=1`, `VirtioStatusDrops` (and `LedWritesDropped`) should increase when the queue is full.

Dump the raw HID report descriptor bytes:

```bat
hidtest.exe --dump-desc
```

Dump the raw bytes returned by `IOCTL_HID_GET_COLLECTION_DESCRIPTOR`:

```bat
hidtest.exe --dump-collection-desc
```

Poll virtio-input diagnostics counters (IOCTL_VIOINPUT_QUERY_COUNTERS; does **not** issue `ReadFile` / `IOCTL_HID_READ_REPORT`):

```bat
hidtest.exe --counters
```

This prints a snapshot including:

- Translation-layer buffering (`virtio_input_device.report_ring`):
  - `ReportRingDepth`, `ReportRingMaxDepth`, `ReportRingDrops`, `ReportRingOverruns`
- Pending backlog while HIDCLASS isn't issuing enough `IOCTL_HID_READ_REPORT`s (`DEVICE_CONTEXT.PendingReportRing[]`):
  - `PendingRingDepth`, `PendingRingMaxDepth`, `PendingRingDrops`
- Pending READ_REPORT IRPs:
  - `ReadReportQueueDepth`, `ReadReportQueueMaxDepth`

When you generate input with no pending reads, `PendingRingDepth` should grow. If you flood input faster than you read it,
`PendingRingDrops` should increase.

Query virtio-input driver diagnostic counters in JSON form:

```bat
hidtest.exe --counters-json
```

You should see non-zero counts after some HID activity (enumeration, input reports, etc). If you run a non-virtio-input HID
device, the IOCTL will fail with an "invalid function" style error.

Reset virtio-input driver diagnostic counters (IOCTL_VIOINPUT_RESET_COUNTERS):

```bat
hidtest.exe --reset-counters
```

Note: `--reset-counters` clears monotonic counters and max-depths, but current-state depth gauges (e.g. `ReadReportQueueDepth`) may remain non-zero if the driver still has queued work.

`--counters` / `--reset-counters` operate on the selected HID interface, so use `--keyboard` / `--mouse` if you want to
inspect/reset the counters for a specific virtio-input device instance.
