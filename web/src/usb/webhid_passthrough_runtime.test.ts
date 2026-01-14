import { describe, expect, it, vi } from "vitest";
import { WebHidPassthroughRuntime, type WebHidPassthroughBridgeLike } from "./webhid_passthrough_runtime";
import type { WebHidPassthroughManager, WebHidPassthroughState } from "../platform/webhid_passthrough";

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
  readonly vendorId: number;
  readonly productId: number;
  readonly productName: string;
  readonly collections: HIDCollectionInfo[];

  readonly open = vi.fn(async () => {
    this.opened = true;
  });

  readonly close = vi.fn(async () => {
    this.opened = false;
  });

  readonly sendReport = vi.fn(async (_reportId: number, _data: BufferSource) => {});
  readonly sendFeatureReport = vi.fn(async (_reportId: number, _data: BufferSource) => {});

  readonly #listeners = new Map<string, Set<Listener>>();

  constructor(options: {
    vendorId?: number;
    productId?: number;
    productName?: string;
    collections?: HIDCollectionInfo[];
  } = {}) {
    this.vendorId = options.vendorId ?? 0x1234;
    this.productId = options.productId ?? 0xabcd;
    this.productName = options.productName ?? "Fake HID";
    this.collections =
      options.collections ??
      ([
        {
          usagePage: 1,
          usage: 2,
          type: "application",
          children: [],
          inputReports: [],
          outputReports: [],
          featureReports: [],
        },
      ] as unknown as HIDCollectionInfo[]);
  }

  addEventListener(type: string, cb: Listener): void {
    let set = this.#listeners.get(type);
    if (!set) {
      set = new Set();
      this.#listeners.set(type, set);
    }
    set.add(cb);
  }

  removeEventListener(type: string, cb: Listener): void {
    this.#listeners.get(type)?.delete(cb);
  }

  dispatchInputReport(reportId: number, bytes: Uint8Array): void {
    const data = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    const ev = { reportId, data } as unknown as HIDInputReportEvent;
    for (const cb of this.#listeners.get("inputreport") ?? []) cb(ev);
  }
}

function bufferSourceToBytes(src: BufferSource): Uint8Array {
  return src instanceof ArrayBuffer ? new Uint8Array(src) : new Uint8Array(src.buffer, src.byteOffset, src.byteLength);
}

describe("WebHidPassthroughRuntime", () => {
  it("subscribes to WebHidPassthroughManager attachedDevices (attachments list)", async () => {
    const device = new FakeHidDevice();

    const push = vi.fn();
    const bridge: WebHidPassthroughBridgeLike = {
      push_input_report: push,
      drain_next_output_report: vi.fn(() => null),
      configured: vi.fn(() => true),
      free: vi.fn(),
    };

    // Minimal manager stub: the real manager exposes attachments, not raw devices.
    const state: WebHidPassthroughState = {
      supported: true,
      knownDevices: [],
      attachedDevices: [{ device: device as unknown as HIDDevice, deviceId: "dev-1", guestPath: [0] }],
    };
    const manager: Pick<WebHidPassthroughManager, "subscribe" | "getState"> = {
      getState: () => state,
      subscribe: (listener: (s: typeof state) => void) => {
        listener(state);
        return () => {};
      },
    };

    // eslint-disable-next-line no-new
    new WebHidPassthroughRuntime({
      manager,
      createBridge: () => bridge,
      pollIntervalMs: 0,
    });

    // Allow the async attach path (`subscribe` -> `syncAttachedDevices` -> `attachDevice`) to run.
    // Use a macrotask turn so all microtasks (device.open() + attach continuation) are drained.
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(device.opened).toBe(true);

    device.dispatchInputReport(4, new Uint8Array([9, 8, 7]));
    expect(push).toHaveBeenCalledTimes(1);
    expect(push).toHaveBeenCalledWith(4, expect.any(Uint8Array));
  });

  it("forwards inputreport events to push_input_report", async () => {
    const device = new FakeHidDevice();

    const push = vi.fn();
    const bridge: WebHidPassthroughBridgeLike = {
      push_input_report: push,
      drain_next_output_report: vi.fn(() => null),
      configured: vi.fn(() => true),
      free: vi.fn(),
    };

    const runtime = new WebHidPassthroughRuntime({
      createBridge: () => bridge,
      pollIntervalMs: 0,
    });
    await runtime.attachDevice(device as unknown as HIDDevice);

    device.dispatchInputReport(7, new Uint8Array([1, 2, 3]));

    expect(push).toHaveBeenCalledTimes(1);
    expect(push.mock.calls[0][0]).toBe(7);
    expect(Array.from(push.mock.calls[0][1] as Uint8Array)).toEqual([1, 2, 3]);
  });

  it("clamps oversized inputreport payloads before forwarding to push_input_report", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      const device = new FakeHidDevice({
        collections: [
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
        ] as unknown as HIDCollectionInfo[],
      });

      const push = vi.fn();
      const bridge: WebHidPassthroughBridgeLike = {
        push_input_report: push,
        drain_next_output_report: vi.fn(() => null),
        configured: vi.fn(() => true),
        free: vi.fn(),
      };

      const runtime = new WebHidPassthroughRuntime({
        createBridge: () => bridge,
        pollIntervalMs: 0,
      });
      await runtime.attachDevice(device as unknown as HIDDevice);

      const huge = new Uint8Array(1024 * 1024);
      huge.set([1, 2, 3, 4], 0);
      device.dispatchInputReport(1, huge);

      expect(push).toHaveBeenCalledTimes(1);
      expect(push.mock.calls[0][0]).toBe(1);
      const payload = push.mock.calls[0][1] as Uint8Array;
      expect(payload.byteLength).toBe(4);
      expect(Array.from(payload)).toEqual([1, 2, 3, 4]);
    } finally {
      warn.mockRestore();
    }
  });

  it("zero-pads short inputreport payloads before forwarding to push_input_report", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      const device = new FakeHidDevice({
        collections: [
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
        ] as unknown as HIDCollectionInfo[],
      });

      const push = vi.fn();
      const bridge: WebHidPassthroughBridgeLike = {
        push_input_report: push,
        drain_next_output_report: vi.fn(() => null),
        configured: vi.fn(() => true),
        free: vi.fn(),
      };

      const runtime = new WebHidPassthroughRuntime({
        createBridge: () => bridge,
        pollIntervalMs: 0,
      });
      await runtime.attachDevice(device as unknown as HIDDevice);

      device.dispatchInputReport(1, new Uint8Array([9, 8]));

      expect(push).toHaveBeenCalledTimes(1);
      const payload = push.mock.calls[0][1] as Uint8Array;
      expect(Array.from(payload)).toEqual([9, 8, 0, 0]);
    } finally {
      warn.mockRestore();
    }
  });

  it("hard-caps unknown inputreport sizes before forwarding to push_input_report", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      const device = new FakeHidDevice({
        collections: [
          {
            usagePage: 1,
            usage: 2,
            type: "application",
            children: [],
            inputReports: [],
            outputReports: [],
            featureReports: [],
          },
        ] as unknown as HIDCollectionInfo[],
      });

      const push = vi.fn();
      const bridge: WebHidPassthroughBridgeLike = {
        push_input_report: push,
        drain_next_output_report: vi.fn(() => null),
        configured: vi.fn(() => true),
        free: vi.fn(),
      };

      const runtime = new WebHidPassthroughRuntime({
        createBridge: () => bridge,
        pollIntervalMs: 0,
      });
      await runtime.attachDevice(device as unknown as HIDDevice);

      const huge = new Uint8Array(1024 * 1024);
      huge.set([1, 2, 3], 0);
      device.dispatchInputReport(99, huge);

      expect(push).toHaveBeenCalledTimes(1);
      const payload = push.mock.calls[0][1] as Uint8Array;
      expect(payload.byteLength).toBe(64);
      expect(Array.from(payload.slice(0, 3))).toEqual([1, 2, 3]);
    } finally {
      warn.mockRestore();
    }
  });

  it("drops inputreport events with invalid reportId values before forwarding to push_input_report", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      const device = new FakeHidDevice();

      const push = vi.fn();
      const bridge: WebHidPassthroughBridgeLike = {
        push_input_report: push,
        drain_next_output_report: vi.fn(() => null),
        configured: vi.fn(() => true),
        free: vi.fn(),
      };

      const runtime = new WebHidPassthroughRuntime({
        createBridge: () => bridge,
        pollIntervalMs: 0,
      });
      await runtime.attachDevice(device as unknown as HIDDevice);

      const huge = new Uint8Array(1024 * 1024);
      huge.set([1, 2, 3], 0);
      device.dispatchInputReport(-1, huge);

      expect(push).toHaveBeenCalledTimes(0);
      expect(warn.mock.calls.some((call) => String(call[0]).includes("invalid reportId"))).toBe(true);
    } finally {
      warn.mockRestore();
    }
  });

  it("drains output reports and calls the correct WebHID send method", async () => {
    const device = new FakeHidDevice();

    const outputData = new Uint8Array([0xaa]);
    const featureData = new Uint8Array([0xbb, 0xcc]);
    const drain = vi
      .fn()
      .mockReturnValueOnce({ reportType: "output", reportId: 1, data: outputData })
      .mockReturnValueOnce({ reportType: "feature", reportId: 2, data: featureData })
      .mockReturnValueOnce(null);

    const bridge: WebHidPassthroughBridgeLike = {
      push_input_report: vi.fn(),
      drain_next_output_report: drain,
      configured: vi.fn(() => true),
      free: vi.fn(),
    };

    const runtime = new WebHidPassthroughRuntime({
      createBridge: () => bridge,
      pollIntervalMs: 0,
    });
    await runtime.attachDevice(device as unknown as HIDDevice);

    runtime.pollOnce();
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(device.sendReport).toHaveBeenCalledTimes(1);
    expect(device.sendReport).toHaveBeenCalledWith(1, outputData);
    expect(device.sendFeatureReport).toHaveBeenCalledTimes(1);
    expect(device.sendFeatureReport).toHaveBeenCalledWith(2, featureData);
  });

  it("hard-caps oversized output report payloads based on the reportId prefix byte", async () => {
    const device = new FakeHidDevice();

    const huge = new Uint8Array(0xffff);
    huge.set([1, 2, 3], 0);
    const drain = vi.fn().mockReturnValueOnce({ reportType: "output", reportId: 9, data: huge }).mockReturnValueOnce(null);

    const bridge: WebHidPassthroughBridgeLike = {
      push_input_report: vi.fn(),
      drain_next_output_report: drain,
      configured: vi.fn(() => true),
      free: vi.fn(),
    };

    const runtime = new WebHidPassthroughRuntime({
      createBridge: () => bridge,
      pollIntervalMs: 0,
    });
    await runtime.attachDevice(device as unknown as HIDDevice);

    runtime.pollOnce();
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(device.sendReport).toHaveBeenCalledTimes(1);
    expect(device.sendReport.mock.calls[0]![0]).toBe(9);
    const payload = bufferSourceToBytes(device.sendReport.mock.calls[0]![1] as BufferSource);
    // reportId != 0 => on-wire report includes a reportId prefix byte, so clamp payload to 0xfffe.
    expect(payload.byteLength).toBe(0xfffe);
    expect(Array.from(payload.slice(0, 3))).toEqual([1, 2, 3]);
  });

  it("bounds per-device output report queue growth when sendReport stalls", async () => {
    const logger = vi.fn();
    const device = new FakeHidDevice();

    const first = deferred<void>();
    device.sendReport.mockImplementationOnce(() => first.promise);
    device.sendReport.mockImplementation(async () => {});

    const drain = vi
      .fn()
      .mockReturnValueOnce({ reportType: "output", reportId: 1, data: new Uint8Array([1]) })
      .mockReturnValueOnce({ reportType: "output", reportId: 2, data: new Uint8Array([2]) })
      .mockReturnValueOnce({ reportType: "output", reportId: 3, data: new Uint8Array([3]) })
      .mockReturnValueOnce(null);

    const bridge: WebHidPassthroughBridgeLike = {
      push_input_report: vi.fn(),
      drain_next_output_report: drain,
      configured: vi.fn(() => true),
      free: vi.fn(),
    };

    const runtime = new WebHidPassthroughRuntime({
      createBridge: () => bridge,
      pollIntervalMs: 0,
      maxPendingOutputReportsPerDevice: 1,
      maxOutputReportsPerPoll: 8,
      logger,
    });
    await runtime.attachDevice(device as unknown as HIDDevice);

    runtime.pollOnce();
    await new Promise((resolve) => setTimeout(resolve, 0));

    // First report starts sending and stalls.
    expect(device.sendReport).toHaveBeenCalledTimes(1);
    expect(device.sendReport.mock.calls[0]![0]).toBe(1);

    first.resolve(undefined);
    await new Promise((resolve) => setTimeout(resolve, 0));
    await new Promise((resolve) => setTimeout(resolve, 0));

    // Second report should still run after the first finishes. Third should be dropped due to queue cap.
    expect(device.sendReport).toHaveBeenCalledTimes(2);
    expect(device.sendReport.mock.calls[1]![0]).toBe(2);

    expect(logger.mock.calls.some((call) => call[0] === "warn" && String(call[1]).includes("Dropping queued output reports"))).toBe(true);
  });

  it("bounds per-device output report queue growth by bytes when sendReport stalls", async () => {
    const logger = vi.fn();
    const device = new FakeHidDevice();

    const first = deferred<void>();
    device.sendReport.mockImplementationOnce(() => first.promise);
    device.sendReport.mockImplementation(async () => {});

    const drain = vi
      .fn()
      .mockReturnValueOnce({ reportType: "output", reportId: 1, data: new Uint8Array([1, 1, 1]) })
      .mockReturnValueOnce({ reportType: "output", reportId: 2, data: new Uint8Array([2, 2, 2]) })
      .mockReturnValueOnce({ reportType: "output", reportId: 3, data: new Uint8Array([3, 3, 3]) })
      .mockReturnValueOnce(null);

    const bridge: WebHidPassthroughBridgeLike = {
      push_input_report: vi.fn(),
      drain_next_output_report: drain,
      configured: vi.fn(() => true),
      free: vi.fn(),
    };

    const runtime = new WebHidPassthroughRuntime({
      createBridge: () => bridge,
      pollIntervalMs: 0,
      maxPendingOutputReportsPerDevice: 32,
      // First report becomes in-flight, second is queued (3 bytes), third would exceed this 4-byte cap and is dropped.
      maxPendingOutputReportBytesPerDevice: 4,
      maxOutputReportsPerPoll: 8,
      logger,
    });
    await runtime.attachDevice(device as unknown as HIDDevice);

    runtime.pollOnce();
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(device.sendReport).toHaveBeenCalledTimes(1);
    expect(device.sendReport.mock.calls[0]![0]).toBe(1);

    expect(logger.mock.calls.some((call) => call[0] === "warn" && String(call[1]).includes("maxPendingOutputReportBytesPerDevice"))).toBe(
      true,
    );

    first.resolve(undefined);
    await new Promise((resolve) => setTimeout(resolve, 0));
    await new Promise((resolve) => setTimeout(resolve, 0));

    // Second report should still run after the first finishes. Third should be dropped due to byte cap.
    expect(device.sendReport).toHaveBeenCalledTimes(2);
    expect(device.sendReport.mock.calls[1]![0]).toBe(2);
  });

  it("executes output reports sequentially per device", async () => {
    const device = new FakeHidDevice();

    const first = deferred<void>();
    device.sendReport.mockImplementationOnce(() => first.promise);

    const drain = vi
      .fn()
      .mockReturnValueOnce({ reportType: "output", reportId: 1, data: new Uint8Array([1]) })
      .mockReturnValueOnce({ reportType: "output", reportId: 2, data: new Uint8Array([2]) })
      .mockReturnValueOnce(null);

    const bridge: WebHidPassthroughBridgeLike = {
      push_input_report: vi.fn(),
      drain_next_output_report: drain,
      configured: vi.fn(() => true),
      free: vi.fn(),
    };

    const runtime = new WebHidPassthroughRuntime({
      createBridge: () => bridge,
      pollIntervalMs: 0,
    });
    await runtime.attachDevice(device as unknown as HIDDevice);

    runtime.pollOnce();

    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(device.sendReport).toHaveBeenCalledTimes(1);
    const sendReportCalls = device.sendReport.mock.calls as unknown as Array<[number, Uint8Array<ArrayBufferLike>]>;
    expect(sendReportCalls[0]![0]).toBe(1);

    first.resolve(undefined);
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(device.sendReport).toHaveBeenCalledTimes(2);
    expect(sendReportCalls[1]![0]).toBe(2);
  });

  it("drops pending output reports on detach", async () => {
    const device = new FakeHidDevice();

    const first = deferred<void>();
    device.sendReport.mockImplementationOnce(() => first.promise);

    const drain = vi
      .fn()
      .mockReturnValueOnce({ reportType: "output", reportId: 1, data: new Uint8Array([1]) })
      .mockReturnValueOnce({ reportType: "output", reportId: 2, data: new Uint8Array([2]) })
      .mockReturnValueOnce(null);

    const bridge: WebHidPassthroughBridgeLike = {
      push_input_report: vi.fn(),
      drain_next_output_report: drain,
      configured: vi.fn(() => true),
      free: vi.fn(),
    };

    const runtime = new WebHidPassthroughRuntime({
      createBridge: () => bridge,
      pollIntervalMs: 0,
    });
    await runtime.attachDevice(device as unknown as HIDDevice);

    runtime.pollOnce();
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(device.sendReport).toHaveBeenCalledTimes(1);

    await runtime.detachDevice(device as unknown as HIDDevice);

    first.resolve(undefined);
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(device.sendReport).toHaveBeenCalledTimes(1);
  });

  it("continues sending output reports after sendReport failure", async () => {
    const device = new FakeHidDevice();

    device.sendReport.mockImplementationOnce(async () => {
      throw new Error("nope");
    });
    device.sendReport.mockImplementationOnce(async () => {});

    const drain = vi
      .fn()
      .mockReturnValueOnce({ reportType: "output", reportId: 1, data: new Uint8Array([1]) })
      .mockReturnValueOnce({ reportType: "output", reportId: 2, data: new Uint8Array([2]) })
      .mockReturnValueOnce(null);

    const bridge: WebHidPassthroughBridgeLike = {
      push_input_report: vi.fn(),
      drain_next_output_report: drain,
      configured: vi.fn(() => true),
      free: vi.fn(),
    };

    const runtime = new WebHidPassthroughRuntime({
      createBridge: () => bridge,
      pollIntervalMs: 0,
      logger: () => {},
    });
    await runtime.attachDevice(device as unknown as HIDDevice);

    runtime.pollOnce();
    await new Promise((resolve) => setTimeout(resolve, 0));
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(device.sendReport).toHaveBeenCalledTimes(2);
    expect(device.sendReport.mock.calls[0]![0]).toBe(1);
    expect(device.sendReport.mock.calls[1]![0]).toBe(2);
  });

  it("cleans up listeners and frees wasm bridge on detach", async () => {
    const device = new FakeHidDevice();

    const push = vi.fn();
    const free = vi.fn();
    const bridge: WebHidPassthroughBridgeLike = {
      push_input_report: push,
      drain_next_output_report: vi.fn(() => null),
      configured: vi.fn(() => true),
      free,
    };

    const runtime = new WebHidPassthroughRuntime({
      createBridge: () => bridge,
      pollIntervalMs: 0,
    });
    await runtime.attachDevice(device as unknown as HIDDevice);

    device.dispatchInputReport(1, new Uint8Array([9]));
    expect(push).toHaveBeenCalledTimes(1);

    await runtime.detachDevice(device as unknown as HIDDevice);
    expect(device.close).toHaveBeenCalledTimes(1);
    expect(free).toHaveBeenCalledTimes(1);

    device.dispatchInputReport(1, new Uint8Array([10]));
    expect(push).toHaveBeenCalledTimes(1);
  });
});
