import type { GuestUsbPath } from "../platform/hid_passthrough_protocol";
import {
  DEFAULT_EXTERNAL_HUB_PORT_COUNT,
  EXTERNAL_HUB_ROOT_PORT,
  UHCI_SYNTHETIC_HID_HUB_PORT_COUNT,
  WEBUSB_GUEST_ROOT_PORT,
  remapLegacyRootPortToExternalHubPort,
} from "../usb/uhci_external_hub";
import { formatOneLineError } from "../text";

export type UhciHidPassthroughDeviceKind = "webhid" | "usb-hid-passthrough";

function clampHubPortCount(value: number): number {
  // UHCI (USB 1.1) hubs advertise an 8-bit downstream port count (1..=255). Do not apply xHCI's
  // Route String constraints here: xHCI encodes hub port numbers in 4-bit nibbles (1..=15).
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
    if (
      path.length === 1 &&
      (path[0] === EXTERNAL_HUB_ROOT_PORT || path[0] === WEBUSB_GUEST_ROOT_PORT)
    )
      return [EXTERNAL_HUB_ROOT_PORT, remapLegacyRootPortToExternalHubPort(path[0])];
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
    this.#maybeAttachHub(EXTERNAL_HUB_ROOT_PORT);

    // Other hubs are still generally attached lazily as devices demand them, but if the host
    // configured a hub explicitly (via `setHubConfig`) before UHCI was initialized, attach it now.
    for (const rootPort of this.#hubPortCountByRoot.keys()) {
      if (rootPort === EXTERNAL_HUB_ROOT_PORT) continue;
      // Root port 1 is reserved for the guest-visible WebUSB passthrough device.
      if (rootPort === WEBUSB_GUEST_ROOT_PORT) continue;
      this.#maybeAttachHub(rootPort);
    }

    this.#flush();
  }

  setHubConfig(path: GuestUsbPath, portCount?: number): void {
    const rootPort = path[0] ?? EXTERNAL_HUB_ROOT_PORT;
    // Root port 1 is reserved for WebUSB passthrough. Do not attach hubs there: WebUSB uses a
    // guest-visible device directly on that root port, and the WASM bridge rejects non-WebUSB
    // attachments at that port.
    if (rootPort === WEBUSB_GUEST_ROOT_PORT) return;
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
    this.#devices.delete(deviceId);
    // Root port 1 is reserved for WebUSB passthrough. Root-port-only paths are remapped by
    // `#normalizeDevicePath`, but reject deeper paths behind that reserved root port to avoid
    // clobbering WebUSB state and avoid surfacing WASM attach errors as runtime exceptions.
    const rootPort = normalizedPath[0] ?? EXTERNAL_HUB_ROOT_PORT;
    if (rootPort === WEBUSB_GUEST_ROOT_PORT) return;
    this.#devices.set(deviceId, { path: normalizedPath, kind, device });
    this.#maybeAttachDevice(deviceId, { throwOnFailure: true });
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

  #maybeAttachHub(rootPort: number, options: { minPortCount?: number; throwOnFailure?: boolean } = {}): boolean {
    if (rootPort === WEBUSB_GUEST_ROOT_PORT) return false;
    const uhci = this.#uhci;
    if (!uhci) return false;
    const uhciAny = uhci as unknown as Record<string, unknown>;
    const attachHub = uhciAny.attach_hub ?? uhciAny.attachHub;
    const detachAtPath = uhciAny.detach_at_path ?? uhciAny.detachAtPath;

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
      if (typeof detachAtPath === "function") {
        try {
          detachAtPath.call(uhci, [rootPort]);
        } catch {
          // ignore
        }
      }
    }
    if (typeof attachHub !== "function") {
      if (options.throwOnFailure) {
        throw new Error("UHCI attach hub export unavailable");
      }
      return false;
    }
    try {
      attachHub.call(uhci, rootPort >>> 0, portCount >>> 0);
      this.#hubAttachedPortCountByRoot.set(rootPort, portCount);
    } catch (err) {
      if (options.throwOnFailure) {
        const message = formatOneLineError(err, 512);
        throw new Error(`UHCI attach_hub failed (rootPort=${rootPort} ports=${portCount}): ${message}`);
      }
      // Best-effort: hub attachment failures should not crash the worker.
      return false;
    }
    return existing !== undefined;
  }

  #maybeDetachPath(path: GuestUsbPath): void {
    const uhci = this.#uhci;
    if (!uhci) return;
    const uhciAny = uhci as unknown as Record<string, unknown>;
    const detachAtPath = uhciAny.detach_at_path ?? uhciAny.detachAtPath;
    if (typeof detachAtPath !== "function") return;
    try {
      detachAtPath.call(uhci, path);
    } catch {
      // ignore
    }
  }

  #maybeAttachDevice(deviceId: number, options: { throwOnFailure?: boolean } = {}): void {
    const rec = this.#devices.get(deviceId) ?? null;
    const uhci = this.#uhci;
    if (!rec || !uhci) return;
    const uhciAny = uhci as unknown as Record<string, unknown>;

    const rootPort = rec.path[0] ?? EXTERNAL_HUB_ROOT_PORT;
    if (rec.path.length > 1) {
      const resized = this.#maybeAttachHub(rootPort, { throwOnFailure: options.throwOnFailure });
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
        const attachWebhid = uhciAny.attach_webhid_device ?? uhciAny.attachWebhidDevice ?? uhciAny.attachWebHidDevice;
        if (typeof attachWebhid !== "function") return;
        attachWebhid.call(uhci, rec.path, rec.device);
      } else {
        const attachUsbHid = uhciAny.attach_usb_hid_passthrough_device ?? uhciAny.attachUsbHidPassthroughDevice;
        if (typeof attachUsbHid !== "function") return;
        attachUsbHid.call(uhci, rec.path, rec.device);
      }
    } catch (err) {
      if (options.throwOnFailure) {
        const message = formatOneLineError(err, 512);
        throw new Error(`UHCI attach device failed (path=${rec.path.join(".")} kind=${rec.kind}): ${message}`);
      }
      // ignore
    }
  }

  #requiredHubPortCount(rootPort: number): number {
    let required = this.#hubPortCountByRoot.get(rootPort) ?? this.#defaultHubPortCount;
    // Root port 0 hosts the external hub that also carries synthetic HID devices on fixed ports.
    // Ensure the hub descriptor always has enough downstream ports to accommodate that reserved range,
    // even before any synthetic device is attached.
    if (rootPort === EXTERNAL_HUB_ROOT_PORT) {
      required = Math.max(required, UHCI_SYNTHETIC_HID_HUB_PORT_COUNT);
    }
    for (const rec of this.#devices.values()) {
      const root = rec.path[0] ?? EXTERNAL_HUB_ROOT_PORT;
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
      const root = rec.path[0] ?? EXTERNAL_HUB_ROOT_PORT;
      if (root !== rootPort) continue;
      if (rec.path.length <= 1) continue;
      this.#maybeAttachDevice(deviceId);
    }
  }

}
