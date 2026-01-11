import { afterEach, describe, expect, it, vi } from "vitest";

import type { UsbHostAction as ProxyUsbHostAction, UsbHostCompletion as ProxyUsbHostCompletion } from "./usb_proxy_protocol";
import type { UsbHostCompletion as BackendUsbHostCompletion } from "./webusb_backend";

const originalNavigatorDescriptor = Object.getOwnPropertyDescriptor(globalThis, "navigator");

function stubNavigatorUsb(usb: unknown): void {
  Object.defineProperty(globalThis, "navigator", {
    value: { usb },
    configurable: true,
    enumerable: true,
    writable: true,
  });
}

function restoreNavigator(): void {
  if (originalNavigatorDescriptor) {
    Object.defineProperty(globalThis, "navigator", originalNavigatorDescriptor);
  } else {
    Reflect.deleteProperty(globalThis as unknown as { navigator?: unknown }, "navigator");
  }
}

class FakeUsb extends EventTarget {
  constructor(private readonly device: USBDevice) {
    super();
  }

  async requestDevice(): Promise<USBDevice> {
    return this.device;
  }

  dispatchDisconnect(device: USBDevice): void {
    const ev = new Event("disconnect");
    (ev as unknown as { device: USBDevice }).device = device;
    this.dispatchEvent(ev);
  }
}

class FakePort {
  readonly posted: unknown[] = [];
  private readonly listeners: Array<(ev: MessageEvent<unknown>) => void> = [];

  addEventListener(type: string, listener: (ev: MessageEvent<unknown>) => void): void {
    if (type !== "message") return;
    this.listeners.push(listener);
  }

  start(): void {
    // No-op; browsers require MessagePort.start() when using addEventListener.
  }

  postMessage(msg: unknown): void {
    this.posted.push(msg);
  }

  emit(msg: unknown): void {
    const ev = { data: msg } as MessageEvent<unknown>;
    for (const listener of this.listeners) {
      listener(ev);
    }
  }
}

afterEach(() => {
  restoreNavigator();
  vi.resetModules();
  vi.clearAllMocks();
});

describe("usb/UsbBroker", () => {
  it("serializes actions via FIFO queue", async () => {
    const callOrder: number[] = [];
    const resolvers: Array<(c: BackendUsbHostCompletion) => void> = [];

    vi.doMock("./webusb_backend", () => ({
      WebUsbBackend: class {
        async ensureOpenAndClaimed(): Promise<void> {
          // No-op
        }

        execute(action: { id: number }): Promise<BackendUsbHostCompletion> {
          callOrder.push(action.id >>> 0);
          return new Promise((resolve) => resolvers.push(resolve));
        }
      },
    }));

    const device = { vendorId: 0x1234, productId: 0x5678, productName: "Demo", close: async () => {} } as unknown as USBDevice;
    const usb = new FakeUsb(device);
    stubNavigatorUsb(usb);

    const { UsbBroker } = await import("./usb_broker");

    const broker = new UsbBroker();
    await broker.requestDevice();

    const a1: ProxyUsbHostAction = { kind: "bulkIn", id: 1, ep: 1, length: 8 };
    const a2: ProxyUsbHostAction = { kind: "bulkIn", id: 2, ep: 1, length: 8 };

    const p1 = broker.execute(a1);
    const p2 = broker.execute(a2);

    await Promise.resolve();
    expect(callOrder).toEqual([1]);
    expect(resolvers).toHaveLength(1);

    const c1: BackendUsbHostCompletion = { kind: "bulkIn", id: 1, status: "success", data: Uint8Array.of(1) };
    resolvers[0](c1);
    await expect(p1).resolves.toEqual({ kind: "okIn", id: 1, data: Uint8Array.of(1) } satisfies ProxyUsbHostCompletion);

    await Promise.resolve();
    expect(callOrder).toEqual([1, 2]);
    expect(resolvers).toHaveLength(2);

    const c2: BackendUsbHostCompletion = { kind: "bulkIn", id: 2, status: "success", data: Uint8Array.of(2) };
    resolvers[1](c2);
    await expect(p2).resolves.toEqual({ kind: "okIn", id: 2, data: Uint8Array.of(2) } satisfies ProxyUsbHostCompletion);
  });

  it("flushes pending actions when the selected device disconnects", async () => {
    const resolvers: Array<(c: BackendUsbHostCompletion) => void> = [];

    vi.doMock("./webusb_backend", () => ({
      WebUsbBackend: class {
        async ensureOpenAndClaimed(): Promise<void> {}

        execute(_action: unknown): Promise<BackendUsbHostCompletion> {
          return new Promise((resolve) => resolvers.push(resolve));
        }
      },
    }));

    const device = { vendorId: 1, productId: 2, close: async () => {} } as unknown as USBDevice;
    const usb = new FakeUsb(device);
    stubNavigatorUsb(usb);

    const { UsbBroker } = await import("./usb_broker");

    const broker = new UsbBroker();
    await broker.requestDevice();

    const p1 = broker.execute({ kind: "bulkIn", id: 1, ep: 1, length: 8 });
    const p2 = broker.execute({ kind: "bulkIn", id: 2, ep: 1, length: 8 });

    await Promise.resolve();
    expect(resolvers).toHaveLength(1);

    usb.dispatchDisconnect(device);

    await expect(p1).resolves.toEqual({ kind: "error", id: 1, error: "WebUSB device disconnected." });
    await expect(p2).resolves.toEqual({ kind: "error", id: 2, error: "WebUSB device disconnected." });
  });

  it("routes usb.action requests to usb.completion responses", async () => {
    vi.doMock("./webusb_backend", () => ({
      WebUsbBackend: class {
        async ensureOpenAndClaimed(): Promise<void> {}

        async execute(action: { id: number }): Promise<BackendUsbHostCompletion> {
          return { kind: "bulkOut", id: action.id, status: "success", bytesWritten: action.id * 2 };
        }
      },
    }));

    const device = { vendorId: 1, productId: 2, close: async () => {} } as unknown as USBDevice;
    const usb = new FakeUsb(device);
    stubNavigatorUsb(usb);

    const { UsbBroker } = await import("./usb_broker");

    const broker = new UsbBroker();
    await broker.requestDevice();

    const port = new FakePort();
    broker.attachWorkerPort(port as unknown as MessagePort);

    // Ignore the initial usb.selected broadcast during attachment.
    port.posted.length = 0;

    port.emit({ type: "usb.action", action: { kind: "bulkOut", id: 1, ep: 1, data: Uint8Array.of(1) } });
    port.emit({ type: "usb.action", action: { kind: "bulkOut", id: 2, ep: 1, data: Uint8Array.of(2) } });

    await new Promise((r) => setTimeout(r, 0));

    const completions = port.posted.filter((m) => (m as { type?: unknown }).type === "usb.completion") as Array<{
      type: string;
      completion: ProxyUsbHostCompletion;
    }>;

    expect(completions).toEqual([
      { type: "usb.completion", completion: { kind: "okOut", id: 1, bytesWritten: 2 } },
      { type: "usb.completion", completion: { kind: "okOut", id: 2, bytesWritten: 4 } },
    ]);
  });

  it("does not resend usb.selected when attaching the same port twice", async () => {
    vi.doMock("./webusb_backend", () => ({
      WebUsbBackend: class {
        async ensureOpenAndClaimed(): Promise<void> {}

        async execute(): Promise<BackendUsbHostCompletion> {
          throw new Error("not used");
        }
      },
    }));

    const device = { vendorId: 1, productId: 2, close: async () => {} } as unknown as USBDevice;
    const usb = new FakeUsb(device);
    stubNavigatorUsb(usb);

    const { UsbBroker } = await import("./usb_broker");

    const broker = new UsbBroker();
    await broker.requestDevice();

    const port = new FakePort();
    broker.attachWorkerPort(port as unknown as MessagePort);
    broker.attachWorkerPort(port as unknown as MessagePort);

    const selected = port.posted.filter((m) => (m as { type?: unknown }).type === "usb.selected");
    expect(selected).toHaveLength(1);
  });

  it("falls back to alternate requestDevice filter shapes when the browser rejects filters: []", async () => {
    vi.doMock("./webusb_backend", () => ({
      WebUsbBackend: class {
        async ensureOpenAndClaimed(): Promise<void> {}

        async execute(): Promise<BackendUsbHostCompletion> {
          throw new Error("not used");
        }
      },
    }));

    const device = { vendorId: 0x1234, productId: 0x5678, close: async () => {} } as unknown as USBDevice;
    const calls: USBDeviceRequestOptions[] = [];
    const usb = new (class extends EventTarget {
      async requestDevice(options: USBDeviceRequestOptions): Promise<USBDevice> {
        calls.push(options);
        if (calls.length === 1) throw new TypeError("filters rejected");
        return device;
      }
    })();
    stubNavigatorUsb(usb);

    const { UsbBroker } = await import("./usb_broker");
    const broker = new UsbBroker();

    const info = await broker.requestDevice();
    expect(info.vendorId).toBe(0x1234);
    expect(info.productId).toBe(0x5678);

    expect(calls).toHaveLength(2);
    expect(calls[0]?.filters).toEqual([]);
    expect(Array.isArray(calls[1]?.filters)).toBe(true);
    expect(calls[1]?.filters?.length).toBe(1);
  });
});
