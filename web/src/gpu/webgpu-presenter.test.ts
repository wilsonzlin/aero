import { describe, expect, it, vi } from "vitest";

import { WebGpuPresenter } from "./webgpu-presenter";

class FakeGpuDevice {
  private listeners = new Map<string, Set<(ev: any) => void>>();

  addEventListener(type: string, handler: (ev: any) => void): void {
    const set = this.listeners.get(type) ?? new Set();
    set.add(handler);
    this.listeners.set(type, set);
  }

  removeEventListener(type: string, handler: (ev: any) => void): void {
    this.listeners.get(type)?.delete(handler);
  }

  emit(type: string, ev: any): void {
    for (const handler of this.listeners.get(type) ?? []) handler(ev);
  }
}

class FakeGpuDeviceOnProperty {
  onuncapturederror: ((ev: any) => void) | null = null;
}

function createBarePresenter(device: any, opts: any): any {
  // Avoid invoking the heavy WebGpuPresenter constructor (pipeline creation); we only want to
  // exercise its uncaptured error handler logic.
  const p = Object.create(WebGpuPresenter.prototype) as any;
  p.device = device;
  p.opts = opts;
  p._uncapturedErrorDevice = null;
  p._onUncapturedError = null;
  p._seenUncapturedErrorKeys = new Set<string>();
  return p;
}

describe("WebGpuPresenter uncaptured error handler", () => {
  it("dedupes and forwards to opts.onError when provided", () => {
    const device = new FakeGpuDevice();
    const onError = vi.fn();
    const presenter = createBarePresenter(device, { onError });

    presenter._installUncapturedErrorHandler();

    const preventDefault = vi.fn();
    device.emit("uncapturederror", { preventDefault, error: { name: "GPUValidationError", message: "oops" } });
    device.emit("uncapturederror", { preventDefault, error: { name: "GPUValidationError", message: "oops" } });

    expect(preventDefault).toHaveBeenCalled();
    expect(onError).toHaveBeenCalledTimes(1);
    expect(onError).toHaveBeenCalledWith({ name: "GPUValidationError", message: "oops" });

    presenter._uninstallUncapturedErrorHandler();
    device.emit("uncapturederror", { error: { name: "GPUValidationError", message: "oops2" } });
    expect(onError).toHaveBeenCalledTimes(1);
  });

  it("does not prefix primitive error values with constructor name", () => {
    const device = new FakeGpuDevice();
    const onError = vi.fn();
    const presenter = createBarePresenter(device, { onError });

    presenter._installUncapturedErrorHandler();
    device.emit("uncapturederror", { error: "boom" });
    expect(onError).toHaveBeenCalledWith("boom");
  });

  it("falls back to device.onuncapturederror when addEventListener is unavailable", () => {
    const device = new FakeGpuDeviceOnProperty();
    const onError = vi.fn();
    const presenter = createBarePresenter(device, { onError });

    presenter._installUncapturedErrorHandler();
    expect(device.onuncapturederror).toBeTypeOf("function");
    device.onuncapturederror?.({ error: "boom2" });
    expect(onError).toHaveBeenCalledWith("boom2");

    presenter._uninstallUncapturedErrorHandler();
    expect(device.onuncapturederror).toBeNull();
  });
});

