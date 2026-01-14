import { describe, expect, it, vi } from "vitest";

import { InputCapture } from "./input_capture";
import { makeCanvasStub, withStubbedDom } from "./test_utils";

describe("InputCapture option sanitization", () => {
  it("sanitizes non-finite flushHz so setInterval is never called with NaN/Infinity", () => {
    withStubbedDom(({ window: win, document: doc }) => {
      doc.addEventListener = vi.fn(() => {});
      doc.removeEventListener = vi.fn(() => {});
      doc.exitPointerLock = vi.fn(() => {});

      win.addEventListener = vi.fn(() => {});
      win.removeEventListener = vi.fn(() => {});
      win.setInterval = vi.fn(() => 1);
      win.clearInterval = vi.fn(() => {});

      const canvas = makeCanvasStub();

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
    withStubbedDom(({ window: win, document: doc }) => {
      doc.addEventListener = vi.fn(() => {});
      doc.removeEventListener = vi.fn(() => {});
      doc.exitPointerLock = vi.fn(() => {});

      win.addEventListener = vi.fn(() => {});
      win.removeEventListener = vi.fn(() => {});
      win.setInterval = vi.fn(() => 1);
      win.clearInterval = vi.fn(() => {});

      const canvas = makeCanvasStub();

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
