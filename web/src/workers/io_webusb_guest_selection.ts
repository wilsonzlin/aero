import type { UsbSelectedMessage } from "../usb/usb_proxy_protocol";
import type { UsbPassthroughBridgeLike } from "../usb/webusb_passthrough_runtime";
import { applyUsbSelectedToWebUsbUhciBridge } from "../usb/uhci_webusb_bridge";
import { applyUsbSelectedToWebUsbXhciBridge } from "../usb/xhci_webusb_bridge";

export type WebUsbGuestControllerKind = "xhci" | "ehci" | "uhci";

export type WebUsbGuestBridgeLike = UsbPassthroughBridgeLike & {
  set_connected(connected: boolean): void;
};

export function isWebUsbGuestBridgeLike(value: unknown): value is WebUsbGuestBridgeLike {
  if (!value || typeof value !== "object") return false;
  const obj = value as Record<string, unknown>;
  return (
    typeof obj.set_connected === "function" &&
    typeof obj.drain_actions === "function" &&
    typeof obj.push_completion === "function" &&
    typeof obj.reset === "function" &&
    typeof obj.free === "function"
  );
}

/**
 * Choose which guest-visible controller to use for WebUSB passthrough.
 *
 * Deterministic policy:
 * - Prefer xHCI when it is available (modern OS support / future-proof).
 * - Fall back to EHCI when xHCI is unavailable (high-speed view, older WASM builds).
 * - Fall back to UHCI when xHCI/EHCI are unavailable (legacy guests, older WASM builds).
 */
export function chooseWebUsbGuestBridge(opts: {
  xhciBridge: unknown | null;
  ehciBridge: unknown | null;
  uhciBridge: unknown | null;
}): { kind: WebUsbGuestControllerKind; bridge: WebUsbGuestBridgeLike } | null {
  if (isWebUsbGuestBridgeLike(opts.xhciBridge)) {
    return { kind: "xhci", bridge: opts.xhciBridge };
  }
  if (isWebUsbGuestBridgeLike(opts.ehciBridge)) {
    return { kind: "ehci", bridge: opts.ehciBridge };
  }
  if (isWebUsbGuestBridgeLike(opts.uhciBridge)) {
    return { kind: "uhci", bridge: opts.uhciBridge };
  }
  return null;
}

export function applyUsbSelectedToWebUsbGuestBridge(
  kind: WebUsbGuestControllerKind,
  bridge: Pick<WebUsbGuestBridgeLike, "set_connected" | "reset">,
  msg: UsbSelectedMessage,
): void {
  if (kind === "xhci" || kind === "ehci") {
    applyUsbSelectedToWebUsbXhciBridge(bridge, msg);
  } else {
    applyUsbSelectedToWebUsbUhciBridge(bridge, msg);
  }
}
