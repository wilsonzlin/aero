# `tools/`

User-mode utilities used to validate/debug the virtio-input driver.

Currently:

- `hidtest/` — minimal HID probe tool (enumeration + report IO + LED output) for quick manual validation.
  - Supports LED writes via `WriteFile` (`IOCTL_HID_WRITE_REPORT`) and `HidD_SetOutputReport` (`IOCTL_HID_SET_OUTPUT_REPORT`).
  - Includes optional negative tests that pass invalid METHOD_NEITHER pointers to validate driver hardening.
