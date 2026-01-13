import type { GuestUsbPath } from "../platform/hid_passthrough_protocol";

export type XhciHidPassthroughDeviceKind = "webhid" | "usb-hid-passthrough";

/**
 * Maximum downstream port count for a USB hub attached behind xHCI.
 *
 * Real xHCI implementations encode the hub route string using 4-bit port numbers
 * (1..15). Keeping hub port counts <=15 avoids creating guest-visible USB
 * topologies that cannot be represented with valid xHCI route strings.
 */
export const XHCI_MAX_HUB_PORT_COUNT = 15;

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
  const root = path[0];
  if (typeof root !== "number" || !Number.isFinite(root) || !Number.isInteger(root) || root < 0) return false;
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
 */
export type XhciTopologyBridge = {
  attach_hub(rootPort: number, portCount: number): void;
  detach_at_path(path: number[]): void;
  attach_webhid_device(path: number[], device: unknown): void;
  attach_usb_hid_passthrough_device(path: number[], device: unknown): void;
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
 * - Unlike {@link UhciHidTopologyManager}, this manager does not reserve any
 *   particular root port numbers (e.g. for an external hub or WebUSB). Callers
 *   must provide the full guest path they intend to use.
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
    // xHCI topology does not have any hard-coded reserved root ports in this layer.
    // Caller-supplied paths are used as-is.
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

    let portCount = this.#requiredHubPortCount(rootPort);
    const minPortCount = options.minPortCount;
    if (typeof minPortCount === "number") {
      portCount = Math.max(portCount, clampHubPortCount(minPortCount));
    }
    const existing = this.#hubAttachedPortCountByRoot.get(rootPort);
    if (existing !== undefined && existing >= portCount) return false;
    if (existing !== undefined) {
      // Detach first so the guest observes a disconnect event and reloads the hub descriptor.
      try {
        xhci.detach_at_path([rootPort]);
      } catch {
        // ignore
      }
    }
    try {
      xhci.attach_hub(rootPort >>> 0, portCount >>> 0);
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
    try {
      xhci.detach_at_path(path);
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
        xhci.attach_webhid_device(rec.path, rec.device);
      } else {
        xhci.attach_usb_hid_passthrough_device(rec.path, rec.device);
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
