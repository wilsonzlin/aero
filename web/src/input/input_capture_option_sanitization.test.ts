import { describe, expect, it, vi } from "vitest";

import { InputCapture } from "./input_capture";
import { withStubbedDocument, withStubbedWindow } from "./test_utils";

function withStubbedDom<T>(run: (ctx: { window: any; document: any }) => T): T {
  return withStubbedWindow((win) =>
    withStubbedDocument((doc) => {
      doc.addEventListener = vi.fn(() => {});
      doc.removeEventListener = vi.fn(() => {});
      doc.exitPointerLock = vi.fn(() => {});

      win.addEventListener = vi.fn(() => {});
      win.removeEventListener = vi.fn(() => {});
      win.setInterval = vi.fn(() => 1);
      win.clearInterval = vi.fn(() => {});

      return run({ window: win, document: doc });
    }),
  );
}

describe("InputCapture option sanitization", () => {
  it("sanitizes non-finite flushHz so setInterval is never called with NaN/Infinity", () => {
    withStubbedDom(({ window: win }) => {
      const canvas = {
        tabIndex: 0,
        addEventListener: () => {},
        removeEventListener: () => {},
        focus: () => {},
      } as unknown as HTMLCanvasElement;

      const ioWorker = { postMessage: vi.fn() };
      const capture = new InputCapture(canvas, ioWorker, {
        enableGamepad: false,
        recycleBuffers: false,
        flushHz: Number.NaN,
      });

      capture.start();

      expect(win.setInterval).toHaveBeenCalled();
      const intervalMs = (win.setInterval as any).mock.calls[0][1] as number;
      expect(Number.isFinite(intervalMs)).toBe(true);
      expect(intervalMs).toBeGreaterThanOrEqual(1);

      capture.stop();
    });
  });

  it("sanitizes zero/negative flushHz to a safe default", () => {
    withStubbedDom(({ window: win }) => {
      const canvas = {
        tabIndex: 0,
        addEventListener: () => {},
        removeEventListener: () => {},
        focus: () => {},
      } as unknown as HTMLCanvasElement;

      const ioWorker = { postMessage: vi.fn() };
      const capture = new InputCapture(canvas, ioWorker, {
        enableGamepad: false,
        recycleBuffers: false,
        flushHz: 0,
      });

      capture.start();

      const intervalMs = (win.setInterval as any).mock.calls[0][1] as number;
      // Default is 125Hz -> 8ms interval.
      expect(intervalMs).toBe(8);

      capture.stop();
    });
  });
});

