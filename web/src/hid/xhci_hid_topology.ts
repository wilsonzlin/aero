import type { GuestUsbPath } from "../platform/hid_passthrough_protocol";
import {
  EXTERNAL_HUB_ROOT_PORT,
  WEBUSB_GUEST_ROOT_PORT,
  remapLegacyRootPortToExternalHubPort,
} from "../usb/uhci_external_hub";

export type XhciHidPassthroughDeviceKind = "webhid" | "usb-hid-passthrough";

/**
 * Maximum downstream port count for a USB hub attached behind xHCI.
 *
 * Real xHCI implementations encode the hub route string using 4-bit port numbers
 * (1..15). Keeping hub port counts <=15 avoids creating guest-visible USB
 * topologies that cannot be represented with valid xHCI route strings.
 *
 * Reference: xHCI 1.2 §6.2.2 "Slot Context" (Route String field).
 */
export const XHCI_MAX_HUB_PORT_COUNT = 15;

/**
 * Maximum number of downstream hub tiers representable by the Slot Context Route String.
 *
 * The Route String is 20 bits wide (5 nibbles), so it can encode up to 5 hub hops
 * downstream from the root port.
 *
 * Reference: xHCI 1.2 §6.2.2 "Slot Context" (Route String field).
 */
export const XHCI_MAX_ROUTE_TIER_COUNT = 5;

function clampHubPortCount(value: number): number {
  if (!Number.isFinite(value)) return 1;
  const int = Math.floor(value);
  return Math.max(1, Math.min(XHCI_MAX_HUB_PORT_COUNT, int));
}

function isValidDownstreamPortNumber(value: number): boolean {
  return Number.isFinite(value) && Number.isInteger(value) && value >= 1 && value <= XHCI_MAX_HUB_PORT_COUNT;
}

function isValidDevicePath(path: GuestUsbPath): boolean {
  if (!Array.isArray(path) || path.length === 0) return false;
  // Root port + up to 5 downstream hub ports (Route String).
  if (path.length > XHCI_MAX_ROUTE_TIER_COUNT + 1) return false;
  const root = path[0];
  if (typeof root !== "number" || !Number.isFinite(root) || !Number.isInteger(root) || root < 0) return false;
  // xHCI root port 1 is reserved for WebUSB passthrough (see `crates/aero-wasm::XhciControllerBridge`).
  // Avoid attaching HID passthrough devices there; callers should prefer root port 0 (external hub).
  if (root === WEBUSB_GUEST_ROOT_PORT) return false;
  for (let i = 1; i < path.length; i += 1) {
    if (!isValidDownstreamPortNumber(path[i]!)) return false;
  }
  return true;
}

/**
 * Subset of the WASM `XhciControllerBridge` API required to manage guest USB topology.
 *
 * The full WASM export also includes xHCI register access + frame stepping; this
 * interface is intentionally narrow so it can be faked in unit tests.
 *
 * Note: these methods are optional because older/alternate WASM builds may expose an xHCI MMIO
 * bridge without any topology helpers. Callers may still pass such a bridge; the topology manager
 * will feature-detect methods and treat missing exports as a no-op (best-effort).
 */
export type XhciTopologyBridge = {
  /**
   * wasm-bindgen handles always expose `free()`. Keep this required so that concrete WASM bridge
   * instances are assignable to this (otherwise all-optional object types are treated as "weak" by
   * TypeScript and require casts).
   */
  free: () => void;
  attach_hub?: (rootPort: number, portCount: number) => void;
  detach_at_path?: (path: number[]) => void;
  attach_webhid_device?: (path: number[], device: unknown) => void;
  attach_usb_hid_passthrough_device?: (path: number[], device: unknown) => void;
};

type DeviceRecord = {
  path: GuestUsbPath;
  kind: XhciHidPassthroughDeviceKind;
  device: unknown;
};

/**
 * Worker-side guest USB topology bookkeeping for xHCI.
 *
 * Responsibilities:
 * - Remember which USB devices should be attached at which guest paths.
 * - Ensure a hub exists before attaching devices behind it.
 * - Support attachments that occur before the xHCI controller is initialized by
 *   deferring all host-controller calls until {@link setXhciBridge} is invoked.
 *
 * Notes:
 * - xHCI shares the guest-visible "root port 0 external hub / root port 1 WebUSB" convention with
 *   UHCI. For backwards compatibility, root-port-only paths (`[0]` / `[1]`) are remapped onto
 *   stable hub-backed paths behind root port 0.
 */
export class XhciHidTopologyManager {
  readonly #defaultHubPortCount: number;

  #xhci: XhciTopologyBridge | null = null;
  readonly #hubPortCountByRoot = new Map<number, number>();
  readonly #hubAttachedPortCountByRoot = new Map<number, number>();
  readonly #devices = new Map<number, DeviceRecord>();

  constructor(options: { defaultHubPortCount?: number } = {}) {
    this.#defaultHubPortCount = (() => {
      const requested = options.defaultHubPortCount;
      if (typeof requested === "number" && Number.isFinite(requested) && Number.isInteger(requested) && requested > 0) {
        return clampHubPortCount(requested);
      }
      return XHCI_MAX_HUB_PORT_COUNT;
    })();
  }

  #normalizeDevicePath(path: GuestUsbPath): GuestUsbPath {
    // Backwards-compatible mapping for older callers that only specify a root-port-only path
    // (`[0]` or `[1]`):
    //
    // - root port 0 is expected to host an external hub for WebHID passthrough
    // - root port 1 is reserved for WebUSB passthrough
    //
    // Remap `[0]` -> `[0, 5]` and `[1]` -> `[0, 6]` (matching UHCI's external hub convention) so:
    // - legacy root-port-only callers can still attach devices when xHCI is selected, and
    // - we avoid silently attaching a device directly on the root port and then replacing it with
    //   a hub when other passthrough devices are attached.
    if (path.length === 1 && (path[0] === EXTERNAL_HUB_ROOT_PORT || path[0] === WEBUSB_GUEST_ROOT_PORT)) {
      return [EXTERNAL_HUB_ROOT_PORT, remapLegacyRootPortToExternalHubPort(path[0])];
    }
    return path;
  }

  setXhciBridge(bridge: XhciTopologyBridge | null): void {
    if (this.#xhci !== bridge) {
      // Hub attachments are per-controller state; if we swap bridges (or clear),
      // force reattachment on the next active bridge.
      this.#hubAttachedPortCountByRoot.clear();
    }
    this.#xhci = bridge;
    if (!bridge) return;

    // Hubs are attached lazily as devices demand them, but if the host configured a hub
    // explicitly (via `setHubConfig`) before xHCI was initialized, attach it now so the
    // guest OS can enumerate it.
    for (const rootPort of this.#hubPortCountByRoot.keys()) {
      this.#maybeAttachHub(rootPort);
    }

    this.#flush();
  }

  setHubConfig(path: GuestUsbPath, portCount?: number): void {
    const rootPort = path[0] ?? 0;
    const count = clampHubPortCount(portCount ?? this.#defaultHubPortCount);
    this.#hubPortCountByRoot.set(rootPort, count);
    const resized = this.#maybeAttachHub(rootPort);
    if (resized) this.#reattachDevicesBehindRoot(rootPort);
  }

  attachDevice(deviceId: number, path: GuestUsbPath, kind: XhciHidPassthroughDeviceKind, device: unknown): void {
    const normalizedPath = this.#normalizeDevicePath(path.slice());
    // Treat (re-)attach as a new session for this deviceId.
    const prev = this.#devices.get(deviceId);
    if (prev) {
      this.#maybeDetachPath(prev.path);
    }
    this.#devices.delete(deviceId);
    if (!isValidDevicePath(normalizedPath)) return;
    this.#devices.set(deviceId, { path: normalizedPath, kind, device });
    this.#maybeAttachDevice(deviceId);
  }

  detachDevice(deviceId: number): void {
    const rec = this.#devices.get(deviceId) ?? null;
    this.#devices.delete(deviceId);
    if (!rec) return;
    this.#maybeDetachPath(rec.path);
  }

  #flush(): void {
    for (const deviceId of this.#devices.keys()) {
      this.#maybeAttachDevice(deviceId);
    }
  }

  #maybeAttachHub(rootPort: number, options: { minPortCount?: number } = {}): boolean {
    const xhci = this.#xhci;
    if (!xhci) return false;
    const attachHub = xhci.attach_hub;
    if (typeof attachHub !== "function") return false;

    let portCount = this.#requiredHubPortCount(rootPort);
    const minPortCount = options.minPortCount;
    if (typeof minPortCount === "number") {
      portCount = Math.max(portCount, clampHubPortCount(minPortCount));
    }
    const existing = this.#hubAttachedPortCountByRoot.get(rootPort);
    if (existing !== undefined && existing >= portCount) return false;
    if (existing !== undefined) {
      // Detach first so the guest observes a disconnect event and reloads the hub descriptor.
      const detach = xhci.detach_at_path;
      if (typeof detach === "function") {
        try {
          detach.call(xhci, [rootPort]);
        } catch {
          // ignore
        }
      }
    }
    try {
      attachHub.call(xhci, rootPort >>> 0, portCount >>> 0);
      this.#hubAttachedPortCountByRoot.set(rootPort, portCount);
    } catch {
      // Best-effort: hub attachment failures should not crash the worker.
      return false;
    }
    return existing !== undefined;
  }

  #maybeDetachPath(path: GuestUsbPath): void {
    const xhci = this.#xhci;
    if (!xhci) return;
    const detach = xhci.detach_at_path;
    if (typeof detach !== "function") return;
    try {
      detach.call(xhci, path);
    } catch {
      // ignore
    }
  }

  #maybeAttachDevice(deviceId: number): void {
    const rec = this.#devices.get(deviceId) ?? null;
    const xhci = this.#xhci;
    if (!rec || !xhci) return;

    const rootPort = rec.path[0] ?? 0;
    if (rec.path.length > 1) {
      const resized = this.#maybeAttachHub(rootPort);
      if (resized) {
        // Replacing the hub disconnects all downstream devices. Reattach everything
        // behind this root port so the guest USB topology returns to the expected state.
        this.#reattachDevicesBehindRoot(rootPort);
        return;
      }
    }

    // Clear any existing device at that path first. This keeps the worker resilient
    // to re-attaches and avoids silently stacking multiple devices behind the same port.
    this.#maybeDetachPath(rec.path);

    try {
      if (rec.kind === "webhid") {
        const attach = xhci.attach_webhid_device;
        if (typeof attach !== "function") return;
        attach.call(xhci, rec.path, rec.device);
      } else {
        const attach = xhci.attach_usb_hid_passthrough_device;
        if (typeof attach !== "function") return;
        attach.call(xhci, rec.path, rec.device);
      }
    } catch {
      // ignore
    }
  }

  #requiredHubPortCount(rootPort: number): number {
    let required = this.#hubPortCountByRoot.get(rootPort) ?? this.#defaultHubPortCount;
    for (const rec of this.#devices.values()) {
      const root = rec.path[0] ?? 0;
      if (root !== rootPort) continue;
      if (rec.path.length <= 1) continue;
      const port = rec.path[1] ?? 0;
      if (typeof port === "number" && Number.isFinite(port) && port > required) {
        required = port;
      }
    }
    return clampHubPortCount(required);
  }

  #reattachDevicesBehindRoot(rootPort: number): void {
    for (const [deviceId, rec] of this.#devices) {
      const root = rec.path[0] ?? 0;
      if (root !== rootPort) continue;
      if (rec.path.length <= 1) continue;
      this.#maybeAttachDevice(deviceId);
    }
  }
}
