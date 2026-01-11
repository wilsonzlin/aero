import { normalizeCollections, type NormalizedHidCollectionInfo } from "../hid/webhid_normalize";
import type { WebHidPassthroughManager, WebHidPassthroughState } from "../platform/webhid_passthrough";

export type WebHidPassthroughOutputReport = {
  reportType: "output" | "feature";
  reportId: number;
  data: Uint8Array;
};

export type WebHidPassthroughBridgeLike = {
  push_input_report(reportId: number, data: Uint8Array): void;
  drain_next_output_report(): WebHidPassthroughOutputReport | null;
  configured?: () => boolean;
  free(): void;
};

type HidDeviceLike = Pick<
  HIDDevice,
  | "opened"
  | "open"
  | "close"
  | "collections"
  | "addEventListener"
  | "removeEventListener"
  | "sendReport"
  | "sendFeatureReport"
  | "vendorId"
  | "productId"
  | "productName"
>;

type BridgeFactory = (args: {
  device: HidDeviceLike;
  normalizedCollections: NormalizedHidCollectionInfo[];
}) => WebHidPassthroughBridgeLike;

export type WebHidPassthroughRuntimeLogger = (level: "debug" | "info" | "warn" | "error", message: string, err?: unknown) => void;

export interface WebHidPassthroughRuntimeOptions {
  /**
   * Optional device manager; when present, the runtime will subscribe to it and
   * automatically attach/detach devices based on `state.attachedDevices`.
   */
  manager?: Pick<WebHidPassthroughManager, "subscribe" | "getState">;
  /**
   * Factory that creates the WASM passthrough bridge for a given HIDDevice.
   */
  createBridge: BridgeFactory;
  /**
   * Poll interval used to drain output/feature reports from the WASM bridge and
   * execute them via WebHID `sendReport`/`sendFeatureReport`.
   *
   * Set to 0 to disable polling (tests may call `pollOnce()` manually).
   */
  pollIntervalMs?: number;
  /**
   * Optional callback invoked once a device has been opened and a bridge has been created.
   *
   * This is the primary "extension point" for wiring the guest USB topology later.
   */
  onDeviceReady?: (device: HidDeviceLike, bridge: WebHidPassthroughBridgeLike) => void;
  /**
   * Optional logger; defaults to `console`.
   */
  logger?: WebHidPassthroughRuntimeLogger;
}

type DeviceSession = {
  device: HidDeviceLike;
  bridge: WebHidPassthroughBridgeLike;
  onInputReport: (event: HIDInputReportEvent) => void;
};

function defaultLogger(level: "debug" | "info" | "warn" | "error", message: string, err?: unknown): void {
  switch (level) {
    case "debug":
      console.debug(message, err);
      break;
    case "info":
      console.info(message, err);
      break;
    case "warn":
      console.warn(message, err);
      break;
    case "error":
      console.error(message, err);
      break;
    default: {
      const neverLevel: never = level;
      console.warn(`Unknown log level: ${String(neverLevel)}`, message, err);
    }
  }
}

function copyDataView(view: DataView): Uint8Array {
  const src = new Uint8Array(view.buffer, view.byteOffset, view.byteLength);
  const out = new Uint8Array(src.byteLength);
  out.set(src);
  return out;
}

export class WebHidPassthroughRuntime {
  readonly #sessions = new Map<HidDeviceLike, DeviceSession>();
  readonly #createBridge: BridgeFactory;
  readonly #pollIntervalMs: number;
  readonly #onDeviceReady?: (device: HidDeviceLike, bridge: WebHidPassthroughBridgeLike) => void;
  readonly #log: WebHidPassthroughRuntimeLogger;
  #pollTimer: number | undefined;
  #unsubscribe: (() => void) | undefined;

  constructor(options: WebHidPassthroughRuntimeOptions) {
    this.#createBridge = options.createBridge;
    this.#pollIntervalMs = options.pollIntervalMs ?? 16;
    this.#onDeviceReady = options.onDeviceReady;
    this.#log = options.logger ?? defaultLogger;

    if (options.manager) {
      this.#unsubscribe = options.manager.subscribe((state: WebHidPassthroughState) => {
        void this.syncAttachedDevices(state.attachedDevices);
      });
    }
  }

  /**
   * Align the runtime's attached device sessions with the provided list.
   *
   * This is used by the `WebHidPassthroughManager` subscription but can also be
   * called directly.
   */
  async syncAttachedDevices(attached: readonly HidDeviceLike[]): Promise<void> {
    const next = new Set(attached);

    for (const device of attached) {
      await this.attachDevice(device);
    }

    for (const device of Array.from(this.#sessions.keys())) {
      if (!next.has(device)) {
        await this.detachDevice(device);
      }
    }
  }

  async attachDevice(device: HidDeviceLike): Promise<void> {
    if (this.#sessions.has(device)) return;

    try {
      if (!device.opened) {
        await device.open();
      }
    } catch (err) {
      this.#log("warn", "WebHID device.open() failed", err);
      return;
    }

    let bridge: WebHidPassthroughBridgeLike | null = null;
    try {
      const normalizedCollections = normalizeCollections(device.collections);
      bridge = this.#createBridge({ device, normalizedCollections });

      const onInputReport = (event: HIDInputReportEvent): void => {
        try {
          bridge?.push_input_report(event.reportId, copyDataView(event.data));
        } catch (err) {
          this.#log("warn", "WebHID inputreport forwarding failed", err);
        }
      };

      device.addEventListener("inputreport", onInputReport);
      this.#sessions.set(device, { device, bridge, onInputReport });
      this.#onDeviceReady?.(device, bridge);

      this.ensurePolling();
    } catch (err) {
      this.#log("warn", "Failed to attach WebHID passthrough runtime for device", err);
      try {
        bridge?.free();
      } catch {
        // ignore
      }
    }
  }

  async detachDevice(device: HidDeviceLike): Promise<void> {
    const session = this.#sessions.get(device);
    if (!session) return;

    this.#sessions.delete(device);

    try {
      device.removeEventListener("inputreport", session.onInputReport);
    } catch (err) {
      this.#log("debug", "WebHID removeEventListener(inputreport) failed", err);
    }

    try {
      session.bridge.free();
    } catch (err) {
      this.#log("debug", "WASM WebHID passthrough bridge free() failed", err);
    }

    try {
      if (device.opened) {
        await device.close();
      }
    } catch (err) {
      this.#log("warn", "WebHID device.close() failed", err);
    }

    this.maybeStopPolling();
  }

  destroy(): void {
    this.#unsubscribe?.();
    this.#unsubscribe = undefined;

    // Best-effort synchronous cleanup; callers that care about close semantics
    // should call `detachDevice` explicitly and await it.
    for (const [device, session] of this.#sessions) {
      try {
        device.removeEventListener("inputreport", session.onInputReport);
      } catch {
        // ignore
      }
      try {
        session.bridge.free();
      } catch {
        // ignore
      }
    }
    this.#sessions.clear();

    this.maybeStopPolling();
  }

  pollOnce(): void {
    for (const session of this.#sessions.values()) {
      const configured = session.bridge.configured ? session.bridge.configured() : true;
      if (!configured) continue;

      while (true) {
        let report: WebHidPassthroughOutputReport | null = null;
        try {
          report = session.bridge.drain_next_output_report();
        } catch (err) {
          this.#log("warn", "drain_next_output_report() threw", err);
          break;
        }
        if (!report) break;

        try {
          if (report.reportType === "feature") {
            void session.device.sendFeatureReport(report.reportId, report.data).catch((err) => {
              this.#log("warn", "WebHID sendFeatureReport() failed", err);
            });
          } else {
            void session.device.sendReport(report.reportId, report.data).catch((err) => {
              this.#log("warn", "WebHID sendReport() failed", err);
            });
          }
        } catch (err) {
          this.#log("warn", "WebHID output report forwarding failed", err);
        }
      }
    }
  }

  private ensurePolling(): void {
    if (this.#pollIntervalMs <= 0) return;
    if (this.#pollTimer !== undefined) return;
    if (this.#sessions.size === 0) return;

    this.#pollTimer = setInterval(() => {
      this.pollOnce();
    }, this.#pollIntervalMs);
  }

  private maybeStopPolling(): void {
    if (this.#sessions.size !== 0) return;
    if (this.#pollTimer === undefined) return;
    clearInterval(this.#pollTimer);
    this.#pollTimer = undefined;
  }
}
