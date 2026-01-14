import type { UhciTopologyBridge } from "./uhci_hid_topology";
import { EXTERNAL_HUB_ROOT_PORT, WEBUSB_GUEST_ROOT_PORT } from "../usb/uhci_external_hub";

function mapEhciRootPortFromUhci(rootPort: number): number {
  // EHCI reserves root port 1 for WebUSB passthrough (see `EhciControllerBridge`).
  //
  // Higher-level WebHID + synthetic HID plumbing (and the `GuestUsbPath` protocol) uses the UHCI
  // convention:
  // - root port 0: external hub
  // - root port 1: WebUSB passthrough device
  //
  // EHCI now matches this convention, so no mapping is required.
  return rootPort;
}

function mapEhciPathFromUhci(path: number[]): number[] {
  if (!Array.isArray(path) || path.length === 0) return path;
  const root = path[0];
  if (root !== EXTERNAL_HUB_ROOT_PORT && root !== WEBUSB_GUEST_ROOT_PORT) return path;
  const mapped = mapEhciRootPortFromUhci(root);
  if (mapped === root) return path;
  const out = path.slice();
  out[0] = mapped;
  return out;
}

/**
 * Wrap an EHCI controller bridge that exposes UHCI-compatible topology helpers so it can be used
 * with {@link UhciHidTopologyManager} without clobbering EHCI's reserved WebUSB root port.
 */
export function createEhciTopologyBridgeShim(bridge: UhciTopologyBridge): UhciTopologyBridge {
  // wasm-bindgen export surfaces can vary between snake_case and camelCase depending on build
  // tooling/version. Prefer snake_case, but accept camelCase fallbacks. Always invoke through
  // `.call(bridge, ...)` to preserve `this` binding for wasm-bindgen methods.
  const bridgeAny = bridge as unknown as Record<string, unknown>;
  const attachHub = bridgeAny.attach_hub ?? bridgeAny.attachHub;
  const detachAtPath = bridgeAny.detach_at_path ?? bridgeAny.detachAtPath;
  const attachWebhid = bridgeAny.attach_webhid_device ?? bridgeAny.attachWebhidDevice ?? bridgeAny.attachWebHidDevice;
  const attachUsbHid = bridgeAny.attach_usb_hid_passthrough_device ?? bridgeAny.attachUsbHidPassthroughDevice;

  return {
    attach_hub: (rootPort, portCount) => {
      if (typeof attachHub !== "function") return;
      attachHub.call(bridge, mapEhciRootPortFromUhci(rootPort), portCount);
    },
    detach_at_path: (path) => {
      if (typeof detachAtPath !== "function") return;
      detachAtPath.call(bridge, mapEhciPathFromUhci(path));
    },
    attach_webhid_device: (path, device) => {
      if (typeof attachWebhid !== "function") return;
      attachWebhid.call(bridge, mapEhciPathFromUhci(path), device);
    },
    attach_usb_hid_passthrough_device: (path, device) => {
      if (typeof attachUsbHid !== "function") return;
      attachUsbHid.call(bridge, mapEhciPathFromUhci(path), device);
    },
  };
}
