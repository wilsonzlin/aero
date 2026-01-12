/**
 * @deprecated Legacy WebUSB type augmentation used by the repo-root demo harness.
 *
 * Canonical USB passthrough stack: `crates/aero-usb/` + `web/src/usb/*` (ADR 0015).
 */
export {};

declare global {
  interface USBDeviceRequestOptions {
    acceptAllDevices?: boolean;
  }
}
