import type { GuestUsbPath } from "../platform/hid_passthrough_protocol";
import {
  DEFAULT_EXTERNAL_HUB_PORT_COUNT,
  UHCI_SYNTHETIC_HID_HUB_PORT_COUNT,
  remapLegacyRootPortToExternalHubPort,
} from "../usb/uhci_external_hub";

export type UhciHidPassthroughDeviceKind = "webhid" | "usb-hid-passthrough";

function clampHubPortCount(value: number): number {
  if (!Number.isFinite(value)) return 1;
  const int = Math.floor(value);
  return Math.max(1, Math.min(255, int));
}

/**
 * Subset of the WASM `UhciControllerBridge` API required to manage guest USB topology.
 *
 * The full WASM export also includes UHCI register access + frame stepping; this
 * interface is intentionally narrow so it can be faked in unit tests.
 */
export type UhciTopologyBridge = {
  attach_hub(rootPort: number, portCount: number): void;
  detach_at_path(path: number[]): void;
  attach_webhid_device(path: number[], device: unknown): void;
  attach_usb_hid_passthrough_device(path: number[], device: unknown): void;
};

type DeviceRecord = {
  path: GuestUsbPath;
  kind: UhciHidPassthroughDeviceKind;
  device: unknown;
};

/**
 * Worker-side guest USB topology bookkeeping for UHCI.
 *
 * Responsibilities:
 * - Remember which USB devices should be attached at which guest paths.
 * - Ensure an emulated external hub exists before attaching devices behind it.
 * - Support attachments that occur before the UHCI controller is initialized by
 *   deferring all host-controller calls until {@link setUhciBridge} is invoked.
 */
export class UhciHidTopologyManager {
  readonly #defaultHubPortCount: number;

  #uhci: UhciTopologyBridge | null = null;
  readonly #hubPortCountByRoot = new Map<number, number>();
  readonly #hubAttachedPortCountByRoot = new Map<number, number>();
  readonly #devices = new Map<number, DeviceRecord>();

  constructor(options: { defaultHubPortCount?: number } = {}) {
    this.#defaultHubPortCount = (() => {
      const requested = options.defaultHubPortCount;
      if (typeof requested === "number" && Number.isFinite(requested) && Number.isInteger(requested) && requested > 0) {
        return Math.min(255, requested);
      }
      return DEFAULT_EXTERNAL_HUB_PORT_COUNT;
    })();
  }

  #normalizeDevicePath(path: GuestUsbPath): GuestUsbPath {
    // Root port 0 is reserved for the external hub and root port 1 is reserved for the
    // guest-visible WebUSB passthrough device. For backwards compatibility, callers may still
    // provide a single-element root-port path (`[0]` or `[1]`). Remap these to stable hub-backed
    // paths so we never clobber the hub or the WebUSB device.
    if (path.length === 1 && (path[0] === 0 || path[0] === 1)) return [0, remapLegacyRootPortToExternalHubPort(path[0])];
    return path;
  }

  setUhciBridge(bridge: UhciTopologyBridge | null): void {
    if (this.#uhci !== bridge) {
      // Hub attachments are per-controller state; if we swap bridges (or clear),
      // force reattachment on the next active bridge.
      this.#hubAttachedPortCountByRoot.clear();
    }
    this.#uhci = bridge;
    if (!bridge) return;

    // Root port 0 is reserved for an emulated external hub used for WebHID passthrough.
    // Attach it eagerly so the guest OS can enumerate the hub even before any devices are
    // attached behind it.
    this.#maybeAttachHub(0);

    // Other hubs are still generally attached lazily as devices demand them, but if the host
    // configured a hub explicitly (via `setHubConfig`) before UHCI was initialized, attach it now.
    for (const rootPort of this.#hubPortCountByRoot.keys()) {
      if (rootPort === 0) continue;
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

  attachDevice(deviceId: number, path: GuestUsbPath, kind: UhciHidPassthroughDeviceKind, device: unknown): void {
    const normalizedPath = this.#normalizeDevicePath(path.slice());
    // Treat (re-)attach as a new session for this deviceId.
    const prev = this.#devices.get(deviceId);
    if (prev) {
      this.#maybeDetachPath(prev.path);
    }
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
    // Root port 0's external hub is attached eagerly when the UHCI bridge becomes available.
    // Other hubs are attached lazily as devices demand them, so only flush devices here.
    for (const deviceId of this.#devices.keys()) {
      this.#maybeAttachDevice(deviceId);
    }
  }

  #maybeAttachHub(rootPort: number, options: { minPortCount?: number } = {}): boolean {
    const uhci = this.#uhci;
    if (!uhci) return false;

    let portCount = this.#requiredHubPortCount(rootPort);
    const minPortCount = options.minPortCount;
    if (typeof minPortCount === "number") {
      portCount = Math.max(portCount, clampHubPortCount(minPortCount));
    }
    const existing = this.#hubAttachedPortCountByRoot.get(rootPort);
    if (existing !== undefined && existing >= portCount) return false;
    if (existing !== undefined) {
      // Replacing the hub without a disconnect would not toggle the UHCI connection-status-change
      // (CSC) bit, so the guest OS could keep using a cached hub descriptor (port count, etc).
      // Detach first so this behaves like a real hotplug event.
      try {
        uhci.detach_at_path([rootPort]);
      } catch {
        // ignore
      }
    }
    try {
      uhci.attach_hub(rootPort >>> 0, portCount >>> 0);
      this.#hubAttachedPortCountByRoot.set(rootPort, portCount);
    } catch {
      // Best-effort: hub attachment failures should not crash the worker.
      return false;
    }
    return existing !== undefined;
  }

  #maybeDetachPath(path: GuestUsbPath): void {
    const uhci = this.#uhci;
    if (!uhci) return;
    try {
      uhci.detach_at_path(path);
    } catch {
      // ignore
    }
  }

  #maybeAttachDevice(deviceId: number): void {
    const rec = this.#devices.get(deviceId) ?? null;
    const uhci = this.#uhci;
    if (!rec || !uhci) return;

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
        uhci.attach_webhid_device(rec.path, rec.device);
      } else {
        uhci.attach_usb_hid_passthrough_device(rec.path, rec.device);
      }
    } catch {
      // ignore
    }
  }

  #requiredHubPortCount(rootPort: number): number {
    let required = this.#hubPortCountByRoot.get(rootPort) ?? this.#defaultHubPortCount;
    // Root port 0 hosts the external hub that also carries synthetic HID devices on fixed ports.
    // Ensure the hub descriptor always has enough downstream ports to accommodate that reserved range,
    // even before any synthetic device is attached.
    if (rootPort === 0) {
      required = Math.max(required, UHCI_SYNTHETIC_HID_HUB_PORT_COUNT);
    }
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
