import { afterEach, describe, expect, it, vi } from "vitest";

import { createIpcBuffer, openRingByKind } from "../ipc/ipc";
import { RingBuffer } from "../ipc/ring_buffer";
import { WebHidPassthroughManager } from "../platform/webhid_passthrough";
import { StatusIndex } from "../runtime/shared_layout";
import { HidReportRing, HidReportType } from "../usb/hid_report_ring";
import { UHCI_EXTERNAL_HUB_FIRST_DYNAMIC_PORT } from "../usb/uhci_external_hub";
import { decodeHidInputReportRingRecord } from "./hid_input_report_ring";
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
  const original = Object.getOwnPropertyDescriptor(globalThis, "crossOriginIsolated");
  if (originalCrossOriginIsolatedDescriptor) {
    Object.defineProperty(globalThis, "crossOriginIsolated", originalCrossOriginIsolatedDescriptor);
  } else if (original) {
    Reflect.deleteProperty(globalThis as any, "crossOriginIsolated");
  }
  vi.clearAllMocks();
});

const originalCrossOriginIsolatedDescriptor = Object.getOwnPropertyDescriptor(globalThis, "crossOriginIsolated");

describe("hid/WebHidBroker", () => {
  it("forwards inputreport events to the worker port with transferred bytes", async () => {
    const manager = new WebHidPassthroughManager({ hid: null });
    const broker = new WebHidBroker({ manager });
    const port = new FakePort();
    broker.attachWorkerPort(port as unknown as MessagePort);
    // When not crossOriginIsolated, the broker must not enable any SAB fast paths.
    expect(port.posted.some((p) => (p.msg as { type?: unknown }).type === "hid.ring.init")).toBe(false);
    expect(port.posted.some((p) => (p.msg as { type?: unknown }).type === "hid.ringAttach")).toBe(false);

    const device = new FakeHidDevice();
    await broker.attachDevice(device as unknown as HIDDevice);

    const attach = port.posted.find((p) => (p.msg as { type?: unknown }).type === "hid.attach")?.msg as HidAttachMessage | undefined;
    expect(attach).toBeTruthy();
    expect(attach!.guestPort).toBe(0);
    // First downstream hub ports are reserved for Aero's built-in synthetic HID devices (kbd/mouse/gamepad).
    expect(attach!.guestPath).toEqual([0, UHCI_EXTERNAL_HUB_FIRST_DYNAMIC_PORT]);

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

  it("forwards reports via SharedArrayBuffer rings when crossOriginIsolated", async () => {
    Object.defineProperty(globalThis, "crossOriginIsolated", { value: true, configurable: true });

    const manager = new WebHidPassthroughManager({ hid: null });
    const broker = new WebHidBroker({ manager });
    const port = new FakePort();
    broker.attachWorkerPort(port as unknown as MessagePort);

    const ringInit = port.posted.find((p) => (p.msg as { type?: unknown }).type === "hid.ring.init")?.msg as
      | { sab: SharedArrayBuffer; offsetBytes: number }
      | undefined;
    expect(ringInit).toBeTruthy();
    const inputReportRing = new RingBuffer(ringInit!.sab, ringInit!.offsetBytes);

    const ringAttach = port.posted.find((p) => (p.msg as { type?: unknown }).type === "hid.ringAttach")?.msg as
      | { inputRing: SharedArrayBuffer; outputRing: SharedArrayBuffer }
      | undefined;
    expect(ringAttach).toBeTruthy();

    const outputRing = new HidReportRing(ringAttach!.outputRing);

    const device = new FakeHidDevice();
    const id = await broker.attachDevice(device as unknown as HIDDevice);

    const before = port.posted.length;
    device.dispatchInputReport(5, Uint8Array.of(1, 2, 3));
    // No per-report postMessage; the input is queued via the SharedArrayBuffer ring.
    expect(port.posted.length).toBe(before);

    const payload = inputReportRing.tryPop();
    expect(payload).toBeTruthy();
    const decoded = decodeHidInputReportRingRecord(payload!);
    expect(decoded).toBeTruthy();
    expect(decoded).toMatchObject({ deviceId: id, reportId: 5 });
    expect(Array.from(decoded!.data)).toEqual([1, 2, 3]);

    // Worker -> main output/feature reports also flow through the ring.
    outputRing.push(id, HidReportType.Output, 7, Uint8Array.of(9));
    await new Promise((r) => setTimeout(r, 20));
    expect(device.sendReport).toHaveBeenCalledWith(7, Uint8Array.of(9));

    broker.destroy();
  });

  it("computes hasInterruptOut based on output reports (feature-only does not require interrupt OUT)", async () => {
    const manager = new WebHidPassthroughManager({ hid: null });
    const broker = new WebHidBroker({ manager });
    const port = new FakePort();
    broker.attachWorkerPort(port as unknown as MessagePort);

    const outputDevice = new FakeHidDevice();
    outputDevice.collections = [
      {
        usagePage: 1,
        usage: 2,
        type: "application",
        children: [],
        inputReports: [],
        outputReports: [{ reportId: 1, items: [] }],
        featureReports: [],
      },
    ] as unknown as HIDCollectionInfo[];
    await broker.attachDevice(outputDevice as unknown as HIDDevice);
    const outputAttach = port.posted.find((p) => (p.msg as { type?: unknown }).type === "hid.attach")?.msg as HidAttachMessage;
    expect(outputAttach.hasInterruptOut).toBe(true);

    await broker.detachDevice(outputDevice as unknown as HIDDevice);

    const featureDevice = new FakeHidDevice();
    featureDevice.collections = [
      {
        usagePage: 1,
        usage: 2,
        type: "application",
        children: [],
        inputReports: [],
        outputReports: [],
        featureReports: [{ reportId: 1, items: [] }],
      },
    ] as unknown as HIDCollectionInfo[];
    await broker.attachDevice(featureDevice as unknown as HIDDevice);
    const featureAttach = port.posted
      .slice()
      .reverse()
      .find((p) => (p.msg as { type?: unknown }).type === "hid.attach")?.msg as HidAttachMessage;
    expect(featureAttach.hasInterruptOut).toBe(false);
  });

  it("writes inputreport events into the configured ring buffer instead of posting hid.inputReport messages", async () => {
    const prev = Object.getOwnPropertyDescriptor(globalThis, "crossOriginIsolated");
    Object.defineProperty(globalThis, "crossOriginIsolated", { value: true, configurable: true });
    try {
      const manager = new WebHidPassthroughManager({ hid: null });
      const broker = new WebHidBroker({ manager });
      const port = new FakePort();
      broker.attachWorkerPort(port as unknown as MessagePort);

      const kind = 1;
      const sab = createIpcBuffer([{ kind, capacityBytes: 4096 }]).buffer;
      const ring = openRingByKind(sab, kind);
      const status = new Int32Array(new SharedArrayBuffer(64 * 4));
      broker.setInputReportRing(ring, status);

      const device = new FakeHidDevice();
      await broker.attachDevice(device as unknown as HIDDevice);
      const attach = port.posted.find((p) => (p.msg as { type?: unknown }).type === "hid.attach")?.msg as HidAttachMessage | undefined;
      expect(attach).toBeTruthy();

      device.dispatchInputReport(5, Uint8Array.of(1, 2, 3));

      expect(port.posted.some((p) => (p.msg as { type?: unknown }).type === "hid.inputReport")).toBe(false);

      const payload = ring.tryPop();
      expect(payload).toBeTruthy();
      const decoded = decodeHidInputReportRingRecord(payload!);
      expect(decoded).toBeTruthy();
      expect(decoded!.deviceId).toBe(attach!.deviceId);
      expect(decoded!.reportId).toBe(5);
      expect(Array.from(decoded!.data)).toEqual([1, 2, 3]);
    } finally {
      if (prev) {
        Object.defineProperty(globalThis, "crossOriginIsolated", prev);
      } else {
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        delete (globalThis as any).crossOriginIsolated;
      }
    }
  });

  it("increments the drop counter when the input report ring is full", async () => {
    const prev = Object.getOwnPropertyDescriptor(globalThis, "crossOriginIsolated");
    Object.defineProperty(globalThis, "crossOriginIsolated", { value: true, configurable: true });
    try {
      const manager = new WebHidPassthroughManager({ hid: null });
      const broker = new WebHidBroker({ manager });
      const port = new FakePort();
      broker.attachWorkerPort(port as unknown as MessagePort);

      // Capacity chosen so exactly one small input report fits.
      const kind = 1;
      const sab = createIpcBuffer([{ kind, capacityBytes: 32 }]).buffer;
      const ring = openRingByKind(sab, kind);
      const status = new Int32Array(new SharedArrayBuffer(64 * 4));
      broker.setInputReportRing(ring, status);

      const device = new FakeHidDevice();
      await broker.attachDevice(device as unknown as HIDDevice);

      device.dispatchInputReport(1, Uint8Array.of(1, 2, 3));
      device.dispatchInputReport(2, Uint8Array.of(4, 5, 6));

      expect(port.posted.some((p) => (p.msg as { type?: unknown }).type === "hid.inputReport")).toBe(false);

      const firstPayload = ring.tryPop();
      expect(firstPayload).toBeTruthy();
      const first = decodeHidInputReportRingRecord(firstPayload!);
      expect(first).toBeTruthy();
      expect(first!.reportId).toBe(1);
      expect(ring.tryPop()).toBeNull();

      expect(Atomics.load(status, StatusIndex.IoHidInputReportDropCounter)).toBe(1);
    } finally {
      if (prev) {
        Object.defineProperty(globalThis, "crossOriginIsolated", prev);
      } else {
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        delete (globalThis as any).crossOriginIsolated;
      }
    }
  });

  it("bridges manager-initiated detaches (e.g. physical disconnect) to the worker", async () => {
    const manager = new WebHidPassthroughManager({ hid: null });
    const broker = new WebHidBroker({ manager });
    const port = new FakePort();
    broker.attachWorkerPort(port as unknown as MessagePort);

    const device = new FakeHidDevice();
    const id = await broker.attachDevice(device as unknown as HIDDevice);

    await manager.detachDevice(device as unknown as HIDDevice);
    // The broker reacts to manager detaches asynchronously.
    await new Promise((r) => setTimeout(r, 0));

    expect(port.posted.some((p) => (p.msg as { type?: unknown }).type === "hid.detach" && (p.msg as any).deviceId === id)).toBe(
      true,
    );

    const before = port.posted.length;
    device.dispatchInputReport(1, Uint8Array.of(1));
    expect(port.posted.length).toBe(before);
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

    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    port2.emit({ type: "hid.sendReport", deviceId: id, reportType: "output", reportId: 1, data: Uint8Array.of(3) });
    await new Promise((r) => setTimeout(r, 0));
    expect(device.sendReport).not.toHaveBeenCalled();
    warn.mockRestore();

    await broker.attachDevice(device as unknown as HIDDevice);
    expect(port2.posted.some((p) => (p.msg as { type?: unknown }).type === "hid.attach" && (p.msg as any).deviceId === id)).toBe(true);
  });
});
