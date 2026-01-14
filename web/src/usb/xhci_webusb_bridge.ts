import type { UsbSelectedMessage } from "./usb_proxy_protocol";

export type WebUsbXhciHotplugBridgeLike = {
  set_connected(connected: boolean): void;
  reset(): void;
  // Backwards compatibility: older wasm-bindgen outputs / shims may use camelCase.
  setConnected?: (connected: boolean) => void;
};

/**
 * Apply a `usb.selected` broadcast to an xHCI WebUSB bridge.
 *
 * Contract:
 * - `ok:true` means a host device is available; attach the passthrough device.
 * - `ok:false` means the device is unavailable (disconnect/chooser error); detach and reset.
 */
export function applyUsbSelectedToWebUsbXhciBridge(bridge: WebUsbXhciHotplugBridgeLike, msg: UsbSelectedMessage): void {
  const anyBridge = bridge as unknown as Record<string, unknown>;
  // Backwards compatibility: accept both snake_case and camelCase method names.
  // Always invoke extracted methods via `.call(bridge, ...)` to avoid wasm-bindgen `this` binding pitfalls.
  const setConnected = anyBridge.set_connected ?? anyBridge.setConnected;
  const reset = anyBridge.reset;
  if (typeof setConnected !== "function") {
    throw new Error("WebUsb xHCI bridge missing set_connected/setConnected export.");
  }
  if (typeof reset !== "function") {
    throw new Error("WebUsb xHCI bridge missing reset() export.");
  }

  if (msg.ok) {
    (setConnected as (connected: boolean) => void).call(bridge, true);
    return;
  }
  (setConnected as (connected: boolean) => void).call(bridge, false);
  (reset as () => void).call(bridge);
}
