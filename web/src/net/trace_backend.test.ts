import { afterEach, describe, expect, it } from "vitest";

import { installNetTraceBackendOnAeroGlobal } from "./trace_backend";
import type { WorkerCoordinator } from "../runtime/coordinator";

type NetTraceStats = { enabled: boolean; records: number; bytes: number; droppedRecords: number; droppedBytes: number };
type NetTraceApi = {
  getStats: () => Promise<NetTraceStats>;
  clearCapture: () => void;
  downloadPcapng: () => Promise<Uint8Array>;
  exportPcapng: () => Promise<Uint8Array>;
};

describe("net/trace_backend", () => {
  const originalWindowDescriptor = Object.getOwnPropertyDescriptor(globalThis, "window");

  function stubWindow(value: unknown): void {
    Object.defineProperty(globalThis, "window", {
      value,
      configurable: true,
      enumerable: true,
      writable: true,
    });
  }

  afterEach(() => {
    if (originalWindowDescriptor) {
      Object.defineProperty(globalThis, "window", originalWindowDescriptor);
    } else {
      Reflect.deleteProperty(globalThis, "window");
    }
  });

  it("installs window.aero.netTrace with getStats and clearCapture shims", async () => {
    const fakeCoordinator = {
      isNetTraceEnabled: () => true,
      setNetTraceEnabled: () => {},
      takeNetTracePcapng: async () => new Uint8Array([1, 2, 3]),
      exportNetTracePcapng: async () => new Uint8Array([4, 5, 6]),
      clearNetTrace: () => {},
      getNetTraceStats: async () => ({ enabled: true, records: 1, bytes: 2, droppedRecords: 3, droppedBytes: 4 }),
    };

    stubWindow({ aero: {} });

    installNetTraceBackendOnAeroGlobal(fakeCoordinator as unknown as WorkerCoordinator);

    const netTrace = (globalThis as unknown as { window: { aero: { netTrace: NetTraceApi } } }).window.aero.netTrace;
    expect(netTrace).toBeTruthy();
    expect(typeof netTrace.getStats).toBe("function");
    expect(typeof netTrace.clearCapture).toBe("function");
    expect(typeof netTrace.exportPcapng).toBe("function");

    await expect(netTrace.getStats()).resolves.toEqual({ enabled: true, records: 1, bytes: 2, droppedRecords: 3, droppedBytes: 4 });
    await expect(netTrace.downloadPcapng()).resolves.toEqual(new Uint8Array([1, 2, 3]));
    await expect(netTrace.exportPcapng()).resolves.toEqual(new Uint8Array([4, 5, 6]));
  });

  it("repairs non-object window.aero values", () => {
    const fakeCoordinator = {
      isNetTraceEnabled: () => false,
      setNetTraceEnabled: () => {},
      takeNetTracePcapng: async () => new Uint8Array(),
      exportNetTracePcapng: async () => new Uint8Array(),
      clearNetTrace: () => {},
      getNetTraceStats: async () => ({ enabled: false, records: 0, bytes: 0, droppedRecords: 0, droppedBytes: 0 }),
    };

    stubWindow({ aero: "not-an-object" });

    expect(() => installNetTraceBackendOnAeroGlobal(fakeCoordinator as unknown as WorkerCoordinator)).not.toThrow();
    const aero = (globalThis as unknown as { window: { aero: unknown } }).window.aero;
    expect(typeof aero).toBe("object");
    expect(typeof (aero as { netTrace?: unknown }).netTrace).toBe("object");
  });

  it("returns an empty PCAPNG when the net worker is unavailable", async () => {
    const fakeCoordinator = {
      isNetTraceEnabled: () => false,
      setNetTraceEnabled: () => {},
      takeNetTracePcapng: async () => {
        throw new Error("net worker not running");
      },
      exportNetTracePcapng: async () => {
        throw new Error("net worker not running");
      },
      clearNetTrace: () => {},
      getNetTraceStats: async () => {
        throw new Error("net worker not running");
      },
    };

    stubWindow({ aero: {} });
    installNetTraceBackendOnAeroGlobal(fakeCoordinator as unknown as WorkerCoordinator);

    const netTrace = (globalThis as unknown as { window: { aero: { netTrace: NetTraceApi } } }).window.aero.netTrace;
    const bytes = await netTrace.downloadPcapng();
    expect(bytes.byteLength).toBeGreaterThan(0);
    // PCAPNG Section Header Block magic.
    expect(Array.from(bytes.slice(0, 4))).toEqual([0x0a, 0x0d, 0x0d, 0x0a]);

    const snapshot = await netTrace.exportPcapng();
    expect(snapshot.byteLength).toBeGreaterThan(0);
    expect(Array.from(snapshot.slice(0, 4))).toEqual([0x0a, 0x0d, 0x0d, 0x0a]);
  });

  it("returns stub stats when the VM or net worker is not running yet", async () => {
    let called = 0;
    const fakeCoordinator = {
      getVmState: () => "stopped",
      getWorkerStatuses: () => ({ net: { state: "starting" } }),
      isNetTraceEnabled: () => true,
      setNetTraceEnabled: () => {},
      takeNetTracePcapng: async () => new Uint8Array(),
      exportNetTracePcapng: async () => new Uint8Array(),
      clearNetTrace: () => {},
      getNetTraceStats: async () => {
        called += 1;
        throw new Error("should not be called");
      },
    };

    stubWindow({ aero: {} });
    installNetTraceBackendOnAeroGlobal(fakeCoordinator as unknown as WorkerCoordinator);

    const netTrace = (globalThis as unknown as { window: { aero: { netTrace: NetTraceApi } } }).window.aero.netTrace;
    await expect(netTrace.getStats()).resolves.toEqual({
      enabled: true,
      records: 0,
      bytes: 0,
      droppedRecords: 0,
      droppedBytes: 0,
    });
    expect(called).toBe(0);
  });
});
