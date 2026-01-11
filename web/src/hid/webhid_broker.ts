import { WebHidPassthroughManager } from "../platform/webhid_passthrough";
import { normalizeCollections, type HidCollectionInfo, type NormalizedHidCollectionInfo } from "./webhid_normalize";
import {
  isHidErrorMessage,
  isHidLogMessage,
  isHidSendReportMessage,
  type HidAttachMessage,
  type HidDetachMessage,
  type HidInputReportMessage,
  type HidProxyMessage,
  type HidSendReportMessage,
} from "./hid_proxy_protocol";

export type WebHidBrokerState = {
  workerAttached: boolean;
  attachedDeviceIds: number[];
};

export type WebHidBrokerListener = (state: WebHidBrokerState) => void;

function computeHasInterruptOut(collections: NormalizedHidCollectionInfo[]): boolean {
  const stack = [...collections];
  while (stack.length) {
    const node = stack.pop()!;
    if (node.outputReports.length > 0 || node.featureReports.length > 0) return true;
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

  #nextDeviceId = 1;
  readonly #deviceIdByDevice = new Map<HIDDevice, number>();
  readonly #deviceById = new Map<number, HIDDevice>();

  readonly #attachedToWorker = new Set<number>();
  readonly #inputReportListeners = new Map<number, (event: HIDInputReportEvent) => void>();

  readonly #listeners = new Set<WebHidBrokerListener>();

  #managerUnsubscribe: (() => void) | null = null;
  #prevManagerAttached = new Set<HIDDevice>();

  constructor(options: { manager?: WebHidPassthroughManager } = {}) {
    this.manager = options.manager ?? new WebHidPassthroughManager();

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

    this.#emit();
  }

  detachWorkerPort(port?: MessagePort | Worker): void {
    const active = this.#workerPort;
    if (!active) return;
    if (port && port !== active) return;

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
    this.#emit();
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

    await this.manager.attachKnownDevice(device);

    const collections = normalizeCollections(device.collections as unknown as readonly HidCollectionInfo[]);
    const hasInterruptOut = computeHasInterruptOut(collections);

    const attachMsg: HidAttachMessage = {
      type: "hid.attach",
      deviceId,
      vendorId: device.vendorId,
      productId: device.productId,
      ...(device.productName ? { productName: device.productName } : {}),
      collections,
      hasInterruptOut,
    };

    this.#postToWorker(worker, attachMsg);
    if (this.#workerPort !== worker) {
      throw new Error("IO worker disconnected while attaching HID device.");
    }

    const onInputReport = (event: HIDInputReportEvent): void => {
      const activeWorker = this.#workerPort;
      if (!activeWorker) return;
      if (!this.#attachedToWorker.has(deviceId)) return;

      const view = event.data;
      if (!(view instanceof DataView)) return;
      const buffer = view.buffer.slice(view.byteOffset, view.byteOffset + view.byteLength);
      const data = new Uint8Array(buffer);

      const msg: HidInputReportMessage = {
        type: "hid.inputReport",
        deviceId,
        reportId: event.reportId,
        data,
        tsMs: typeof event.timeStamp === "number" ? event.timeStamp : undefined,
      };
      this.#postToWorker(activeWorker, msg, [data.buffer]);
    };

    device.addEventListener("inputreport", onInputReport);
    this.#inputReportListeners.set(deviceId, onInputReport);
    this.#attachedToWorker.add(deviceId);
    this.#emit();

    return deviceId;
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

    if (options.sendDetach && this.#workerPort) {
      const detachMsg: HidDetachMessage = { type: "hid.detach", deviceId };
      this.#postToWorker(this.#workerPort, detachMsg);
    }
  }

  async #handleSendReportRequest(msg: HidSendReportMessage): Promise<void> {
    const device = this.#deviceById.get(msg.deviceId);
    if (!device) {
      console.warn(`[webhid] sendReport for unknown deviceId=${msg.deviceId}`);
      return;
    }

    try {
      if (msg.reportType === "output") {
        await device.sendReport(msg.reportId, ensureArrayBufferBacked(msg.data));
      } else {
        await device.sendFeatureReport(msg.reportId, ensureArrayBufferBacked(msg.data));
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
