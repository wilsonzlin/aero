import type { UhciTopologyBridge } from "./uhci_hid_topology";

function mapEhciRootPortFromUhci(rootPort: number): number {
  // EHCI reserves root port 0 for WebUSB passthrough (see `EhciControllerBridge`).
  //
  // Higher-level WebHID + synthetic HID plumbing (and the `GuestUsbPath` protocol) uses the
  // UHCI convention:
  // - root port 0: external hub
  // - root port 1: WebUSB passthrough device
  //
  // When EHCI is the only available guest-visible controller (i.e. UHCI is omitted from the WASM
  // build), we still want both the external hub and WebUSB passthrough to coexist. Swap root ports
  // so the external hub is attached on EHCI root port 1 while WebUSB remains on EHCI root port 0.
  if (rootPort === 0) return 1;
  if (rootPort === 1) return 0;
  return rootPort;
}

function mapEhciPathFromUhci(path: number[]): number[] {
  if (!Array.isArray(path) || path.length === 0) return path;
  const root = path[0];
  if (root !== 0 && root !== 1) return path;
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
  return {
    attach_hub: (rootPort, portCount) => bridge.attach_hub(mapEhciRootPortFromUhci(rootPort), portCount),
    detach_at_path: (path) => bridge.detach_at_path(mapEhciPathFromUhci(path)),
    attach_webhid_device: (path, device) => bridge.attach_webhid_device(mapEhciPathFromUhci(path), device),
    attach_usb_hid_passthrough_device: (path, device) =>
      bridge.attach_usb_hid_passthrough_device(mapEhciPathFromUhci(path), device),
  };
}

