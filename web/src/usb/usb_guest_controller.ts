import type { UsbGuestControllerMode, UsbSelectedMessage } from "./usb_proxy_protocol";
import { WEBUSB_GUEST_ROOT_PORT } from "./uhci_external_hub";
import { applyUsbSelectedToWebUsbUhciBridge } from "./uhci_webusb_bridge";
import { applyUsbSelectedToWebUsbXhciBridge } from "./xhci_webusb_bridge";
import type { UsbPassthroughBridgeLike } from "./webusb_passthrough_runtime";

/**
 * Root port reserved for the guest-visible WebUSB passthrough device.
 *
 * Keep this consistent with the UHCI/EHCI/xHCI convention:
 * - root port 0 is available for an external hub / WebHID / synthetic HID topology, and
 * - root port 1 is reserved for WebUSB passthrough.
 */
export function webUsbGuestRootPortForMode(mode: UsbGuestControllerMode): number {
  void mode;
  return WEBUSB_GUEST_ROOT_PORT;
}

export type WebUsbGuestPassthroughBridgeLike = UsbPassthroughBridgeLike & {
  set_connected(connected: boolean): void;
};

export function getWebUsbGuestBridgeForMode(
  mode: UsbGuestControllerMode,
  bridges: {
    uhci: WebUsbGuestPassthroughBridgeLike | null;
    ehci: WebUsbGuestPassthroughBridgeLike | null;
    xhci: WebUsbGuestPassthroughBridgeLike | null;
  },
): WebUsbGuestPassthroughBridgeLike | null {
  if (mode === "xhci") return bridges.xhci;
  if (mode === "ehci") return bridges.ehci;
  return bridges.uhci;
}

export function applyUsbSelectedToWebUsbBridgeForMode(
  mode: UsbGuestControllerMode,
  bridges: {
    uhci: WebUsbGuestPassthroughBridgeLike | null;
    ehci: WebUsbGuestPassthroughBridgeLike | null;
    xhci: WebUsbGuestPassthroughBridgeLike | null;
  },
  msg: UsbSelectedMessage,
): WebUsbGuestPassthroughBridgeLike | null {
  const bridge = getWebUsbGuestBridgeForMode(mode, bridges);
  if (!bridge) return null;
  if (mode === "ehci" || mode === "xhci") {
    applyUsbSelectedToWebUsbXhciBridge(bridge, msg);
  } else {
    applyUsbSelectedToWebUsbUhciBridge(bridge, msg);
  }
  return bridge;
}
