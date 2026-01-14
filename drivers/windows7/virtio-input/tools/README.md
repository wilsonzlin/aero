# `tools/`

User-mode utilities used to validate/debug the virtio-input driver.

Currently:

- `hidtest/` — minimal HID probe tool (enumeration + report IO + LED output) for quick manual validation.
  - Supports LED writes via:
    - `WriteFile` (`IOCTL_HID_WRITE_REPORT`)
    - `HidD_SetOutputReport` (`IOCTL_HID_SET_OUTPUT_REPORT`)
    - `DeviceIoControl(IOCTL_HID_SET_OUTPUT_REPORT)` (explicit IOCTL path)
  - In diagnostics (DBG) driver builds, can get/set the driver's DiagnosticsMask at runtime via:
    - `DeviceIoControl(IOCTL_VIOINPUT_GET_LOG_MASK)` / `DeviceIoControl(IOCTL_VIOINPUT_SET_LOG_MASK)`
    - `hidtest.exe --get-log-mask` / `hidtest.exe --set-log-mask 0x...`
  - Can reset in-driver diagnostics counters during a session:
    - `DeviceIoControl(IOCTL_VIOINPUT_RESET_COUNTERS)`
    - `hidtest.exe --reset-counters` (requires write access, rerun elevated if needed)
    - Tip: `hidtest.exe --reset-counters --counters` / `--counters-json` to reset and immediately verify that the monotonic counters are cleared.
  - Can query a virtio-input interrupt diagnostics snapshot (MSI-X vs INTx, vector routing) via:
    - `DeviceIoControl(IOCTL_VIOINPUT_QUERY_INTERRUPT_INFO)`
    - `hidtest.exe --interrupt-info` / `hidtest.exe --interrupt-info-json`
  - Includes optional probes for `IOCTL_VIOINPUT_QUERY_COUNTERS` / `IOCTL_VIOINPUT_QUERY_STATE` /
    `IOCTL_VIOINPUT_QUERY_INTERRUPT_INFO` using short output buffers (verifies that the driver returns
    `STATUS_BUFFER_TOO_SMALL` while still returning `Size`/`Version` for version negotiation).
  - Includes optional negative tests that pass invalid METHOD_NEITHER pointers to validate driver hardening.
  - Useful for stressing the keyboard LED/statusq path when `StatusQDropOnFull` is enabled:
    - `hidtest.exe --keyboard --led-cycle` (cycle the 5 HID boot keyboard LED bits: Num/Caps/Scroll/Compose/Kana)
    - `hidtest.exe --keyboard --led-spam 10000`
    - `hidtest.exe --keyboard --reset-counters` (start from a clean monotonic-counter baseline; requires write access, rerun elevated if needed)
    - `hidtest.exe --keyboard --counters` (watch `LedWritesRequested` vs `LedWritesSubmitted`/`StatusQSubmits`, `StatusQCompletions`, and `StatusQFull`; with drop-on-full enabled also watch `VirtioStatusDrops` / `LedWritesDropped`)
    - Tip: `hidtest.exe --keyboard --state` shows whether LED output is enabled (`StatusQActive=1` and `KeyboardLedSupportedMask!=0`).
    - Note: by default `--led-spam` alternates `0` and `0x1F` (all 5 defined HID boot keyboard LED bits). Override the “on” value by combining with `--led 0xMASK` (or `--led-hidd` / `--led-ioctl-set-output`).
  - Useful for diagnosing buffered input when there are no pending `IOCTL_HID_READ_REPORT` IRPs:
    - `hidtest.exe --counters`
      - watch `PendingRingDepth`/`PendingRingDrops` (READ_REPORT backlog in `PendingReportRing[]`)
      - compare with `ReportRingDepth`/`ReportRingDrops` (translation-layer ring)
