import { describe, expect, it, vi } from "vitest";
import { WebHidPassthroughRuntime, type WebHidPassthroughBridgeLike } from "./webhid_passthrough_runtime";

type Listener = (event: unknown) => void;

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

  readonly sendReport = vi.fn(async () => {});
  readonly sendFeatureReport = vi.fn(async () => {});

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

describe("WebHidPassthroughRuntime", () => {
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

    expect(device.sendReport).toHaveBeenCalledTimes(1);
    expect(device.sendReport).toHaveBeenCalledWith(1, outputData);
    expect(device.sendFeatureReport).toHaveBeenCalledTimes(1);
    expect(device.sendFeatureReport).toHaveBeenCalledWith(2, featureData);
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

