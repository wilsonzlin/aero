# WebUSB

WebUSB lets a web app talk directly to USB devices (typically over control/bulk/interrupt transfers) from
Chromium-based browsers.

In practice, WebUSB failures are dominated by two constraints:

1. **Browser restrictions**: some USB interface classes are treated as “protected” and cannot be accessed via WebUSB.
2. **Host OS driver / permissions**: even when `navigator.usb` exists, `open()` / `claimInterface()` can fail if the OS
   driver binding or permissions are incorrect (especially on Windows).

## Troubleshooting

If WebUSB calls like `requestDevice()`, `device.open()`, or `device.claimInterface()` fail with an opaque `DOMException`:

- **Secure context required**: WebUSB requires `https://` or `http://localhost` (`isSecureContext === true`).
- **User gesture required**: `navigator.usb.requestDevice()` must be triggered by a user gesture (e.g. a button click).
  If you `await` before calling `requestDevice()`, the user gesture can be lost.
- **Protected interface classes**: WebUSB cannot access some interface classes (HID, mass storage, audio/video, etc.).
  Prefer a vendor-specific interface (class `0xFF`) or a more appropriate Web API (e.g. WebHID/WebSerial).
- **Windows (WinUSB)**: WebUSB typically requires the relevant interface to be bound to **WinUSB**.
  - For development: tools like **Zadig** can install WinUSB for a specific VID/PID/interface.
  - For production devices: ship **Microsoft OS 2.0 descriptors** / WinUSB Compatible ID descriptors so Windows binds
    WinUSB automatically.
- **Linux (udev / kernel driver)**:
  - Ensure your user has permission to access the device (via `udev` rules).
  - Ensure no kernel driver is attached to the interface; a bound kernel driver can prevent `claimInterface()`.

