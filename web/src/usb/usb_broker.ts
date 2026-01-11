import {
  isUsbActionMessage,
  isUsbSelectDeviceMessage,
  usbErrorCompletion,
  type UsbActionMessage,
  type UsbCompletionMessage,
  type UsbHostAction,
  type UsbHostCompletion,
  type UsbSelectDeviceMessage,
  type UsbSelectedMessage,
} from "./usb_proxy_protocol";
import {
  WebUsbBackend,
  type UsbHostAction as BackendUsbHostAction,
  type UsbHostCompletion as BackendUsbHostCompletion,
} from "./webusb_backend";
import { formatWebUsbError } from "../platform/webusb_troubleshooting";

type UsbDeviceInfo = { vendorId: number; productId: number; productName?: string };

type QueueItem = {
  action: UsbHostAction;
  resolve: (completion: UsbHostCompletion) => void;
};

function createDeferred(): { promise: Promise<void>; resolve: () => void } {
  let resolve!: () => void;
  const promise = new Promise<void>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}

function getNavigatorUsb(): USB | null {
  // Keep the access tolerant so unit tests (node environment) can stub navigator.
  const nav = (globalThis as unknown as { navigator?: unknown }).navigator as (Navigator & { usb?: USB }) | undefined;
  return nav?.usb ?? null;
}

function proxyActionToBackendAction(action: UsbHostAction): BackendUsbHostAction {
  switch (action.kind) {
    case "controlIn":
      return { kind: "controlIn", id: action.id, setup: action.setup };
    case "controlOut":
      return { kind: "controlOut", id: action.id, setup: action.setup, data: action.data };
    case "bulkIn":
      return { kind: "bulkIn", id: action.id, endpoint: action.ep, length: action.length };
    case "bulkOut":
      return { kind: "bulkOut", id: action.id, endpoint: action.ep, data: action.data };
    default: {
      const neverAction: never = action;
      throw new Error(`Unknown USB action kind: ${String((neverAction as { kind?: unknown }).kind)}`);
    }
  }
}

function backendCompletionToProxyCompletion(completion: BackendUsbHostCompletion): UsbHostCompletion {
  const id = completion.id;
  switch (completion.kind) {
    case "controlIn":
    case "bulkIn": {
      if (completion.status === "success") {
        return { kind: "okIn", id, data: completion.data };
      }
      if (completion.status === "stall") return { kind: "stall", id };
      return usbErrorCompletion(id, completion.message);
    }
    case "controlOut":
    case "bulkOut": {
      if (completion.status === "success") {
        return { kind: "okOut", id, bytesWritten: completion.bytesWritten };
      }
      if (completion.status === "stall") return { kind: "stall", id };
      return usbErrorCompletion(id, completion.message);
    }
    default: {
      const neverCompletion: never = completion;
      return usbErrorCompletion(id, `Unknown USB completion kind: ${String((neverCompletion as { kind?: unknown }).kind)}`);
    }
  }
}

export class UsbBroker {
  private device: USBDevice | null = null;
  private backend: WebUsbBackend | null = null;
  private selectedInfo: UsbDeviceInfo | null = null;

  private disconnectError: string | null = null;
  private disconnectSignal = (() => {
    const signal = createDeferred();
    signal.resolve();
    return signal;
  })();

  private readonly ports = new Set<MessagePort | Worker>();
  private readonly portListeners = new Map<MessagePort | Worker, EventListener>();

  private readonly queue: QueueItem[] = [];
  private processing = false;

  constructor() {
    getNavigatorUsb()?.addEventListener?.("disconnect", (ev: Event) => {
      const device = (ev as unknown as { device?: USBDevice }).device;
      if (!device || !this.device) return;
      if (device !== this.device) return;
      this.handleDisconnect("WebUSB device disconnected.");
    });
  }

  async requestDevice(filters?: USBDeviceFilter[]): Promise<UsbDeviceInfo> {
    const usb = getNavigatorUsb();
    if (!usb || typeof usb.requestDevice !== "function") {
      throw new Error("WebUSB is unavailable (navigator.usb.requestDevice missing).");
    }

    // `requestDevice` must be called from a user gesture; callers should invoke this from a click handler.
    const attempts: Array<USBDeviceRequestOptions> = [];
    if (filters && filters.length > 0) {
      attempts.push({ filters });
    } else {
      // Chromium versions differ on whether `filters: []` or `filters: [{}]` are accepted. Try a handful of
      // "broad" options so local smoke tests work across more builds.
      attempts.push({ filters: [] });
      attempts.push({ filters: [{}] });
      attempts.push({ filters: [{ classCode: 0x00 }, { classCode: 0xff }] });
      attempts.push({ filters: [{ classCode: 0xff }] });
    }

    let device: USBDevice | null = null;
    let lastErr: unknown = null;
    for (const opts of attempts) {
      try {
        device = await usb.requestDevice(opts);
        break;
      } catch (err) {
        lastErr = err;

        // User cancelled the chooser (or no matching devices).
        if (err instanceof DOMException && err.name === "NotFoundError") {
          throw err;
        }

        // If the browser rejected the filter shape, try the next fallback.
        if (err instanceof TypeError) continue;
        if (err instanceof DOMException && err.name === "TypeError") continue;

        throw err;
      }
    }

    if (!device) {
      throw lastErr ?? new Error("WebUSB requestDevice failed.");
    }
    const backend = new WebUsbBackend(device);

    try {
      await backend.ensureOpenAndClaimed();
    } catch (err) {
      try {
        await device.close();
      } catch {
        // Ignore close errors; failing to open/claim is the root issue.
      }
      throw err;
    }

    // Replace any previously-selected device and fail outstanding actions so they don't accidentally run on the new device.
    this.resetSelectedDevice("WebUSB device replaced.");

    this.device = device;
    this.backend = backend;
    this.selectedInfo = {
      vendorId: device.vendorId,
      productId: device.productId,
      productName: device.productName ?? undefined,
    };
    this.disconnectError = null;
    this.disconnectSignal = createDeferred();

    const msg: UsbSelectedMessage = { type: "usb.selected", ok: true, info: this.selectedInfo };
    this.broadcast(msg);
    return this.selectedInfo;
  }

  async execute(action: UsbHostAction): Promise<UsbHostCompletion> {
    if (this.disconnectError) return usbErrorCompletion(action.id, this.disconnectError);
    if (!this.backend || !this.device) return usbErrorCompletion(action.id, "WebUSB device not selected.");

    return await new Promise<UsbHostCompletion>((resolve) => {
      this.queue.push({ action, resolve });
      this.kickQueue();
    });
  }

  attachWorkerPort(port: MessagePort | Worker): void {
    const isNew = !this.ports.has(port);
    if (isNew) this.ports.add(port);

    if (isNew) {
      const onMessage: EventListener = (ev) => {
        const data = (ev as MessageEvent<unknown>).data;
        if (isUsbActionMessage(data)) {
          void this.execute(data.action).then((completion) => {
            const msg: UsbCompletionMessage = { type: "usb.completion", completion };
            this.postToPort(port, msg);
          });
          return;
        }

        if (isUsbSelectDeviceMessage(data)) {
          void this.handleSelectDevice(port, data);
        }
      };
      this.portListeners.set(port, onMessage);
      port.addEventListener("message", onMessage);

      // When using addEventListener() MessagePorts need start() to begin dispatch.
      (port as unknown as { start?: () => void }).start?.();

      // Newly attached ports should learn the current selection/disconnect state.
      if (this.selectedInfo && !this.disconnectError) {
        this.postToPort(port, { type: "usb.selected", ok: true, info: this.selectedInfo } satisfies UsbSelectedMessage);
      } else if (this.disconnectError) {
        this.postToPort(port, { type: "usb.selected", ok: false, error: this.disconnectError } satisfies UsbSelectedMessage);
      }
    }
  }

  detachWorkerPort(port: MessagePort | Worker): void {
    const listener = this.portListeners.get(port);
    if (listener) {
      this.portListeners.delete(port);
      port.removeEventListener("message", listener);
    }
    this.ports.delete(port);
  }

  private async handleSelectDevice(port: MessagePort | Worker, msg: UsbSelectDeviceMessage): Promise<void> {
    try {
      await this.requestDevice(msg.filters);
    } catch (err) {
      const message = formatWebUsbError(err);
      this.postToPort(port, { type: "usb.selected", ok: false, error: message } satisfies UsbSelectedMessage);
    }
  }

  private kickQueue(): void {
    if (this.processing) return;
    this.processing = true;
    void this.drainQueue();
  }

  private async drainQueue(): Promise<void> {
    // Chromium's WebUSB implementation does not handle concurrent transfers well for all device/OS combinations.
    // Serializing actions here also matches the upstream UHCI "action queue" contract (one in-flight action at a time).
    while (this.queue.length) {
      const item = this.queue.shift()!;

      const backend = this.backend;
      if (!backend || !this.device) {
        item.resolve(usbErrorCompletion(item.action.id, "WebUSB device not selected."));
        continue;
      }

      if (this.disconnectError) {
        item.resolve(usbErrorCompletion(item.action.id, this.disconnectError));
        continue;
      }

      const backendPromise = backend.execute(proxyActionToBackendAction(item.action)).then(backendCompletionToProxyCompletion);
      // If the device disconnects, the race below resolves and we drop the backend promise.
      // Avoid unhandled rejections if the backend rejects after disconnect.
      backendPromise.catch(() => undefined);

      let completion: UsbHostCompletion;
      try {
        completion = await Promise.race([
          backendPromise,
          this.disconnectSignal.promise.then(() =>
            usbErrorCompletion(
              item.action.id,
              this.disconnectError ? this.disconnectError : "WebUSB device disconnected.",
            ),
          ),
        ]);
      } catch (err) {
        completion = usbErrorCompletion(item.action.id, formatWebUsbError(err));
      }

      item.resolve(completion);

      if (this.disconnectError) {
        // `handleDisconnect` already flushed queued requests.
        break;
      }
    }

    this.processing = false;
  }

  private resetSelectedDevice(reason: string): void {
    const prevDevice = this.device;
    if (this.backend || this.device) {
      // Resolve in-flight actions (if any) via the disconnect signal.
      this.disconnectError = reason;
      this.disconnectSignal.resolve();
    }

    this.backend = null;
    this.device = null;
    this.selectedInfo = null;

    if (prevDevice) {
      void prevDevice.close?.().catch(() => undefined);
    }

    // Fail any queued actions immediately.
    while (this.queue.length) {
      const item = this.queue.shift()!;
      item.resolve(usbErrorCompletion(item.action.id, reason));
    }
  }

  private handleDisconnect(reason: string): void {
    this.resetSelectedDevice(reason);
    this.broadcast({ type: "usb.selected", ok: false, error: reason } satisfies UsbSelectedMessage);
  }

  private postToPort(port: MessagePort | Worker, msg: UsbCompletionMessage | UsbSelectedMessage): void {
    try {
      port.postMessage(msg);
    } catch {
      this.detachWorkerPort(port);
    }
  }

  private broadcast(msg: UsbSelectedMessage): void {
    for (const port of this.ports) {
      this.postToPort(port, msg);
    }
  }
}
