# `tools/`

User-mode utilities used to validate/debug the virtio-input driver.

Currently:

- `hidtest/` — minimal HID probe tool (enumeration + report IO + LED output) for quick manual validation.
  - Supports LED writes via:
    - `WriteFile` (`IOCTL_HID_WRITE_REPORT`)
    - `HidD_SetOutputReport` (`IOCTL_HID_SET_OUTPUT_REPORT`)
    - `DeviceIoControl(IOCTL_HID_SET_OUTPUT_REPORT)` (explicit IOCTL path)
  - Includes an optional probe for `IOCTL_VIOINPUT_QUERY_COUNTERS` using a short output buffer (verifies that the
    driver returns `STATUS_BUFFER_TOO_SMALL` while still returning `Size`/`Version` for version negotiation).
  - Includes optional negative tests that pass invalid METHOD_NEITHER pointers to validate driver hardening.
