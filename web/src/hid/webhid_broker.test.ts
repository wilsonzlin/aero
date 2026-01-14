import { afterEach, describe, expect, it, vi } from "vitest";

import { createIpcBuffer, openRingByKind } from "../ipc/ipc";
import { ringCtrl } from "../ipc/layout";
import { RingBuffer } from "../ipc/ring_buffer";
import { WebHidPassthroughManager } from "../platform/webhid_passthrough";
import { StatusIndex } from "../runtime/shared_layout";
import { HID_REPORT_RING_CTRL_BYTES, HidReportRing, HidReportType } from "../usb/hid_report_ring";
import { UHCI_EXTERNAL_HUB_FIRST_DYNAMIC_PORT } from "../usb/uhci_external_hub";
import { decodeHidInputReportRingRecord } from "./hid_input_report_ring";
import { WebHidBroker } from "./webhid_broker";
import type { HidAttachMessage, HidFeatureReportResultMessage, HidInputReportMessage } from "./hid_proxy_protocol";

type FakeListener = (ev: MessageEvent<unknown>) => void;

function deferred<T>(): { promise: Promise<T>; resolve: (value: T) => void; reject: (reason?: unknown) => void } {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

async function flushMicrotasks(iterations = 8): Promise<void> {
  // `await Promise.resolve()` yields to the microtask queue. Loop a few times so nested async/await
  // chains (like the broker's per-device send queue runner) have a chance to fully drain.
  for (let i = 0; i < iterations; i += 1) {
    await Promise.resolve();
  }
}

class FakePort {
  readonly posted: Array<{ msg: unknown; transfer?: Transferable[] }> = [];
  private readonly listeners: FakeListener[] = [];
  autoAttachResult = true;
  onPost: ((msg: unknown, transfer?: Transferable[]) => void) | null = null;

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
    this.onPost?.(msg, transfer);
    if (this.autoAttachResult && !this.onPost && (msg as { type?: unknown }).type === "hid.attach") {
      const deviceId = (msg as { deviceId: number }).deviceId;
      this.emit({ type: "hid.attachResult", deviceId, ok: true });
    }
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

  // Match the WebHID method signatures so `vi.fn().mock.calls` is typed with the correct tuple shape.
  readonly sendReport = vi.fn(async (_reportId: number, _data: BufferSource) => {});
  readonly sendFeatureReport = vi.fn(async (_reportId: number, _data: BufferSource) => {});
  readonly receiveFeatureReport = vi.fn(async (_reportId: number) => new DataView(new ArrayBuffer(0)));

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
    Reflect.deleteProperty(globalThis, "crossOriginIsolated");
  }
  vi.clearAllMocks();
});

const originalCrossOriginIsolatedDescriptor = Object.getOwnPropertyDescriptor(globalThis, "crossOriginIsolated");

describe("hid/WebHidBroker", () => {
  it("validates maxPendingDeviceSends/maxPendingSendsPerDevice alias mismatch", () => {
    const manager = new WebHidPassthroughManager({ hid: null });
    expect(() => new WebHidBroker({ manager, maxPendingDeviceSends: 1, maxPendingSendsPerDevice: 2 })).toThrow(/must match/);
  });

  it("validates inputReportRingCapacityBytes", () => {
    const manager = new WebHidPassthroughManager({ hid: null });
    expect(() => new WebHidBroker({ manager, inputReportRingCapacityBytes: -1 })).toThrow(/invalid inputReportRingCapacityBytes/);
    expect(() => new WebHidBroker({ manager, inputReportRingCapacityBytes: 0 })).toThrow(/invalid inputReportRingCapacityBytes/);
    expect(() => new WebHidBroker({ manager, inputReportRingCapacityBytes: 17 * 1024 * 1024 })).toThrow(
      /inputReportRingCapacityBytes must be <=/,
    );
  });

  it("waits for hid.attachResult before forwarding inputreport events to the worker port", async () => {
    const manager = new WebHidPassthroughManager({ hid: null });
    const broker = new WebHidBroker({ manager });
    const port = new FakePort();
    port.autoAttachResult = false;
    broker.attachWorkerPort(port as unknown as MessagePort);
    // When not crossOriginIsolated, the broker must not enable any SAB fast paths.
    expect(port.posted.some((p) => (p.msg as { type?: unknown }).type === "hid.ring.init")).toBe(false);
    expect(port.posted.some((p) => (p.msg as { type?: unknown }).type === "hid.ringAttach")).toBe(false);

    const device = new FakeHidDevice();
    const attachPromise = broker.attachDevice(device as unknown as HIDDevice);

    // Wait until the broker posts the hid.attach message (it will block on hid.attachResult).
    let attach: HidAttachMessage | undefined;
    for (let i = 0; i < 10 && !attach; i += 1) {
      attach = port.posted.find((p) => (p.msg as { type?: unknown }).type === "hid.attach")?.msg as HidAttachMessage | undefined;
      if (!attach) await new Promise((r) => setTimeout(r, 0));
    }
    expect(attach).toBeTruthy();

    // While attach is still pending, input reports must not be forwarded.
    device.dispatchInputReport(5, Uint8Array.of(1, 2, 3));
    expect(port.posted.some((p) => (p.msg as { type?: unknown }).type === "hid.inputReport")).toBe(false);

    port.emit({ type: "hid.attachResult", deviceId: attach!.deviceId, ok: true });
    await attachPromise;

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

  it("sets hasInterruptOut=false when any output report exceeds a full-speed interrupt packet", async () => {
    const manager = new WebHidPassthroughManager({ hid: null });
    const broker = new WebHidBroker({ manager });
    const port = new FakePort();
    broker.attachWorkerPort(port as unknown as MessagePort);

    const device = new FakeHidDevice();
    device.collections = [
      {
        usagePage: 1,
        usage: 2,
        type: "application",
        children: [],
        inputReports: [],
        outputReports: [
          {
            reportId: 0,
            items: [{ reportSize: 8, reportCount: 65 }],
          },
        ],
        featureReports: [],
      },
    ] as unknown as HIDCollectionInfo[];

    await broker.attachDevice(device as unknown as HIDDevice);

    const attach = port.posted.find((p) => (p.msg as { type?: unknown }).type === "hid.attach")?.msg as HidAttachMessage | undefined;
    expect(attach).toBeTruthy();
    expect(attach!.hasInterruptOut).toBe(false);
  });

  it("rejects attachDevice when detachDevice is called before hid.attachResult", async () => {
    const manager = new WebHidPassthroughManager({ hid: null });
    const broker = new WebHidBroker({ manager, attachResultTimeoutMs: 60_000 });
    const port = new FakePort();
    port.autoAttachResult = false;
    broker.attachWorkerPort(port as unknown as MessagePort);

    const device = new FakeHidDevice();
    const attachPromise = broker.attachDevice(device as unknown as HIDDevice);

    // Wait for the attach message so we know the broker is blocked on the result.
    let attach: HidAttachMessage | undefined;
    for (let i = 0; i < 10 && !attach; i += 1) {
      attach = port.posted.find((p) => (p.msg as { type?: unknown }).type === "hid.attach")?.msg as HidAttachMessage | undefined;
      if (!attach) await new Promise((r) => setTimeout(r, 0));
    }
    expect(attach).toBeTruthy();

    await broker.detachDevice(device as unknown as HIDDevice);
    await expect(attachPromise).rejects.toThrow(/detached while waiting for hid\.attachResult/);

    // Even if the worker responds later, it should not resurrect the attach.
    port.emit({ type: "hid.attachResult", deviceId: attach!.deviceId, ok: true });
    await new Promise((r) => setTimeout(r, 0));
    expect(broker.getState().attachedDeviceIds).not.toContain(attach!.deviceId);
  });

  it("clamps oversized inputreport payloads to the expected report size before forwarding", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      const manager = new WebHidPassthroughManager({ hid: null });
      const broker = new WebHidBroker({ manager });
      const port = new FakePort();
      port.onPost = (msg) => {
        if ((msg as { type?: unknown }).type === "hid.attach") {
          const deviceId = (msg as { deviceId: number }).deviceId;
          port.emit({ type: "hid.attachResult", deviceId, ok: true });
        }
      };
      broker.attachWorkerPort(port as unknown as MessagePort);

      const device = new FakeHidDevice();
      // One input report (ID 1) with 4 bytes of payload (8*4 bits).
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
      await broker.attachDevice(device as unknown as HIDDevice);

      const huge = new Uint8Array(1024 * 1024);
      huge.set([1, 2, 3, 4], 0);
      device.dispatchInputReport(1, huge);

      const input = port.posted.find((p) => (p.msg as { type?: unknown }).type === "hid.inputReport") as
        | { msg: HidInputReportMessage; transfer?: Transferable[] }
        | undefined;
      expect(input).toBeTruthy();
      expect(input!.msg.reportId).toBe(1);
      expect(input!.msg.data.byteLength).toBe(4);
      expect(Array.from(input!.msg.data)).toEqual([1, 2, 3, 4]);
      expect(input!.transfer?.[0]).toBe(input!.msg.data.buffer);
    } finally {
      warn.mockRestore();
    }
  });

  it("zero-pads short inputreport payloads to the expected report size before forwarding", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      const manager = new WebHidPassthroughManager({ hid: null });
      const broker = new WebHidBroker({ manager });
      const port = new FakePort();
      broker.attachWorkerPort(port as unknown as MessagePort);

      const device = new FakeHidDevice();
      // One input report (ID 1) with 4 bytes of payload (8*4 bits).
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
      await broker.attachDevice(device as unknown as HIDDevice);

      device.dispatchInputReport(1, Uint8Array.of(9, 8));

      const input = port.posted.find((p) => (p.msg as { type?: unknown }).type === "hid.inputReport") as
        | { msg: HidInputReportMessage; transfer?: Transferable[] }
        | undefined;
      expect(input).toBeTruthy();
      expect(input!.msg.reportId).toBe(1);
      expect(Array.from(input!.msg.data)).toEqual([9, 8, 0, 0]);
      expect(input!.transfer?.[0]).toBe(input!.msg.data.buffer);
    } finally {
      warn.mockRestore();
    }
  });

  it("hard-caps unknown inputreport payload sizes before forwarding", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      const manager = new WebHidPassthroughManager({ hid: null });
      const broker = new WebHidBroker({ manager });
      const port = new FakePort();
      broker.attachWorkerPort(port as unknown as MessagePort);

      const device = new FakeHidDevice();
      // No input report metadata -> reportId will be treated as unknown.
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
      await broker.attachDevice(device as unknown as HIDDevice);

      const huge = new Uint8Array(1024 * 1024);
      huge.set([1, 2, 3], 0);
      device.dispatchInputReport(99, huge);

      const input = port.posted.find((p) => (p.msg as { type?: unknown }).type === "hid.inputReport") as
        | { msg: HidInputReportMessage; transfer?: Transferable[] }
        | undefined;
      expect(input).toBeTruthy();
      expect(input!.msg.reportId).toBe(99);
      expect(input!.msg.data.byteLength).toBe(64);
      expect(Array.from(input!.msg.data.slice(0, 3))).toEqual([1, 2, 3]);
    } finally {
      warn.mockRestore();
    }
  });

  it("drops inputreport events with invalid reportId values before forwarding", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      const manager = new WebHidPassthroughManager({ hid: null });
      const broker = new WebHidBroker({ manager });
      const port = new FakePort();
      broker.attachWorkerPort(port as unknown as MessagePort);

      const device = new FakeHidDevice();
      await broker.attachDevice(device as unknown as HIDDevice);

      const huge = new Uint8Array(1024 * 1024);
      huge.set([1, 2, 3], 0);
      device.dispatchInputReport(-1, huge);

      expect(port.posted.some((p) => (p.msg as { type?: unknown }).type === "hid.inputReport")).toBe(false);
      expect(warn.mock.calls.some((call) => String(call[0]).includes("invalid reportId"))).toBe(true);
    } finally {
      warn.mockRestore();
    }
  });

  it("clamps oversized inputreport payloads before writing to the SharedArrayBuffer input report ring", async () => {
    Object.defineProperty(globalThis, "crossOriginIsolated", { value: true, configurable: true });

    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      const manager = new WebHidPassthroughManager({ hid: null });
      const broker = new WebHidBroker({ manager });
      const port = new FakePort();
      port.onPost = (msg) => {
        if ((msg as { type?: unknown }).type === "hid.attach") {
          const deviceId = (msg as { deviceId: number }).deviceId;
          port.emit({ type: "hid.attachResult", deviceId, ok: true });
        }
      };
      broker.attachWorkerPort(port as unknown as MessagePort);

      const ringInit = port.posted.find((p) => (p.msg as { type?: unknown }).type === "hid.ring.init")?.msg as
        | { sab: SharedArrayBuffer; offsetBytes: number }
        | undefined;
      expect(ringInit).toBeTruthy();
      const inputReportRing = new RingBuffer(ringInit!.sab, ringInit!.offsetBytes);

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
      const id = await broker.attachDevice(device as unknown as HIDDevice);

      const huge = new Uint8Array(1024 * 1024);
      huge.set([1, 2, 3, 4], 0);
      device.dispatchInputReport(1, huge);

      const payload = inputReportRing.tryPop();
      expect(payload).toBeTruthy();
      const decoded = decodeHidInputReportRingRecord(payload!);
      expect(decoded).toBeTruthy();
      expect(decoded).toMatchObject({ deviceId: id, reportId: 1 });
      expect(decoded!.data.byteLength).toBe(4);
      expect(Array.from(decoded!.data)).toEqual([1, 2, 3, 4]);
    } finally {
      warn.mockRestore();
    }
  });

  it("zero-pads short inputreport payloads before writing to the SharedArrayBuffer input report ring", async () => {
    Object.defineProperty(globalThis, "crossOriginIsolated", { value: true, configurable: true });

    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      const manager = new WebHidPassthroughManager({ hid: null });
      const broker = new WebHidBroker({ manager });
      const port = new FakePort();
      broker.attachWorkerPort(port as unknown as MessagePort);

      const ringInit = port.posted.find((p) => (p.msg as { type?: unknown }).type === "hid.ring.init")?.msg as
        | { sab: SharedArrayBuffer; offsetBytes: number }
        | undefined;
      expect(ringInit).toBeTruthy();
      const inputReportRing = new RingBuffer(ringInit!.sab, ringInit!.offsetBytes);

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
      const id = await broker.attachDevice(device as unknown as HIDDevice);

      device.dispatchInputReport(1, Uint8Array.of(9, 8));

      const payload = inputReportRing.tryPop();
      expect(payload).toBeTruthy();
      const decoded = decodeHidInputReportRingRecord(payload!);
      expect(decoded).toBeTruthy();
      expect(decoded).toMatchObject({ deviceId: id, reportId: 1 });
      expect(Array.from(decoded!.data)).toEqual([9, 8, 0, 0]);
    } finally {
      warn.mockRestore();
    }
  });

  it("hard-caps unknown oversized inputreport payloads before writing to the SharedArrayBuffer input report ring", async () => {
    Object.defineProperty(globalThis, "crossOriginIsolated", { value: true, configurable: true });

    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      const manager = new WebHidPassthroughManager({ hid: null });
      const broker = new WebHidBroker({ manager });
      const port = new FakePort();
      broker.attachWorkerPort(port as unknown as MessagePort);

      const ringInit = port.posted.find((p) => (p.msg as { type?: unknown }).type === "hid.ring.init")?.msg as
        | { sab: SharedArrayBuffer; offsetBytes: number }
        | undefined;
      expect(ringInit).toBeTruthy();
      const inputReportRing = new RingBuffer(ringInit!.sab, ringInit!.offsetBytes);

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
      const id = await broker.attachDevice(device as unknown as HIDDevice);

      const huge = new Uint8Array(1024 * 1024);
      huge.set([1, 2, 3], 0);
      device.dispatchInputReport(99, huge);

      const payload = inputReportRing.tryPop();
      expect(payload).toBeTruthy();
      const decoded = decodeHidInputReportRingRecord(payload!);
      expect(decoded).toBeTruthy();
      expect(decoded).toMatchObject({ deviceId: id, reportId: 99 });
      expect(decoded!.data.byteLength).toBe(64);
      expect(Array.from(decoded!.data.slice(0, 3))).toEqual([1, 2, 3]);
    } finally {
      warn.mockRestore();
    }
  });

  it("detaches and surfaces errors when the worker reports attach failure", async () => {
    const manager = new WebHidPassthroughManager({ hid: null });
    const broker = new WebHidBroker({ manager });
    const port = new FakePort();
    port.onPost = (msg) => {
      if ((msg as { type?: unknown }).type === "hid.attach") {
        const deviceId = (msg as { deviceId: number }).deviceId;
        port.emit({ type: "hid.attachResult", deviceId, ok: false, error: "nope" });
      }
    };
    broker.attachWorkerPort(port as unknown as MessagePort);

    const device = new FakeHidDevice();
    await expect(broker.attachDevice(device as unknown as HIDDevice)).rejects.toThrow(/nope/);

    expect(device.open).toHaveBeenCalledTimes(1);
    expect(device.close).toHaveBeenCalledTimes(1);
    expect(device.opened).toBe(false);
    expect(manager.getState().attachedDevices).toHaveLength(0);
    expect(broker.isAttachedToWorker(device as unknown as HIDDevice)).toBe(false);

    // A failed attach should be cleaned up and must not leave an inputreport listener installed.
    const before = port.posted.length;
    device.dispatchInputReport(1, Uint8Array.of(1));
    expect(port.posted.length).toBe(before);

    // Best-effort detach is still sent to the worker to clear partial state.
    expect(port.posted.some((p) => (p.msg as { type?: unknown }).type === "hid.detach")).toBe(true);
  });

  it("times out when the worker never responds with hid.attachResult", async () => {
    vi.useFakeTimers();
    try {
      const manager = new WebHidPassthroughManager({ hid: null });
      const broker = new WebHidBroker({ manager, attachResultTimeoutMs: 10 });
      const port = new FakePort();
      port.autoAttachResult = false;
      broker.attachWorkerPort(port as unknown as MessagePort);

      const device = new FakeHidDevice();
      const attachPromise = broker.attachDevice(device as unknown as HIDDevice);
      const attachRejected = expect(attachPromise).rejects.toThrow(/timed out/i);

      // Allow the attach message to be posted before advancing timers.
      for (let i = 0; i < 10; i += 1) {
        if (port.posted.some((p) => (p.msg as { type?: unknown }).type === "hid.attach")) break;
        await Promise.resolve();
      }
      expect(port.posted.some((p) => (p.msg as { type?: unknown }).type === "hid.attach")).toBe(true);

      await vi.advanceTimersByTimeAsync(20);

      await attachRejected;
      expect(device.opened).toBe(false);
      expect(device.close).toHaveBeenCalledTimes(1);
      expect(manager.getState().attachedDevices).toHaveLength(0);
      expect(broker.isAttachedToWorker(device as unknown as HIDDevice)).toBe(false);
    } finally {
      vi.useRealTimers();
    }
  });

  it("cancels pending attaches when detachDevice is called before hid.attachResult", async () => {
    const manager = new WebHidPassthroughManager({ hid: null });
    const broker = new WebHidBroker({ manager });
    const port = new FakePort();
    port.autoAttachResult = false;
    broker.attachWorkerPort(port as unknown as MessagePort);

    const device = new FakeHidDevice();
    const attachPromise = broker.attachDevice(device as unknown as HIDDevice);
    const attachRejected = expect(attachPromise).rejects.toThrow(/detached while waiting for hid\.attachResult/i);

    for (let i = 0; i < 10; i += 1) {
      if (port.posted.some((p) => (p.msg as { type?: unknown }).type === "hid.attach")) break;
      await Promise.resolve();
    }
    expect(port.posted.some((p) => (p.msg as { type?: unknown }).type === "hid.attach")).toBe(true);

    await broker.detachDevice(device as unknown as HIDDevice);

    await attachRejected;
    expect(device.opened).toBe(false);
    expect(manager.getState().attachedDevices).toHaveLength(0);
    expect(broker.isAttachedToWorker(device as unknown as HIDDevice)).toBe(false);
    expect(port.posted.some((p) => (p.msg as { type?: unknown }).type === "hid.detach")).toBe(true);
  });

  it("cancels pending attaches when the manager reports the device detached before hid.attachResult", async () => {
    const manager = new WebHidPassthroughManager({ hid: null });
    const broker = new WebHidBroker({ manager });
    const port = new FakePort();
    port.autoAttachResult = false;
    broker.attachWorkerPort(port as unknown as MessagePort);

    const device = new FakeHidDevice();
    const attachPromise = broker.attachDevice(device as unknown as HIDDevice);
    const attachRejected = expect(attachPromise).rejects.toThrow(/disconnected while waiting for hid\.attachResult/i);

    for (let i = 0; i < 10; i += 1) {
      if (port.posted.some((p) => (p.msg as { type?: unknown }).type === "hid.attach")) break;
      await Promise.resolve();
    }
    expect(port.posted.some((p) => (p.msg as { type?: unknown }).type === "hid.attach")).toBe(true);

    await manager.detachDevice(device as unknown as HIDDevice);

    await attachRejected;
    expect(device.opened).toBe(false);
    expect(manager.getState().attachedDevices).toHaveLength(0);
    expect(broker.isAttachedToWorker(device as unknown as HIDDevice)).toBe(false);
  });

  it("forwards reports via SharedArrayBuffer rings when crossOriginIsolated", async () => {
    Object.defineProperty(globalThis, "crossOriginIsolated", { value: true, configurable: true });

    const manager = new WebHidPassthroughManager({ hid: null });
    const broker = new WebHidBroker({ manager });
    const port = new FakePort();
    port.onPost = (msg) => {
      if ((msg as { type?: unknown }).type === "hid.attach") {
        const deviceId = (msg as { deviceId: number }).deviceId;
        port.emit({ type: "hid.attachResult", deviceId, ok: true });
      }
    };
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
    expect(outputRing.dataCapacityBytes()).toBeGreaterThan(64 * 1024);

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
    // Use payload sizes large enough that a legacy 64KiB ring would drop after ~2 records.
    const payloadBytes = 24 * 1024;
    const p1 = new Uint8Array(payloadBytes).fill(0x11);
    const p2 = new Uint8Array(payloadBytes).fill(0x22);
    const p3 = new Uint8Array(payloadBytes).fill(0x33);
    expect(outputRing.push(id, HidReportType.Output, 7, p1)).toBe(true);
    expect(outputRing.push(id, HidReportType.Output, 8, p2)).toBe(true);
    expect(outputRing.push(id, HidReportType.Output, 9, p3)).toBe(true);
    expect(outputRing.dropped()).toBe(0);

    await new Promise((r) => setTimeout(r, 40));
    expect(device.sendReport).toHaveBeenCalledTimes(3);
    // Vitest stores calls as an untyped `unknown[][]`; cast via `unknown` so TypeScript
    // doesn't reject the conversion under stricter `--noUncheckedIndexedAccess` + TS 5.9+ rules.
    const calls = device.sendReport.mock.calls as unknown as Array<[number, Uint8Array<ArrayBufferLike>]>;
    expect(calls[0][0]).toBe(7);
    expect(calls[1][0]).toBe(8);
    expect(calls[2][0]).toBe(9);
    expect(calls[0][1].byteLength).toBe(payloadBytes);
    expect(calls[1][1].byteLength).toBe(payloadBytes);
    expect(calls[2][1].byteLength).toBe(payloadBytes);
    expect(calls[0][1][0]).toBe(0x11);
    expect(calls[1][1][0]).toBe(0x22);
    expect(calls[2][1][0]).toBe(0x33);

    broker.destroy();
  });

  it("respects configured output ring capacity", () => {
    Object.defineProperty(globalThis, "crossOriginIsolated", { value: true, configurable: true });

    const manager = new WebHidPassthroughManager({ hid: null });
    const broker = new WebHidBroker({ manager, outputRingCapacityBytes: 128 * 1024 });
    const port = new FakePort();
    broker.attachWorkerPort(port as unknown as MessagePort);

    const ringAttach = port.posted.find((p) => (p.msg as { type?: unknown }).type === "hid.ringAttach")?.msg as
      | { inputRing: SharedArrayBuffer; outputRing: SharedArrayBuffer }
      | undefined;
    expect(ringAttach).toBeTruthy();

    const outputRing = new HidReportRing(ringAttach!.outputRing);
    expect(outputRing.dataCapacityBytes()).toBe(128 * 1024);

    broker.destroy();
  });

  it("detaches rings and posts hid.ringDetach when the output ring is corrupted", async () => {
    Object.defineProperty(globalThis, "crossOriginIsolated", { value: true, configurable: true });

    const manager = new WebHidPassthroughManager({ hid: null });
    const broker = new WebHidBroker({ manager });
    const port = new FakePort();
    broker.attachWorkerPort(port as unknown as MessagePort);

    const ringAttach = port.posted.find((p) => (p.msg as { type?: unknown }).type === "hid.ringAttach")?.msg as
      | { inputRing: SharedArrayBuffer; outputRing: SharedArrayBuffer }
      | undefined;
    expect(ringAttach).toBeTruthy();

    const outputRingSab = ringAttach!.outputRing;
    const outputRing = new HidReportRing(outputRingSab);

    const device = new FakeHidDevice();
    const id = await broker.attachDevice(device as unknown as HIDDevice);

    // Push a valid-looking record, then corrupt the payload length so the consumer-side
    // decoder detects an invalid record that straddles the wrap boundary.
    outputRing.push(id, HidReportType.Output, 1, Uint8Array.of(1, 2, 3));
    const view = new DataView(outputRingSab, HID_REPORT_RING_CTRL_BYTES);
    view.setUint16(6, 0xffff, true);

    await new Promise((r) => setTimeout(r, 20));

    expect(port.posted.some((p) => (p.msg as { type?: unknown }).type === "hid.ringDetach")).toBe(true);

    // After detach, the broker should stop draining the output ring fast path.
    outputRing.push(id, HidReportType.Output, 2, Uint8Array.of(4));
    await new Promise((r) => setTimeout(r, 20));
    expect(device.sendReport).not.toHaveBeenCalled();

    // After ring detach, input reports should fall back to per-message postMessage as well.
    const before = port.posted.length;
    device.dispatchInputReport(3, Uint8Array.of(5));
    const input = port.posted
      .slice(before)
      .find((p) => (p.msg as { type?: unknown }).type === "hid.inputReport") as
      | { msg: HidInputReportMessage; transfer?: Transferable[] }
      | undefined;
    expect(input).toBeTruthy();
    expect(input!.msg.deviceId).toBe(id);
    expect(input!.msg.reportId).toBe(3);
    expect(Array.from(input!.msg.data)).toEqual([5]);

    broker.destroy();
  });

  it("detaches rings and posts hid.ringDetach when the input report ring is corrupted", async () => {
    Object.defineProperty(globalThis, "crossOriginIsolated", { value: true, configurable: true });

    const manager = new WebHidPassthroughManager({ hid: null });
    const broker = new WebHidBroker({ manager });
    const port = new FakePort();
    broker.attachWorkerPort(port as unknown as MessagePort);

    const ringInit = port.posted.find((p) => (p.msg as { type?: unknown }).type === "hid.ring.init")?.msg as
      | { sab: SharedArrayBuffer; offsetBytes: number }
      | undefined;
    expect(ringInit).toBeTruthy();

    // Corrupt the ring control header so the producer-side push path detects a bogus head/tail relationship.
    const ctrl = new Int32Array(ringInit!.sab, ringInit!.offsetBytes, ringCtrl.WORDS);
    Atomics.store(ctrl, ringCtrl.HEAD, 0x7fff_ffff);

    const device = new FakeHidDevice();
    const id = await broker.attachDevice(device as unknown as HIDDevice);

    const before = port.posted.length;
    device.dispatchInputReport(1, Uint8Array.of(9));

    const after = port.posted.slice(before);
    expect(after.some((p) => (p.msg as { type?: unknown }).type === "hid.ringDetach")).toBe(true);

    const input = after.find((p) => (p.msg as { type?: unknown }).type === "hid.inputReport") as
      | { msg: HidInputReportMessage; transfer?: Transferable[] }
      | undefined;
    expect(input).toBeTruthy();
    expect(input!.msg.deviceId).toBe(id);
    expect(input!.msg.reportId).toBe(1);
    expect(Array.from(input!.msg.data)).toEqual([9]);
    expect(input!.transfer?.[0]).toBe(input!.msg.data.buffer);

    broker.destroy();
  });

  it("computes hasInterruptOut based on output reports (feature-only does not require interrupt OUT)", async () => {
    const manager = new WebHidPassthroughManager({ hid: null });
    const broker = new WebHidBroker({ manager });
    const port = new FakePort();
    port.onPost = (msg) => {
      if ((msg as { type?: unknown }).type === "hid.attach") {
        const deviceId = (msg as { deviceId: number }).deviceId;
        port.emit({ type: "hid.attachResult", deviceId, ok: true });
      }
    };
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
      port.onPost = (msg) => {
        if ((msg as { type?: unknown }).type === "hid.attach") {
          const deviceId = (msg as { deviceId: number }).deviceId;
          port.emit({ type: "hid.attachResult", deviceId, ok: true });
        }
      };
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
        Reflect.deleteProperty(globalThis, "crossOriginIsolated");
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
      port.onPost = (msg) => {
        if ((msg as { type?: unknown }).type === "hid.attach") {
          const deviceId = (msg as { deviceId: number }).deviceId;
          port.emit({ type: "hid.attachResult", deviceId, ok: true });
        }
      };
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
        Reflect.deleteProperty(globalThis, "crossOriginIsolated");
      }
    }
  });

  it("bridges manager-initiated detaches (e.g. physical disconnect) to the worker", async () => {
    const manager = new WebHidPassthroughManager({ hid: null });
    const broker = new WebHidBroker({ manager });
    const port = new FakePort();
    port.onPost = (msg) => {
      if ((msg as { type?: unknown }).type === "hid.attach") {
        const deviceId = (msg as { deviceId: number }).deviceId;
        port.emit({ type: "hid.attachResult", deviceId, ok: true });
      }
    };
    broker.attachWorkerPort(port as unknown as MessagePort);

    const device = new FakeHidDevice();
    const id = await broker.attachDevice(device as unknown as HIDDevice);

    await manager.detachDevice(device as unknown as HIDDevice);
    // The broker reacts to manager detaches asynchronously.
    await new Promise((r) => setTimeout(r, 0));

    expect(
      port.posted.some(
        (p) =>
          (p.msg as { type?: unknown; deviceId?: unknown }).type === "hid.detach" &&
          (p.msg as { type?: unknown; deviceId?: unknown }).deviceId === id,
      ),
    ).toBe(true);

    const before = port.posted.length;
    device.dispatchInputReport(1, Uint8Array.of(1));
    expect(port.posted.length).toBe(before);
  });

  it("forgets deviceId mappings on explicit detaches so HIDDevice objects are not retained", async () => {
    const manager = new WebHidPassthroughManager({ hid: null });
    const broker = new WebHidBroker({ manager });
    const port = new FakePort();
    broker.attachWorkerPort(port as unknown as MessagePort);

    const device = new FakeHidDevice();
    const firstId = await broker.attachDevice(device as unknown as HIDDevice);

    await broker.detachDevice(device as unknown as HIDDevice);
    const secondId = broker.getDeviceId(device as unknown as HIDDevice);
    expect(secondId).not.toBe(firstId);

    // Detach is idempotent: it must not throw when called again.
    await expect(broker.detachDevice(device as unknown as HIDDevice)).resolves.toBeUndefined();
  });

  it("forgets deviceId mappings when the manager reports a device detached", async () => {
    const manager = new WebHidPassthroughManager({ hid: null });
    const broker = new WebHidBroker({ manager });
    const port = new FakePort();
    broker.attachWorkerPort(port as unknown as MessagePort);

    const device = new FakeHidDevice();
    const firstId = await broker.attachDevice(device as unknown as HIDDevice);

    await manager.detachDevice(device as unknown as HIDDevice);
    // The broker reacts to manager detaches asynchronously.
    await new Promise((r) => setTimeout(r, 0));

    const secondId = broker.getDeviceId(device as unknown as HIDDevice);
    expect(secondId).not.toBe(firstId);
  });

  it("handles hid.sendReport requests from the worker", async () => {
    const manager = new WebHidPassthroughManager({ hid: null });
    const broker = new WebHidBroker({ manager });
    const port = new FakePort();
    port.onPost = (msg) => {
      if ((msg as { type?: unknown }).type === "hid.attach") {
        const deviceId = (msg as { deviceId: number }).deviceId;
        port.emit({ type: "hid.attachResult", deviceId, ok: true });
      }
    };
    broker.attachWorkerPort(port as unknown as MessagePort);

    const device = new FakeHidDevice();
    const id = await broker.attachDevice(device as unknown as HIDDevice);

    port.emit({ type: "hid.sendReport", deviceId: id, reportType: "output", reportId: 7, data: Uint8Array.of(9) });
    port.emit({ type: "hid.sendReport", deviceId: id, reportType: "feature", reportId: 8, data: Uint8Array.of(10) });

    await new Promise((r) => setTimeout(r, 0));

    expect(device.sendReport).toHaveBeenCalledWith(7, Uint8Array.of(9));
    expect(device.sendFeatureReport).toHaveBeenCalledWith(8, Uint8Array.of(10));
  });

  it("clamps mismatched hid.sendReport payload sizes to the expected report size", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const manager = new WebHidPassthroughManager({ hid: null });
    const broker = new WebHidBroker({ manager });
    try {
      const port = new FakePort();
      port.onPost = (msg) => {
        if ((msg as { type?: unknown }).type === "hid.attach") {
          const deviceId = (msg as { deviceId: number }).deviceId;
          port.emit({ type: "hid.attachResult", deviceId, ok: true });
        }
      };
      broker.attachWorkerPort(port as unknown as MessagePort);

      const device = new FakeHidDevice();
      device.collections = [
        {
          usagePage: 1,
          usage: 2,
          type: "application",
          children: [],
          inputReports: [],
          outputReports: [
            {
              reportId: 7,
              items: [{ reportSize: 8, reportCount: 4 }],
            },
          ],
          featureReports: [
            {
              reportId: 8,
              items: [{ reportSize: 8, reportCount: 4 }],
            },
          ],
        },
      ] as unknown as HIDCollectionInfo[];

      const id = await broker.attachDevice(device as unknown as HIDDevice);

      const huge = new Uint8Array(1024);
      huge.set([1, 2, 3, 4], 0);

      port.emit({ type: "hid.sendReport", deviceId: id, reportType: "output", reportId: 7, data: huge });
      port.emit({ type: "hid.sendReport", deviceId: id, reportType: "feature", reportId: 8, data: Uint8Array.of(9, 8) });

      await flushMicrotasks();

      expect(device.sendReport).toHaveBeenCalledTimes(1);
      expect(device.sendReport.mock.calls[0]![0]).toBe(7);
      const outputData = device.sendReport.mock.calls[0]![1] as BufferSource;
      const outputBytes =
        outputData instanceof ArrayBuffer
          ? new Uint8Array(outputData)
          : new Uint8Array(outputData.buffer, outputData.byteOffset, outputData.byteLength);
      expect(Array.from(outputBytes)).toEqual([1, 2, 3, 4]);

      expect(device.sendFeatureReport).toHaveBeenCalledTimes(1);
      expect(device.sendFeatureReport.mock.calls[0]![0]).toBe(8);
      const featureData = device.sendFeatureReport.mock.calls[0]![1] as BufferSource;
      const featureBytes =
        featureData instanceof ArrayBuffer
          ? new Uint8Array(featureData)
          : new Uint8Array(featureData.buffer, featureData.byteOffset, featureData.byteLength);
      expect(Array.from(featureBytes)).toEqual([9, 8, 0, 0]);

      expect(warn).toHaveBeenCalledTimes(2);
    } finally {
      broker.destroy();
      warn.mockRestore();
    }
  });

  it("hard-caps unknown hid.sendReport payload sizes based on the reportId prefix byte", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const manager = new WebHidPassthroughManager({ hid: null });
    const broker = new WebHidBroker({ manager });
    try {
      const port = new FakePort();
      broker.attachWorkerPort(port as unknown as MessagePort);

      const device = new FakeHidDevice();
      // No output report metadata -> size will be treated as unknown for `hid.sendReport`.
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

      const id = await broker.attachDevice(device as unknown as HIDDevice);

      const huge = new Uint8Array(0xffff + 64);
      huge.set([1, 2, 3], 0);
      port.emit({ type: "hid.sendReport", deviceId: id, reportType: "output", reportId: 9, data: huge });

      await flushMicrotasks();

      expect(device.sendReport).toHaveBeenCalledTimes(1);
      expect(device.sendReport.mock.calls[0]![0]).toBe(9);
      const outputData = device.sendReport.mock.calls[0]![1] as BufferSource;
      const outputBytes =
        outputData instanceof ArrayBuffer
          ? new Uint8Array(outputData)
          : new Uint8Array(outputData.buffer, outputData.byteOffset, outputData.byteLength);
      // reportId != 0 => on-wire report includes a reportId prefix byte, so clamp payload to 0xfffe.
      expect(outputBytes.byteLength).toBe(0xfffe);
      expect(Array.from(outputBytes.slice(0, 3))).toEqual([1, 2, 3]);

      expect(warn).toHaveBeenCalledTimes(1);
    } finally {
      broker.destroy();
      warn.mockRestore();
    }
  });

  it("drains the SAB output ring when handling hid.sendReport messages to preserve report ordering", async () => {
    vi.useFakeTimers();
    Object.defineProperty(globalThis, "crossOriginIsolated", { value: true, configurable: true });
    let broker: WebHidBroker | null = null;
    try {
      const manager = new WebHidPassthroughManager({ hid: null });
      broker = new WebHidBroker({ manager });
      const port = new FakePort();
      broker.attachWorkerPort(port as unknown as MessagePort);

      const ringAttach = port.posted.find((p) => (p.msg as { type?: unknown }).type === "hid.ringAttach")?.msg as
        | { inputRing: SharedArrayBuffer; outputRing: SharedArrayBuffer }
        | undefined;
      expect(ringAttach).toBeTruthy();
      const outputRing = new HidReportRing(ringAttach!.outputRing);

      const device = new FakeHidDevice();
      const id = await broker.attachDevice(device as unknown as HIDDevice);

      // Queue report A in the SAB ring but do NOT advance timers; the background interval
      // drain must not run.
      expect(outputRing.push(id, HidReportType.Output, 1, Uint8Array.of(0xaa))).toBe(true);

      // Immediately send report B via structured message (fallback path when the ring is full or
      // a record is too large). The broker should drain the ring synchronously so A runs first.
      port.emit({ type: "hid.sendReport", deviceId: id, reportType: "output", reportId: 2, data: Uint8Array.of(0xbb) });

      // Allow the per-device queue runner to flush.
      await flushMicrotasks();

      expect(device.sendReport).toHaveBeenCalledTimes(2);
      expect(device.sendReport).toHaveBeenNthCalledWith(1, 1, Uint8Array.of(0xaa));
      expect(device.sendReport).toHaveBeenNthCalledWith(2, 2, Uint8Array.of(0xbb));
    } finally {
      broker?.destroy();
      vi.useRealTimers();
    }
  });

  it("drains the SAB output ring when handling hid.getFeatureReport messages to preserve report ordering", async () => {
    vi.useFakeTimers();
    Object.defineProperty(globalThis, "crossOriginIsolated", { value: true, configurable: true });
    let broker: WebHidBroker | null = null;
    try {
      const manager = new WebHidPassthroughManager({ hid: null });
      broker = new WebHidBroker({ manager });
      const port = new FakePort();
      broker.attachWorkerPort(port as unknown as MessagePort);

      const ringAttach = port.posted.find((p) => (p.msg as { type?: unknown }).type === "hid.ringAttach")?.msg as
        | { inputRing: SharedArrayBuffer; outputRing: SharedArrayBuffer }
        | undefined;
      expect(ringAttach).toBeTruthy();
      const outputRing = new HidReportRing(ringAttach!.outputRing);

      const device = new FakeHidDevice();
      const order: string[] = [];
      device.sendReport.mockImplementation(async () => {
        order.push("sendReport");
      });
      device.receiveFeatureReport.mockImplementation(async () => {
        order.push("receiveFeatureReport");
        return new DataView(Uint8Array.of(1, 2, 3).buffer);
      });

      const id = await broker.attachDevice(device as unknown as HIDDevice);

      // Queue an output report send in the SAB ring, then immediately enqueue a getFeatureReport
      // request. The ring send must execute first even if the background timer isn't running.
      expect(outputRing.push(id, HidReportType.Output, 1, Uint8Array.of(0xaa))).toBe(true);
      port.emit({ type: "hid.getFeatureReport", requestId: 1, deviceId: id, reportId: 7 });

      await flushMicrotasks();

      expect(order).toEqual(["sendReport", "receiveFeatureReport"]);
      expect(device.receiveFeatureReport).toHaveBeenCalledWith(7);

      const result = port.posted.find(
        (p) =>
          (p.msg as { type?: unknown; requestId?: unknown }).type === "hid.featureReportResult" &&
          (p.msg as { type?: unknown; requestId?: unknown }).requestId === 1,
      ) as { msg: HidFeatureReportResultMessage; transfer?: Transferable[] } | undefined;
      expect(result).toBeTruthy();
      expect(result!.msg).toMatchObject({ deviceId: id, reportId: 7, requestId: 1, ok: true });
      expect(Array.from(result!.msg.data!)).toEqual([1, 2, 3]);
      expect(result!.transfer?.[0]).toBe(result!.msg.data!.buffer);
    } finally {
      broker?.destroy();
      vi.useRealTimers();
    }
  });

  it("executes output/feature reports sequentially per device across message and ring delivery paths", async () => {
    Object.defineProperty(globalThis, "crossOriginIsolated", { value: true, configurable: true });

    const manager = new WebHidPassthroughManager({ hid: null });
    const broker = new WebHidBroker({ manager });
    const port = new FakePort();
    broker.attachWorkerPort(port as unknown as MessagePort);

    const ringAttach = port.posted.find((p) => (p.msg as { type?: unknown }).type === "hid.ringAttach")?.msg as
      | { inputRing: SharedArrayBuffer; outputRing: SharedArrayBuffer }
      | undefined;
    expect(ringAttach).toBeTruthy();
    const outputRing = new HidReportRing(ringAttach!.outputRing);

    const device = new FakeHidDevice();
    const first = deferred<void>();
    device.sendReport.mockImplementationOnce(() => first.promise);
    const id = await broker.attachDevice(device as unknown as HIDDevice);

    // First report via structured postMessage.
    port.emit({ type: "hid.sendReport", deviceId: id, reportType: "output", reportId: 1, data: Uint8Array.of(1) });
    await new Promise((r) => setTimeout(r, 0));
    expect(device.sendReport).toHaveBeenCalledTimes(1);
    expect(device.sendFeatureReport).toHaveBeenCalledTimes(0);

    // Second report via SAB ring: must not be invoked until the first resolves.
    outputRing.push(id, HidReportType.Feature, 2, Uint8Array.of(2));
    await new Promise((r) => setTimeout(r, 20));
    expect(device.sendFeatureReport).toHaveBeenCalledTimes(0);

    first.resolve(undefined);
    await new Promise((r) => setTimeout(r, 0));

    expect(device.sendFeatureReport).toHaveBeenCalledTimes(1);
    expect(device.sendFeatureReport).toHaveBeenCalledWith(2, Uint8Array.of(2));

    broker.destroy();
  });

  it("executes hid.sendReport messages sequentially per device", async () => {
    const manager = new WebHidPassthroughManager({ hid: null });
    const broker = new WebHidBroker({ manager });
    const port = new FakePort();
    broker.attachWorkerPort(port as unknown as MessagePort);

    const device = new FakeHidDevice();
    const first = deferred<void>();
    device.sendReport.mockImplementationOnce(() => first.promise);
    const id = await broker.attachDevice(device as unknown as HIDDevice);

    port.emit({ type: "hid.sendReport", deviceId: id, reportType: "output", reportId: 1, data: Uint8Array.of(1) });
    port.emit({ type: "hid.sendReport", deviceId: id, reportType: "output", reportId: 2, data: Uint8Array.of(2) });

    await new Promise((r) => setTimeout(r, 0));
    expect(device.sendReport).toHaveBeenCalledTimes(1);

    first.resolve(undefined);
    await new Promise((r) => setTimeout(r, 0));

    expect(device.sendReport).toHaveBeenCalledTimes(2);
    expect(device.sendReport).toHaveBeenNthCalledWith(1, 1, Uint8Array.of(1));
    expect(device.sendReport).toHaveBeenNthCalledWith(2, 2, Uint8Array.of(2));

    broker.destroy();
  });

  it("bounds pending per-device output sends when sendReport stalls", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      const limit = 3;
      const manager = new WebHidPassthroughManager({ hid: null });
      const broker = new WebHidBroker({ manager, maxPendingDeviceSends: limit });
      const port = new FakePort();
      broker.attachWorkerPort(port as unknown as MessagePort);

      const device = new FakeHidDevice();
      const first = deferred<void>();
      device.sendReport.mockImplementationOnce(() => first.promise);
      const id = await broker.attachDevice(device as unknown as HIDDevice);

      // First report begins executing and stalls; subsequent reports are queued.
      port.emit({ type: "hid.sendReport", deviceId: id, reportType: "output", reportId: 1, data: Uint8Array.of(1) });
      for (let i = 2; i <= 10; i += 1) {
        port.emit({ type: "hid.sendReport", deviceId: id, reportType: "output", reportId: i, data: Uint8Array.of(i) });
      }

      await new Promise((r) => setTimeout(r, 0));
      expect(device.sendReport).toHaveBeenCalledTimes(1);

      expect(broker.getOutputSendStats().droppedTotal).toBeGreaterThan(0);
      expect(warn.mock.calls.some((call) => String(call[0]).includes(`deviceId=${id}`))).toBe(true);

      first.resolve(undefined);
      await new Promise((r) => setTimeout(r, 0));
      expect(device.sendReport.mock.calls.length).toBe(limit + 1);
      // Drop policy is deterministic: drop newest when the queue is full, so the
      // earliest queued reports should be the ones delivered after the in-flight
      // send completes.
      expect(device.sendReport.mock.calls.map((call) => call[0])).toEqual([1, 2, 3, 4]);
      expect(
        warn.mock.calls.filter((call) => String(call[0]).includes(`[webhid] Dropping queued HID report tasks for deviceId=${id}`)).length,
      ).toBe(1);
      const warnMsg = warn.mock.calls
        .map((call) => String(call[0]))
        .find((msg) => msg.includes(`[webhid] Dropping queued HID report tasks for deviceId=${id}`));
      expect(warnMsg).toBeTruthy();
      expect(warnMsg).toContain(`pending=${limit}`);
      expect(warnMsg).toContain(`maxPendingDeviceSends=${limit}`);

      broker.destroy();
    } finally {
      warn.mockRestore();
    }
  });

  it("bounds pending per-device output sends by bytes when sendReport stalls", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      const maxPendingSendBytesPerDevice = 4;
      const manager = new WebHidPassthroughManager({ hid: null });
      const broker = new WebHidBroker({ manager, maxPendingDeviceSends: 100, maxPendingSendBytesPerDevice });
      const port = new FakePort();
      broker.attachWorkerPort(port as unknown as MessagePort);

      const device = new FakeHidDevice();
      const first = deferred<void>();
      device.sendReport.mockImplementationOnce(() => first.promise);
      const id = await broker.attachDevice(device as unknown as HIDDevice);

      // First report begins executing and stalls; subsequent reports are queued and capped by bytes.
      port.emit({ type: "hid.sendReport", deviceId: id, reportType: "output", reportId: 1, data: Uint8Array.of(1) });
      // Each of these reports retains 3 bytes.
      port.emit({ type: "hid.sendReport", deviceId: id, reportType: "output", reportId: 2, data: Uint8Array.of(1, 2, 3) });
      port.emit({ type: "hid.sendReport", deviceId: id, reportType: "output", reportId: 3, data: Uint8Array.of(4, 5, 6) });

      await new Promise((r) => setTimeout(r, 0));
      expect(device.sendReport).toHaveBeenCalledTimes(1);

      const stats = broker.getOutputSendStats();
      const perDevice = stats.devices.find((d) => d.deviceId === id);
      expect(perDevice).toBeTruthy();
      // Only one 3-byte report should be queued; the next would exceed the 4-byte budget.
      expect(perDevice!.pending).toBe(1);
      expect(perDevice!.pendingBytes).toBe(3);
      expect(stats.pendingBytesTotal).toBe(3);
      expect(stats.droppedTotal).toBeGreaterThan(0);

      const warnMsg = warn.mock.calls
        .map((call) => String(call[0]))
        .find((msg) => msg.includes(`[webhid] Dropping queued HID report tasks for deviceId=${id}`));
      expect(warnMsg).toBeTruthy();
      expect(warnMsg).toContain("pendingBytes=3");
      expect(warnMsg).toContain(`maxPendingSendBytesPerDevice=${maxPendingSendBytesPerDevice}`);

      first.resolve(undefined);
      await new Promise((r) => setTimeout(r, 0));

      // Only the first + second report should run; the third is dropped.
      expect(device.sendReport.mock.calls.map((call) => call[0])).toEqual([1, 2]);

      broker.destroy();
    } finally {
      warn.mockRestore();
    }
  });

  it("bounds pending per-device feature sends when sendFeatureReport stalls", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      const limit = 3;
      const manager = new WebHidPassthroughManager({ hid: null });
      const broker = new WebHidBroker({ manager, maxPendingDeviceSends: limit });
      const port = new FakePort();
      broker.attachWorkerPort(port as unknown as MessagePort);

      const device = new FakeHidDevice();
      const first = deferred<void>();
      device.sendFeatureReport.mockImplementationOnce(() => first.promise);
      const id = await broker.attachDevice(device as unknown as HIDDevice);

      port.emit({ type: "hid.sendReport", deviceId: id, reportType: "feature", reportId: 1, data: Uint8Array.of(1) });
      for (let i = 2; i <= 10; i += 1) {
        port.emit({ type: "hid.sendReport", deviceId: id, reportType: "feature", reportId: i, data: Uint8Array.of(i) });
      }

      await new Promise((r) => setTimeout(r, 0));
      expect(device.sendFeatureReport).toHaveBeenCalledTimes(1);

      expect(broker.getOutputSendStats().droppedTotal).toBeGreaterThan(0);
      expect(warn.mock.calls.some((call) => String(call[0]).includes(`deviceId=${id}`))).toBe(true);

      first.resolve(undefined);
      await new Promise((r) => setTimeout(r, 0));
      expect(device.sendFeatureReport.mock.calls.length).toBe(limit + 1);
      expect(device.sendFeatureReport.mock.calls.map((call) => call[0])).toEqual([1, 2, 3, 4]);
      expect(
        warn.mock.calls.filter((call) => String(call[0]).includes(`[webhid] Dropping queued HID report tasks for deviceId=${id}`)).length,
      ).toBe(1);
      const warnMsg = warn.mock.calls
        .map((call) => String(call[0]))
        .find((msg) => msg.includes(`[webhid] Dropping queued HID report tasks for deviceId=${id}`));
      expect(warnMsg).toBeTruthy();
      expect(warnMsg).toContain(`pending=${limit}`);
      expect(warnMsg).toContain(`maxPendingDeviceSends=${limit}`);

      broker.destroy();
    } finally {
      warn.mockRestore();
    }
  });

  it("bounds pending per-device output sends when sendReport stalls (output ring path)", async () => {
    Object.defineProperty(globalThis, "crossOriginIsolated", { value: true, configurable: true });
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      const limit = 3;
      const manager = new WebHidPassthroughManager({ hid: null });
      const broker = new WebHidBroker({ manager, maxPendingDeviceSends: limit });
      const port = new FakePort();
      broker.attachWorkerPort(port as unknown as MessagePort);

      const ringAttach = port.posted.find((p) => (p.msg as { type?: unknown }).type === "hid.ringAttach")?.msg as
        | { inputRing: SharedArrayBuffer; outputRing: SharedArrayBuffer }
        | undefined;
      expect(ringAttach).toBeTruthy();
      const outputRing = new HidReportRing(ringAttach!.outputRing);

      const device = new FakeHidDevice();
      const first = deferred<void>();
      device.sendReport.mockImplementationOnce(() => first.promise);
      const id = await broker.attachDevice(device as unknown as HIDDevice);

      expect(outputRing.push(id, HidReportType.Output, 1, Uint8Array.of(1))).toBe(true);
      for (let i = 2; i <= 10; i += 1) {
        expect(outputRing.push(id, HidReportType.Output, i, Uint8Array.of(i))).toBe(true);
      }

      await new Promise((r) => setTimeout(r, 20));
      expect(device.sendReport).toHaveBeenCalledTimes(1);

      expect(broker.getOutputSendStats().droppedTotal).toBeGreaterThan(0);
      expect(warn.mock.calls.some((call) => String(call[0]).includes(`deviceId=${id}`))).toBe(true);

      first.resolve(undefined);
      await new Promise((r) => setTimeout(r, 20));
      expect(device.sendReport.mock.calls.length).toBe(limit + 1);
      expect(device.sendReport.mock.calls.map((call) => call[0])).toEqual([1, 2, 3, 4]);
      expect(
        warn.mock.calls.filter((call) => String(call[0]).includes(`[webhid] Dropping queued HID report tasks for deviceId=${id}`)).length,
      ).toBe(1);
      const warnMsg = warn.mock.calls
        .map((call) => String(call[0]))
        .find((msg) => msg.includes(`[webhid] Dropping queued HID report tasks for deviceId=${id}`));
      expect(warnMsg).toBeTruthy();
      expect(warnMsg).toContain(`pending=${limit}`);
      expect(warnMsg).toContain(`maxPendingDeviceSends=${limit}`);

      broker.destroy();
    } finally {
      warn.mockRestore();
    }
  });

  it("bounds pending per-device feature sends when sendFeatureReport stalls (output ring path)", async () => {
    Object.defineProperty(globalThis, "crossOriginIsolated", { value: true, configurable: true });
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      const limit = 3;
      const manager = new WebHidPassthroughManager({ hid: null });
      const broker = new WebHidBroker({ manager, maxPendingDeviceSends: limit });
      const port = new FakePort();
      broker.attachWorkerPort(port as unknown as MessagePort);

      const ringAttach = port.posted.find((p) => (p.msg as { type?: unknown }).type === "hid.ringAttach")?.msg as
        | { inputRing: SharedArrayBuffer; outputRing: SharedArrayBuffer }
        | undefined;
      expect(ringAttach).toBeTruthy();
      const outputRing = new HidReportRing(ringAttach!.outputRing);

      const device = new FakeHidDevice();
      const first = deferred<void>();
      device.sendFeatureReport.mockImplementationOnce(() => first.promise);
      const id = await broker.attachDevice(device as unknown as HIDDevice);

      expect(outputRing.push(id, HidReportType.Feature, 1, Uint8Array.of(1))).toBe(true);
      for (let i = 2; i <= 10; i += 1) {
        expect(outputRing.push(id, HidReportType.Feature, i, Uint8Array.of(i))).toBe(true);
      }

      await new Promise((r) => setTimeout(r, 20));
      expect(device.sendFeatureReport).toHaveBeenCalledTimes(1);

      expect(broker.getOutputSendStats().droppedTotal).toBeGreaterThan(0);
      expect(warn.mock.calls.some((call) => String(call[0]).includes(`deviceId=${id}`))).toBe(true);

      first.resolve(undefined);
      await new Promise((r) => setTimeout(r, 20));
      expect(device.sendFeatureReport.mock.calls.length).toBe(limit + 1);
      expect(device.sendFeatureReport.mock.calls.map((call) => call[0])).toEqual([1, 2, 3, 4]);
      expect(
        warn.mock.calls.filter((call) => String(call[0]).includes(`[webhid] Dropping queued HID report tasks for deviceId=${id}`)).length,
      ).toBe(1);
      const warnMsg = warn.mock.calls
        .map((call) => String(call[0]))
        .find((msg) => msg.includes(`[webhid] Dropping queued HID report tasks for deviceId=${id}`));
      expect(warnMsg).toBeTruthy();
      expect(warnMsg).toContain(`pending=${limit}`);
      expect(warnMsg).toContain(`maxPendingDeviceSends=${limit}`);

      broker.destroy();
    } finally {
      warn.mockRestore();
    }
  });

  it("responds with ok:false when hid.getFeatureReport is dropped due to a full send queue", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      const limit = 1;
      const manager = new WebHidPassthroughManager({ hid: null });
      const broker = new WebHidBroker({ manager, maxPendingDeviceSends: limit });
      const port = new FakePort();
      broker.attachWorkerPort(port as unknown as MessagePort);

      const status = new Int32Array(new SharedArrayBuffer(64 * 4));
      broker.setInputReportRing(null, status);

      const device = new FakeHidDevice();
      device.sendReport.mockImplementationOnce(() => new Promise<void>(() => {}));
      const id = await broker.attachDevice(device as unknown as HIDDevice);

      // Start one in-flight send that never resolves.
      port.emit({ type: "hid.sendReport", deviceId: id, reportType: "output", reportId: 1, data: Uint8Array.of(1) });
      await new Promise((r) => setTimeout(r, 0));
      expect(device.sendReport).toHaveBeenCalledTimes(1);

      // Fill the bounded pending queue.
      port.emit({ type: "hid.sendReport", deviceId: id, reportType: "output", reportId: 2, data: Uint8Array.of(2) });
      await new Promise((r) => setTimeout(r, 0));
      expect(broker.getOutputSendStats().pendingTotal).toBe(limit);

      // This getFeatureReport must be dropped immediately and responded to with ok:false.
      port.emit({ type: "hid.getFeatureReport", requestId: 99, deviceId: id, reportId: 7 });
      await new Promise((r) => setTimeout(r, 0));

      const res = port.posted
        .slice()
        .reverse()
        .find(
          (p) =>
            (p.msg as { type?: unknown; requestId?: unknown }).type === "hid.featureReportResult" &&
            (p.msg as { type?: unknown; requestId?: unknown }).requestId === 99,
        ) as { msg: HidFeatureReportResultMessage; transfer?: Transferable[] } | undefined;
      expect(res).toBeTruthy();
      expect(res!.msg).toMatchObject({ requestId: 99, deviceId: id, reportId: 7, ok: false });
      expect(String(res!.msg.error)).toMatch(/Too many pending HID report tasks/i);

      expect(broker.getOutputSendStats().droppedTotal).toBeGreaterThan(0);
      expect(warn.mock.calls.some((call) => String(call[0]).includes(`deviceId=${id}`))).toBe(true);
      expect(
        warn.mock.calls.filter((call) => String(call[0]).includes(`[webhid] Dropping queued HID report tasks for deviceId=${id}`)).length,
      ).toBe(1);
      const warnMsg = warn.mock.calls
        .map((call) => String(call[0]))
        .find((msg) => msg.includes(`[webhid] Dropping queued HID report tasks for deviceId=${id}`));
      expect(warnMsg).toBeTruthy();
      expect(warnMsg).toContain("pending=1");
      expect(warnMsg).toContain("maxPendingDeviceSends=1");
      expect(Atomics.load(status, StatusIndex.IoHidOutputReportDropCounter)).toBeGreaterThan(0);

      broker.destroy();
    } finally {
      warn.mockRestore();
    }
  });

  it("executes output ring reports sequentially per device", async () => {
    Object.defineProperty(globalThis, "crossOriginIsolated", { value: true, configurable: true });

    const manager = new WebHidPassthroughManager({ hid: null });
    const broker = new WebHidBroker({ manager });
    const port = new FakePort();
    broker.attachWorkerPort(port as unknown as MessagePort);

    const ringAttach = port.posted.find((p) => (p.msg as { type?: unknown }).type === "hid.ringAttach")?.msg as
      | { inputRing: SharedArrayBuffer; outputRing: SharedArrayBuffer }
      | undefined;
    expect(ringAttach).toBeTruthy();
    const outputRing = new HidReportRing(ringAttach!.outputRing);

    const device = new FakeHidDevice();
    const first = deferred<void>();
    device.sendReport.mockImplementationOnce(() => first.promise);
    const id = await broker.attachDevice(device as unknown as HIDDevice);

    expect(outputRing.push(id, HidReportType.Output, 1, Uint8Array.of(1))).toBe(true);
    expect(outputRing.push(id, HidReportType.Output, 2, Uint8Array.of(2))).toBe(true);
    await new Promise((r) => setTimeout(r, 20));
    expect(device.sendReport).toHaveBeenCalledTimes(1);

    first.resolve(undefined);
    await new Promise((r) => setTimeout(r, 0));
    expect(device.sendReport).toHaveBeenCalledTimes(2);
    expect(device.sendReport).toHaveBeenNthCalledWith(1, 1, Uint8Array.of(1));
    expect(device.sendReport).toHaveBeenNthCalledWith(2, 2, Uint8Array.of(2));

    broker.destroy();
  });

  it("hard-caps unknown output ring report payload sizes based on the reportId prefix byte", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    Object.defineProperty(globalThis, "crossOriginIsolated", { value: true, configurable: true });
    try {
      const manager = new WebHidPassthroughManager({ hid: null });
      const broker = new WebHidBroker({ manager });
      const port = new FakePort();
      broker.attachWorkerPort(port as unknown as MessagePort);

      const ringAttach = port.posted.find((p) => (p.msg as { type?: unknown }).type === "hid.ringAttach")?.msg as
        | { inputRing: SharedArrayBuffer; outputRing: SharedArrayBuffer }
        | undefined;
      expect(ringAttach).toBeTruthy();
      const outputRing = new HidReportRing(ringAttach!.outputRing);

      const device = new FakeHidDevice();
      // No output report metadata -> report size will be treated as unknown.
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
      const id = await broker.attachDevice(device as unknown as HIDDevice);

      const huge = new Uint8Array(0xffff);
      huge.set([1, 2, 3], 0);
      expect(outputRing.push(id, HidReportType.Output, 9, huge)).toBe(true);

      await new Promise((r) => setTimeout(r, 30));

      expect(device.sendReport).toHaveBeenCalledTimes(1);
      expect(device.sendReport.mock.calls[0]![0]).toBe(9);
      const outputData = device.sendReport.mock.calls[0]![1] as BufferSource;
      const outputBytes =
        outputData instanceof ArrayBuffer
          ? new Uint8Array(outputData)
          : new Uint8Array(outputData.buffer, outputData.byteOffset, outputData.byteLength);
      // reportId != 0 => on-wire report includes a reportId prefix byte, so clamp payload to 0xfffe.
      expect(outputBytes.byteLength).toBe(0xfffe);
      expect(Array.from(outputBytes.slice(0, 3))).toEqual([1, 2, 3]);

      expect(warn).toHaveBeenCalledTimes(1);

      broker.destroy();
    } finally {
      warn.mockRestore();
    }
  });

  it("queues hid.sendReport behind output ring sends for the same device", async () => {
    Object.defineProperty(globalThis, "crossOriginIsolated", { value: true, configurable: true });

    const manager = new WebHidPassthroughManager({ hid: null });
    const broker = new WebHidBroker({ manager });
    const port = new FakePort();
    broker.attachWorkerPort(port as unknown as MessagePort);

    const ringAttach = port.posted.find((p) => (p.msg as { type?: unknown }).type === "hid.ringAttach")?.msg as
      | { inputRing: SharedArrayBuffer; outputRing: SharedArrayBuffer }
      | undefined;
    expect(ringAttach).toBeTruthy();
    const outputRing = new HidReportRing(ringAttach!.outputRing);

    const device = new FakeHidDevice();
    const first = deferred<void>();
    device.sendReport.mockImplementationOnce(() => first.promise);
    const id = await broker.attachDevice(device as unknown as HIDDevice);

    expect(outputRing.push(id, HidReportType.Output, 1, Uint8Array.of(1))).toBe(true);
    await new Promise((r) => setTimeout(r, 20));
    expect(device.sendReport).toHaveBeenCalledTimes(1);

    port.emit({ type: "hid.sendReport", deviceId: id, reportType: "feature", reportId: 2, data: Uint8Array.of(2) });
    await new Promise((r) => setTimeout(r, 0));
    expect(device.sendFeatureReport).toHaveBeenCalledTimes(0);

    first.resolve(undefined);
    await new Promise((r) => setTimeout(r, 0));
    expect(device.sendFeatureReport).toHaveBeenCalledTimes(1);

    broker.destroy();
  });

  it("drains the output ring before enqueuing hid.sendReport messages (preserves ring->message order even without timer)", async () => {
    Object.defineProperty(globalThis, "crossOriginIsolated", { value: true, configurable: true });

    const manager = new WebHidPassthroughManager({ hid: null });
    const broker = new WebHidBroker({ manager });
    const port = new FakePort();
    broker.attachWorkerPort(port as unknown as MessagePort);

    const ringAttach = port.posted.find((p) => (p.msg as { type?: unknown }).type === "hid.ringAttach")?.msg as
      | { inputRing: SharedArrayBuffer; outputRing: SharedArrayBuffer }
      | undefined;
    expect(ringAttach).toBeTruthy();
    const outputRing = new HidReportRing(ringAttach!.outputRing);

    const device = new FakeHidDevice();
    const id = await broker.attachDevice(device as unknown as HIDDevice);

    // Push a ring record and immediately deliver a message report without giving the periodic ring drain
    // timer a chance to run. The broker should drain the ring synchronously while handling the message so
    // the ring record is sent first.
    expect(outputRing.push(id, HidReportType.Output, 1, Uint8Array.of(1))).toBe(true);
    port.emit({ type: "hid.sendReport", deviceId: id, reportType: "output", reportId: 2, data: Uint8Array.of(2) });

    await new Promise((r) => setTimeout(r, 40));

    expect(device.sendReport).toHaveBeenCalledTimes(2);
    expect(device.sendReport).toHaveBeenNthCalledWith(1, 1, Uint8Array.of(1));
    expect(device.sendReport).toHaveBeenNthCalledWith(2, 2, Uint8Array.of(2));

    broker.destroy();
  });

  it("drains the output ring before enqueuing hid.getFeatureReport messages (preserves ring->featureReport order even without timer)", async () => {
    Object.defineProperty(globalThis, "crossOriginIsolated", { value: true, configurable: true });

    const manager = new WebHidPassthroughManager({ hid: null });
    const broker = new WebHidBroker({ manager });
    const port = new FakePort();
    broker.attachWorkerPort(port as unknown as MessagePort);

    const ringAttach = port.posted.find((p) => (p.msg as { type?: unknown }).type === "hid.ringAttach")?.msg as
      | { inputRing: SharedArrayBuffer; outputRing: SharedArrayBuffer }
      | undefined;
    expect(ringAttach).toBeTruthy();
    const outputRing = new HidReportRing(ringAttach!.outputRing);

    const device = new FakeHidDevice();
    const first = deferred<void>();
    device.sendReport.mockImplementationOnce(() => first.promise);
    device.receiveFeatureReport.mockImplementationOnce(async () => new DataView(new Uint8Array([9]).buffer));
    const id = await broker.attachDevice(device as unknown as HIDDevice);

    expect(outputRing.push(id, HidReportType.Output, 1, Uint8Array.of(1))).toBe(true);
    port.emit({ type: "hid.getFeatureReport", requestId: 1, deviceId: id, reportId: 2 });

    await new Promise((r) => setTimeout(r, 0));
    expect(device.receiveFeatureReport).toHaveBeenCalledTimes(0);

    first.resolve(undefined);
    await new Promise((r) => setTimeout(r, 0));

    expect(device.receiveFeatureReport).toHaveBeenCalledTimes(1);
    expect(device.receiveFeatureReport).toHaveBeenCalledWith(2);
    expect(port.posted.some((p) => (p.msg as { type?: unknown }).type === "hid.featureReportResult")).toBe(true);

    broker.destroy();
  });

  it("uses hid.sendReport.outputRingTail to preserve message->ring ordering when the ring write races ahead of message delivery", async () => {
    Object.defineProperty(globalThis, "crossOriginIsolated", { value: true, configurable: true });
    vi.useFakeTimers();
    try {
      const manager = new WebHidPassthroughManager({ hid: null });
      const broker = new WebHidBroker({ manager });
      const port = new FakePort();
      broker.attachWorkerPort(port as unknown as MessagePort);

      const ringAttach = port.posted.find((p) => (p.msg as { type?: unknown }).type === "hid.ringAttach")?.msg as
        | { inputRing: SharedArrayBuffer; outputRing: SharedArrayBuffer }
        | undefined;
      expect(ringAttach).toBeTruthy();
      const outputRing = new HidReportRing(ringAttach!.outputRing);

      const device = new FakeHidDevice();
      const id = await broker.attachDevice(device as unknown as HIDDevice);

      // Simulate the worker posting a fallback message and then producing a ring record before the main thread
      // delivers the message event. `outputRingTail` snapshots the tail at post time so the main thread drains
      // only earlier ring records before enqueuing the message.
      const tailAtPost = outputRing.debugState().tail;
      setTimeout(() => {
        port.emit({
          type: "hid.sendReport",
          deviceId: id,
          reportType: "output",
          reportId: 1,
          data: Uint8Array.of(1),
          outputRingTail: tailAtPost,
        });
      }, 0);

      expect(outputRing.push(id, HidReportType.Output, 2, Uint8Array.of(2))).toBe(true);

      vi.advanceTimersByTime(1);
      await flushMicrotasks();

      expect(device.sendReport).toHaveBeenCalledTimes(1);
      expect(device.sendReport).toHaveBeenCalledWith(1, Uint8Array.of(1));

      // Let the periodic ring drain enqueue the later record; it must run after the message send.
      vi.advanceTimersByTime(20);
      await flushMicrotasks();

      expect(device.sendReport).toHaveBeenCalledTimes(2);
      expect(device.sendReport).toHaveBeenNthCalledWith(1, 1, Uint8Array.of(1));
      expect(device.sendReport).toHaveBeenNthCalledWith(2, 2, Uint8Array.of(2));

      broker.destroy();
    } finally {
      vi.useRealTimers();
    }
  });

  it("does not drain ring records beyond hid.sendReport.outputRingTail when the periodic drain already passed that tail (minimizes ordering inversions)", async () => {
    Object.defineProperty(globalThis, "crossOriginIsolated", { value: true, configurable: true });
    vi.useFakeTimers();
    try {
      const manager = new WebHidPassthroughManager({ hid: null });
      const broker = new WebHidBroker({ manager });
      const port = new FakePort();
      broker.attachWorkerPort(port as unknown as MessagePort);

      const ringAttach = port.posted.find((p) => (p.msg as { type?: unknown }).type === "hid.ringAttach")?.msg as
        | { inputRing: SharedArrayBuffer; outputRing: SharedArrayBuffer }
        | undefined;
      expect(ringAttach).toBeTruthy();
      const outputRing = new HidReportRing(ringAttach!.outputRing);

      const device = new FakeHidDevice();
      const id = await broker.attachDevice(device as unknown as HIDDevice);

      // The message claims it was posted when the output ring tail was at 0.
      const tailAtPost = 0;

      // Write a ring record and let the periodic drain consume it, advancing the ring head beyond 0.
      expect(outputRing.push(id, HidReportType.Output, 2, Uint8Array.of(2))).toBe(true);
      vi.advanceTimersByTime(20);
      await flushMicrotasks();
      expect(device.sendReport).toHaveBeenCalledTimes(1);
      expect(device.sendReport).toHaveBeenCalledWith(2, Uint8Array.of(2));

      // Now queue another ring record, but do not let the timer drain it yet.
      expect(outputRing.push(id, HidReportType.Output, 3, Uint8Array.of(3))).toBe(true);

      // Deliver the older message. The broker must not synchronously drain the newer ring record (reportId=3)
      // because the ring head has already passed `tailAtPost`; draining would only worsen ordering.
      port.emit({
        type: "hid.sendReport",
        deviceId: id,
        reportType: "output",
        reportId: 1,
        data: Uint8Array.of(1),
        outputRingTail: tailAtPost,
      });
      await flushMicrotasks();

      expect(device.sendReport).toHaveBeenCalledTimes(2);
      expect(device.sendReport).toHaveBeenNthCalledWith(1, 2, Uint8Array.of(2));
      expect(device.sendReport).toHaveBeenNthCalledWith(2, 1, Uint8Array.of(1));

      // The pending ring record should be sent later.
      vi.advanceTimersByTime(20);
      await flushMicrotasks();
      expect(device.sendReport).toHaveBeenCalledTimes(3);
      expect(device.sendReport).toHaveBeenNthCalledWith(3, 3, Uint8Array.of(3));

      broker.destroy();
    } finally {
      vi.useRealTimers();
    }
  });

  it("uses hid.getFeatureReport.outputRingTail to preserve feature report ordering when output ring records race ahead of message delivery", async () => {
    Object.defineProperty(globalThis, "crossOriginIsolated", { value: true, configurable: true });
    vi.useFakeTimers();
    try {
      const manager = new WebHidPassthroughManager({ hid: null });
      const broker = new WebHidBroker({ manager });
      const port = new FakePort();
      broker.attachWorkerPort(port as unknown as MessagePort);

      const ringAttach = port.posted.find((p) => (p.msg as { type?: unknown }).type === "hid.ringAttach")?.msg as
        | { inputRing: SharedArrayBuffer; outputRing: SharedArrayBuffer }
        | undefined;
      expect(ringAttach).toBeTruthy();
      const outputRing = new HidReportRing(ringAttach!.outputRing);

      const device = new FakeHidDevice();
      device.receiveFeatureReport.mockResolvedValueOnce(new DataView(Uint8Array.of(9).buffer));
      const id = await broker.attachDevice(device as unknown as HIDDevice);

      const tailAtPost = outputRing.debugState().tail;
      setTimeout(() => {
        port.emit({
          type: "hid.getFeatureReport",
          requestId: 1,
          deviceId: id,
          reportId: 7,
          outputRingTail: tailAtPost,
        });
      }, 0);

      expect(outputRing.push(id, HidReportType.Output, 2, Uint8Array.of(2))).toBe(true);

      vi.advanceTimersByTime(1);
      await flushMicrotasks();

      expect(device.receiveFeatureReport).toHaveBeenCalledTimes(1);
      expect(device.sendReport).toHaveBeenCalledTimes(0);
      expect(port.posted.some((p) => (p.msg as { type?: unknown }).type === "hid.featureReportResult")).toBe(true);

      vi.advanceTimersByTime(20);
      await flushMicrotasks();

      expect(device.sendReport).toHaveBeenCalledTimes(1);
      expect(device.sendReport).toHaveBeenCalledWith(2, Uint8Array.of(2));

      broker.destroy();
    } finally {
      vi.useRealTimers();
    }
  });

  it("does not drain ring records beyond hid.getFeatureReport.outputRingTail when the periodic drain already passed that tail", async () => {
    Object.defineProperty(globalThis, "crossOriginIsolated", { value: true, configurable: true });
    vi.useFakeTimers();
    try {
      const manager = new WebHidPassthroughManager({ hid: null });
      const broker = new WebHidBroker({ manager });
      const port = new FakePort();
      broker.attachWorkerPort(port as unknown as MessagePort);

      const ringAttach = port.posted.find((p) => (p.msg as { type?: unknown }).type === "hid.ringAttach")?.msg as
        | { inputRing: SharedArrayBuffer; outputRing: SharedArrayBuffer }
        | undefined;
      expect(ringAttach).toBeTruthy();
      const outputRing = new HidReportRing(ringAttach!.outputRing);

      const device = new FakeHidDevice();
      device.receiveFeatureReport.mockResolvedValueOnce(new DataView(Uint8Array.of(9).buffer));
      const id = await broker.attachDevice(device as unknown as HIDDevice);

      // The message claims it was posted when the output ring tail was at 0.
      const tailAtPost = 0;

      // Drain one ring record so the ring head advances beyond 0.
      expect(outputRing.push(id, HidReportType.Output, 2, Uint8Array.of(2))).toBe(true);
      vi.advanceTimersByTime(20);
      await flushMicrotasks();
      expect(device.sendReport).toHaveBeenCalledTimes(1);
      expect(device.sendReport).toHaveBeenCalledWith(2, Uint8Array.of(2));

      // Queue another ring record, but do not let the timer drain it yet.
      expect(outputRing.push(id, HidReportType.Output, 3, Uint8Array.of(3))).toBe(true);

      port.emit({
        type: "hid.getFeatureReport",
        requestId: 1,
        deviceId: id,
        reportId: 7,
        outputRingTail: tailAtPost,
      });
      await flushMicrotasks();

      // The broker must not drain the newer ring record (reportId=3) ahead of this feature report request.
      expect(device.receiveFeatureReport).toHaveBeenCalledTimes(1);
      expect(device.sendReport).toHaveBeenCalledTimes(1);
      expect(port.posted.some((p) => (p.msg as { type?: unknown }).type === "hid.featureReportResult")).toBe(true);

      // The pending ring record should be sent later.
      vi.advanceTimersByTime(20);
      await flushMicrotasks();
      expect(device.sendReport).toHaveBeenCalledTimes(2);
      expect(device.sendReport).toHaveBeenNthCalledWith(2, 3, Uint8Array.of(3));

      broker.destroy();
    } finally {
      vi.useRealTimers();
    }
  });

  it("preserves send order when mixing output ring records with hid.sendReport fallback", async () => {
    Object.defineProperty(globalThis, "crossOriginIsolated", { value: true, configurable: true });
    vi.useFakeTimers();
    try {
      const manager = new WebHidPassthroughManager({ hid: null });
      const broker = new WebHidBroker({ manager });
      const port = new FakePort();
      broker.attachWorkerPort(port as unknown as MessagePort);

      const ringAttach = port.posted.find((p) => (p.msg as { type?: unknown }).type === "hid.ringAttach")?.msg as
        | { inputRing: SharedArrayBuffer; outputRing: SharedArrayBuffer }
        | undefined;
      expect(ringAttach).toBeTruthy();

      const outputRing = new HidReportRing(ringAttach!.outputRing);

      const device = new FakeHidDevice();
      const id = await broker.attachDevice(device as unknown as HIDDevice);

      // Report A delivered via ring.
      expect(outputRing.push(id, HidReportType.Output, 1, Uint8Array.of(1))).toBe(true);
      // Report B delivered via structured postMessage fallback.
      port.emit({ type: "hid.sendReport", deviceId: id, reportType: "output", reportId: 2, data: Uint8Array.of(2) });

      // Await enough microtasks for the per-device send queue to run both tasks.
      for (let i = 0; i < 10 && device.sendReport.mock.calls.length < 2; i += 1) {
        await Promise.resolve();
      }

      expect(device.sendReport).toHaveBeenCalledTimes(2);
      expect(device.sendReport).toHaveBeenNthCalledWith(1, 1, Uint8Array.of(1));
      expect(device.sendReport).toHaveBeenNthCalledWith(2, 2, Uint8Array.of(2));

      broker.destroy();
    } finally {
      vi.useRealTimers();
    }
  });

  it("buffers output ring reports received before hid.attachResult and sends them after attach completes", async () => {
    Object.defineProperty(globalThis, "crossOriginIsolated", { value: true, configurable: true });

    const manager = new WebHidPassthroughManager({ hid: null });
    const broker = new WebHidBroker({ manager });
    const port = new FakePort();
    port.autoAttachResult = false;
    let attachedId: number | null = null;
    port.onPost = (msg) => {
      if ((msg as { type?: unknown }).type === "hid.attach") {
        attachedId = (msg as { deviceId: number }).deviceId;
      }
    };
    broker.attachWorkerPort(port as unknown as MessagePort);

    const ringAttach = port.posted.find((p) => (p.msg as { type?: unknown }).type === "hid.ringAttach")?.msg as
      | { inputRing: SharedArrayBuffer; outputRing: SharedArrayBuffer }
      | undefined;
    expect(ringAttach).toBeTruthy();
    const outputRing = new HidReportRing(ringAttach!.outputRing);

    const device = new FakeHidDevice();
    const attachPromise = broker.attachDevice(device as unknown as HIDDevice);
    await new Promise((r) => setTimeout(r, 0));
    expect(attachedId).toBeTruthy();
    const id = attachedId!;

    outputRing.push(id, HidReportType.Output, 1, Uint8Array.of(1));
    // Let the periodic ring drain fire while attach is still pending.
    await new Promise((r) => setTimeout(r, 40));
    expect(device.sendReport).not.toHaveBeenCalled();

    port.emit({ type: "hid.attachResult", deviceId: id, ok: true });
    await attachPromise;
    await new Promise((r) => setTimeout(r, 20));

    expect(device.sendReport).toHaveBeenCalledTimes(1);
    expect(device.sendReport).toHaveBeenCalledWith(1, Uint8Array.of(1));

    broker.destroy();
  });

  it("buffers hid.sendReport messages received before hid.attachResult and sends them after attach completes", async () => {
    const manager = new WebHidPassthroughManager({ hid: null });
    const broker = new WebHidBroker({ manager });
    const port = new FakePort();
    port.autoAttachResult = false;
    let attachedId: number | null = null;
    port.onPost = (msg) => {
      if ((msg as { type?: unknown }).type === "hid.attach") {
        attachedId = (msg as { deviceId: number }).deviceId;
      }
    };
    broker.attachWorkerPort(port as unknown as MessagePort);

    const device = new FakeHidDevice();
    const attachPromise = broker.attachDevice(device as unknown as HIDDevice);
    await new Promise((r) => setTimeout(r, 0));
    expect(attachedId).toBeTruthy();
    const id = attachedId!;

    port.emit({ type: "hid.sendReport", deviceId: id, reportType: "output", reportId: 1, data: Uint8Array.of(1) });
    await new Promise((r) => setTimeout(r, 0));
    expect(device.sendReport).not.toHaveBeenCalled();

    port.emit({ type: "hid.attachResult", deviceId: id, ok: true });
    await attachPromise;
    await new Promise((r) => setTimeout(r, 0));

    expect(device.sendReport).toHaveBeenCalledTimes(1);
    expect(device.sendReport).toHaveBeenCalledWith(1, Uint8Array.of(1));

    broker.destroy();
  });

  it("does not send buffered output ring reports if hid.attachResult fails", async () => {
    Object.defineProperty(globalThis, "crossOriginIsolated", { value: true, configurable: true });

    const manager = new WebHidPassthroughManager({ hid: null });
    const broker = new WebHidBroker({ manager });
    const port = new FakePort();
    port.autoAttachResult = false;
    let attachedId: number | null = null;
    port.onPost = (msg) => {
      if ((msg as { type?: unknown }).type === "hid.attach") {
        attachedId = (msg as { deviceId: number }).deviceId;
      }
    };
    broker.attachWorkerPort(port as unknown as MessagePort);

    const ringAttach = port.posted.find((p) => (p.msg as { type?: unknown }).type === "hid.ringAttach")?.msg as
      | { inputRing: SharedArrayBuffer; outputRing: SharedArrayBuffer }
      | undefined;
    expect(ringAttach).toBeTruthy();
    const outputRing = new HidReportRing(ringAttach!.outputRing);

    const device = new FakeHidDevice();
    const attachPromise = broker.attachDevice(device as unknown as HIDDevice);
    await new Promise((r) => setTimeout(r, 0));
    expect(attachedId).toBeTruthy();
    const id = attachedId!;

    expect(outputRing.push(id, HidReportType.Output, 1, Uint8Array.of(1))).toBe(true);
    await new Promise((r) => setTimeout(r, 40));
    expect(device.sendReport).not.toHaveBeenCalled();

    port.emit({ type: "hid.attachResult", deviceId: id, ok: false, error: "boom" });
    await expect(attachPromise).rejects.toThrow("boom");
    await new Promise((r) => setTimeout(r, 20));
    expect(device.sendReport).not.toHaveBeenCalled();
    expect(broker.getState().attachedDeviceIds).not.toContain(id);

    broker.destroy();
  });

  it("does not send buffered hid.sendReport messages if hid.attachResult fails", async () => {
    const manager = new WebHidPassthroughManager({ hid: null });
    const broker = new WebHidBroker({ manager });
    const port = new FakePort();
    port.autoAttachResult = false;
    let attachedId: number | null = null;
    port.onPost = (msg) => {
      if ((msg as { type?: unknown }).type === "hid.attach") {
        attachedId = (msg as { deviceId: number }).deviceId;
      }
    };
    broker.attachWorkerPort(port as unknown as MessagePort);

    const device = new FakeHidDevice();
    const attachPromise = broker.attachDevice(device as unknown as HIDDevice);
    await new Promise((r) => setTimeout(r, 0));
    expect(attachedId).toBeTruthy();
    const id = attachedId!;

    port.emit({ type: "hid.sendReport", deviceId: id, reportType: "output", reportId: 1, data: Uint8Array.of(1) });
    await new Promise((r) => setTimeout(r, 0));
    expect(device.sendReport).not.toHaveBeenCalled();

    port.emit({ type: "hid.attachResult", deviceId: id, ok: false, error: "boom" });
    await expect(attachPromise).rejects.toThrow("boom");
    await new Promise((r) => setTimeout(r, 0));
    expect(device.sendReport).not.toHaveBeenCalled();
    expect(broker.getState().attachedDeviceIds).not.toContain(id);

    broker.destroy();
  });

  it("responds to hid.getFeatureReport messages queued before hid.attachResult when attach fails", async () => {
    const manager = new WebHidPassthroughManager({ hid: null });
    const broker = new WebHidBroker({ manager });
    const port = new FakePort();
    port.autoAttachResult = false;
    let attachedId: number | null = null;
    port.onPost = (msg) => {
      if ((msg as { type?: unknown }).type === "hid.attach") {
        attachedId = (msg as { deviceId: number }).deviceId;
      }
    };
    broker.attachWorkerPort(port as unknown as MessagePort);

    const device = new FakeHidDevice();
    const attachPromise = broker.attachDevice(device as unknown as HIDDevice);
    await new Promise((r) => setTimeout(r, 0));
    expect(attachedId).toBeTruthy();
    const id = attachedId!;

    port.emit({ type: "hid.getFeatureReport", requestId: 123, deviceId: id, reportId: 7 });
    await new Promise((r) => setTimeout(r, 0));
    expect(device.receiveFeatureReport).not.toHaveBeenCalled();
    expect(port.posted.some((p) => (p.msg as { type?: unknown }).type === "hid.featureReportResult")).toBe(false);

    port.emit({ type: "hid.attachResult", deviceId: id, ok: false, error: "boom" });
    await expect(attachPromise).rejects.toThrow("boom");
    await new Promise((r) => setTimeout(r, 0));

    const result = port.posted.find((p) => (p.msg as { type?: unknown }).type === "hid.featureReportResult") as
      | { msg: HidFeatureReportResultMessage; transfer?: Transferable[] }
      | undefined;
    expect(result).toBeTruthy();
    expect(result!.msg).toMatchObject({ requestId: 123, deviceId: id, reportId: 7, ok: false });
    expect(String(result!.msg.error)).toContain("boom");
    expect(device.receiveFeatureReport).not.toHaveBeenCalled();

    broker.destroy();
  });

  it("does not block other devices when output ring sends are pending", async () => {
    Object.defineProperty(globalThis, "crossOriginIsolated", { value: true, configurable: true });

    const manager = new WebHidPassthroughManager({ hid: null });
    const broker = new WebHidBroker({ manager });
    const port = new FakePort();
    broker.attachWorkerPort(port as unknown as MessagePort);

    const ringAttach = port.posted.find((p) => (p.msg as { type?: unknown }).type === "hid.ringAttach")?.msg as
      | { inputRing: SharedArrayBuffer; outputRing: SharedArrayBuffer }
      | undefined;
    expect(ringAttach).toBeTruthy();
    const outputRing = new HidReportRing(ringAttach!.outputRing);

    const deviceA = new FakeHidDevice();
    const deviceB = new FakeHidDevice();
    const first = deferred<void>();
    deviceA.sendReport.mockImplementationOnce(() => first.promise);

    const idA = await broker.attachDevice(deviceA as unknown as HIDDevice);
    const idB = await broker.attachDevice(deviceB as unknown as HIDDevice);

    expect(outputRing.push(idA, HidReportType.Output, 1, Uint8Array.of(1))).toBe(true);
    await new Promise((r) => setTimeout(r, 20));
    expect(deviceA.sendReport).toHaveBeenCalledTimes(1);
    expect(deviceB.sendReport).toHaveBeenCalledTimes(0);

    expect(outputRing.push(idB, HidReportType.Output, 1, Uint8Array.of(2))).toBe(true);
    await new Promise((r) => setTimeout(r, 20));
    expect(deviceB.sendReport).toHaveBeenCalledTimes(1);

    first.resolve(undefined);
    await new Promise((r) => setTimeout(r, 0));
    broker.destroy();
  });

  it("does not serialize report sends across devices", async () => {
    const manager = new WebHidPassthroughManager({ hid: null });
    const broker = new WebHidBroker({ manager });
    const port = new FakePort();
    broker.attachWorkerPort(port as unknown as MessagePort);

    const deviceA = new FakeHidDevice();
    const deviceB = new FakeHidDevice();
    const holdA = deferred<void>();
    deviceA.sendReport.mockImplementationOnce(() => holdA.promise);

    const idA = await broker.attachDevice(deviceA as unknown as HIDDevice);
    const idB = await broker.attachDevice(deviceB as unknown as HIDDevice);

    port.emit({ type: "hid.sendReport", deviceId: idA, reportType: "output", reportId: 1, data: Uint8Array.of(1) });
    port.emit({ type: "hid.sendReport", deviceId: idB, reportType: "output", reportId: 2, data: Uint8Array.of(2) });

    await new Promise((r) => setTimeout(r, 0));
    expect(deviceA.sendReport).toHaveBeenCalledTimes(1);
    expect(deviceB.sendReport).toHaveBeenCalledTimes(1);

    holdA.resolve(undefined);
    await new Promise((r) => setTimeout(r, 0));
    broker.destroy();
  });

  it("continues draining the per-device send queue after a sendReport failure", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      const manager = new WebHidPassthroughManager({ hid: null });
      const broker = new WebHidBroker({ manager });
      const port = new FakePort();
      broker.attachWorkerPort(port as unknown as MessagePort);

      const device = new FakeHidDevice();
      device.sendReport.mockImplementationOnce(async () => {
        throw new Error("nope");
      });
      device.sendReport.mockImplementationOnce(async () => {});

      const id = await broker.attachDevice(device as unknown as HIDDevice);

      port.emit({ type: "hid.sendReport", deviceId: id, reportType: "output", reportId: 1, data: Uint8Array.of(1) });
      port.emit({ type: "hid.sendReport", deviceId: id, reportType: "output", reportId: 2, data: Uint8Array.of(2) });

      await new Promise((r) => setTimeout(r, 0));
      expect(device.sendReport).toHaveBeenCalledTimes(2);
      expect(device.sendReport).toHaveBeenNthCalledWith(1, 1, Uint8Array.of(1));
      expect(device.sendReport).toHaveBeenNthCalledWith(2, 2, Uint8Array.of(2));

      broker.destroy();
    } finally {
      warn.mockRestore();
    }
  });

  it("continues draining the per-device send queue after a ring sendReport failure", async () => {
    Object.defineProperty(globalThis, "crossOriginIsolated", { value: true, configurable: true });
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      const manager = new WebHidPassthroughManager({ hid: null });
      const broker = new WebHidBroker({ manager });
      const port = new FakePort();
      broker.attachWorkerPort(port as unknown as MessagePort);

      const ringAttach = port.posted.find((p) => (p.msg as { type?: unknown }).type === "hid.ringAttach")?.msg as
        | { inputRing: SharedArrayBuffer; outputRing: SharedArrayBuffer }
        | undefined;
      expect(ringAttach).toBeTruthy();
      const outputRing = new HidReportRing(ringAttach!.outputRing);

      const device = new FakeHidDevice();
      device.sendReport.mockImplementationOnce(async () => {
        throw new Error("nope");
      });
      device.sendReport.mockImplementationOnce(async () => {});
      const id = await broker.attachDevice(device as unknown as HIDDevice);

      expect(outputRing.push(id, HidReportType.Output, 1, Uint8Array.of(1))).toBe(true);
      expect(outputRing.push(id, HidReportType.Output, 2, Uint8Array.of(2))).toBe(true);

      await new Promise((r) => setTimeout(r, 40));

      expect(device.sendReport).toHaveBeenCalledTimes(2);
      expect(device.sendReport).toHaveBeenNthCalledWith(1, 1, Uint8Array.of(1));
      expect(device.sendReport).toHaveBeenNthCalledWith(2, 2, Uint8Array.of(2));

      broker.destroy();
    } finally {
      warn.mockRestore();
    }
  });

  it("bounds background output ring draining work per tick", async () => {
    Object.defineProperty(globalThis, "crossOriginIsolated", { value: true, configurable: true });
    vi.useFakeTimers();
    try {
      const manager = new WebHidPassthroughManager({ hid: null });
      const broker = new WebHidBroker({ manager });
      const port = new FakePort();
      broker.attachWorkerPort(port as unknown as MessagePort);

      const ringAttach = port.posted.find((p) => (p.msg as { type?: unknown }).type === "hid.ringAttach")?.msg as
        | { inputRing: SharedArrayBuffer; outputRing: SharedArrayBuffer }
        | undefined;
      expect(ringAttach).toBeTruthy();
      const outputRing = new HidReportRing(ringAttach!.outputRing);

      const device = new FakeHidDevice();
      // Stall the first send so queued tasks can't drain; this makes the test independent of
      // async task timing and lets us focus on ring consumption.
      device.sendReport.mockImplementationOnce(() => new Promise<void>(() => {}));
      const id = await broker.attachDevice(device as unknown as HIDDevice);

      const total = 300;
      for (let i = 0; i < total; i += 1) {
        expect(outputRing.push(id, HidReportType.Output, 1, Uint8Array.of(i & 0xff))).toBe(true);
      }
      expect(outputRing.isEmpty()).toBe(false);

      // First drain tick should be bounded and leave records for the next tick.
      vi.advanceTimersByTime(8);
      await flushMicrotasks();
      expect(outputRing.isEmpty()).toBe(false);

      // Second tick should drain the remaining records.
      vi.advanceTimersByTime(8);
      await flushMicrotasks();
      expect(outputRing.isEmpty()).toBe(true);

      broker.destroy();
    } finally {
      vi.useRealTimers();
    }
  });

  it("bounds background output ring draining by payload bytes per tick", async () => {
    Object.defineProperty(globalThis, "crossOriginIsolated", { value: true, configurable: true });
    vi.useFakeTimers();
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      const manager = new WebHidPassthroughManager({ hid: null });
      const broker = new WebHidBroker({ manager, outputRingCapacityBytes: 2 * 1024 * 1024 });
      const port = new FakePort();
      broker.attachWorkerPort(port as unknown as MessagePort);

      const ringAttach = port.posted.find((p) => (p.msg as { type?: unknown }).type === "hid.ringAttach")?.msg as
        | { inputRing: SharedArrayBuffer; outputRing: SharedArrayBuffer }
        | undefined;
      expect(ringAttach).toBeTruthy();
      const outputRing = new HidReportRing(ringAttach!.outputRing);

      const device = new FakeHidDevice();
      device.sendReport.mockImplementationOnce(() => new Promise<void>(() => {}));
      const id = await broker.attachDevice(device as unknown as HIDDevice);

      const payload = new Uint8Array(0xffff);
      const total = 20;
      for (let i = 0; i < total; i += 1) {
        payload[0] = i & 0xff;
        expect(outputRing.push(id, HidReportType.Output, 1, payload)).toBe(true);
      }
      expect(outputRing.isEmpty()).toBe(false);

      // First drain tick should hit the byte budget and leave records for the next tick.
      vi.advanceTimersByTime(8);
      await flushMicrotasks();
      expect(outputRing.isEmpty()).toBe(false);

      vi.advanceTimersByTime(8);
      await flushMicrotasks();
      expect(outputRing.isEmpty()).toBe(true);

      broker.destroy();
    } finally {
      warn.mockRestore();
      vi.useRealTimers();
    }
  });

  it("caps pending per-device sends and counts drops when sendReport never resolves", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      const manager = new WebHidPassthroughManager({ hid: null });
      const broker = new WebHidBroker({ manager, maxPendingDeviceSends: 4 });
      const port = new FakePort();
      broker.attachWorkerPort(port as unknown as MessagePort);

      const device = new FakeHidDevice();
      device.sendReport.mockImplementationOnce(() => new Promise<void>(() => {}));
      const id = await broker.attachDevice(device as unknown as HIDDevice);

      // Start one in-flight send that never resolves so the queue cannot drain.
      port.emit({ type: "hid.sendReport", deviceId: id, reportType: "output", reportId: 1, data: Uint8Array.of(1) });
      await new Promise((r) => setTimeout(r, 0));
      expect(device.sendReport).toHaveBeenCalledTimes(1);

      // Flood the broker with sends; only `maxPendingDeviceSends` should be buffered.
      const flood = 20;
      for (let i = 0; i < flood; i += 1) {
        port.emit({
          type: "hid.sendReport",
          deviceId: id,
          reportType: "output",
          reportId: 2 + i,
          data: Uint8Array.of(i),
        });
      }

      const stats = broker.getOutputSendStats();
      const perDevice = stats.devices.find((d) => d.deviceId === id);
      expect(perDevice).toBeTruthy();
      expect(perDevice!.pending).toBe(4);
      expect(perDevice!.dropped).toBe(flood - 4);
      expect(stats.pendingTotal).toBe(4);
      expect(stats.droppedTotal).toBe(flood - 4);

      broker.destroy();
    } finally {
      warn.mockRestore();
    }
  });

  it("drops pending per-device sends on detach (does not run queued reports after detach)", async () => {
    const manager = new WebHidPassthroughManager({ hid: null });
    const broker = new WebHidBroker({ manager });
    const port = new FakePort();
    broker.attachWorkerPort(port as unknown as MessagePort);

    const device = new FakeHidDevice();
    const first = deferred<void>();
    device.sendReport.mockImplementationOnce(() => first.promise);
    const id = await broker.attachDevice(device as unknown as HIDDevice);

    port.emit({ type: "hid.sendReport", deviceId: id, reportType: "output", reportId: 1, data: Uint8Array.of(1) });
    port.emit({ type: "hid.sendReport", deviceId: id, reportType: "output", reportId: 2, data: Uint8Array.of(2) });

    await new Promise((r) => setTimeout(r, 0));
    expect(device.sendReport).toHaveBeenCalledTimes(1);

    await broker.detachDevice(device as unknown as HIDDevice);

    first.resolve(undefined);
    await new Promise((r) => setTimeout(r, 0));

    expect(device.sendReport).toHaveBeenCalledTimes(1);
  });

  it("drops pending per-device sends when the manager reports a device detached", async () => {
    const manager = new WebHidPassthroughManager({ hid: null });
    const broker = new WebHidBroker({ manager });
    const port = new FakePort();
    broker.attachWorkerPort(port as unknown as MessagePort);

    const device = new FakeHidDevice();
    const first = deferred<void>();
    device.sendReport.mockImplementationOnce(() => first.promise);
    const id = await broker.attachDevice(device as unknown as HIDDevice);

    port.emit({ type: "hid.sendReport", deviceId: id, reportType: "output", reportId: 1, data: Uint8Array.of(1) });
    port.emit({ type: "hid.sendReport", deviceId: id, reportType: "output", reportId: 2, data: Uint8Array.of(2) });

    await new Promise((r) => setTimeout(r, 0));
    expect(device.sendReport).toHaveBeenCalledTimes(1);

    // Simulate a physical disconnect / manager-driven detach.
    await manager.detachDevice(device as unknown as HIDDevice);
    // The broker reacts to manager detaches asynchronously.
    await new Promise((r) => setTimeout(r, 0));

    first.resolve(undefined);
    await new Promise((r) => setTimeout(r, 0));

    // The queued report must be dropped and never executed after the device is detached.
    expect(device.sendReport).toHaveBeenCalledTimes(1);
  });

  it("handles hid.getFeatureReport requests from the worker with ordered request processing", async () => {
    const manager = new WebHidPassthroughManager({ hid: null });
    const broker = new WebHidBroker({ manager });
    const port = new FakePort();
    broker.attachWorkerPort(port as unknown as MessagePort);

    const device = new FakeHidDevice();
    const first = deferred<DataView<ArrayBuffer>>();
    device.receiveFeatureReport
      .mockImplementationOnce(async () => first.promise)
      .mockImplementationOnce(async () => {
        const backing = new ArrayBuffer(8);
        new Uint8Array(backing, 2, 2).set([9, 10]);
        return new DataView(backing, 2, 2);
      });
    const id = await broker.attachDevice(device as unknown as HIDDevice);

    port.emit({ type: "hid.getFeatureReport", requestId: 1, deviceId: id, reportId: 7 });
    port.emit({ type: "hid.getFeatureReport", requestId: 2, deviceId: id, reportId: 7 });

    await new Promise((r) => setTimeout(r, 0));
    // Second request must not start until the first finishes.
    expect(device.receiveFeatureReport).toHaveBeenCalledTimes(1);

    first.resolve(new DataView(Uint8Array.of(1, 2, 3).buffer));
    await new Promise((r) => setTimeout(r, 0));
    expect(device.receiveFeatureReport).toHaveBeenCalledTimes(2);

    const results = port.posted.filter((p) => (p.msg as { type?: unknown }).type === "hid.featureReportResult") as Array<{
      msg: any;
      transfer?: Transferable[];
    }>;
    expect(results).toHaveLength(2);

    expect(results[0].msg).toMatchObject({ requestId: 1, deviceId: id, reportId: 7, ok: true });
    expect(Array.from(results[0].msg.data)).toEqual([1, 2, 3]);
    expect(results[0].transfer?.[0]).toBe(results[0].msg.data.buffer);

    // Second response should use an ArrayBuffer-backed Uint8Array with the DataView slice copied out.
    expect(results[1].msg).toMatchObject({ requestId: 2, deviceId: id, reportId: 7, ok: true });
    expect(Array.from(results[1].msg.data)).toEqual([9, 10]);
    expect(results[1].transfer?.[0]).toBe(results[1].msg.data.buffer);

    // Detached/unknown device should respond with ok:false.
    port.emit({ type: "hid.getFeatureReport", requestId: 3, deviceId: id + 999, reportId: 1 });
    await new Promise((r) => setTimeout(r, 0));
    const failed = port.posted
      .slice()
      .reverse()
      .find(
        (p) =>
          (p.msg as { type?: unknown; requestId?: unknown }).type === "hid.featureReportResult" &&
          (p.msg as { type?: unknown; requestId?: unknown }).requestId === 3,
      ) as { msg: HidFeatureReportResultMessage; transfer?: Transferable[] } | undefined;
    expect(failed).toBeTruthy();
    expect(failed!.msg).toMatchObject({ requestId: 3, ok: false });
  });

  it("clamps oversized feature report payloads to the expected report size before forwarding", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      const manager = new WebHidPassthroughManager({ hid: null });
      const broker = new WebHidBroker({ manager });
      const port = new FakePort();
      broker.attachWorkerPort(port as unknown as MessagePort);

      const device = new FakeHidDevice();
      // One feature report (ID 7) with 4 bytes of payload.
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

      const id = await broker.attachDevice(device as unknown as HIDDevice);

      port.emit({ type: "hid.getFeatureReport", requestId: 1, deviceId: id, reportId: 7 });
      await new Promise((r) => setTimeout(r, 0));

      const result = port.posted.find(
        (p) =>
          (p.msg as { type?: unknown; requestId?: unknown }).type === "hid.featureReportResult" &&
          (p.msg as { type?: unknown; requestId?: unknown }).requestId === 1,
      ) as { msg: HidFeatureReportResultMessage; transfer?: Transferable[] } | undefined;
      expect(result).toBeTruthy();
      expect(result!.msg).toMatchObject({ requestId: 1, deviceId: id, reportId: 7, ok: true });
      expect(result!.msg.data!.byteLength).toBe(4);
      expect(Array.from(result!.msg.data!)).toEqual([1, 2, 3, 4]);
      expect(result!.transfer?.[0]).toBe(result!.msg.data!.buffer);
      expect(warn).toHaveBeenCalledTimes(1);
    } finally {
      warn.mockRestore();
    }
  });

  it("zero-pads short feature report payloads to the expected report size before forwarding", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      const manager = new WebHidPassthroughManager({ hid: null });
      const broker = new WebHidBroker({ manager });
      const port = new FakePort();
      broker.attachWorkerPort(port as unknown as MessagePort);

      const device = new FakeHidDevice();
      // One feature report (ID 7) with 4 bytes of payload.
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

      device.receiveFeatureReport.mockImplementationOnce(async () => new DataView(Uint8Array.of(9, 8).buffer));

      const id = await broker.attachDevice(device as unknown as HIDDevice);

      port.emit({ type: "hid.getFeatureReport", requestId: 1, deviceId: id, reportId: 7 });
      await new Promise((r) => setTimeout(r, 0));

      const result = port.posted.find(
        (p) =>
          (p.msg as { type?: unknown; requestId?: unknown }).type === "hid.featureReportResult" &&
          (p.msg as { type?: unknown; requestId?: unknown }).requestId === 1,
      ) as { msg: HidFeatureReportResultMessage; transfer?: Transferable[] } | undefined;
      expect(result).toBeTruthy();
      expect(result!.msg).toMatchObject({ requestId: 1, deviceId: id, reportId: 7, ok: true });
      expect(Array.from(result!.msg.data!)).toEqual([9, 8, 0, 0]);
      expect(result!.transfer?.[0]).toBe(result!.msg.data!.buffer);
      expect(warn).toHaveBeenCalledTimes(1);
    } finally {
      warn.mockRestore();
    }
  });

  it("hard-caps unknown feature report payload sizes before forwarding", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      const manager = new WebHidPassthroughManager({ hid: null });
      const broker = new WebHidBroker({ manager });
      const port = new FakePort();
      broker.attachWorkerPort(port as unknown as MessagePort);

      const device = new FakeHidDevice();
      // No feature report metadata -> size will be treated as unknown.
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

      device.receiveFeatureReport.mockImplementation(async () => {
        const huge = new Uint8Array(1024 * 1024);
        huge.set([1, 2, 3], 0);
        return new DataView(huge.buffer);
      });

      const id = await broker.attachDevice(device as unknown as HIDDevice);

      port.emit({ type: "hid.getFeatureReport", requestId: 1, deviceId: id, reportId: 99 });
      port.emit({ type: "hid.getFeatureReport", requestId: 2, deviceId: id, reportId: 99 });

      await new Promise((r) => setTimeout(r, 0));
      await new Promise((r) => setTimeout(r, 0));

      type PostedFeatureReportResult = { msg: HidFeatureReportResultMessage; transfer?: Transferable[] };
      const results = port.posted.filter((p): p is PostedFeatureReportResult => {
        const msg = p.msg as { type?: unknown; ok?: unknown };
        return msg.type === "hid.featureReportResult" && msg.ok === true;
      });
      expect(results.length).toBeGreaterThanOrEqual(2);

      const a = results.find((r) => r.msg.requestId === 1);
      const b = results.find((r) => r.msg.requestId === 2);
      expect(a).toBeTruthy();
      expect(b).toBeTruthy();
      expect(a!.msg.data!.byteLength).toBe(4096);
      expect(Array.from(a!.msg.data!.slice(0, 3))).toEqual([1, 2, 3]);
      expect(b!.msg.data!.byteLength).toBe(4096);
      expect(Array.from(b!.msg.data!.slice(0, 3))).toEqual([1, 2, 3]);

      // Warn once per (deviceId, reportId) when hard-capping unknown report sizes.
      expect(warn).toHaveBeenCalledTimes(1);
    } finally {
      warn.mockRestore();
    }
  });

  it("does not auto-attach devices when the worker port is replaced", async () => {
    const manager = new WebHidPassthroughManager({ hid: null });
    const broker = new WebHidBroker({ manager });

    const port1 = new FakePort();
    port1.onPost = (msg) => {
      if ((msg as { type?: unknown }).type === "hid.attach") {
        const deviceId = (msg as { deviceId: number }).deviceId;
        port1.emit({ type: "hid.attachResult", deviceId, ok: true });
      }
    };
    broker.attachWorkerPort(port1 as unknown as MessagePort);

    const device = new FakeHidDevice();
    const id = await broker.attachDevice(device as unknown as HIDDevice);

    device.dispatchInputReport(1, Uint8Array.of(1));
    expect(port1.posted.some((p) => (p.msg as { type?: unknown }).type === "hid.inputReport")).toBe(true);

    const port2 = new FakePort();
    port2.onPost = (msg) => {
      if ((msg as { type?: unknown }).type === "hid.attach") {
        const deviceId = (msg as { deviceId: number }).deviceId;
        port2.emit({ type: "hid.attachResult", deviceId, ok: true });
      }
    };
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
    expect(
      port2.posted.some(
        (p) =>
          (p.msg as { type?: unknown; deviceId?: unknown }).type === "hid.attach" &&
          (p.msg as { type?: unknown; deviceId?: unknown }).deviceId === id,
      ),
    ).toBe(true);
  });
});
