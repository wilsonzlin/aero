import { afterEach, describe, expect, it } from "vitest";
import { ensurePersistentStorage, getPersistentStorageInfo, getStorageEstimate } from "./storage_quota";

const originalNavigatorDescriptor = Object.getOwnPropertyDescriptor(globalThis, "navigator");

function stubNavigator(value: unknown): void {
  Object.defineProperty(globalThis, "navigator", {
    value,
    configurable: true,
    writable: true,
  });
}

afterEach(() => {
  if (originalNavigatorDescriptor) {
    Object.defineProperty(globalThis, "navigator", originalNavigatorDescriptor);
  } else {
    Reflect.deleteProperty(globalThis as any, "navigator");
  }
});

describe("getStorageEstimate", () => {
  it("returns supported=false when StorageManager is unavailable", async () => {
    stubNavigator({});

    const estimate = await getStorageEstimate();

    expect(estimate).toEqual({
      supported: false,
      usageBytes: null,
      quotaBytes: null,
      usagePercent: null,
      remainingBytes: null,
      warning: false,
    });
  });

  it("returns usage/quota/percent and sets warning when over threshold", async () => {
    stubNavigator({
      storage: {
        estimate: async () => ({ usage: 81, quota: 100 }),
      },
    });

    const estimate = await getStorageEstimate({ warningThresholdPercent: 80 });

    expect(estimate.supported).toBe(true);
    expect(estimate.usageBytes).toBe(81);
    expect(estimate.quotaBytes).toBe(100);
    expect(estimate.usagePercent).toBeCloseTo(81);
    expect(estimate.remainingBytes).toBe(19);
    expect(estimate.warning).toBe(true);
  });

  it("returns unknown percent if quota is missing/invalid", async () => {
    stubNavigator({
      storage: {
        estimate: async () => ({ usage: 10, quota: 0 }),
      },
    });

    const estimate = await getStorageEstimate();

    expect(estimate.supported).toBe(true);
    expect(estimate.usageBytes).toBe(10);
    expect(estimate.quotaBytes).toBe(0);
    expect(estimate.usagePercent).toBeNull();
    expect(estimate.remainingBytes).toBe(0);
    expect(estimate.warning).toBe(false);
  });
});

describe("persistent storage", () => {
  it("returns supported=false when persistence APIs are missing", async () => {
    stubNavigator({ storage: {} });

    const info = await getPersistentStorageInfo();
    expect(info).toEqual({ supported: false, persisted: null });

    const ensured = await ensurePersistentStorage();
    expect(ensured).toEqual({ supported: false, persisted: null, granted: null });
  });

  it("does not call persist() if already persisted", async () => {
    let persistCalls = 0;

    stubNavigator({
      storage: {
        persisted: async () => true,
        persist: async () => {
          persistCalls += 1;
          return true;
        },
      },
    });

    const info = await getPersistentStorageInfo();
    expect(info).toEqual({ supported: true, persisted: true });

    const ensured = await ensurePersistentStorage();
    expect(ensured).toEqual({ supported: true, persisted: true, granted: true });
    expect(persistCalls).toBe(0);
  });

  it("requests persist() when not yet persisted", async () => {
    stubNavigator({
      storage: {
        persisted: async () => false,
        persist: async () => true,
      },
    });

    const ensured = await ensurePersistentStorage();
    expect(ensured).toEqual({ supported: true, persisted: true, granted: true });
  });

  it("handles denied persistence requests", async () => {
    stubNavigator({
      storage: {
        persisted: async () => false,
        persist: async () => false,
      },
    });

    const ensured = await ensurePersistentStorage();
    expect(ensured).toEqual({ supported: true, persisted: false, granted: false });
  });
});
