import { WebUsbBackend, type UsbHostAction, type UsbHostCompletion } from "./webusb_backend";

function formatThrownError(err: unknown): string {
  if (err instanceof Error) return err.message;
  return String(err);
}

function errorCompletion(action: UsbHostAction, message: string): UsbHostCompletion {
  switch (action.kind) {
    case "controlIn":
      return { kind: "controlIn", id: action.id, status: "error", message };
    case "controlOut":
      return { kind: "controlOut", id: action.id, status: "error", message };
    case "bulkIn":
      return { kind: "bulkIn", id: action.id, status: "error", message };
    case "bulkOut":
      return { kind: "bulkOut", id: action.id, status: "error", message };
  }
}

/**
 * Main-thread WebUSB executor for the Rust/WASM `UsbHostAction` queue.
 *
 * The worker/WASM side drains `UsbHostAction[]`, sends them to the main thread,
 * and receives `UsbHostCompletion` objects back once WebUSB Promises resolve.
 */
export class WebUsbExecutor {
  readonly #backend: WebUsbBackend;

  constructor(device: USBDevice) {
    this.#backend = new WebUsbBackend(device);
  }

  async execute(action: UsbHostAction): Promise<UsbHostCompletion> {
    try {
      return await this.#backend.execute(action);
    } catch (err) {
      return errorCompletion(action, formatThrownError(err));
    }
  }

  async executeAll(actions: UsbHostAction[]): Promise<UsbHostCompletion[]> {
    return await Promise.all(actions.map((action) => this.execute(action)));
  }
}

