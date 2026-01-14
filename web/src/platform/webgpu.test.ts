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
    expect(uncapturedHandler).toBeTypeOf("function");

    const preventDefault = vi.fn();
    uncapturedHandler?.({ preventDefault, error: "boom" });
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

    expect(device.onuncapturederror).toBeTypeOf("function");
    device.onuncapturederror({ error: "boom2" });
    expect(onUncapturedError).toHaveBeenCalledWith("boom2");
  });
});

