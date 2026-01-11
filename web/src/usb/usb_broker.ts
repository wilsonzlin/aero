import {
  getTransferablesForUsbCompletionMessage,
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
import { WebUsbBackend } from "./webusb_backend";
import { formatWebUsbError } from "../platform/webusb_troubleshooting";

type UsbDeviceInfo = { vendorId: number; productId: number; productName?: string };

type QueueItem = {
  action: UsbHostAction;
  resolve: (completion: UsbHostCompletion) => void;
};

type UsbForgettableDevice = USBDevice & { forget: () => Promise<void> };

function canForgetUsbDevice(device: USBDevice): device is UsbForgettableDevice {
  // `USBDevice.forget()` is currently Chromium-specific. Keep this check tolerant
  // so the broker continues to work on browsers without the API.
  return typeof (device as unknown as { forget?: unknown }).forget === "function";
}

function createDeferred<T>(): { promise: Promise<T>; resolve: (value: T) => void } {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}

function wrapWithCause(message: string, cause: unknown): Error {
  const error = new Error(message);
  // Not all runtimes support the `ErrorOptions` constructor parameter, but
  // attaching `cause` is still useful for debugging and for our WebUSB
  // troubleshooting helper, which can walk `Error.cause` chains.
  try {
    (error as Error & { cause?: unknown }).cause = cause;
  } catch {
    // ignore
  }
  return error;
}

function getNavigatorUsb(): USB | null {
  // Keep the access tolerant so unit tests (node environment) can stub navigator.
  const nav = (globalThis as unknown as { navigator?: unknown }).navigator as (Navigator & { usb?: USB }) | undefined;
  return nav?.usb ?? null;
}

export class UsbBroker {
  private device: USBDevice | null = null;
  private backend: WebUsbBackend | null = null;
  private selectedInfo: UsbDeviceInfo | null = null;

  private disconnectError: string | null = null;
  private disconnectSignal = createDeferred<string>();

  private readonly ports = new Set<MessagePort | Worker>();
  private readonly portListeners = new Map<MessagePort | Worker, EventListener>();
  private readonly deviceChangeListeners = new Set<() => void>();

  private readonly queue: QueueItem[] = [];
  private processing = false;

  constructor() {
    const usb = getNavigatorUsb();
    usb?.addEventListener?.("disconnect", (ev: Event) => {
      const device = (ev as unknown as { device?: USBDevice }).device;
      if (device && this.device && device === this.device) {
        this.handleDisconnect("WebUSB device disconnected.");
      }
      this.emitDeviceChange();
    });

    usb?.addEventListener?.("connect", () => {
      this.emitDeviceChange();
    });
  }

  subscribeToDeviceChanges(listener: () => void): () => void {
    this.deviceChangeListeners.add(listener);
    return () => {
      this.deviceChangeListeners.delete(listener);
    };
  }

  async getKnownDevices(): Promise<USBDevice[]> {
    const usb = getNavigatorUsb();
    if (!usb || typeof usb.getDevices !== "function") {
      throw new Error("WebUSB is unavailable (navigator.usb.getDevices missing).");
    }

    try {
      return await usb.getDevices();
    } catch (err) {
      const wrapped = new Error("navigator.usb.getDevices() failed.");
      try {
        (wrapped as Error & { cause?: unknown }).cause = err;
      } catch {
        // ignore
      }
      throw wrapped;
    }
  }

  async attachKnownDevice(device: USBDevice): Promise<UsbDeviceInfo> {
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

  async detachSelectedDevice(reason = "WebUSB device detached."): Promise<void> {
    if (!this.device && !this.backend && !this.selectedInfo) return;
    this.resetSelectedDevice(reason);
    this.broadcast({ type: "usb.selected", ok: false, error: reason } satisfies UsbSelectedMessage);
  }

  async getPermittedDevices(): Promise<USBDevice[]> {
    const usb = getNavigatorUsb();
    if (!usb || typeof usb.getDevices !== "function") {
      throw new Error("WebUSB is unavailable (navigator.usb.getDevices missing).");
    }

    try {
      return await usb.getDevices();
    } catch (err) {
      throw wrapWithCause("Failed to list permitted WebUSB devices.", err);
    }
  }

  async attachPermittedDevice(device: USBDevice): Promise<UsbDeviceInfo> {
    if (!device) {
      throw new Error("WebUSB device not provided.");
    }
    const usb = getNavigatorUsb();
    if (!usb) {
      throw new Error("WebUSB is unavailable (navigator.usb missing).");
    }
    return await this.adoptDevice(device);
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
    return await this.adoptDevice(device);
  }

  canForgetSelectedDevice(): boolean {
    return this.device !== null && canForgetUsbDevice(this.device);
  }

  async forgetSelectedDevice(): Promise<void> {
    const device = this.device;
    if (!device) {
      throw new Error("WebUSB device not selected.");
    }
    if (!canForgetUsbDevice(device)) {
      throw new Error("USBDevice.forget() is unavailable in this browser.");
    }

    // Prevent any further actions from racing with forget().
    this.resetSelectedDevice("WebUSB device forgotten.");

    // WebHID/WebUSB forget flows are more reliable when the device is closed first.
    try {
      await device.close();
    } catch {
      // Best-effort; proceed to forget().
    }

    await device.forget();

    // Return to the initial "no device selected" state (no sticky disconnect error).
    this.disconnectError = null;
    this.disconnectSignal = createDeferred<string>();

    this.broadcast({ type: "usb.selected", ok: false });
    this.emitDeviceChange();
  }

  async execute(action: UsbHostAction): Promise<UsbHostCompletion> {
    if (this.disconnectError) return usbErrorCompletion(action.kind, action.id, this.disconnectError);
    if (!this.backend || !this.device) return usbErrorCompletion(action.kind, action.id, "WebUSB device not selected.");

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

  private async adoptDevice(device: USBDevice): Promise<UsbDeviceInfo> {
    if (device === this.device && this.backend && !this.disconnectError && this.selectedInfo) {
      return this.selectedInfo;
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

      if (this.disconnectError) {
        item.resolve(usbErrorCompletion(item.action.kind, item.action.id, this.disconnectError));
        continue;
      }

      const backend = this.backend;
      if (!backend || !this.device) {
        item.resolve(usbErrorCompletion(item.action.kind, item.action.id, "WebUSB device not selected."));
        continue;
      }

      const backendPromise = backend.execute(item.action);
      // If the device disconnects, the race below resolves and we drop the backend promise.
      // Avoid unhandled rejections if the backend rejects after disconnect.
      backendPromise.catch(() => undefined);

      let completion: UsbHostCompletion;
      try {
        completion = await Promise.race([
          backendPromise,
          this.disconnectSignal.promise.then((reason) => usbErrorCompletion(item.action.kind, item.action.id, reason)),
        ]);
      } catch (err) {
        completion = usbErrorCompletion(item.action.kind, item.action.id, formatWebUsbError(err));
      }

      item.resolve(completion);

      if (this.disconnectError) {
        // `handleDisconnect` already flushed queued requests.
        break;
      }
    }

    this.processing = false;
  }

  private resetSelectedDevice(reason: string, options: { closeDevice?: boolean } = {}): void {
    const prevDevice = this.device;
    if (this.backend || this.device) {
      // Resolve in-flight actions (if any) via the disconnect signal.
      this.disconnectError = reason;
      this.disconnectSignal.resolve(reason);
    }

    this.backend = null;
    this.device = null;
    this.selectedInfo = null;

    const shouldCloseDevice = options.closeDevice !== false;
    if (prevDevice && shouldCloseDevice) {
      void prevDevice.close?.().catch(() => undefined);
    }

    // Fail any queued actions immediately.
    while (this.queue.length) {
      const item = this.queue.shift()!;
      item.resolve(usbErrorCompletion(item.action.kind, item.action.id, reason));
    }
  }

  private handleDisconnect(reason: string): void {
    this.resetSelectedDevice(reason);
    this.broadcast({ type: "usb.selected", ok: false, error: reason } satisfies UsbSelectedMessage);
  }

  private postToPort(port: MessagePort | Worker, msg: UsbCompletionMessage | UsbSelectedMessage): void {
    const transfer = msg.type === "usb.completion" ? getTransferablesForUsbCompletionMessage(msg) : undefined;
    if (transfer) {
      try {
        port.postMessage(msg, transfer);
        return;
      } catch {
        // Some ArrayBuffers (e.g. WebAssembly.Memory.buffer) cannot be transferred.
        // Fall back to a regular structured clone (copy) before treating the port as dead.
        try {
          port.postMessage(msg);
          return;
        } catch {
          this.detachWorkerPort(port);
          return;
        }
      }
    }

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

  private emitDeviceChange(): void {
    for (const listener of this.deviceChangeListeners) {
      try {
        listener();
      } catch (err) {
        console.warn("UsbBroker device-change listener failed", err);
      }
    }
  }
}
