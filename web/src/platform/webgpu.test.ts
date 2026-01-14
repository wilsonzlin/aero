import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { requestWebGpuDevice } from "./webgpu";

describe("requestWebGpuDevice()", () => {
  const originalNavigatorDesc = Object.getOwnPropertyDescriptor(globalThis, "navigator");

  beforeEach(() => {
    // Ensure tests can stub navigator.gpu even on Node versions where `navigator` is an accessor.
    if (!originalNavigatorDesc || !originalNavigatorDesc.configurable) {
      throw new Error("globalThis.navigator is not configurable in this environment");
    }
  });

  afterEach(() => {
    if (originalNavigatorDesc) {
      Object.defineProperty(globalThis, "navigator", originalNavigatorDesc);
    }
  });

  it("registers an uncapturederror handler via addEventListener when available", async () => {
    let uncapturedHandler: ((ev: any) => void) | null = null;

    const device = {
      addEventListener: vi.fn((type: string, handler: (ev: any) => void) => {
        if (type === "uncapturederror") uncapturedHandler = handler;
      }),
    };

    const adapter = {
      requestDevice: vi.fn(async () => device),
    };

    const gpu = {
      requestAdapter: vi.fn(async () => adapter),
      getPreferredCanvasFormat: vi.fn(() => "bgra8unorm"),
    };

    Object.defineProperty(globalThis, "navigator", { value: { gpu }, configurable: true });

    const onUncapturedError = vi.fn();
    const info = await requestWebGpuDevice({ onUncapturedError });

    expect(info.device).toBe(device);
    expect(info.adapter).toBe(adapter);
    expect(info.preferredFormat).toBe("bgra8unorm");

    expect(device.addEventListener).toHaveBeenCalled();
    expect(typeof uncapturedHandler).toBe("function");

    const preventDefault = vi.fn();
    // TypeScript control-flow analysis doesn't model the callback write into `uncapturedHandler`
    // inside the addEventListener mock, so it may narrow the variable to `null` here. Assert the
    // expected callable shape for the test.
    (uncapturedHandler as ((ev: any) => void) | null)?.({ preventDefault, error: "boom" });
    expect(preventDefault).toHaveBeenCalled();
    expect(onUncapturedError).toHaveBeenCalledWith("boom");
  });

  it("falls back to onuncapturederror property when addEventListener is unavailable", async () => {
    const device: any = {
      onuncapturederror: null,
    };

    const adapter = {
      requestDevice: vi.fn(async () => device),
    };

    const gpu = {
      requestAdapter: vi.fn(async () => adapter),
      getPreferredCanvasFormat: vi.fn(() => "bgra8unorm"),
    };

    Object.defineProperty(globalThis, "navigator", { value: { gpu }, configurable: true });

    const onUncapturedError = vi.fn();
    await requestWebGpuDevice({ onUncapturedError });

    expect(typeof device.onuncapturederror).toBe("function");
    (device.onuncapturederror as (ev: any) => void)({ error: "boom2" });
    expect(onUncapturedError).toHaveBeenCalledWith("boom2");
  });

  it("defaults to console.error logging when onUncapturedError is not provided", async () => {
    let uncapturedHandler: ((ev: any) => void) | null = null;
    const device = {
      addEventListener: vi.fn((type: string, handler: (ev: any) => void) => {
        if (type === "uncapturederror") uncapturedHandler = handler;
      }),
    };

    const adapter = { requestDevice: vi.fn(async () => device) };
    const gpu = {
      requestAdapter: vi.fn(async () => adapter),
      getPreferredCanvasFormat: vi.fn(() => "bgra8unorm"),
    };
    Object.defineProperty(globalThis, "navigator", { value: { gpu }, configurable: true });

    const spy = vi.spyOn(console, "error").mockImplementation(() => {});
    await requestWebGpuDevice();

    expect(typeof uncapturedHandler).toBe("function");
    (uncapturedHandler as ((ev: any) => void) | null)?.({ error: "boom3" });
    (uncapturedHandler as ((ev: any) => void) | null)?.({ error: "boom3" });
    expect(spy).toHaveBeenCalledTimes(1);
    spy.mockRestore();
  });

  it("treats non-function onUncapturedError values as unset", async () => {
    let uncapturedHandler: ((ev: any) => void) | null = null;
    const device = {
      addEventListener: vi.fn((type: string, handler: (ev: any) => void) => {
        if (type === "uncapturederror") uncapturedHandler = handler;
      }),
    };

    const adapter = { requestDevice: vi.fn(async () => device) };
    const gpu = {
      requestAdapter: vi.fn(async () => adapter),
      getPreferredCanvasFormat: vi.fn(() => "bgra8unorm"),
    };
    Object.defineProperty(globalThis, "navigator", { value: { gpu }, configurable: true });

    const spy = vi.spyOn(console, "error").mockImplementation(() => {});
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    await requestWebGpuDevice({ onUncapturedError: 123 as any });

    expect(typeof uncapturedHandler).toBe("function");
    (uncapturedHandler as ((ev: any) => void) | null)?.({ error: "boom4" });
    (uncapturedHandler as ((ev: any) => void) | null)?.({ error: "boom4" });
    expect(spy).toHaveBeenCalledTimes(1);
    spy.mockRestore();
  });
});
