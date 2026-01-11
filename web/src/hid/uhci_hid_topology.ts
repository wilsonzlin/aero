import type { GuestUsbPath } from "../platform/hid_passthrough_protocol";
import { DEFAULT_EXTERNAL_HUB_PORT_COUNT } from "../platform/webhid_passthrough";

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
  readonly #hubAttachedRoots = new Set<number>();
  readonly #devices = new Map<number, DeviceRecord>();

  constructor(options: { defaultHubPortCount?: number } = {}) {
    this.#defaultHubPortCount = options.defaultHubPortCount ?? DEFAULT_EXTERNAL_HUB_PORT_COUNT;
  }

  setUhciBridge(bridge: UhciTopologyBridge | null): void {
    if (this.#uhci !== bridge) {
      // Hub attachments are per-controller state; if we swap bridges (or clear),
      // force reattachment on the next active bridge.
      this.#hubAttachedRoots.clear();
    }
    this.#uhci = bridge;
    if (bridge) this.#flush();
  }

  setHubConfig(path: GuestUsbPath, portCount?: number): void {
    const rootPort = path[0] ?? 0;
    const count = clampHubPortCount(portCount ?? this.#defaultHubPortCount);
    this.#hubPortCountByRoot.set(rootPort, count);
    this.#maybeAttachHub(rootPort);
  }

  attachDevice(deviceId: number, path: GuestUsbPath, kind: UhciHidPassthroughDeviceKind, device: unknown): void {
    this.#devices.set(deviceId, { path, kind, device });
    this.#maybeAttachDevice(deviceId);
  }

  detachDevice(deviceId: number): void {
    const rec = this.#devices.get(deviceId) ?? null;
    this.#devices.delete(deviceId);
    if (!rec) return;
    this.#maybeDetachPath(rec.path);
  }

  #flush(): void {
    // Hubs are attached lazily as devices demand them, so only flush devices here.
    for (const deviceId of this.#devices.keys()) {
      this.#maybeAttachDevice(deviceId);
    }
  }

  #maybeAttachHub(rootPort: number, opts: { minPortCount?: number } = {}): void {
    const uhci = this.#uhci;
    if (!uhci) return;
    if (this.#hubAttachedRoots.has(rootPort)) return;

    const configured = this.#hubPortCountByRoot.get(rootPort) ?? this.#defaultHubPortCount;
    const minPortCount = opts.minPortCount ?? 0;
    const portCount = clampHubPortCount(Math.max(configured, minPortCount));
    try {
      uhci.attach_hub(rootPort >>> 0, portCount >>> 0);
      this.#hubAttachedRoots.add(rootPort);
    } catch {
      // Best-effort: hub attachment failures should not crash the worker.
    }
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
      // Ensure the hub has enough downstream ports to cover the requested path.
      const requestedPort = rec.path[1] ?? 0;
      this.#maybeAttachHub(rootPort, { minPortCount: requestedPort });
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
}
