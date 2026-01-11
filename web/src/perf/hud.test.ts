// @vitest-environment jsdom

import { afterEach, describe, expect, it, vi } from "vitest";

import { installHud } from "./hud";
import type { PerfApi } from "./types";

type Deferred<T> = {
  promise: Promise<T>;
  resolve: (value: T) => void;
  reject: (err: unknown) => void;
};

function defer<T>(): Deferred<T> {
  let resolve!: (value: T) => void;
  let reject!: (err: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

describe("Perf HUD Trace JSON export", () => {
  const originalCreateObjectURL = (URL as unknown as { createObjectURL?: unknown }).createObjectURL;
  const originalRevokeObjectURL = (URL as unknown as { revokeObjectURL?: unknown }).revokeObjectURL;
  const originalBlob = globalThis.Blob;

  afterEach(() => {
    document.body.innerHTML = "";
    vi.restoreAllMocks();
    (URL as unknown as { createObjectURL?: unknown }).createObjectURL = originalCreateObjectURL;
    (URL as unknown as { revokeObjectURL?: unknown }).revokeObjectURL = originalRevokeObjectURL;
    globalThis.Blob = originalBlob;
  });

  it("awaits exportTrace({ asString: true }) and downloads the returned string", async () => {
    class FakeBlob {
      readonly parts: unknown[];
      readonly type: string;
      constructor(parts: unknown[], opts?: { type?: string }) {
        this.parts = parts;
        this.type = opts?.type ?? "";
      }
    }

    // JSDOM's `Blob` implementation is intentionally minimal; override it so we
    // can assert on the payload passed into `new Blob([...])`.
    globalThis.Blob = FakeBlob as unknown as typeof Blob;

    const ctx = {
      setTransform: vi.fn(),
      clearRect: vi.fn(),
      beginPath: vi.fn(),
      moveTo: vi.fn(),
      lineTo: vi.fn(),
      stroke: vi.fn(),
      strokeStyle: "",
      lineWidth: 0,
    };
    vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockImplementation(() => ctx as unknown as CanvasRenderingContext2D);
    vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(() => {});

    const createdUrls: Blob[] = [];
    const createObjectURL = vi.fn((blob: Blob) => {
      createdUrls.push(blob);
      return "blob:trace";
    });
    const revokeObjectURL = vi.fn();
    (URL as unknown as { createObjectURL?: unknown }).createObjectURL = createObjectURL;
    (URL as unknown as { revokeObjectURL?: unknown }).revokeObjectURL = revokeObjectURL;

    const deferred = defer<string>();
    const exportTrace = vi.fn(() => deferred.promise);

    const perf = {
      getHudSnapshot: (out) => out,
      setHudActive: () => {},
      captureStart: () => {},
      captureStop: () => {},
      captureReset: () => {},
      export: () => ({}),
      traceStart: () => {},
      traceStop: () => {},
      exportTrace,
      traceEnabled: false,
    } as unknown as PerfApi;

    installHud(perf);

    const traceButton = Array.from(document.querySelectorAll<HTMLButtonElement>("button")).find(
      (btn) => btn.textContent === "Trace JSON",
    );
    expect(traceButton).toBeTruthy();

    traceButton!.click();

    expect(exportTrace).toHaveBeenCalledWith({ asString: true });
    expect(traceButton!.disabled).toBe(true);
    expect(createObjectURL).not.toHaveBeenCalled();

    const payload = '{"traceEvents":[]}';
    deferred.resolve(payload);
    await deferred.promise;

    // Let the async click handler resume after the awaited exportTrace promise.
    for (let i = 0; i < 5 && createObjectURL.mock.calls.length === 0; i += 1) {
      await Promise.resolve();
    }

    expect(createObjectURL).toHaveBeenCalledTimes(1);
    expect(createdUrls).toHaveLength(1);
    const blob = createdUrls[0] as unknown as FakeBlob;
    expect(blob.type).toBe("application/json");
    expect(blob.parts).toEqual([payload]);
    expect(traceButton!.disabled).toBe(false);
  });
});
