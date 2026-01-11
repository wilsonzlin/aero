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
import { WebUsbBackend } from "./web_usb_backend";

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
    const device = await usb.requestDevice({ filters: filters ?? [] });
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
      port.addEventListener("message", (ev: MessageEvent<unknown>) => {
        const data = ev.data;
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
      });

      // When using addEventListener() MessagePorts need start() to begin dispatch.
      (port as unknown as { start?: () => void }).start?.();
    }

    // Newly attached ports should learn the current selection/disconnect state.
    if (this.selectedInfo && !this.disconnectError) {
      this.postToPort(port, { type: "usb.selected", ok: true, info: this.selectedInfo } satisfies UsbSelectedMessage);
    } else if (this.disconnectError) {
      this.postToPort(port, { type: "usb.selected", ok: false, error: this.disconnectError } satisfies UsbSelectedMessage);
    }
  }

  private async handleSelectDevice(port: MessagePort | Worker, msg: UsbSelectDeviceMessage): Promise<void> {
    try {
      await this.requestDevice(msg.filters);
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
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

      const backendPromise = backend.execute(item.action);
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
        completion = usbErrorCompletion(item.action.id, err instanceof Error ? err.message : String(err));
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
    if (this.backend || this.device) {
      // Resolve in-flight actions (if any) via the disconnect signal.
      this.disconnectError = reason;
      this.disconnectSignal.resolve();
    }

    this.backend = null;
    this.device = null;
    this.selectedInfo = null;

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
      this.ports.delete(port);
    }
  }

  private broadcast(msg: UsbSelectedMessage): void {
    for (const port of this.ports) {
      this.postToPort(port, msg);
    }
  }
}
