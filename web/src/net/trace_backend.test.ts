import { afterEach, describe, expect, it } from "vitest";

import { installNetTraceBackendOnAeroGlobal } from "./trace_backend";

describe("net/trace_backend", () => {
  const originalWindow = (globalThis as any).window;

  afterEach(() => {
    if (originalWindow === undefined) {
      delete (globalThis as any).window;
    } else {
      (globalThis as any).window = originalWindow;
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

    (globalThis as any).window = { aero: {} };

    installNetTraceBackendOnAeroGlobal(fakeCoordinator as any);

    const netTrace = (globalThis as any).window.aero.netTrace;
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

    (globalThis as any).window = { aero: "not-an-object" };

    expect(() => installNetTraceBackendOnAeroGlobal(fakeCoordinator as any)).not.toThrow();
    expect(typeof (globalThis as any).window.aero).toBe("object");
    expect(typeof (globalThis as any).window.aero.netTrace).toBe("object");
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
      getNetTraceStats: async () => ({ enabled: false, records: 0, bytes: 0, droppedRecords: 0, droppedBytes: 0 }),
    };

    (globalThis as any).window = { aero: {} };
    installNetTraceBackendOnAeroGlobal(fakeCoordinator as any);

    const netTrace = (globalThis as any).window.aero.netTrace;
    const bytes = await netTrace.downloadPcapng();
    expect(bytes.byteLength).toBeGreaterThan(0);
    // PCAPNG Section Header Block magic.
    expect(Array.from(bytes.slice(0, 4))).toEqual([0x0a, 0x0d, 0x0d, 0x0a]);

    const snapshot = await netTrace.exportPcapng();
    expect(snapshot.byteLength).toBeGreaterThan(0);
    expect(Array.from(snapshot.slice(0, 4))).toEqual([0x0a, 0x0d, 0x0d, 0x0a]);
  });
});
