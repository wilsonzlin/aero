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
      clearNetTrace: () => {},
      getNetTraceStats: async () => ({ enabled: true, records: 1, bytes: 2, droppedRecords: 3, droppedBytes: 4 }),
    };

    (globalThis as any).window = { aero: {} };

    installNetTraceBackendOnAeroGlobal(fakeCoordinator as any);

    const netTrace = (globalThis as any).window.aero.netTrace;
    expect(netTrace).toBeTruthy();
    expect(typeof netTrace.getStats).toBe("function");
    expect(typeof netTrace.clearCapture).toBe("function");

    await expect(netTrace.getStats()).resolves.toEqual({ enabled: true, records: 1, bytes: 2, droppedRecords: 3, droppedBytes: 4 });
  });
});

