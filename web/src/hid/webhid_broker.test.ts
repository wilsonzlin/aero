import { afterEach, describe, expect, it, vi } from "vitest";

import { WebHidPassthroughManager } from "../platform/webhid_passthrough";
import { WebHidBroker } from "./webhid_broker";
import type { HidAttachMessage, HidInputReportMessage } from "./hid_proxy_protocol";

type FakeListener = (ev: MessageEvent<unknown>) => void;

class FakePort {
  readonly posted: Array<{ msg: unknown; transfer?: Transferable[] }> = [];
  private readonly listeners: FakeListener[] = [];

  addEventListener(type: string, listener: FakeListener): void {
    if (type !== "message") return;
    this.listeners.push(listener);
  }

  removeEventListener(type: string, listener: FakeListener): void {
    if (type !== "message") return;
    const idx = this.listeners.indexOf(listener);
    if (idx >= 0) this.listeners.splice(idx, 1);
  }

  start(): void {
    // No-op; browsers require MessagePort.start() when using addEventListener.
  }

  postMessage(msg: unknown, transfer?: Transferable[]): void {
    this.posted.push({ msg, transfer });
  }

  emit(msg: unknown): void {
    const ev = { data: msg } as MessageEvent<unknown>;
    for (const listener of this.listeners.slice()) listener(ev);
  }
}

type DeviceListener = (ev: unknown) => void;

class FakeHidDevice {
  productName: string | undefined;
  vendorId = 0x1234;
  productId = 0xabcd;
  collections: HIDCollectionInfo[] = [];

  opened = false;
  readonly open = vi.fn(async () => {
    this.opened = true;
  });
  readonly close = vi.fn(async () => {
    this.opened = false;
  });

  readonly sendReport = vi.fn(async () => {});
  readonly sendFeatureReport = vi.fn(async () => {});

  readonly #listeners = new Map<string, Set<DeviceListener>>();

  addEventListener(type: string, listener: DeviceListener): void {
    let set = this.#listeners.get(type);
    if (!set) {
      set = new Set();
      this.#listeners.set(type, set);
    }
    set.add(listener);
  }

  removeEventListener(type: string, listener: DeviceListener): void {
    this.#listeners.get(type)?.delete(listener);
  }

  dispatchInputReport(reportId: number, bytes: Uint8Array, timeStamp = 12.34): void {
    const view = new DataView(bytes.slice().buffer);
    const event = { reportId, data: view, timeStamp } as unknown;
    for (const listener of this.#listeners.get("inputreport") ?? []) {
      listener(event);
    }
  }
}

afterEach(() => {
  vi.clearAllMocks();
});

describe("hid/WebHidBroker", () => {
  it("forwards inputreport events to the worker port with transferred bytes", async () => {
    const manager = new WebHidPassthroughManager({ hid: null });
    const broker = new WebHidBroker({ manager });
    const port = new FakePort();
    broker.attachWorkerPort(port as unknown as MessagePort);

    const device = new FakeHidDevice();
    await broker.attachDevice(device as unknown as HIDDevice);

    const attach = port.posted.find((p) => (p.msg as { type?: unknown }).type === "hid.attach")?.msg as HidAttachMessage | undefined;
    expect(attach).toBeTruthy();

    device.dispatchInputReport(5, Uint8Array.of(1, 2, 3));

    const input = port.posted.find((p) => (p.msg as { type?: unknown }).type === "hid.inputReport") as
      | { msg: HidInputReportMessage; transfer?: Transferable[] }
      | undefined;
    expect(input).toBeTruthy();
    expect(input!.msg.deviceId).toBe(attach!.deviceId);
    expect(input!.msg.reportId).toBe(5);
    expect(Array.from(input!.msg.data)).toEqual([1, 2, 3]);
    expect(input!.transfer?.[0]).toBe(input!.msg.data.buffer);
  });

  it("handles hid.sendReport requests from the worker", async () => {
    const manager = new WebHidPassthroughManager({ hid: null });
    const broker = new WebHidBroker({ manager });
    const port = new FakePort();
    broker.attachWorkerPort(port as unknown as MessagePort);

    const device = new FakeHidDevice();
    const id = await broker.attachDevice(device as unknown as HIDDevice);

    port.emit({ type: "hid.sendReport", deviceId: id, reportType: "output", reportId: 7, data: Uint8Array.of(9) });
    port.emit({ type: "hid.sendReport", deviceId: id, reportType: "feature", reportId: 8, data: Uint8Array.of(10) });

    await new Promise((r) => setTimeout(r, 0));

    expect(device.sendReport).toHaveBeenCalledWith(7, Uint8Array.of(9));
    expect(device.sendFeatureReport).toHaveBeenCalledWith(8, Uint8Array.of(10));
  });

  it("does not auto-attach devices when the worker port is replaced", async () => {
    const manager = new WebHidPassthroughManager({ hid: null });
    const broker = new WebHidBroker({ manager });

    const port1 = new FakePort();
    broker.attachWorkerPort(port1 as unknown as MessagePort);

    const device = new FakeHidDevice();
    const id = await broker.attachDevice(device as unknown as HIDDevice);

    device.dispatchInputReport(1, Uint8Array.of(1));
    expect(port1.posted.some((p) => (p.msg as { type?: unknown }).type === "hid.inputReport")).toBe(true);

    const port2 = new FakePort();
    broker.attachWorkerPort(port2 as unknown as MessagePort);

    expect(port2.posted.some((p) => (p.msg as { type?: unknown }).type === "hid.attach")).toBe(false);

    device.dispatchInputReport(2, Uint8Array.of(2));
    expect(port2.posted.some((p) => (p.msg as { type?: unknown }).type === "hid.inputReport")).toBe(false);

    await broker.attachDevice(device as unknown as HIDDevice);
    expect(port2.posted.some((p) => (p.msg as { type?: unknown }).type === "hid.attach" && (p.msg as any).deviceId === id)).toBe(true);
  });
});

