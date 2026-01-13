import type { UsbSelectedMessage } from "./usb_proxy_protocol";

export type WebUsbXhciHotplugBridgeLike = {
  set_connected(connected: boolean): void;
  reset(): void;
};

/**
 * Apply a `usb.selected` broadcast to an xHCI WebUSB bridge.
 *
 * Contract:
 * - `ok:true` means a host device is available; attach the passthrough device.
 * - `ok:false` means the device is unavailable (disconnect/chooser error); detach and reset.
 */
export function applyUsbSelectedToWebUsbXhciBridge(bridge: WebUsbXhciHotplugBridgeLike, msg: UsbSelectedMessage): void {
  if (msg.ok) {
    bridge.set_connected(true);
    return;
  }
  bridge.set_connected(false);
  bridge.reset();
}
