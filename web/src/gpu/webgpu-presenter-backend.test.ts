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

class FakeGpuDeviceOnProperty {
  onuncapturederror: ((ev: any) => void) | null = null;
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

  it("does not prefix primitive errors with constructor name (e.g. String: ...)", () => {
    const backend = new WebGpuPresenterBackend();
    const device = new FakeGpuDevice();

    const onError = vi.fn();
    (backend as any).opts = { onError };
    (backend as any).destroyed = false;
    (backend as any).device = device;

    (backend as any).installUncapturedErrorHandler(device);

    device.emit("uncapturederror", { error: "boom" });
    expect(onError).toHaveBeenCalledTimes(1);
    const err = onError.mock.calls[0]?.[0] as unknown as PresenterError;
    expect(err).toBeInstanceOf(PresenterError);
    expect(err.message).toBe("boom");
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

  it("falls back to onuncapturederror property when addEventListener is unavailable", () => {
    const backend = new WebGpuPresenterBackend();
    const device = new FakeGpuDeviceOnProperty();

    const onError = vi.fn();
    (backend as any).opts = { onError };
    (backend as any).destroyed = false;
    (backend as any).device = device;

    (backend as any).installUncapturedErrorHandler(device);

    expect(device.onuncapturederror).toBeTypeOf("function");
    device.onuncapturederror?.({ error: { name: "GPUValidationError", message: "oops" } });

    expect(onError).toHaveBeenCalledTimes(1);
    const err = onError.mock.calls[0]?.[0] as unknown as PresenterError;
    expect(err).toBeInstanceOf(PresenterError);
    expect(err.code).toBe("webgpu_uncaptured_error");

    (backend as any).uninstallUncapturedErrorHandler();
    expect(device.onuncapturederror).toBeNull();
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

describe("WebGpuPresenterBackend presentDirtyRects", () => {
  it("passes only the populated portion of the dirty-rect staging buffer to writeTexture()", () => {
    const backend = new WebGpuPresenterBackend();

    const writeTexture = vi.fn();
    (backend as any).canvas = {} as any;
    (backend as any).device = {} as any;
    (backend as any).queue = { writeTexture } as any;
    (backend as any).ctx = {} as any;
    (backend as any).pipeline = {} as any;
    (backend as any).bindGroup = {} as any;
    (backend as any).frameTexture = {} as any;
    // Avoid pulling in the full WebGPU rendering path; presentDirtyRects should still upload.
    (backend as any).renderToCanvas = () => true;

    const srcWidth = 256;
    const srcHeight = 256;
    const stride = srcWidth * 4;
    (backend as any).srcWidth = srcWidth;
    (backend as any).srcHeight = srcHeight;

    const frame = new Uint8Array(stride * srcHeight);

    // First rect is large, causing the staging buffer to be allocated/grown.
    // Second rect is tiny and reuses the existing staging buffer; we should not
    // pass the full oversized buffer to writeTexture().
    const dirtyRects = [
      { x: 0, y: 0, w: 128, h: 128 },
      { x: 0, y: 0, w: 1, h: 1 },
    ];

    const didPresent = backend.presentDirtyRects(frame, stride, dirtyRects);
    expect(didPresent).toBe(true);

    expect(writeTexture).toHaveBeenCalledTimes(2);
    const call0 = writeTexture.mock.calls[0];
    const call1 = writeTexture.mock.calls[1];

    const data0 = call0?.[1] as Uint8Array;
    const data1 = call1?.[1] as Uint8Array;
    expect(data0).toBeInstanceOf(Uint8Array);
    expect(data1).toBeInstanceOf(Uint8Array);

    // 128px wide rect => rowBytes=512, bytesPerRow=512, height=128 => 512*128 bytes.
    expect(data0.byteLength).toBe(512 * 128);
    // 1px wide rect => rowBytes=4, bytesPerRow=256, height=1 => 4 bytes (no last-row padding required).
    expect(data1.byteLength).toBe(4);
  });
});
