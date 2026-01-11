export {};

// WebUSB type augmentation for the repo-root harness.
//
// We pull in the base WebUSB interfaces via `@types/w3c-web-usb`, but Chromium
// exposes an additional `acceptAllDevices` option on `requestDevice` that is
// not present in those typings. Augment it here so the harness UI can probe
// acceptAllDevices behavior without sprinkling `any` casts everywhere.

declare global {
  interface USBDeviceRequestOptions {
    acceptAllDevices?: boolean;
  }
}
