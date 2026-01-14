import type { GuestUsbPath } from "../platform/hid_passthrough_protocol";
import type { HidTopologyManager } from "../hid/wasm_hid_guest_bridge";
import type { XhciTopologyBridge } from "../hid/xhci_hid_topology";

type XhciTopologyBridgeLike = {
  attach_hub: (rootPort: number, portCount: number) => void;
  detach_at_path: (path: number[]) => void;
  attach_webhid_device: (path: number[], device: unknown) => void;
  attach_usb_hid_passthrough_device: (path: number[], device: unknown) => void;
  // wasm-bindgen bridges expose `free()`, but unit tests and alternate shims may not.
  free?: () => void;
};

/**
 * Runtime check for whether an xHCI controller bridge supports the subset of
 * exports required to manage guest USB topology for HID passthrough.
 */
export function isXhciTopologyBridgeLike(value: unknown): value is XhciTopologyBridgeLike {
  if (!value || typeof value !== "object") return false;
  const obj = value as Record<string, unknown>;
  const free = obj.free;
  if (free !== undefined && typeof free !== "function") return false;
  return (
    typeof obj.attach_hub === "function" &&
    typeof obj.detach_at_path === "function" &&
    typeof obj.attach_webhid_device === "function" &&
    typeof obj.attach_usb_hid_passthrough_device === "function"
  );
}

/**
 * Wrap a WASM xHCI controller bridge into the narrow {@link XhciTopologyBridge}
 * interface expected by {@link XhciHidTopologyManager}.
 *
 * Returns `null` when the bridge does not expose the required exports.
 *
 * Note: the wrapper preserves method `this` binding by invoking methods on the original bridge
 * object so it works with wasm-bindgen generated glue.
 */
export function createXhciTopologyBridgeShim(bridge: unknown): XhciTopologyBridge | null {
  if (!isXhciTopologyBridgeLike(bridge)) return null;
  const obj = bridge as XhciTopologyBridgeLike;
  const rawFree = (bridge as { free?: unknown }).free;
  const freeFn = typeof rawFree === "function" ? rawFree : () => {};
  return {
    free: () => {
      try {
        freeFn.call(bridge);
      } catch {
        // ignore
      }
    },
    attach_hub: (rootPort, portCount) => obj.attach_hub.call(bridge, rootPort, portCount),
    detach_at_path: (path) => obj.detach_at_path.call(bridge, path),
    attach_webhid_device: (path, device) => obj.attach_webhid_device.call(bridge, path, device),
    attach_usb_hid_passthrough_device: (path, device) => obj.attach_usb_hid_passthrough_device.call(bridge, path, device),
  };
}

type HidBackend = "xhci" | "uhci";

export type IoWorkerHidTopologyMuxOpts = {
  xhci: Pick<HidTopologyManager, "attachDevice" | "detachDevice">;
  uhci: Pick<HidTopologyManager, "attachDevice" | "detachDevice">;
  /**
   * Whether new device attachments should be routed to xHCI.
   */
  useXhci: () => boolean;
};

/**
 * {@link HidTopologyManager} implementation that routes HID passthrough
 * attachments to xHCI when available, falling back to UHCI otherwise.
 *
 * The caller is responsible for wiring controller bridge state into the
 * underlying topology managers.
 */
export class IoWorkerHidTopologyMux implements HidTopologyManager {
  readonly #xhci: Pick<HidTopologyManager, "attachDevice" | "detachDevice">;
  readonly #uhci: Pick<HidTopologyManager, "attachDevice" | "detachDevice">;
  readonly #useXhci: () => boolean;

  readonly #backendByDeviceId = new Map<number, HidBackend>();

  constructor(opts: IoWorkerHidTopologyMuxOpts) {
    this.#xhci = opts.xhci;
    this.#uhci = opts.uhci;
    this.#useXhci = opts.useXhci;
  }

  attachDevice(deviceId: number, path: GuestUsbPath, kind: "webhid" | "usb-hid-passthrough", device: unknown): void {
    const backend: HidBackend = this.#useXhci() ? "xhci" : "uhci";
    this.#backendByDeviceId.set(deviceId, backend);
    if (backend === "xhci") {
      this.#xhci.attachDevice(deviceId, path, kind, device);
    } else {
      this.#uhci.attachDevice(deviceId, path, kind, device);
    }
  }

  detachDevice(deviceId: number): void {
    const backend = this.#backendByDeviceId.get(deviceId);
    this.#backendByDeviceId.delete(deviceId);
    if (backend === "xhci") {
      this.#xhci.detachDevice(deviceId);
    } else if (backend === "uhci") {
      this.#uhci.detachDevice(deviceId);
    } else {
      // Device was never attached (or already detached).
    }
  }
}
