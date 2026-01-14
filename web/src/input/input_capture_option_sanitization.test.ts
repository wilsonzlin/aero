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
      const setIntervalMock = vi.fn((_handler: TimerHandler, _timeout?: number) => 1);
      win.setInterval = setIntervalMock;
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
      const intervalMs = setIntervalMock.mock.calls[0]?.[1];
      if (typeof intervalMs !== "number") {
        throw new Error(`expected setInterval to be called with a number timeout, got ${String(intervalMs)}`);
      }
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
      const setIntervalMock = vi.fn((_handler: TimerHandler, _timeout?: number) => 1);
      win.setInterval = setIntervalMock;
      win.clearInterval = vi.fn(() => {});

      const canvas = makeCanvasStub();

      const ioWorker = { postMessage: vi.fn() };
      const capture = new InputCapture(canvas, ioWorker, {
        enableGamepad: false,
        recycleBuffers: false,
        flushHz: 0,
      });

      capture.start();

      const intervalMs = setIntervalMock.mock.calls[0]?.[1];
      if (typeof intervalMs !== "number") {
        throw new Error(`expected setInterval to be called with a number timeout, got ${String(intervalMs)}`);
      }
      // Default is 125Hz -> 8ms interval.
      expect(intervalMs).toBe(8);

      capture.stop();
    });
  });
});
