import { afterEach, describe, expect, it, vi } from "vitest";

import type { UsbHostAction as ProxyUsbHostAction, UsbHostCompletion as ProxyUsbHostCompletion } from "./usb_proxy_protocol";
import { WEBUSB_GUEST_ROOT_PORT } from "./uhci_external_hub";
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
  private readonly device: USBDevice;

  constructor(device: USBDevice) {
    super();
    this.device = device;
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

class FakeUsbSequence extends EventTarget {
  private nextIndex = 0;
  private readonly devices: USBDevice[];

  constructor(devices: USBDevice[]) {
    super();
    this.devices = devices;
  }

  async requestDevice(): Promise<USBDevice> {
    const dev = this.devices[this.nextIndex];
    if (!dev) throw new Error("FakeUsbSequence exhausted");
    this.nextIndex += 1;
    return dev;
  }
}

class FakePort {
  readonly posted: unknown[] = [];
  readonly transfers: Array<Transferable[] | undefined> = [];
  private readonly listeners: Array<(ev: MessageEvent<unknown>) => void> = [];

  addEventListener(type: string, listener: (ev: MessageEvent<unknown>) => void): void {
    if (type !== "message") return;
    this.listeners.push(listener);
  }

  removeEventListener(type: string, listener: (ev: MessageEvent<unknown>) => void): void {
    if (type !== "message") return;
    const idx = this.listeners.indexOf(listener);
    if (idx >= 0) this.listeners.splice(idx, 1);
  }

  start(): void {
    // No-op; browsers require MessagePort.start() when using addEventListener.
  }

  postMessage(msg: unknown, transfer?: Transferable[]): void {
    this.posted.push(msg);
    this.transfers.push(transfer);
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
  it("getKnownDevices() returns navigator.usb.getDevices()", async () => {
    const deviceA = { vendorId: 0x1234, productId: 0x5678 } as unknown as USBDevice;
    const deviceB = { vendorId: 0xabcd, productId: 0x0001 } as unknown as USBDevice;

    const getDevices = vi.fn(async () => [deviceA, deviceB]);
    stubNavigatorUsb({ getDevices });

    const { UsbBroker } = await import("./usb_broker");
    const broker = new UsbBroker();

    await expect(broker.getKnownDevices()).resolves.toEqual([deviceA, deviceB]);
    expect(getDevices).toHaveBeenCalledTimes(1);
  }, 15000);

  it("attachKnownDevice() opens/claims via WebUsbBackend and does not call requestDevice()", async () => {
    const ensureOpenAndClaimed = vi.fn(async () => {});

    vi.doMock("./webusb_backend", () => ({
      WebUsbBackend: class {
        async ensureOpenAndClaimed(): Promise<void> {
          await ensureOpenAndClaimed();
        }

        async execute(): Promise<BackendUsbHostCompletion> {
          throw new Error("not used");
        }
      },
    }));

    const device = {
      vendorId: 0x1234,
      productId: 0x5678,
      productName: "Demo",
      close: vi.fn(async () => {}),
    } as unknown as USBDevice;

    const requestDevice = vi.fn(async () => device);
    stubNavigatorUsb({ requestDevice });

    const { UsbBroker } = await import("./usb_broker");
    const broker = new UsbBroker();

    await broker.attachKnownDevice(device);

    expect(ensureOpenAndClaimed).toHaveBeenCalledTimes(1);
    expect(requestDevice).toHaveBeenCalledTimes(0);
  });

  it("attachKnownDevice() broadcasts usb.selected to attached ports", async () => {
    vi.doMock("./webusb_backend", () => ({
      WebUsbBackend: class {
        async ensureOpenAndClaimed(): Promise<void> {}

        async execute(): Promise<BackendUsbHostCompletion> {
          throw new Error("not used");
        }
      },
    }));

    const device = {
      vendorId: 0x1234,
      productId: 0x5678,
      productName: "Demo",
      close: async () => {},
    } as unknown as USBDevice;
    stubNavigatorUsb({ requestDevice: vi.fn(async () => device) });

    const { UsbBroker } = await import("./usb_broker");
    const broker = new UsbBroker();

    const port = new FakePort();
    broker.attachWorkerPort(port as unknown as MessagePort);

    await broker.attachKnownDevice(device);

    const selected = port.posted.filter((m) => (m as { type?: unknown }).type === "usb.selected");
    expect(selected).toEqual([
      { type: "usb.selected", ok: true, info: { vendorId: 0x1234, productId: 0x5678, productName: "Demo" } },
    ]);
  }, 15000);

  it("responds to usb.querySelected with the current usb.selected state", async () => {
    vi.doMock("./webusb_backend", () => ({
      WebUsbBackend: class {
        async ensureOpenAndClaimed(): Promise<void> {}

        async execute(): Promise<BackendUsbHostCompletion> {
          throw new Error("not used");
        }
      },
    }));

    const { UsbBroker } = await import("./usb_broker");
    const broker = new UsbBroker();

    const port = new FakePort();
    broker.attachWorkerPort(port as unknown as MessagePort);

    // Ignore the initial controller-mode broadcast during attachment.
    port.posted.length = 0;

    port.emit({ type: "usb.querySelected" });
    expect(port.posted).toEqual([{ type: "usb.selected", ok: false }]);

    const device = {
      vendorId: 0x1234,
      productId: 0x5678,
      productName: "Demo",
      close: async () => {},
    } as unknown as USBDevice;

    await broker.attachKnownDevice(device);

    port.emit({ type: "usb.querySelected" });
    expect(port.posted.at(-1)).toEqual({
      type: "usb.selected",
      ok: true,
      info: { vendorId: 0x1234, productId: 0x5678, productName: "Demo" },
    });
  });

  it("broadcasts usb.guest.status to attached ports and replays the latest snapshot to newly attached ports", async () => {
    const { UsbBroker } = await import("./usb_broker");
    const broker = new UsbBroker();

    const portA = new FakePort();
    const portB = new FakePort();
    broker.attachWorkerPort(portA as unknown as MessagePort);
    broker.attachWorkerPort(portB as unknown as MessagePort);

    portA.posted.length = 0;
    portB.posted.length = 0;

    const snapshot = { available: true, attached: true, blocked: false, rootPort: WEBUSB_GUEST_ROOT_PORT, lastError: null };
    portA.emit({ type: "usb.guest.status", snapshot });

    expect(portA.posted).toContainEqual({ type: "usb.guest.status", snapshot });
    expect(portB.posted).toContainEqual({ type: "usb.guest.status", snapshot });

    const portC = new FakePort();
    broker.attachWorkerPort(portC as unknown as MessagePort);
    const guestMsgs = portC.posted.filter((m) => (m as { type?: unknown }).type === "usb.guest.status");
    expect(guestMsgs).toEqual([{ type: "usb.guest.status", snapshot }]);
  });

  it("detaches child ports attached via usb.broker.attachPort when the parent port is detached", async () => {
    vi.doMock("./webusb_backend", () => ({
      WebUsbBackend: class {
        async ensureOpenAndClaimed(): Promise<void> {}

        async execute(): Promise<BackendUsbHostCompletion> {
          throw new Error("not used");
        }
      },
    }));

    const { UsbBroker } = await import("./usb_broker");
    const broker = new UsbBroker();

    const parent = new FakePort();
    const child = new FakePort();
    const other = new FakePort();

    broker.attachWorkerPort(parent as unknown as MessagePort);
    broker.attachWorkerPort(other as unknown as MessagePort);

    // Ask the broker to attach the child port on behalf of the parent.
    parent.emit({ type: "usb.broker.attachPort", port: child, attachRings: false });

    const device = {
      vendorId: 0x1234,
      productId: 0x5678,
      productName: "Demo",
      close: vi.fn(async () => {}),
    } as unknown as USBDevice;
    stubNavigatorUsb({ requestDevice: vi.fn(async () => device) });

    await broker.attachKnownDevice(device);

    expect(child.posted).toContainEqual({
      type: "usb.selected",
      ok: true,
      info: { vendorId: 0x1234, productId: 0x5678, productName: "Demo" },
    });

    // Detach the parent: the broker should also detach the child port so it does not leak.
    broker.detachWorkerPort(parent as unknown as MessagePort);

    child.posted.length = 0;
    other.posted.length = 0;

    await broker.detachSelectedDevice("bye");

    expect(other.posted).toContainEqual({ type: "usb.selected", ok: false, error: "bye" });
    expect(child.posted).toEqual([]);
  });

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

    const a1: ProxyUsbHostAction = { kind: "bulkIn", id: 1, endpoint: 0x81, length: 8 };
    const a2: ProxyUsbHostAction = { kind: "bulkIn", id: 2, endpoint: 0x81, length: 8 };

    const p1 = broker.execute(a1);
    const p2 = broker.execute(a2);

    await Promise.resolve();
    expect(callOrder).toEqual([1]);
    expect(resolvers).toHaveLength(1);

    const c1: BackendUsbHostCompletion = { kind: "bulkIn", id: 1, status: "success", data: Uint8Array.of(1) };
    resolvers[0](c1);
    await expect(p1).resolves.toEqual(c1 satisfies ProxyUsbHostCompletion);

    await Promise.resolve();
    expect(callOrder).toEqual([1, 2]);
    expect(resolvers).toHaveLength(2);

    const c2: BackendUsbHostCompletion = { kind: "bulkIn", id: 2, status: "success", data: Uint8Array.of(2) };
    resolvers[1](c2);
    await expect(p2).resolves.toEqual(c2 satisfies ProxyUsbHostCompletion);
  }, 15000);

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

    const p1 = broker.execute({ kind: "bulkIn", id: 1, endpoint: 0x81, length: 8 });
    const p2 = broker.execute({ kind: "bulkIn", id: 2, endpoint: 0x81, length: 8 });

    await Promise.resolve();
    expect(resolvers).toHaveLength(1);

    usb.dispatchDisconnect(device);

    await expect(p1).resolves.toEqual({ kind: "bulkIn", id: 1, status: "error", message: "WebUSB device disconnected." });
    await expect(p2).resolves.toEqual({ kind: "bulkIn", id: 2, status: "error", message: "WebUSB device disconnected." });
  }, 15000);

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
    port.transfers.length = 0;

    port.emit({ type: "usb.action", action: { kind: "bulkOut", id: 1, endpoint: 1, data: Uint8Array.of(1) } });
    port.emit({ type: "usb.action", action: { kind: "bulkOut", id: 2, endpoint: 1, data: Uint8Array.of(2) } });

    await new Promise((r) => setTimeout(r, 0));

    const completions = port.posted.filter((m) => (m as { type?: unknown }).type === "usb.completion") as Array<{
      type: string;
      completion: ProxyUsbHostCompletion;
    }>;

    expect(completions).toEqual([
      { type: "usb.completion", completion: { kind: "bulkOut", id: 1, status: "success", bytesWritten: 2 } },
      { type: "usb.completion", completion: { kind: "bulkOut", id: 2, status: "success", bytesWritten: 4 } },
    ]);
  });

  it("transfers completion payload buffers when posting usb.completion messages", async () => {
    vi.doMock("./webusb_backend", () => ({
      WebUsbBackend: class {
        async ensureOpenAndClaimed(): Promise<void> {}

        async execute(action: { id: number }): Promise<BackendUsbHostCompletion> {
          return { kind: "bulkIn", id: action.id, status: "success", data: Uint8Array.of(action.id) };
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
    port.posted.length = 0;
    port.transfers.length = 0;

    port.emit({ type: "usb.action", action: { kind: "bulkIn", id: 1, endpoint: 0x81, length: 1 } });

    await new Promise((r) => setTimeout(r, 0));

    const idx = port.posted.findIndex((m) => (m as { type?: unknown }).type === "usb.completion");
    expect(idx).toBeGreaterThanOrEqual(0);

    const msg = port.posted[idx] as { type: string; completion: ProxyUsbHostCompletion };
    expect(msg.type).toBe("usb.completion");
    expect(msg.completion).toEqual({ kind: "bulkIn", id: 1, status: "success", data: Uint8Array.of(1) });

    if (msg.completion.kind !== "bulkIn" || msg.completion.status !== "success") throw new Error("unreachable");
    expect(port.transfers[idx]).toEqual([msg.completion.data.buffer]);
  });

  it("falls back to non-transfer postMessage when completion payload buffers cannot be transferred", async () => {
    vi.doMock("./webusb_backend", () => ({
      WebUsbBackend: class {
        async ensureOpenAndClaimed(): Promise<void> {}

        async execute(action: { id: number }): Promise<BackendUsbHostCompletion> {
          return { kind: "bulkIn", id: action.id, status: "success", data: Uint8Array.of(action.id) };
        }
      },
    }));

    const device = { vendorId: 1, productId: 2, close: async () => {} } as unknown as USBDevice;
    const usb = new FakeUsb(device);
    stubNavigatorUsb(usb);

    const { UsbBroker } = await import("./usb_broker");
    const broker = new UsbBroker();
    await broker.requestDevice();

    class ThrowOnTransferPort extends FakePort {
      override postMessage(msg: unknown, transfer?: Transferable[]): void {
        if (transfer && transfer.length > 0) throw new Error("transfer not supported");
        super.postMessage(msg, transfer);
      }
    }

    const port = new ThrowOnTransferPort();
    broker.attachWorkerPort(port as unknown as MessagePort);
    port.posted.length = 0;
    port.transfers.length = 0;

    port.emit({ type: "usb.action", action: { kind: "bulkIn", id: 1, endpoint: 0x81, length: 1 } });
    port.emit({ type: "usb.action", action: { kind: "bulkIn", id: 2, endpoint: 0x81, length: 1 } });

    await new Promise((r) => setTimeout(r, 0));

    const completions = port.posted.filter((m) => (m as { type?: unknown }).type === "usb.completion") as Array<{
      type: string;
      completion: ProxyUsbHostCompletion;
    }>;
    expect(completions).toEqual([
      { type: "usb.completion", completion: { kind: "bulkIn", id: 1, status: "success", data: Uint8Array.of(1) } },
      { type: "usb.completion", completion: { kind: "bulkIn", id: 2, status: "success", data: Uint8Array.of(2) } },
    ]);

    // Both completion buffers attempted to transfer (and failed) but the broker should
    // have retried without transferables.
    const completionIndices = port.posted
      .map((m, i) => ({ m, i }))
      .filter(({ m }) => (m as { type?: unknown }).type === "usb.completion")
      .map(({ i }) => i);
    expect(completionIndices).toHaveLength(2);
    for (const i of completionIndices) {
      expect(port.transfers[i]).toBeUndefined();
    }
  });

  it("responds with an error completion when a usb.action envelope is received with an invalid action payload", async () => {
    stubNavigatorUsb(new EventTarget());

    const { UsbBroker } = await import("./usb_broker");
    const broker = new UsbBroker();

    const port = new FakePort();
    broker.attachWorkerPort(port as unknown as MessagePort);

    port.emit({ type: "usb.action", action: { kind: "bulkIn", id: 7, endpoint: 0x81, length: "bad" } });
    await Promise.resolve();

    const completions = port.posted.filter((m) => (m as { type?: unknown }).type === "usb.completion");
    expect(completions).toEqual([
      { type: "usb.completion", completion: { kind: "bulkIn", id: 7, status: "error", message: "Invalid UsbHostAction received from worker." } },
    ]);
  });

  it("does not synthesize a usb.completion when an invalid usb.action envelope includes an invalid id", async () => {
    stubNavigatorUsb(new EventTarget());

    const { UsbBroker } = await import("./usb_broker");
    const broker = new UsbBroker();

    const port = new FakePort();
    broker.attachWorkerPort(port as unknown as MessagePort);
    // Ignore the initial controller-mode broadcast during attachment.
    port.posted.length = 0;

    port.emit({ type: "usb.action", action: { kind: "bulkIn", id: 1.5, endpoint: 0x81, length: "bad" } });
    await Promise.resolve();

    expect(port.posted).toEqual([]);
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

  it("cancels in-flight actions with WebUSB device replaced. when selecting a new device", async () => {
    const resolvers: Array<(c: BackendUsbHostCompletion) => void> = [];

    vi.doMock("./webusb_backend", () => ({
      WebUsbBackend: class {
        async ensureOpenAndClaimed(): Promise<void> {}

        execute(_action: unknown): Promise<BackendUsbHostCompletion> {
          return new Promise((resolve) => resolvers.push(resolve));
        }
      },
    }));

    const device1 = { vendorId: 0x0001, productId: 0x0002, close: async () => {} } as unknown as USBDevice;
    const device2 = { vendorId: 0x0003, productId: 0x0004, close: async () => {} } as unknown as USBDevice;
    const usb = new FakeUsbSequence([device1, device2]);
    stubNavigatorUsb(usb);

    const { UsbBroker } = await import("./usb_broker");

    const broker = new UsbBroker();
    await broker.requestDevice([{ vendorId: 1 }]);

    const p1 = broker.execute({ kind: "bulkIn", id: 1, endpoint: 0x81, length: 8 });
    await Promise.resolve();
    expect(resolvers).toHaveLength(1);

    await broker.requestDevice([{ vendorId: 2 }]);

    await expect(p1).resolves.toEqual({ kind: "bulkIn", id: 1, status: "error", message: "WebUSB device replaced." });
  });

  it("detachSelectedDevice() broadcasts usb.selected and blocks further actions until a new selection", async () => {
    vi.doMock("./webusb_backend", () => ({
      WebUsbBackend: class {
        async ensureOpenAndClaimed(): Promise<void> {}

        async execute(): Promise<BackendUsbHostCompletion> {
          throw new Error("not used");
        }
      },
    }));

    const device = {
      vendorId: 0x1234,
      productId: 0x5678,
      close: vi.fn(async () => {}),
    } as unknown as USBDevice;

    stubNavigatorUsb({ requestDevice: vi.fn(async () => device) });

    const { UsbBroker } = await import("./usb_broker");
    const broker = new UsbBroker();

    const port = new FakePort();
    broker.attachWorkerPort(port as unknown as MessagePort);

    await broker.requestDevice();
    port.posted.length = 0;
    port.transfers.length = 0;

    await broker.detachSelectedDevice("device detached");

    const selected = port.posted.filter((m) => (m as { type?: unknown }).type === "usb.selected");
    expect(selected).toEqual([{ type: "usb.selected", ok: false, error: "device detached" }]);

    await expect(broker.execute({ kind: "bulkIn", id: 1, endpoint: 0x81, length: 8 })).resolves.toEqual({
      kind: "bulkIn",
      id: 1,
      status: "error",
      message: "device detached",
    });
  });

  it("forgetSelectedDevice() calls USBDevice.forget (when available) and resets to no-device state", async () => {
    vi.doMock("./webusb_backend", () => ({
      WebUsbBackend: class {
        async ensureOpenAndClaimed(): Promise<void> {}

        async execute(): Promise<BackendUsbHostCompletion> {
          throw new Error("not used");
        }
      },
    }));

    const close = vi.fn(async () => {});
    const forget = vi.fn(async () => {});
    const device = {
      vendorId: 0x1234,
      productId: 0x5678,
      close,
      forget,
    } as unknown as USBDevice;

    stubNavigatorUsb({ requestDevice: vi.fn(async () => device) });

    const { UsbBroker } = await import("./usb_broker");
    const broker = new UsbBroker();

    const port = new FakePort();
    broker.attachWorkerPort(port as unknown as MessagePort);

    await broker.attachKnownDevice(device);
    port.posted.length = 0;
    port.transfers.length = 0;

    expect(broker.canForgetSelectedDevice()).toBe(true);
    await broker.forgetSelectedDevice();

    expect(forget).toHaveBeenCalledTimes(1);
    expect(close).toHaveBeenCalled();
    expect(broker.canForgetSelectedDevice()).toBe(false);

    const selected = port.posted.filter((m) => (m as { type?: unknown }).type === "usb.selected");
    expect(selected).toEqual([{ type: "usb.selected", ok: false }]);

    await expect(broker.execute({ kind: "bulkIn", id: 123, endpoint: 0x81, length: 8 })).resolves.toEqual({
      kind: "bulkIn",
      id: 123,
      status: "error",
      message: "WebUSB device not selected.",
    });
  });

  it("subscribeToDeviceChanges() fires for navigator.usb connect/disconnect events", async () => {
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

    const onChange = vi.fn();
    broker.subscribeToDeviceChanges(onChange);

    await broker.attachKnownDevice(device);

    usb.dispatchEvent(new Event("connect"));
    usb.dispatchDisconnect(device);

    expect(onChange).toHaveBeenCalledTimes(2);
  });

  it("lists permitted devices and can attach one without showing the chooser", async () => {
    let openAndClaimCalls = 0;

    vi.doMock("./webusb_backend", () => ({
      WebUsbBackend: class {
        async ensureOpenAndClaimed(): Promise<void> {
          openAndClaimCalls += 1;
        }

        async execute(): Promise<BackendUsbHostCompletion> {
          throw new Error("not used");
        }
      },
    }));

    const device = {
      vendorId: 0x1234,
      productId: 0x5678,
      productName: "Permitted",
      close: async () => {},
    } as unknown as USBDevice;

    const getDevices = vi.fn(async () => [device]);
    stubNavigatorUsb({ getDevices });

    const { UsbBroker } = await import("./usb_broker");

    const broker = new UsbBroker();
    await expect(broker.getPermittedDevices()).resolves.toEqual([device]);
    expect(getDevices).toHaveBeenCalledTimes(1);

    const port = new FakePort();
    broker.attachWorkerPort(port as unknown as MessagePort);

    await broker.attachPermittedDevice(device);
    expect(openAndClaimCalls).toBe(1);

    const selected = port.posted.filter((m) => (m as { type?: unknown }).type === "usb.selected");
    expect(selected).toEqual([
      {
        type: "usb.selected",
        ok: true,
        info: { vendorId: 0x1234, productId: 0x5678, productName: "Permitted" },
      },
    ]);
  });

  it("cancels in-flight actions with WebUSB device replaced. when attaching a new permitted device", async () => {
    const resolvers: Array<(c: BackendUsbHostCompletion) => void> = [];

    vi.doMock("./webusb_backend", () => ({
      WebUsbBackend: class {
        async ensureOpenAndClaimed(): Promise<void> {}

        execute(_action: unknown): Promise<BackendUsbHostCompletion> {
          return new Promise((resolve) => resolvers.push(resolve));
        }
      },
    }));

    const device1 = { vendorId: 0x0001, productId: 0x0002, close: async () => {} } as unknown as USBDevice;
    const device2 = { vendorId: 0x0003, productId: 0x0004, close: async () => {} } as unknown as USBDevice;
    stubNavigatorUsb({});

    const { UsbBroker } = await import("./usb_broker");

    const broker = new UsbBroker();
    await broker.attachPermittedDevice(device1);

    const p1 = broker.execute({ kind: "bulkIn", id: 1, endpoint: 0x81, length: 8 });
    await Promise.resolve();
    expect(resolvers).toHaveLength(1);

    await broker.attachPermittedDevice(device2);

    await expect(p1).resolves.toEqual({ kind: "bulkIn", id: 1, status: "error", message: "WebUSB device replaced." });
  });

  it("rejects new actions when maxPendingActions is exceeded (queue backpressure)", async () => {
    vi.doMock("./webusb_backend", () => ({
      WebUsbBackend: class {
        async ensureOpenAndClaimed(): Promise<void> {}

        execute(): Promise<BackendUsbHostCompletion> {
          // Never resolve: simulate a stalled device transfer so the broker queue backs up.
          return new Promise(() => undefined);
        }
      },
    }));

    const device = { vendorId: 1, productId: 2, close: async () => {} } as unknown as USBDevice;
    const usb = new FakeUsb(device);
    stubNavigatorUsb(usb);

    const { UsbBroker } = await import("./usb_broker");
    const broker = new UsbBroker({ maxPendingActions: 2 });
    await broker.requestDevice();

    const p1 = broker.execute({ kind: "bulkIn", id: 1, endpoint: 0x81, length: 8 });
    const p2 = broker.execute({ kind: "bulkIn", id: 2, endpoint: 0x81, length: 8 });
    const p3 = broker.execute({ kind: "bulkIn", id: 3, endpoint: 0x81, length: 8 });

    await expect(p3).resolves.toEqual({
      kind: "bulkIn",
      id: 3,
      status: "error",
      message: "WebUSB broker queue full (too many pending actions).",
    });

    await broker.detachSelectedDevice("bye");
    await expect(p1).resolves.toEqual({ kind: "bulkIn", id: 1, status: "error", message: "bye" });
    await expect(p2).resolves.toEqual({ kind: "bulkIn", id: 2, status: "error", message: "bye" });
  }, 15000);

  it("rejects new actions when maxPendingActionBytes is exceeded (queue backpressure)", async () => {
    vi.doMock("./webusb_backend", () => ({
      WebUsbBackend: class {
        async ensureOpenAndClaimed(): Promise<void> {}

        execute(): Promise<BackendUsbHostCompletion> {
          return new Promise(() => undefined);
        }
      },
    }));

    const device = { vendorId: 1, productId: 2, close: async () => {} } as unknown as USBDevice;
    const usb = new FakeUsb(device);
    stubNavigatorUsb(usb);

    const { UsbBroker } = await import("./usb_broker");
    const broker = new UsbBroker({ maxPendingActions: 10, maxPendingActionBytes: 10 });
    await broker.requestDevice();

    const payload = new Uint8Array(8);
    const p1 = broker.execute({ kind: "bulkOut", id: 1, endpoint: 1, data: payload });
    const p2 = broker.execute({ kind: "bulkOut", id: 2, endpoint: 1, data: payload });

    await expect(p2).resolves.toEqual({
      kind: "bulkOut",
      id: 2,
      status: "error",
      message: "WebUSB broker queue full (too many pending actions).",
    });

    await broker.detachSelectedDevice("bye");
    await expect(p1).resolves.toEqual({ kind: "bulkOut", id: 1, status: "error", message: "bye" });
  }, 15000);

  it("does not drain the action ring when a payload would exceed maxPendingActionBytes (ring backpressure)", async () => {
    vi.useFakeTimers();
    const originalCoiDescriptor = Object.getOwnPropertyDescriptor(globalThis, "crossOriginIsolated");
    Object.defineProperty(globalThis, "crossOriginIsolated", {
      value: true,
      configurable: true,
      enumerable: true,
      writable: true,
    });

    try {
      const resolvers: Array<(c: BackendUsbHostCompletion) => void> = [];

      vi.doMock("./webusb_backend", () => ({
        WebUsbBackend: class {
          async ensureOpenAndClaimed(): Promise<void> {}

          execute(action: { id: number }): Promise<BackendUsbHostCompletion> {
            return new Promise((resolve) => {
              resolvers.push(resolve);
              void action;
            });
          }
        },
      }));

      const device = { vendorId: 1, productId: 2, close: async () => {} } as unknown as USBDevice;
      const usb = new FakeUsb(device);
      stubNavigatorUsb(usb);

      const { UsbBroker } = await import("./usb_broker");
      const { UsbProxyRing } = await import("./usb_proxy_ring");

      const broker = new UsbBroker({
        // Keep the byte budget small so two 800-byte bulkOut payloads exceed it.
        maxPendingActions: 10,
        maxPendingActionBytes: 1000,
        // Ensure the action ring can hold an 800-byte payload record.
        ringActionCapacityBytes: 2048,
        ringDrainIntervalMs: 8,
      });
      await broker.requestDevice();

      const port = new FakePort();
      broker.attachWorkerPort(port as unknown as MessagePort);

      const ringAttach = port.posted.find((m) => (m as { type?: unknown }).type === "usb.ringAttach") as
        | { type: "usb.ringAttach"; actionRing: SharedArrayBuffer; completionRing: SharedArrayBuffer }
        | undefined;
      expect(ringAttach).toBeTruthy();

      // Fill the broker's byte budget with an in-flight action.
      const payload = new Uint8Array(800);
      const pending = broker.execute({ kind: "bulkOut", id: 1, endpoint: 0x01, data: payload });

      await Promise.resolve();
      expect(resolvers).toHaveLength(1);

      // Send another action via the ring; it should not be drained until bytes free.
      const ringProducer = new UsbProxyRing(ringAttach!.actionRing);
      expect(ringProducer.pushAction({ kind: "bulkOut", id: 2, endpoint: 0x01, data: payload })).toBe(true);

      const ctrl = new Int32Array(ringAttach!.actionRing, 0, 3);
      const headBefore = Atomics.load(ctrl, 0);

      vi.advanceTimersByTime(8);

      const headAfter = Atomics.load(ctrl, 0);
      expect(headAfter).toBe(headBefore);

      // Clean up the in-flight action so the broker's async queue can settle.
      resolvers[0]!({ kind: "bulkOut", id: 1, status: "success", bytesWritten: payload.byteLength });
      await expect(pending).resolves.toEqual({
        kind: "bulkOut",
        id: 1,
        status: "success",
        bytesWritten: payload.byteLength,
      });

      broker.detachWorkerPort(port as unknown as MessagePort);
      await broker.detachSelectedDevice("bye");
    } finally {
      vi.useRealTimers();
      if (originalCoiDescriptor) {
        Object.defineProperty(globalThis, "crossOriginIsolated", originalCoiDescriptor);
      } else {
        Reflect.deleteProperty(globalThis as unknown as { crossOriginIsolated?: unknown }, "crossOriginIsolated");
      }
    }
  }, 15000);

  it("drains the action ring via popActionInfo when no device is selected (avoids payload copies)", async () => {
    vi.useFakeTimers();
    const originalCoiDescriptor = Object.getOwnPropertyDescriptor(globalThis, "crossOriginIsolated");
    Object.defineProperty(globalThis, "crossOriginIsolated", {
      value: true,
      configurable: true,
      enumerable: true,
      writable: true,
    });

    try {
      stubNavigatorUsb(new EventTarget());

      const { UsbBroker } = await import("./usb_broker");
      const { UsbProxyRing } = await import("./usb_proxy_ring");

      const popActionRecordSpy = vi.spyOn(UsbProxyRing.prototype, "popActionRecord");
      const popActionInfoSpy = vi.spyOn(UsbProxyRing.prototype, "popActionInfo");

      const broker = new UsbBroker({ ringActionCapacityBytes: 2048, ringDrainIntervalMs: 8 });
      const port = new FakePort();
      broker.attachWorkerPort(port as unknown as MessagePort);

      const ringAttach = port.posted.find((m) => (m as { type?: unknown }).type === "usb.ringAttach") as
        | { type: "usb.ringAttach"; actionRing: SharedArrayBuffer; completionRing: SharedArrayBuffer }
        | undefined;
      expect(ringAttach).toBeTruthy();

      // Push a payload-bearing action; without popActionInfo the broker would copy it out of the ring.
      const payload = new Uint8Array(512);
      const ringProducer = new UsbProxyRing(ringAttach!.actionRing);
      expect(ringProducer.pushAction({ kind: "bulkOut", id: 42, endpoint: 0x01, data: payload })).toBe(true);

      // Clear any messages produced during attachment.
      port.posted.length = 0;

      vi.advanceTimersByTime(8);

      expect(popActionRecordSpy).toHaveBeenCalledTimes(0);
      expect(popActionInfoSpy).toHaveBeenCalled();
      const completionRing = new UsbProxyRing(ringAttach!.completionRing);
      expect(completionRing.popCompletion()).toEqual({
        kind: "bulkOut",
        id: 42,
        status: "error",
        message: "WebUSB device not selected.",
      });

      popActionRecordSpy.mockRestore();
      popActionInfoSpy.mockRestore();
      broker.detachWorkerPort(port as unknown as MessagePort);
    } finally {
      vi.useRealTimers();
      if (originalCoiDescriptor) {
        Object.defineProperty(globalThis, "crossOriginIsolated", originalCoiDescriptor);
      } else {
        Reflect.deleteProperty(globalThis as unknown as { crossOriginIsolated?: unknown }, "crossOriginIsolated");
      }
    }
  }, 15000);
});
