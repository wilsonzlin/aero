import type { UsbGuestControllerMode, UsbSelectedMessage } from "./usb_proxy_protocol";
import { WEBUSB_GUEST_ROOT_PORT } from "./uhci_external_hub";
import { applyUsbSelectedToWebUsbUhciBridge, type WebUsbUhciHotplugBridgeLike } from "./uhci_webusb_bridge";
import type { UsbPassthroughBridgeLike } from "./webusb_passthrough_runtime";

/**
 * Root port reserved for the guest-visible EHCI WebUSB passthrough device.
 *
 * UHCI uses root port 1 because root port 0 is reserved for the external WebHID hub topology.
 * EHCI currently has no external hub attachment in the browser runtime, so we reserve port 0.
 */
export const EHCI_WEBUSB_GUEST_ROOT_PORT = 0;

export function webUsbGuestRootPortForMode(mode: UsbGuestControllerMode): number {
  return mode === "ehci" ? EHCI_WEBUSB_GUEST_ROOT_PORT : WEBUSB_GUEST_ROOT_PORT;
}

export type WebUsbGuestPassthroughBridgeLike = WebUsbUhciHotplugBridgeLike & UsbPassthroughBridgeLike;

export function getWebUsbGuestBridgeForMode(
  mode: UsbGuestControllerMode,
  bridges: { uhci: WebUsbGuestPassthroughBridgeLike | null; ehci: WebUsbGuestPassthroughBridgeLike | null },
): WebUsbGuestPassthroughBridgeLike | null {
  return mode === "ehci" ? bridges.ehci : bridges.uhci;
}

export function applyUsbSelectedToWebUsbBridgeForMode(
  mode: UsbGuestControllerMode,
  bridges: { uhci: WebUsbGuestPassthroughBridgeLike | null; ehci: WebUsbGuestPassthroughBridgeLike | null },
  msg: UsbSelectedMessage,
): WebUsbGuestPassthroughBridgeLike | null {
  const bridge = getWebUsbGuestBridgeForMode(mode, bridges);
  if (!bridge) return null;
  applyUsbSelectedToWebUsbUhciBridge(bridge, msg);
  return bridge;
}

