import type { UsbSelectedMessage } from "./usb_proxy_protocol";

export type WebUsbUhciHotplugBridgeLike = {
  set_connected(connected: boolean): void;
  reset(): void;
};

/**
 * Apply a `usb.selected` broadcast to a UHCI WebUSB bridge.
 *
 * Contract:
 * - `ok:true` means a host device is available; attach the passthrough device to the emulated bus.
 * - `ok:false` means the device is unavailable (disconnect/chooser error); detach and reset.
 */
export function applyUsbSelectedToWebUsbUhciBridge(bridge: WebUsbUhciHotplugBridgeLike, msg: UsbSelectedMessage): void {
  if (msg.ok) {
    bridge.set_connected(true);
    return;
  }
  bridge.set_connected(false);
  bridge.reset();
}
