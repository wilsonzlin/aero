import type { UsbGuestWebUsbControllerKind, UsbSelectedMessage } from "../usb/usb_proxy_protocol";
import type { UsbPassthroughBridgeLike } from "../usb/webusb_passthrough_runtime";
import { applyUsbSelectedToWebUsbUhciBridge } from "../usb/uhci_webusb_bridge";
import { applyUsbSelectedToWebUsbXhciBridge } from "../usb/xhci_webusb_bridge";

export type WebUsbGuestControllerKind = UsbGuestWebUsbControllerKind;

export type WebUsbGuestBridgeLike = UsbPassthroughBridgeLike & {
  set_connected(connected: boolean): void;
};

function normalizeWebUsbGuestBridgeLike(value: unknown): WebUsbGuestBridgeLike | null {
  if (!value || typeof value !== "object") return null;
  const obj = value as Record<string, unknown>;

  const setConnected = obj.set_connected ?? obj.setConnected;
  const drainActions = obj.drain_actions ?? obj.drainActions;
  const pushCompletion = obj.push_completion ?? obj.pushCompletion;
  const reset = obj.reset;
  const free = obj.free;
  const pendingSummary = obj.pending_summary ?? obj.pendingSummary;

  if (typeof setConnected !== "function") return null;
  if (typeof drainActions !== "function") return null;
  if (typeof pushCompletion !== "function") return null;
  if (typeof reset !== "function") return null;
  if (typeof free !== "function") return null;

  // Wrap all methods to preserve wasm-bindgen `this` binding regardless of whether the underlying
  // bridge uses snake_case or camelCase names.
  const wrapped: WebUsbGuestBridgeLike = {
    set_connected: (connected) => {
      (setConnected as (connected: boolean) => void).call(value, connected);
    },
    drain_actions: () => {
      return (drainActions as () => unknown).call(value);
    },
    push_completion: (completion) => {
      (pushCompletion as (completion: unknown) => void).call(value, completion);
    },
    reset: () => {
      (reset as () => void).call(value);
    },
    ...(typeof pendingSummary === "function"
      ? {
          pending_summary: () => {
            return (pendingSummary as () => unknown).call(value);
          },
        }
      : {}),
    free: () => {
      (free as () => void).call(value);
    },
  };

  return wrapped;
}

export function isWebUsbGuestBridgeLike(value: unknown): value is WebUsbGuestBridgeLike {
  return normalizeWebUsbGuestBridgeLike(value) !== null;
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
  const xhci = normalizeWebUsbGuestBridgeLike(opts.xhciBridge);
  if (xhci) return { kind: "xhci", bridge: xhci };
  const ehci = normalizeWebUsbGuestBridgeLike(opts.ehciBridge);
  if (ehci) return { kind: "ehci", bridge: ehci };
  const uhci = normalizeWebUsbGuestBridgeLike(opts.uhciBridge);
  if (uhci) return { kind: "uhci", bridge: uhci };
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
