import { afterEach, describe, expect, it, vi } from "vitest";

import { createIpcBuffer, openRingByKind } from "../ipc/ipc";
import { decodeHidInputReportRingRecord } from "../hid/hid_input_report_ring";
import { UHCI_EXTERNAL_HUB_FIRST_DYNAMIC_PORT } from "../usb/uhci_external_hub";
import type { HidPassthroughMessage } from "./hid_passthrough_protocol";
import { WebHidPassthroughManager } from "./webhid_passthrough";

type Listener = (event: unknown) => void;

function deferred<T>(): { promise: Promise<T>; resolve: (value: T) => void; reject: (reason?: unknown) => void } {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

class FakeHidDevice {
  opened = false;
  readonly vendorId = 0x1234;
  readonly productId = 0xabcd;
  readonly productName = "Fake HID";
  collections: HIDCollectionInfo[] = [
    {
      usagePage: 1,
      usage: 2,
      type: "application",
      children: [],
      inputReports: [],
      outputReports: [],
      featureReports: [],
    } as unknown as HIDCollectionInfo,
  ];

  readonly open = vi.fn(async () => {
    this.opened = true;
  });

  readonly close = vi.fn(async () => {
    this.opened = false;
  });

  readonly sendReport = vi.fn(async (_reportId: number, _data: BufferSource) => {});
  readonly sendFeatureReport = vi.fn(async (_reportId: number, _data: BufferSource) => {});
  readonly receiveFeatureReport = vi.fn(async (_reportId: number) => new DataView(new ArrayBuffer(0)));

  readonly #listeners = new Map<string, Set<Listener>>();

  readonly addEventListener = vi.fn((type: string, cb: Listener): void => {
    let set = this.#listeners.get(type);
    if (!set) {
      set = new Set();
      this.#listeners.set(type, set);
    }
    set.add(cb);
  });

  readonly removeEventListener = vi.fn((type: string, cb: Listener): void => {
    this.#listeners.get(type)?.delete(cb);
  });

  dispatchInputReport(reportId: number, data: DataView): void {
    const ev = { reportId, data } as unknown as HIDInputReportEvent;
    for (const cb of this.#listeners.get("inputreport") ?? []) cb(ev);
  }
}

type Posted = { message: HidPassthroughMessage; transfer?: Transferable[] };

class TestTarget {
  readonly posted: Posted[] = [];

  postMessage(message: HidPassthroughMessage, transfer?: Transferable[]): void {
    this.posted.push({ message, transfer });
  }
}

function bufferSourceToBytes(src: BufferSource): Uint8Array {
  return src instanceof ArrayBuffer ? new Uint8Array(src) : new Uint8Array(src.buffer, src.byteOffset, src.byteLength);
}

const originalCrossOriginIsolatedDescriptor = Object.getOwnPropertyDescriptor(globalThis, "crossOriginIsolated");

afterEach(() => {
  const original = Object.getOwnPropertyDescriptor(globalThis, "crossOriginIsolated");
  if (originalCrossOriginIsolatedDescriptor) {
    Object.defineProperty(globalThis, "crossOriginIsolated", originalCrossOriginIsolatedDescriptor);
  } else if (original) {
    Reflect.deleteProperty(globalThis as any, "crossOriginIsolated");
  }
});

describe("WebHidPassthroughManager broker (main thread ↔ I/O worker)", () => {
  it("posts hid:attach with normalized collections and forwards inputreport events", async () => {
    const device = new FakeHidDevice();
    const target = new TestTarget();
    const manager = new WebHidPassthroughManager({ hid: null, target });

    await manager.attachKnownDevice(device as unknown as HIDDevice);

    expect(target.posted).toHaveLength(2);
    expect(target.posted[0]!.message.type).toBe("hid:attachHub");
    const attach = target.posted[1]!.message;
    expect(attach.type).toBe("hid:attach");
    expect(attach).toMatchObject({
      guestPort: 0,
      guestPath: [0, UHCI_EXTERNAL_HUB_FIRST_DYNAMIC_PORT],
      vendorId: device.vendorId,
      productId: device.productId,
      productName: device.productName,
    });
    expect(typeof (attach as any).deviceId).toBe("string");
    expect(Array.isArray((attach as any).collections)).toBe(true);
    expect(((attach as any).collections as unknown[]).length).toBeGreaterThan(0);

    const deviceId = (attach as any).deviceId as string;

    const backing = new Uint8Array([0xde, 0xad, 0xbe, 0xef]);
    const slice = backing.subarray(1, 3); // [0xad, 0xbe]
    device.dispatchInputReport(7, new DataView(slice.buffer, slice.byteOffset, slice.byteLength));

    expect(target.posted).toHaveLength(3);
    const input = target.posted[2]!.message;
    expect(input.type).toBe("hid:inputReport");
    expect(input).toMatchObject({ deviceId, reportId: 7 });

    const data = (input as any).data as ArrayBuffer;
    expect(Array.from(new Uint8Array(data))).toEqual([0xad, 0xbe]);

    const transfer = target.posted[2]!.transfer;
    expect(transfer).toHaveLength(1);
    expect(transfer?.[0]).toBe(data);
  });

  it("clamps oversized inputreport payloads to the expected report size before forwarding", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      const device = new FakeHidDevice();
      device.collections = [
        {
          usagePage: 1,
          usage: 2,
          type: "application",
          children: [],
          inputReports: [
            {
              reportId: 1,
              items: [{ reportSize: 8, reportCount: 4 }],
            },
          ],
          outputReports: [],
          featureReports: [],
        },
      ] as unknown as HIDCollectionInfo[];
      const target = new TestTarget();
      const manager = new WebHidPassthroughManager({ hid: null, target });

      await manager.attachKnownDevice(device as unknown as HIDDevice);
      const attach = target.posted.find((entry) => entry.message.type === "hid:attach")!.message as any;
      const deviceId = attach.deviceId as string;

      const huge = new Uint8Array(1024 * 1024);
      huge.set([1, 2, 3, 4], 0);
      device.dispatchInputReport(1, new DataView(huge.buffer));

      const input = target.posted.find((p) => p.message.type === "hid:inputReport")!.message as any;
      expect(input.deviceId).toBe(deviceId);
      expect(input.reportId).toBe(1);
      const data = input.data as ArrayBuffer;
      expect(new Uint8Array(data).byteLength).toBe(4);
      expect(Array.from(new Uint8Array(data))).toEqual([1, 2, 3, 4]);
    } finally {
      warn.mockRestore();
    }
  });

  it("zero-pads short inputreport payloads to the expected report size before forwarding", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      const device = new FakeHidDevice();
      device.collections = [
        {
          usagePage: 1,
          usage: 2,
          type: "application",
          children: [],
          inputReports: [
            {
              reportId: 1,
              items: [{ reportSize: 8, reportCount: 4 }],
            },
          ],
          outputReports: [],
          featureReports: [],
        },
      ] as unknown as HIDCollectionInfo[];
      const target = new TestTarget();
      const manager = new WebHidPassthroughManager({ hid: null, target });

      await manager.attachKnownDevice(device as unknown as HIDDevice);
      const attach = target.posted.find((entry) => entry.message.type === "hid:attach")!.message as any;
      const deviceId = attach.deviceId as string;

      device.dispatchInputReport(1, new DataView(new Uint8Array([9, 8]).buffer));

      const input = target.posted.find((p) => p.message.type === "hid:inputReport")!.message as any;
      expect(input.deviceId).toBe(deviceId);
      expect(input.reportId).toBe(1);
      expect(Array.from(new Uint8Array(input.data as ArrayBuffer))).toEqual([9, 8, 0, 0]);
    } finally {
      warn.mockRestore();
    }
  });

  it("hard-caps unknown inputreport payload sizes before forwarding", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      const device = new FakeHidDevice();
      device.collections = [
        {
          usagePage: 1,
          usage: 2,
          type: "application",
          children: [],
          inputReports: [],
          outputReports: [],
          featureReports: [],
        },
      ] as unknown as HIDCollectionInfo[];
      const target = new TestTarget();
      const manager = new WebHidPassthroughManager({ hid: null, target });

      await manager.attachKnownDevice(device as unknown as HIDDevice);
      const attach = target.posted.find((entry) => entry.message.type === "hid:attach")!.message as any;
      const deviceId = attach.deviceId as string;

      const huge = new Uint8Array(1024 * 1024);
      huge.set([1, 2, 3], 0);
      device.dispatchInputReport(99, new DataView(huge.buffer));

      const input = target.posted.find((p) => p.message.type === "hid:inputReport")!.message as any;
      expect(input.deviceId).toBe(deviceId);
      expect(input.reportId).toBe(99);
      const bytes = new Uint8Array(input.data as ArrayBuffer);
      expect(bytes.byteLength).toBe(64);
      expect(Array.from(bytes.slice(0, 3))).toEqual([1, 2, 3]);
    } finally {
      warn.mockRestore();
    }
  });

  it("handles hid:sendReport from the worker and detaches cleanly", async () => {
    const device = new FakeHidDevice();
    const target = new TestTarget();
    const manager = new WebHidPassthroughManager({ hid: null, target });

    await manager.attachKnownDevice(device as unknown as HIDDevice);
    const attach = target.posted.find((entry) => entry.message.type === "hid:attach")!.message as any;
    const deviceId = attach.deviceId as string;

    device.dispatchInputReport(1, new DataView(new Uint8Array([9]).buffer));
    const initialForwarded = target.posted.filter((p) => p.message.type === "hid:inputReport");
    expect(initialForwarded).toHaveLength(1);

    manager.handleWorkerMessage({
      type: "hid:sendReport",
      deviceId,
      reportType: "output",
      reportId: 3,
      data: new Uint8Array([1, 2, 3]).buffer,
    });

    manager.handleWorkerMessage({
      type: "hid:sendReport",
      deviceId,
      reportType: "feature",
      reportId: 4,
      data: new Uint8Array([4, 5]).buffer,
    });

    await new Promise((r) => setTimeout(r, 0));

    expect(device.sendReport).toHaveBeenCalledTimes(1);
    expect(device.sendReport.mock.calls[0]![0]).toBe(3);
    expect(Array.from(bufferSourceToBytes(device.sendReport.mock.calls[0]![1]))).toEqual([1, 2, 3]);

    expect(device.sendFeatureReport).toHaveBeenCalledTimes(1);
    expect(device.sendFeatureReport.mock.calls[0]![0]).toBe(4);
    expect(Array.from(bufferSourceToBytes(device.sendFeatureReport.mock.calls[0]![1]))).toEqual([4, 5]);

    await manager.detachDevice(device as unknown as HIDDevice);

    expect(device.removeEventListener).toHaveBeenCalledWith("inputreport", expect.any(Function));
    expect(target.posted.map((p) => p.message.type)).toContain("hid:detach");

    // After detaching, inputreport events should no longer be forwarded.
    device.dispatchInputReport(2, new DataView(new Uint8Array([10]).buffer));
    const forwarded = target.posted.filter((p) => p.message.type === "hid:inputReport");
    expect(forwarded).toHaveLength(1);
  });

  it("handles hid:getFeatureReport from the worker and responds with hid:featureReportResult", async () => {
    const device = new FakeHidDevice();
    const target = new TestTarget();
    const manager = new WebHidPassthroughManager({ hid: null, target });

    device.receiveFeatureReport.mockImplementationOnce(async () => new DataView(Uint8Array.of(1, 2, 3).buffer));

    await manager.attachKnownDevice(device as unknown as HIDDevice);
    const attach = target.posted.find((entry) => entry.message.type === "hid:attach")!.message as any;
    const deviceId = attach.deviceId as string;

    manager.handleWorkerMessage({
      type: "hid:getFeatureReport",
      deviceId,
      requestId: 1,
      reportId: 7,
    });

    await new Promise((r) => setTimeout(r, 0));

    expect(device.receiveFeatureReport).toHaveBeenCalledTimes(1);
    expect(device.receiveFeatureReport).toHaveBeenCalledWith(7);

    const resEntry = target.posted.find((p) => p.message.type === "hid:featureReportResult")!;
    expect(resEntry.message).toMatchObject({ deviceId, requestId: 1, reportId: 7, ok: true });
    const data = (resEntry.message as any).data as ArrayBuffer;
    expect(Array.from(new Uint8Array(data))).toEqual([1, 2, 3]);
    expect(resEntry.transfer?.[0]).toBe(data);
  });

  it("clamps oversized feature report payloads to the expected report size before forwarding", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      const device = new FakeHidDevice();
      device.collections = [
        {
          usagePage: 1,
          usage: 2,
          type: "application",
          children: [],
          inputReports: [],
          outputReports: [],
          featureReports: [
            {
              reportId: 7,
              items: [{ reportSize: 8, reportCount: 4 }],
            },
          ],
        },
      ] as unknown as HIDCollectionInfo[];

      device.receiveFeatureReport.mockImplementationOnce(async () => {
        const huge = new Uint8Array(1024 * 1024);
        huge.set([1, 2, 3, 4], 0);
        return new DataView(huge.buffer);
      });

      const target = new TestTarget();
      const manager = new WebHidPassthroughManager({ hid: null, target });

      await manager.attachKnownDevice(device as unknown as HIDDevice);
      const attach = target.posted.find((entry) => entry.message.type === "hid:attach")!.message as any;
      const deviceId = attach.deviceId as string;

      manager.handleWorkerMessage({
        type: "hid:getFeatureReport",
        deviceId,
        requestId: 1,
        reportId: 7,
      });

      await new Promise((r) => setTimeout(r, 0));

      const resEntry = target.posted.find(
        (p) => p.message.type === "hid:featureReportResult" && (p.message as any).requestId === 1,
      )!;
      expect(resEntry.message).toMatchObject({ deviceId, requestId: 1, reportId: 7, ok: true });
      const data = (resEntry.message as any).data as ArrayBuffer;
      expect(new Uint8Array(data).byteLength).toBe(4);
      expect(Array.from(new Uint8Array(data))).toEqual([1, 2, 3, 4]);
      expect(resEntry.transfer?.[0]).toBe(data);
      expect(warn).toHaveBeenCalledTimes(1);
    } finally {
      warn.mockRestore();
    }
  });

  it("hard-caps unknown oversized feature report payload sizes before forwarding", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      const device = new FakeHidDevice();
      // No feature report metadata -> expected size unknown.
      device.collections = [
        {
          usagePage: 1,
          usage: 2,
          type: "application",
          children: [],
          inputReports: [],
          outputReports: [],
          featureReports: [],
        },
      ] as unknown as HIDCollectionInfo[];

      const huge = new Uint8Array(1024 * 1024);
      huge.set([1, 2, 3], 0);
      device.receiveFeatureReport.mockImplementation(async () => new DataView(huge.buffer));

      const target = new TestTarget();
      const manager = new WebHidPassthroughManager({ hid: null, target });

      await manager.attachKnownDevice(device as unknown as HIDDevice);
      const attach = target.posted.find((entry) => entry.message.type === "hid:attach")!.message as any;
      const deviceId = attach.deviceId as string;

      manager.handleWorkerMessage({ type: "hid:getFeatureReport", deviceId, requestId: 1, reportId: 99 });
      manager.handleWorkerMessage({ type: "hid:getFeatureReport", deviceId, requestId: 2, reportId: 99 });

      await new Promise((r) => setTimeout(r, 0));
      await new Promise((r) => setTimeout(r, 0));

      const results = target.posted.filter(
        (p) => p.message.type === "hid:featureReportResult" && (p.message as any).ok === true,
      ) as any[];
      const a = results.find((r) => r.message.requestId === 1);
      const b = results.find((r) => r.message.requestId === 2);
      expect(a).toBeTruthy();
      expect(b).toBeTruthy();

      for (const entry of [a, b]) {
        const data = entry.message.data as ArrayBuffer;
        expect(new Uint8Array(data).byteLength).toBe(4096);
        expect(Array.from(new Uint8Array(data).slice(0, 3))).toEqual([1, 2, 3]);
      }

      // Warn once per (deviceId, reportId) when hard-capping unknown report sizes.
      expect(warn).toHaveBeenCalledTimes(1);
    } finally {
      warn.mockRestore();
    }
  });

  it("executes hid:sendReport sequentially per device", async () => {
    const device = new FakeHidDevice();
    const target = new TestTarget();
    const manager = new WebHidPassthroughManager({ hid: null, target });

    await manager.attachKnownDevice(device as unknown as HIDDevice);
    const attach = target.posted.find((entry) => entry.message.type === "hid:attach")!.message as any;
    const deviceId = attach.deviceId as string;

    const first = deferred<void>();
    device.sendReport.mockImplementationOnce(() => first.promise);

    manager.handleWorkerMessage({
      type: "hid:sendReport",
      deviceId,
      reportType: "output",
      reportId: 1,
      data: new Uint8Array([1]).buffer,
    });

    manager.handleWorkerMessage({
      type: "hid:sendReport",
      deviceId,
      reportType: "output",
      reportId: 2,
      data: new Uint8Array([2]).buffer,
    });

    await new Promise((r) => setTimeout(r, 0));
    expect(device.sendReport).toHaveBeenCalledTimes(1);

    first.resolve(undefined);
    await new Promise((r) => setTimeout(r, 0));

    expect(device.sendReport).toHaveBeenCalledTimes(2);
    expect(device.sendReport.mock.calls[0]![0]).toBe(1);
    expect(Array.from(bufferSourceToBytes(device.sendReport.mock.calls[0]![1]))).toEqual([1]);
    expect(device.sendReport.mock.calls[1]![0]).toBe(2);
    expect(Array.from(bufferSourceToBytes(device.sendReport.mock.calls[1]![1]))).toEqual([2]);
  });

  it("drops pending hid:sendReport tasks on detach", async () => {
    const device = new FakeHidDevice();
    const target = new TestTarget();
    const manager = new WebHidPassthroughManager({ hid: null, target });

    await manager.attachKnownDevice(device as unknown as HIDDevice);
    const attach = target.posted.find((entry) => entry.message.type === "hid:attach")!.message as any;
    const deviceId = attach.deviceId as string;

    const first = deferred<void>();
    device.sendReport.mockImplementationOnce(() => first.promise);

    manager.handleWorkerMessage({
      type: "hid:sendReport",
      deviceId,
      reportType: "output",
      reportId: 1,
      data: new Uint8Array([1]).buffer,
    });

    manager.handleWorkerMessage({
      type: "hid:sendReport",
      deviceId,
      reportType: "output",
      reportId: 2,
      data: new Uint8Array([2]).buffer,
    });

    await new Promise((r) => setTimeout(r, 0));
    expect(device.sendReport).toHaveBeenCalledTimes(1);

    await manager.detachDevice(device as unknown as HIDDevice);

    first.resolve(undefined);
    await new Promise((r) => setTimeout(r, 0));

    expect(device.sendReport).toHaveBeenCalledTimes(1);
  });

  it("continues draining hid:sendReport queue after sendReport failure", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      const device = new FakeHidDevice();
      const target = new TestTarget();
      const manager = new WebHidPassthroughManager({ hid: null, target });

      await manager.attachKnownDevice(device as unknown as HIDDevice);
      const attach = target.posted.find((entry) => entry.message.type === "hid:attach")!.message as any;
      const deviceId = attach.deviceId as string;

      device.sendReport.mockImplementationOnce(async () => {
        throw new Error("nope");
      });
      device.sendReport.mockImplementationOnce(async () => {});

      manager.handleWorkerMessage({
        type: "hid:sendReport",
        deviceId,
        reportType: "output",
        reportId: 1,
        data: new Uint8Array([1]).buffer,
      });

      manager.handleWorkerMessage({
        type: "hid:sendReport",
        deviceId,
        reportType: "output",
        reportId: 2,
        data: new Uint8Array([2]).buffer,
      });

      await new Promise((r) => setTimeout(r, 0));
      await new Promise((r) => setTimeout(r, 0));

      expect(device.sendReport).toHaveBeenCalledTimes(2);
      expect(device.sendReport.mock.calls[0]![0]).toBe(1);
      expect(device.sendReport.mock.calls[1]![0]).toBe(2);
    } finally {
      warn.mockRestore();
    }
  });

  it("writes inputreport events into the configured SAB ring instead of posting hid:inputReport messages", async () => {
    Object.defineProperty(globalThis, "crossOriginIsolated", { value: true, configurable: true });

    const device = new FakeHidDevice();
    const target = new TestTarget();
    const manager = new WebHidPassthroughManager({ hid: null, target });

    const kind = 1;
    const sab = createIpcBuffer([{ kind, capacityBytes: 4096 }]).buffer;
    const ring = openRingByKind(sab, kind);
    const status = new Int32Array(new SharedArrayBuffer(64 * 4));
    manager.setInputReportRing(ring, status);

    await manager.attachKnownDevice(device as unknown as HIDDevice);
    const attach = target.posted.find((p) => p.message.type === "hid:attach")!.message as any;
    expect(typeof attach.numericDeviceId).toBe("number");

    const before = target.posted.length;
    device.dispatchInputReport(7, new DataView(new Uint8Array([0xad, 0xbe]).buffer));
    expect(target.posted.length).toBe(before);

    const payload = ring.tryPop();
    expect(payload).not.toBeNull();
    const record = decodeHidInputReportRingRecord(payload!);
    expect(record).toMatchObject({ deviceId: attach.numericDeviceId, reportId: 7, tsMs: 0 });
    expect(Array.from(record!.data)).toEqual([0xad, 0xbe]);
  });

  it("clamps oversized inputreport payloads before writing to the configured SAB ring", async () => {
    Object.defineProperty(globalThis, "crossOriginIsolated", { value: true, configurable: true });

    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      const device = new FakeHidDevice();
      device.collections = [
        {
          usagePage: 1,
          usage: 2,
          type: "application",
          children: [],
          inputReports: [
            {
              reportId: 1,
              items: [{ reportSize: 8, reportCount: 4 }],
            },
          ],
          outputReports: [],
          featureReports: [],
        },
      ] as unknown as HIDCollectionInfo[];
      const target = new TestTarget();
      const manager = new WebHidPassthroughManager({ hid: null, target });

      const kind = 1;
      const sab = createIpcBuffer([{ kind, capacityBytes: 4096 }]).buffer;
      const ring = openRingByKind(sab, kind);
      const status = new Int32Array(new SharedArrayBuffer(64 * 4));
      manager.setInputReportRing(ring, status);

      await manager.attachKnownDevice(device as unknown as HIDDevice);
      const attach = target.posted.find((p) => p.message.type === "hid:attach")!.message as any;
      expect(typeof attach.numericDeviceId).toBe("number");

      const before = target.posted.length;
      const huge = new Uint8Array(1024 * 1024);
      huge.set([1, 2, 3, 4], 0);
      device.dispatchInputReport(1, new DataView(huge.buffer));
      expect(target.posted.length).toBe(before);

      const payload = ring.tryPop();
      expect(payload).not.toBeNull();
      const record = decodeHidInputReportRingRecord(payload!);
      expect(record).toMatchObject({ deviceId: attach.numericDeviceId, reportId: 1, tsMs: 0 });
      expect(record!.data.byteLength).toBe(4);
      expect(Array.from(record!.data)).toEqual([1, 2, 3, 4]);
    } finally {
      warn.mockRestore();
    }
  });

  it("zero-pads short inputreport payloads before writing to the configured SAB ring", async () => {
    Object.defineProperty(globalThis, "crossOriginIsolated", { value: true, configurable: true });

    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      const device = new FakeHidDevice();
      device.collections = [
        {
          usagePage: 1,
          usage: 2,
          type: "application",
          children: [],
          inputReports: [
            {
              reportId: 1,
              items: [{ reportSize: 8, reportCount: 4 }],
            },
          ],
          outputReports: [],
          featureReports: [],
        },
      ] as unknown as HIDCollectionInfo[];
      const target = new TestTarget();
      const manager = new WebHidPassthroughManager({ hid: null, target });

      const kind = 1;
      const sab = createIpcBuffer([{ kind, capacityBytes: 4096 }]).buffer;
      const ring = openRingByKind(sab, kind);
      const status = new Int32Array(new SharedArrayBuffer(64 * 4));
      manager.setInputReportRing(ring, status);

      await manager.attachKnownDevice(device as unknown as HIDDevice);
      const attach = target.posted.find((p) => p.message.type === "hid:attach")!.message as any;
      expect(typeof attach.numericDeviceId).toBe("number");

      const before = target.posted.length;
      device.dispatchInputReport(1, new DataView(new Uint8Array([9, 8]).buffer));
      expect(target.posted.length).toBe(before);

      const payload = ring.tryPop();
      expect(payload).not.toBeNull();
      const record = decodeHidInputReportRingRecord(payload!);
      expect(record).toMatchObject({ deviceId: attach.numericDeviceId, reportId: 1, tsMs: 0 });
      expect(Array.from(record!.data)).toEqual([9, 8, 0, 0]);
    } finally {
      warn.mockRestore();
    }
  });

  it("hard-caps unknown oversized inputreport payloads before writing to the configured SAB ring", async () => {
    Object.defineProperty(globalThis, "crossOriginIsolated", { value: true, configurable: true });

    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      const device = new FakeHidDevice();
      device.collections = [
        {
          usagePage: 1,
          usage: 2,
          type: "application",
          children: [],
          inputReports: [],
          outputReports: [],
          featureReports: [],
        },
      ] as unknown as HIDCollectionInfo[];
      const target = new TestTarget();
      const manager = new WebHidPassthroughManager({ hid: null, target });

      const kind = 1;
      const sab = createIpcBuffer([{ kind, capacityBytes: 4096 }]).buffer;
      const ring = openRingByKind(sab, kind);
      const status = new Int32Array(new SharedArrayBuffer(64 * 4));
      manager.setInputReportRing(ring, status);

      await manager.attachKnownDevice(device as unknown as HIDDevice);
      const attach = target.posted.find((p) => p.message.type === "hid:attach")!.message as any;
      expect(typeof attach.numericDeviceId).toBe("number");

      const before = target.posted.length;
      const huge = new Uint8Array(1024 * 1024);
      huge.set([1, 2, 3], 0);
      device.dispatchInputReport(99, new DataView(huge.buffer));
      expect(target.posted.length).toBe(before);

      const payload = ring.tryPop();
      expect(payload).not.toBeNull();
      const record = decodeHidInputReportRingRecord(payload!);
      expect(record).toMatchObject({ deviceId: attach.numericDeviceId, reportId: 99, tsMs: 0 });
      expect(record!.data.byteLength).toBe(64);
      expect(Array.from(record!.data.slice(0, 3))).toEqual([1, 2, 3]);
    } finally {
      warn.mockRestore();
    }
  });
});
