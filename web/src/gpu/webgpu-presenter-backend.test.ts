import { describe, expect, it, vi } from "vitest";

import { WebGpuPresenterBackend } from "./webgpu-presenter-backend";
import { PresenterError } from "./presenter";

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
    for (const handler of this.listeners.get(type) ?? []) {
      handler(ev);
    }
  }
}

describe("WebGpuPresenterBackend uncaptured error handler", () => {
  it("forwards uncapturederror via onError and dedupes identical messages", () => {
    const backend = new WebGpuPresenterBackend();
    const device = new FakeGpuDevice();

    const onError = vi.fn();
    (backend as any).opts = { onError };
    (backend as any).destroyed = false;
    (backend as any).device = device;

    (backend as any).installUncapturedErrorHandler(device);

    const preventDefault = vi.fn();
    device.emit("uncapturederror", { preventDefault, error: { name: "GPUValidationError", message: "oops" } });
    device.emit("uncapturederror", { preventDefault, error: { name: "GPUValidationError", message: "oops" } });

    expect(preventDefault).toHaveBeenCalled();
    expect(onError).toHaveBeenCalledTimes(1);

    const err = onError.mock.calls[0]?.[0] as unknown;
    expect(err).toBeInstanceOf(PresenterError);
    const pe = err as PresenterError;
    expect(pe.code).toBe("webgpu_uncaptured_error");
    expect(pe.message).toContain("GPUValidationError");
  });

  it("uninstall prevents further forwarding", () => {
    const backend = new WebGpuPresenterBackend();
    const device = new FakeGpuDevice();

    const onError = vi.fn();
    (backend as any).opts = { onError };
    (backend as any).destroyed = false;
    (backend as any).device = device;

    (backend as any).installUncapturedErrorHandler(device);
    (backend as any).uninstallUncapturedErrorHandler();

    device.emit("uncapturederror", { error: { name: "GPUValidationError", message: "oops" } });
    expect(onError).toHaveBeenCalledTimes(0);
  });

  it("bounds its per-init dedupe cache size", () => {
    const backend = new WebGpuPresenterBackend();
    const device = new FakeGpuDevice();

    const onError = vi.fn();
    (backend as any).opts = { onError };
    (backend as any).destroyed = false;
    (backend as any).device = device;

    (backend as any).installUncapturedErrorHandler(device);

    for (let i = 0; i < 200; i += 1) {
      device.emit("uncapturederror", { error: { name: "GPUValidationError", message: `msg-${i}` } });
    }

    expect(onError).toHaveBeenCalledTimes(200);
    expect(((backend as any).seenUncapturedErrorKeys as Set<string>).size).toBeLessThanOrEqual(128);
  });
});
