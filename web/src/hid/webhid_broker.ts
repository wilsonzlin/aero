import { WebHidPassthroughManager } from "../platform/webhid_passthrough";
import { alignUp, RECORD_ALIGN, ringCtrl } from "../ipc/layout";
import { RingBuffer } from "../ipc/ring_buffer";
import { StatusIndex } from "../runtime/shared_layout";
import { normalizeCollections, type NormalizedHidCollectionInfo } from "./webhid_normalize";
import {
  isHidErrorMessage,
  isHidLogMessage,
  isHidSendReportMessage,
  type HidAttachMessage,
  type HidDetachMessage,
  type HidInputReportMessage,
  type HidProxyMessage,
  type HidRingAttachMessage,
  type HidRingInitMessage,
  type HidSendReportMessage,
} from "./hid_proxy_protocol";
import type { GuestUsbPath } from "../platform/hid_passthrough_protocol";
import { createHidReportRingBuffer, HidReportRing, HidReportType as HidRingReportType } from "../usb/hid_report_ring";
import {
  HID_INPUT_REPORT_RECORD_HEADER_BYTES,
  writeHidInputReportRingRecord,
} from "./hid_input_report_ring";

export type WebHidBrokerState = {
  workerAttached: boolean;
  attachedDeviceIds: number[];
};

export type WebHidBrokerListener = (state: WebHidBrokerState) => void;

export type WebHidLastInputReportInfo = {
  tsMs: number;
  byteLength: number;
};

export type WebHidInputReportRingStats = Readonly<{
  enabled: boolean;
  pushed: number;
  dropped: number;
  fallback: number;
}>;

function computeHasInterruptOut(collections: NormalizedHidCollectionInfo[]): boolean {
  const stack = [...collections];
  while (stack.length) {
    const node = stack.pop()!;
    // Feature reports are transferred over the control endpoint (SET_REPORT/GET_REPORT) and do
    // not require an interrupt OUT endpoint. Only output reports imply an interrupt OUT endpoint.
    if (node.outputReports.length > 0) return true;
    for (const child of node.children) stack.push(child);
  }
  return false;
}

function ensureArrayBufferBacked(bytes: Uint8Array): Uint8Array<ArrayBuffer> {
  // TypeScript's `BufferSource` type excludes `SharedArrayBuffer` in some lib.dom
  // versions, even though Chromium accepts it for WebHID calls. Keep this module
  // strict-friendly by copying when the buffer is shared.
  if (bytes.buffer instanceof ArrayBuffer) return bytes as unknown as Uint8Array<ArrayBuffer>;
  const out = new Uint8Array(bytes.byteLength);
  out.set(bytes);
  return out;
}

export class WebHidBroker {
  readonly manager: WebHidPassthroughManager;

  #workerPort: MessagePort | Worker | null = null;
  #workerPortListener: EventListener | null = null;

  #inputReportRing: RingBuffer | null = null;
  #inputReportRingPushed = 0;
  #inputReportRingDropped = 0;
  #inputReportFallback = 0;
  readonly #inputReportRingCapacityBytes: number;
  #status: Int32Array | null = null;

  #nextDeviceId = 1;
  readonly #deviceIdByDevice = new Map<HIDDevice, number>();
  readonly #deviceById = new Map<number, HIDDevice>();

  readonly #attachedToWorker = new Set<number>();
  readonly #inputReportListeners = new Map<number, (event: HIDInputReportEvent) => void>();
  readonly #lastInputReportInfo = new Map<number, WebHidLastInputReportInfo>();

  readonly #listeners = new Set<WebHidBrokerListener>();

  #inputReportEmitTimer: ReturnType<typeof setTimeout> | null = null;

  #inputRing: HidReportRing | null = null;
  #outputRing: HidReportRing | null = null;
  #outputRingDrainTimer: ReturnType<typeof setInterval> | null = null;

  #managerUnsubscribe: (() => void) | null = null;
  #prevManagerAttached = new Set<HIDDevice>();

  constructor(options: { manager?: WebHidPassthroughManager; inputReportRingCapacityBytes?: number } = {}) {
    this.manager = options.manager ?? new WebHidPassthroughManager();
    this.#inputReportRingCapacityBytes = options.inputReportRingCapacityBytes ?? 2 * 1024 * 1024;

    // Ensure we clean up bridged state when the underlying manager closes a device
    // (e.g., after a physical disconnect).
    this.#prevManagerAttached = new Set(this.manager.getState().attachedDevices.map((entry) => entry.device));
    this.#managerUnsubscribe = this.manager.subscribe((state) => {
      const next = new Set(state.attachedDevices.map((entry) => entry.device));
      for (const device of this.#prevManagerAttached) {
        if (!next.has(device)) {
          void this.#handleManagerDeviceDetached(device);
        }
      }
      this.#prevManagerAttached = next;
    });
  }

  destroy(): void {
    this.detachWorkerPort(this.#workerPort ?? undefined);
    this.#managerUnsubscribe?.();
    this.#managerUnsubscribe = null;
    if (this.#inputReportEmitTimer) {
      clearTimeout(this.#inputReportEmitTimer);
      this.#inputReportEmitTimer = null;
    }
    this.#listeners.clear();
  }

  getState(): WebHidBrokerState {
    return {
      workerAttached: !!this.#workerPort,
      attachedDeviceIds: Array.from(this.#attachedToWorker),
    };
  }

  subscribe(listener: WebHidBrokerListener): () => void {
    this.#listeners.add(listener);
    listener(this.getState());
    return () => {
      this.#listeners.delete(listener);
    };
  }

  isWorkerAttached(): boolean {
    return !!this.#workerPort;
  }

  setInputReportRing(ring: RingBuffer | null, status: Int32Array | null = null): void {
    if (ring && ring !== this.#inputReportRing) {
      ring.reset();
    }
    this.#inputReportRing = ring;
    this.#status = status;
    this.#inputReportRingPushed = 0;
    this.#inputReportRingDropped = 0;
    this.#inputReportFallback = 0;
  }

  getInputReportRingStats(): WebHidInputReportRingStats {
    return {
      enabled: this.#inputReportRing !== null,
      pushed: this.#inputReportRingPushed,
      dropped: this.#inputReportRingDropped,
      fallback: this.#inputReportFallback,
    };
  }

  getLastInputReportInfo(device: HIDDevice): WebHidLastInputReportInfo | null {
    const deviceId = this.#deviceIdByDevice.get(device);
    if (deviceId === undefined) return null;
    return this.#lastInputReportInfo.get(deviceId) ?? null;
  }

  attachWorkerPort(port: MessagePort | Worker): void {
    if (this.#workerPort === port) return;

    // Replacing the worker is treated as a new guest session: previously-attached
    // devices must be explicitly re-attached by the user before the new worker is
    // allowed to access them.
    if (this.#workerPort) {
      this.detachWorkerPort(this.#workerPort);
    }

    this.#workerPort = port;

    const onMessage: EventListener = (ev) => {
      const data = (ev as MessageEvent<unknown>).data;
      if (isHidSendReportMessage(data)) {
        void this.#handleSendReportRequest(data);
        return;
      }

      if (isHidLogMessage(data)) {
        console.log(`[webhid] ${data.message}`);
        return;
      }

      if (isHidErrorMessage(data)) {
        console.warn(`[webhid] ${data.message}`);
        return;
      }
    };

    this.#workerPortListener = onMessage;
    port.addEventListener("message", onMessage);
    // When using addEventListener() MessagePorts need start() to begin dispatch.
    (port as unknown as { start?: () => void }).start?.();

    this.#maybeInitInputReportRing(port);
    this.#attachRings(port);
    this.#emit();
  }

  detachWorkerPort(port?: MessagePort | Worker): void {
    const active = this.#workerPort;
    if (!active) return;
    if (port && port !== active) return;

    this.#detachRings();

    // Best-effort notify the worker that all devices are detached.
    for (const deviceId of this.#attachedToWorker) {
      const msg: HidDetachMessage = { type: "hid.detach", deviceId };
      try {
        active.postMessage(msg);
      } catch {
        // ignore
      }
    }

    // Remove input listeners so devices are no longer forwarded to a new worker
    // without an explicit user action.
    for (const deviceId of this.#attachedToWorker) {
      void this.#unbridgeDevice(deviceId, { sendDetach: false });
    }
    this.#attachedToWorker.clear();

    if (this.#workerPortListener) {
      active.removeEventListener("message", this.#workerPortListener);
    }

    this.#workerPort = null;
    this.#workerPortListener = null;
    this.#inputReportRing = null;
    this.#emit();
  }

  #canUseSharedMemory(): boolean {
    // SharedArrayBuffer requires cross-origin isolation in browsers. Node/Vitest may still provide it,
    // but keep the check aligned with the browser contract so behaviour matches production.
    if ((globalThis as any).crossOriginIsolated !== true) return false;
    if (typeof SharedArrayBuffer === "undefined") return false;
    if (typeof Atomics === "undefined") return false;
    return true;
  }

  #attachRings(worker: MessagePort | Worker): void {
    if (this.#inputRing && this.#outputRing) return;
    if (!this.#canUseSharedMemory()) return;

    const inputSab = createHidReportRingBuffer(64 * 1024);
    const outputSab = createHidReportRingBuffer(64 * 1024);
    this.#inputRing = new HidReportRing(inputSab);
    this.#outputRing = new HidReportRing(outputSab);

    const msg: HidRingAttachMessage = { type: "hid.ringAttach", inputRing: inputSab, outputRing: outputSab };
    this.#postToWorker(worker, msg);

    // Drain output reports in the background. In Node (Vitest), `unref()` the timer so it doesn't
    // keep the test runner alive when a broker isn't explicitly destroyed.
    this.#outputRingDrainTimer = setInterval(() => this.#drainOutputRing(), 8);
    (this.#outputRingDrainTimer as unknown as { unref?: () => void }).unref?.();
  }

  #maybeInitInputReportRing(worker: MessagePort | Worker): void {
    // If the ring was explicitly configured by the caller, respect that.
    if (this.#inputReportRing) return;
    if (!this.#canUseSharedMemory()) return;

    const cap = alignUp(this.#inputReportRingCapacityBytes >>> 0, RECORD_ALIGN);
    const sab = new SharedArrayBuffer(ringCtrl.BYTES + cap);
    new Int32Array(sab, 0, ringCtrl.WORDS).set([0, 0, 0, cap]);
    this.#inputReportRing = new RingBuffer(sab, 0);
    this.#inputReportRingPushed = 0;
    this.#inputReportRingDropped = 0;
    this.#inputReportFallback = 0;

    const msg: HidRingInitMessage = { type: "hid.ring.init", sab, offsetBytes: 0 };
    this.#postToWorker(worker, msg);
  }

  #detachRings(): void {
    if (this.#outputRingDrainTimer) {
      clearInterval(this.#outputRingDrainTimer);
      this.#outputRingDrainTimer = null;
    }
    this.#inputRing = null;
    this.#outputRing = null;
  }

  #drainOutputRing(): void {
    const ring = this.#outputRing;
    if (!ring) return;

    while (true) {
      const rec = ring.pop();
      if (!rec) break;
      if (rec.reportType !== HidRingReportType.Output && rec.reportType !== HidRingReportType.Feature) continue;

      const deviceId = rec.deviceId >>> 0;
      if (!this.#attachedToWorker.has(deviceId)) continue;

      const device = this.#deviceById.get(deviceId);
      if (!device) continue;

      const data = ensureArrayBufferBacked(rec.payload);
      const promise =
        rec.reportType === HidRingReportType.Feature
          ? device.sendFeatureReport(rec.reportId, data)
          : device.sendReport(rec.reportId, data);
      void promise.catch((err) => {
        const message = err instanceof Error ? err.message : String(err);
        console.warn(
          `[webhid] Failed to send ${rec.reportType === HidRingReportType.Feature ? "feature" : "output"} reportId=${rec.reportId} deviceId=${deviceId}: ${message}`,
        );
      });
    }
  }

  getDeviceId(device: HIDDevice): number {
    const existing = this.#deviceIdByDevice.get(device);
    if (existing !== undefined) return existing;
    const id = this.#nextDeviceId++;
    this.#deviceIdByDevice.set(device, id);
    this.#deviceById.set(id, device);
    return id;
  }

  isAttachedToWorker(device: HIDDevice): boolean {
    const id = this.#deviceIdByDevice.get(device);
    if (id === undefined) return false;
    return this.#attachedToWorker.has(id);
  }

  async attachDevice(device: HIDDevice): Promise<number> {
    const worker = this.#workerPort;
    if (!worker) throw new Error("IO worker is not attached; start the VM workers first.");

    const deviceId = this.getDeviceId(device);
    if (this.#attachedToWorker.has(deviceId)) return deviceId;

    let sentAttachToWorker = false;
    try {
      await this.manager.attachKnownDevice(device);

      const guestPathHint: GuestUsbPath | undefined = this.manager
        .getState()
        .attachedDevices.find((entry) => entry.device === device)?.guestPath;
      const guestPortHint = guestPathHint?.[0];

      // The WebHID `@types/w3c-web-hid` definitions mark many collection fields as optional,
      // but real Chromium devices always populate them. `normalizeCollections` expects a
      // fully-populated shape matching the Rust contract, so cast and let the normalizer
      // throw if a browser provides incomplete metadata.
      //
      // Validate key invariants here (mixed report IDs, out-of-order isRange bounds, etc.) so we
      // fail deterministically before sending metadata to the worker.
      const collections = normalizeCollections(device.collections, { validate: true });
      const hasInterruptOut = computeHasInterruptOut(collections);

      const attachMsg: HidAttachMessage = {
        type: "hid.attach",
        deviceId,
        vendorId: device.vendorId,
        productId: device.productId,
        ...(device.productName ? { productName: device.productName } : {}),
        ...(guestPathHint ? { guestPath: guestPathHint } : {}),
        ...(guestPortHint === 0 || guestPortHint === 1 ? { guestPort: guestPortHint } : {}),
        collections,
        hasInterruptOut,
      };

      this.#postToWorker(worker, attachMsg);
      sentAttachToWorker = true;
      if (this.#workerPort !== worker) {
        // Best-effort: detach from the worker we just posted to so it doesn't retain stale state.
        try {
          worker.postMessage({ type: "hid.detach", deviceId } satisfies HidDetachMessage);
        } catch {
          // ignore
        }
        throw new Error("IO worker disconnected while attaching HID device.");
      }

      const onInputReport = (event: HIDInputReportEvent): void => {
        const activeWorker = this.#workerPort;
        if (!activeWorker) return;
        if (!this.#attachedToWorker.has(deviceId)) return;

        const view = event.data;
        if (!(view instanceof DataView)) return;
        const src = new Uint8Array(view.buffer, view.byteOffset, view.byteLength);

        const tsMs = typeof event.timeStamp === "number" ? event.timeStamp : undefined;
        this.#lastInputReportInfo.set(deviceId, { tsMs: tsMs ?? performance.now(), byteLength: src.byteLength });
        this.#scheduleEmitForInputReports();

        const ring = this.#inputReportRing;
        if (ring && this.#canUseSharedMemory()) {
          const ok = ring.tryPushWithWriter(HID_INPUT_REPORT_RECORD_HEADER_BYTES + src.byteLength, (dest) => {
            writeHidInputReportRingRecord(dest, {
              deviceId,
              reportId: event.reportId,
              tsMs,
              data: src,
            });
          });
          if (ok) {
            this.#inputReportRingPushed += 1;
            return;
          }
          // Drop rather than blocking/spinning; this is a best-effort fast path.
          this.#inputReportRingDropped += 1;
          const status = this.#status;
          if (status) {
            try {
              Atomics.add(status, StatusIndex.IoHidInputReportDropCounter, 1);
            } catch {
              // ignore (status may not be SharedArrayBuffer-backed in tests/harnesses)
            }
          }
          return;
        }

        const inputRing = this.#inputRing;
        if (inputRing) {
          inputRing.push(deviceId >>> 0, HidRingReportType.Input, event.reportId >>> 0, src);
          return;
        }

        const data = new Uint8Array(src.byteLength);
        data.set(src);
        const msg: HidInputReportMessage = {
          type: "hid.inputReport",
          deviceId,
          reportId: event.reportId,
          data,
          tsMs,
        };
        this.#inputReportFallback += 1;
        this.#postToWorker(activeWorker, msg, [data.buffer]);
      };

      device.addEventListener("inputreport", onInputReport);
      this.#inputReportListeners.set(deviceId, onInputReport);
      this.#attachedToWorker.add(deviceId);
      this.#emit();

      return deviceId;
    } catch (err) {
      if (sentAttachToWorker) {
        try {
          worker.postMessage({ type: "hid.detach", deviceId } satisfies HidDetachMessage);
        } catch {
          // ignore
        }
      }

      // Ensure we don't leak manager-side guest paths / open handles when attaching fails.
      await this.#unbridgeDevice(deviceId, { sendDetach: false }).catch(() => undefined);
      this.#attachedToWorker.delete(deviceId);
      this.#emit();
      await this.manager.detachDevice(device).catch(() => undefined);

      throw err;
    }
  }

  async detachDevice(device: HIDDevice): Promise<void> {
    const deviceId = this.#deviceIdByDevice.get(device);
    if (deviceId !== undefined) {
      await this.#unbridgeDevice(deviceId, { sendDetach: true });
      this.#attachedToWorker.delete(deviceId);
      this.#emit();
    }

    await this.manager.detachDevice(device);
  }

  async #handleManagerDeviceDetached(device: HIDDevice): Promise<void> {
    const deviceId = this.#deviceIdByDevice.get(device);
    if (deviceId === undefined) return;

    if (this.#attachedToWorker.has(deviceId)) {
      await this.#unbridgeDevice(deviceId, { sendDetach: true });
      this.#attachedToWorker.delete(deviceId);
      this.#emit();
    }
  }

  async #unbridgeDevice(deviceId: number, options: { sendDetach: boolean }): Promise<void> {
    const device = this.#deviceById.get(deviceId);
    const listener = this.#inputReportListeners.get(deviceId);
    if (device && listener) {
      try {
        device.removeEventListener("inputreport", listener);
      } catch {
        // ignore
      }
    }
    this.#inputReportListeners.delete(deviceId);
    this.#lastInputReportInfo.delete(deviceId);

    if (options.sendDetach && this.#workerPort) {
      const detachMsg: HidDetachMessage = { type: "hid.detach", deviceId };
      this.#postToWorker(this.#workerPort, detachMsg);
    }
  }

  #scheduleEmitForInputReports(): void {
    if (this.#listeners.size === 0) return;
    if (this.#inputReportEmitTimer) return;
    this.#inputReportEmitTimer = setTimeout(() => {
      this.#inputReportEmitTimer = null;
      this.#emit();
    }, 100);
    (this.#inputReportEmitTimer as unknown as { unref?: () => void }).unref?.();
  }

  async #handleSendReportRequest(msg: HidSendReportMessage): Promise<void> {
    if (!this.#attachedToWorker.has(msg.deviceId)) {
      console.warn(`[webhid] sendReport for detached deviceId=${msg.deviceId}`);
      return;
    }

    const device = this.#deviceById.get(msg.deviceId);
    if (!device) {
      console.warn(`[webhid] sendReport for unknown deviceId=${msg.deviceId}`);
      return;
    }

    try {
      const data = ensureArrayBufferBacked(msg.data);
      if (msg.reportType === "output") {
        await device.sendReport(msg.reportId, data);
      } else {
        await device.sendFeatureReport(msg.reportId, data);
      }
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      console.warn(`[webhid] Failed to send ${msg.reportType} reportId=${msg.reportId} deviceId=${msg.deviceId}: ${message}`);
    }
  }

  #postToWorker(worker: MessagePort | Worker, msg: HidProxyMessage, transfer?: Transferable[]): void {
    try {
      if (transfer) {
        worker.postMessage(msg, transfer);
      } else {
        worker.postMessage(msg);
      }
    } catch {
      // If the worker is gone, treat this as detached.
      if (this.#workerPort === worker) {
        this.detachWorkerPort(worker);
      }
    }
  }

  #emit(): void {
    const state = this.getState();
    for (const listener of this.#listeners) listener(state);
  }
}
