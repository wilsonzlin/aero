import { afterEach, describe, expect, it, vi } from "vitest";

import type { UsbHostAction, UsbHostCompletion } from "./usb_proxy_protocol";

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
    const resolvers: Array<(c: UsbHostCompletion) => void> = [];

    vi.doMock("./web_usb_backend", () => ({
      WebUsbBackend: class {
        async ensureOpenAndClaimed(): Promise<void> {
          // No-op
        }

        execute(action: UsbHostAction): Promise<UsbHostCompletion> {
          callOrder.push(action.id);
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

    const a1: UsbHostAction = { kind: "bulkIn", id: 1, ep: 1, length: 8 };
    const a2: UsbHostAction = { kind: "bulkIn", id: 2, ep: 1, length: 8 };

    const p1 = broker.execute(a1);
    const p2 = broker.execute(a2);

    await Promise.resolve();
    expect(callOrder).toEqual([1]);
    expect(resolvers).toHaveLength(1);

    const c1: UsbHostCompletion = { kind: "okIn", id: 1, data: Uint8Array.of(1) };
    resolvers[0](c1);
    await expect(p1).resolves.toEqual(c1);

    await Promise.resolve();
    expect(callOrder).toEqual([1, 2]);
    expect(resolvers).toHaveLength(2);

    const c2: UsbHostCompletion = { kind: "okIn", id: 2, data: Uint8Array.of(2) };
    resolvers[1](c2);
    await expect(p2).resolves.toEqual(c2);
  });

  it("flushes pending actions when the selected device disconnects", async () => {
    const resolvers: Array<(c: UsbHostCompletion) => void> = [];

    vi.doMock("./web_usb_backend", () => ({
      WebUsbBackend: class {
        async ensureOpenAndClaimed(): Promise<void> {}

        execute(action: UsbHostAction): Promise<UsbHostCompletion> {
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
    vi.doMock("./web_usb_backend", () => ({
      WebUsbBackend: class {
        async ensureOpenAndClaimed(): Promise<void> {}

        async execute(action: UsbHostAction): Promise<UsbHostCompletion> {
          return { kind: "okOut", id: action.id, bytesWritten: action.id * 2 };
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
      completion: UsbHostCompletion;
    }>;

    expect(completions).toEqual([
      { type: "usb.completion", completion: { kind: "okOut", id: 1, bytesWritten: 2 } },
      { type: "usb.completion", completion: { kind: "okOut", id: 2, bytesWritten: 4 } },
    ]);
  });
});

