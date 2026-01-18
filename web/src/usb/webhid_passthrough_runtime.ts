import { normalizeCollections, type HidCollectionInfo, type NormalizedHidCollectionInfo } from "../hid/webhid_normalize";
import { computeInputReportPayloadByteLengths } from "../hid/hid_report_sizes";
import type { WebHidPassthroughManager, WebHidPassthroughState } from "../platform/webhid_passthrough";
import { unrefBestEffort } from "../unrefSafe";

export type WebHidPassthroughOutputReport = {
  reportType: "output" | "feature";
  reportId: number;
  data: Uint8Array<ArrayBuffer>;
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
   *
   * Note: `WebHidPassthroughManager` exposes attachments (including `deviceId`/guest port),
   * but this runtime currently keys sessions by the underlying `HIDDevice` object.
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
   * Maximum number of output/feature reports to drain (total across all devices) per {@link pollOnce} call.
   *
   * This bounds the amount of JS work/allocations a busy guest can force in a single poll tick.
   */
  maxOutputReportsPerPoll?: number;
  /**
   * Maximum number of queued (not yet running) WebHID output/feature reports per device.
   *
   * WebHID requires per-device serialization. If an in-flight `sendReport` stalls and the guest keeps
   * producing output reports, the queue can otherwise grow without bound.
   */
  maxPendingOutputReportsPerDevice?: number;
  /**
   * Maximum total byte size of queued (not yet running) WebHID output/feature reports per device.
   *
   * WebHID output/feature reports are sent via USB control transfers, which can be up to 64KiB each.
   * A malicious guest can otherwise queue many max-sized reports while `sendReport` is stalled,
   * retaining tens of megabytes per device.
   */
  maxPendingOutputReportBytesPerDevice?: number;
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
  outputQueue: WebHidPassthroughOutputReport[];
  outputQueueBytes: number;
  outputRunnerToken: number | null;
  outputDropped: number;
  outputDropWarnedAtMs: number;
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

const UNKNOWN_INPUT_REPORT_HARD_CAP_BYTES = 64;
const DEFAULT_MAX_HID_OUTPUT_REPORTS_PER_POLL = 64;
const DEFAULT_MAX_PENDING_OUTPUT_REPORTS_PER_DEVICE = 1024;
const DEFAULT_MAX_PENDING_OUTPUT_REPORT_BYTES_PER_DEVICE = 4 * 1024 * 1024;
const OUTPUT_REPORT_DROP_WARN_INTERVAL_MS = 5000;
const MAX_HID_CONTROL_TRANSFER_BYTES = 0xffff;

function maxHidControlPayloadBytes(reportId: number): number {
  // USB control transfers have a u16 `wLength`. When `reportId != 0` the on-wire report includes a
  // 1-byte reportId prefix, so the payload must be <= 0xfffe.
  return (reportId >>> 0) === 0 ? MAX_HID_CONTROL_TRANSFER_BYTES : MAX_HID_CONTROL_TRANSFER_BYTES - 1;
}

function ensureArrayBufferBacked(bytes: Uint8Array): Uint8Array<ArrayBuffer> {
  // WebHID expects `BufferSource`. Some TS libdefs model `BufferSource` as
  // `ArrayBuffer | ArrayBufferView<ArrayBuffer>`, which rejects views backed by a
  // SharedArrayBuffer. Copy when needed to keep strict typechecking clean.
  if (bytes.buffer instanceof ArrayBuffer) return bytes as unknown as Uint8Array<ArrayBuffer>;
  const out = new Uint8Array(bytes.byteLength);
  out.set(bytes);
  return out;
}

export class WebHidPassthroughRuntime {
  readonly #sessions = new Map<HidDeviceLike, DeviceSession>();
  readonly #createBridge: BridgeFactory;
  readonly #pollIntervalMs: number;
  readonly #maxOutputReportsPerPoll: number;
  readonly #maxPendingOutputReportsPerDevice: number;
  readonly #maxPendingOutputReportBytesPerDevice: number;
  readonly #onDeviceReady?: (device: HidDeviceLike, bridge: WebHidPassthroughBridgeLike) => void;
  readonly #log: WebHidPassthroughRuntimeLogger;
  #pollTimer: ReturnType<typeof setInterval> | undefined;
  #unsubscribe: (() => void) | undefined;
  #nextOutputRunnerToken = 1;

  constructor(options: WebHidPassthroughRuntimeOptions) {
    this.#createBridge = options.createBridge;
    this.#pollIntervalMs = options.pollIntervalMs ?? 16;
    this.#maxOutputReportsPerPoll = (() => {
      const requested = options.maxOutputReportsPerPoll;
      if (requested === undefined) return DEFAULT_MAX_HID_OUTPUT_REPORTS_PER_POLL;
      if (!Number.isSafeInteger(requested) || requested <= 0) {
        throw new Error(`invalid maxOutputReportsPerPoll: ${requested}`);
      }
      return requested;
    })();
    this.#maxPendingOutputReportsPerDevice = (() => {
      const requested = options.maxPendingOutputReportsPerDevice;
      if (requested === undefined) return DEFAULT_MAX_PENDING_OUTPUT_REPORTS_PER_DEVICE;
      if (!Number.isSafeInteger(requested) || requested <= 0) {
        throw new Error(`invalid maxPendingOutputReportsPerDevice: ${requested}`);
      }
      return requested;
    })();
    this.#maxPendingOutputReportBytesPerDevice = (() => {
      const requested = options.maxPendingOutputReportBytesPerDevice;
      if (requested === undefined) return DEFAULT_MAX_PENDING_OUTPUT_REPORT_BYTES_PER_DEVICE;
      if (!Number.isSafeInteger(requested) || requested <= 0) {
        throw new Error(`invalid maxPendingOutputReportBytesPerDevice: ${requested}`);
      }
      return requested;
    })();
    this.#onDeviceReady = options.onDeviceReady;
    this.#log = options.logger ?? defaultLogger;

    if (options.manager) {
      this.#unsubscribe = options.manager.subscribe((state: WebHidPassthroughState) => {
        void this.syncAttachedDevices(state.attachedDevices.map((attachment) => attachment.device));
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
      // The community WebHID typings (`@types/w3c-web-hid`) model `HIDDevice.collections`
      // as a loose dictionary shape with lots of optional fields. Chromium's runtime
      // objects are stricter (and match the shape expected by our normalizer), so
      // cast through `unknown` here to avoid infecting downstream code with `| undefined`.
      const normalizedCollections = normalizeCollections(device.collections as unknown as readonly HidCollectionInfo[], {
        validate: true,
      });
      const expectedInputPayloadBytes = computeInputReportPayloadByteLengths(normalizedCollections);
      const warned = new Set<string>();
      const warnOnce = (key: string, message: string): void => {
        if (warned.has(key)) return;
        warned.add(key);
        this.#log("warn", message);
      };
      bridge = this.#createBridge({ device, normalizedCollections });

      const onInputReport = (event: HIDInputReportEvent): void => {
        try {
          const view = event.data;
          if (!(view instanceof DataView)) return;

          const rawReportId = (event as unknown as { reportId?: unknown }).reportId;
          if (
            rawReportId !== undefined &&
            (typeof rawReportId !== "number" || !Number.isInteger(rawReportId) || rawReportId < 0 || rawReportId > 0xff)
          ) {
            warnOnce(
              "invalidReportId",
              `[webhid] inputreport has invalid reportId=${String(rawReportId)}; dropping`,
            );
            return;
          }
          const reportId = (rawReportId === undefined ? 0 : rawReportId) >>> 0;
          const srcLen = view.byteLength;
          const expected = expectedInputPayloadBytes.get(reportId);
          let destLen: number;
          if (expected !== undefined) {
            destLen = expected;
            if (srcLen > expected) {
              warnOnce(
                `${reportId}:truncated`,
                `[webhid] inputreport length mismatch (reportId=${reportId} expected=${expected} got=${srcLen}); truncating`,
              );
            } else if (srcLen < expected) {
              warnOnce(
                `${reportId}:padded`,
                `[webhid] inputreport length mismatch (reportId=${reportId} expected=${expected} got=${srcLen}); zero-padding`,
              );
            }
          } else if (srcLen > UNKNOWN_INPUT_REPORT_HARD_CAP_BYTES) {
            destLen = UNKNOWN_INPUT_REPORT_HARD_CAP_BYTES;
            warnOnce(
              `${reportId}:hardCap`,
              `[webhid] inputreport reportId=${reportId} has unknown expected size; capping ${srcLen} bytes to ${UNKNOWN_INPUT_REPORT_HARD_CAP_BYTES}`,
            );
          } else {
            destLen = srcLen;
          }

          const copyLen = Math.min(srcLen, destLen);
          const src = new Uint8Array(view.buffer, view.byteOffset, copyLen);
          if (copyLen === destLen) {
            bridge?.push_input_report(reportId, src);
          } else {
            const out = new Uint8Array(destLen);
            out.set(src);
            bridge?.push_input_report(reportId, out);
          }
        } catch (err) {
          this.#log("warn", "WebHID inputreport forwarding failed", err);
        }
      };

      device.addEventListener("inputreport", onInputReport);
      this.#sessions.set(device, {
        device,
        bridge,
        onInputReport,
        outputQueue: [],
        outputQueueBytes: 0,
        outputRunnerToken: null,
        outputDropped: 0,
        outputDropWarnedAtMs: 0,
      });
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

    // Drop any queued output reports to avoid retaining buffers after detach.
    session.outputQueue.length = 0;
    session.outputQueueBytes = 0;
    session.outputRunnerToken = null;

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
      // Drop any queued output reports; if a sendReport is already in-flight we can't cancel it,
      // but clearing the queue avoids retaining unbounded buffers.
      session.outputQueue.length = 0;
      session.outputQueueBytes = 0;
      session.outputRunnerToken = null;
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
    let remainingReports = this.#maxOutputReportsPerPoll;
    for (const session of this.#sessions.values()) {
      if (remainingReports <= 0) return;
      const configured = session.bridge.configured ? session.bridge.configured() : true;
      if (!configured) continue;

      while (remainingReports > 0) {
        let report: WebHidPassthroughOutputReport | null = null;
        try {
          report = session.bridge.drain_next_output_report();
        } catch (err) {
          this.#log("warn", "drain_next_output_report() threw", err);
          break;
        }
        if (!report) break;
        remainingReports -= 1;

        try {
          const reportId = report.reportId >>> 0;
          // Defensive clamp: keep payload sizes within a single USB control transfer.
          const maxPayloadBytes = maxHidControlPayloadBytes(reportId);
          const clamped = report.data.byteLength > maxPayloadBytes ? report.data.subarray(0, maxPayloadBytes) : report.data;
          this.#enqueueOutputReport(session, report.reportType, reportId, clamped);
        } catch (err) {
          this.#log("warn", "WebHID output report forwarding failed", err);
        }
      }
    }
  }

  #enqueueOutputReport(session: DeviceSession, reportType: WebHidPassthroughOutputReport["reportType"], reportId: number, data: Uint8Array): void {
    const dataLen = data.byteLength >>> 0;
    if (session.outputQueue.length >= this.#maxPendingOutputReportsPerDevice) {
      session.outputDropped += 1;
      const now = typeof performance !== "undefined" ? performance.now() : Date.now();
      if (session.outputDropWarnedAtMs === 0 || now - session.outputDropWarnedAtMs >= OUTPUT_REPORT_DROP_WARN_INTERVAL_MS) {
        session.outputDropWarnedAtMs = now;
        this.#log(
          "warn",
          `[webhid] Dropping queued output reports (pending=${session.outputQueue.length} pendingBytes=${session.outputQueueBytes} maxPendingOutputReportsPerDevice=${this.#maxPendingOutputReportsPerDevice} maxPendingOutputReportBytesPerDevice=${this.#maxPendingOutputReportBytesPerDevice} dropped=${session.outputDropped})`,
        );
      }
      return;
    }
    if (dataLen > this.#maxPendingOutputReportBytesPerDevice || session.outputQueueBytes + dataLen > this.#maxPendingOutputReportBytesPerDevice) {
      session.outputDropped += 1;
      const now = typeof performance !== "undefined" ? performance.now() : Date.now();
      if (session.outputDropWarnedAtMs === 0 || now - session.outputDropWarnedAtMs >= OUTPUT_REPORT_DROP_WARN_INTERVAL_MS) {
        session.outputDropWarnedAtMs = now;
        this.#log(
          "warn",
          `[webhid] Dropping queued output reports (pending=${session.outputQueue.length} pendingBytes=${session.outputQueueBytes} maxPendingOutputReportsPerDevice=${this.#maxPendingOutputReportsPerDevice} maxPendingOutputReportBytesPerDevice=${this.#maxPendingOutputReportBytesPerDevice} dropped=${session.outputDropped})`,
        );
      }
      return;
    }

    // wasm-bindgen views may be backed by SharedArrayBuffer (threaded WASM);
    // WebHID expects an ArrayBuffer-backed BufferSource. Also ensure we don't retain a slice into a large buffer.
    const stored = ensureArrayBufferBacked(new Uint8Array(data));
    session.outputQueue.push({ reportType, reportId, data: stored });
    session.outputQueueBytes += stored.byteLength >>> 0;
    if (session.outputRunnerToken !== null) return;
    const token = this.#nextOutputRunnerToken++;
    session.outputRunnerToken = token;
    void this.#runOutputReportQueue(session, token);
  }

  async #runOutputReportQueue(session: DeviceSession, token: number): Promise<void> {
    try {
      // eslint-disable-next-line no-constant-condition
      while (true) {
        if (session.outputRunnerToken !== token) break;
        const next = session.outputQueue.shift();
        if (!next) break;
        session.outputQueueBytes = Math.max(0, session.outputQueueBytes - (next.data.byteLength >>> 0));
        try {
          if (next.reportType === "feature") {
            await session.device.sendFeatureReport(next.reportId, next.data);
          } else {
            await session.device.sendReport(next.reportId, next.data);
          }
        } catch (err) {
          this.#log("warn", `WebHID ${next.reportType === "feature" ? "sendFeatureReport" : "sendReport"}() failed`, err);
        }
      }
    } finally {
      if (session.outputRunnerToken === token) {
        session.outputRunnerToken = null;
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
    unrefBestEffort(this.#pollTimer);
  }

  private maybeStopPolling(): void {
    if (this.#sessions.size !== 0) return;
    if (this.#pollTimer === undefined) return;
    clearInterval(this.#pollTimer);
    this.#pollTimer = undefined;
  }
}
