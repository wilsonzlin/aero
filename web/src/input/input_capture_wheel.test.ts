import { describe, expect, it, vi } from "vitest";

import { InputEventType } from "./event_queue";
import { InputCapture } from "./input_capture";

function withStubbedDocument<T>(run: (doc: any) => T): T {
  const original = (globalThis as any).document;
  const doc = {
    pointerLockElement: null,
    visibilityState: "visible",
    hasFocus: () => true,
    addEventListener: () => {},
    removeEventListener: () => {},
    exitPointerLock: () => {},
  };
  (globalThis as any).document = doc;
  try {
    return run(doc);
  } finally {
    (globalThis as any).document = original;
  }
}

function decodeFirstEventWords(buffer: ArrayBuffer): Int32Array {
  const words = new Int32Array(buffer);
  expect(words[0]).toBeGreaterThan(0);
  return words;
}

describe("InputCapture wheel handling", () => {
  it("accumulates small DOM_DELTA_PIXEL wheel deltas into discrete steps", () => {
    withStubbedDocument(() => {
      const canvas = {
        tabIndex: 0,
        addEventListener: () => {},
        removeEventListener: () => {},
        focus: () => {},
      } as unknown as HTMLCanvasElement;

      const posted: any[] = [];
      const ioWorker = {
        postMessage: (msg: unknown) => posted.push(msg),
      };

      const capture = new InputCapture(canvas, ioWorker, { enableGamepad: false, recycleBuffers: false });

      // Simulate the canvas being focused.
      (capture as any).hasFocus = true;

      const preventDefault = vi.fn();
      const stopPropagation = vi.fn();

      // 20px per event in DOM_DELTA_PIXEL mode -> 0.2 "clicks" per event (see input_capture.ts).
      // Ensure we don't lose small deltas entirely.
      for (let i = 0; i < 4; i++) {
        const ev = { deltaY: 20, deltaMode: 0, preventDefault, stopPropagation, timeStamp: i } as unknown as WheelEvent;
        (capture as any).handleWheel(ev);
        expect((capture as any).queue.size).toBe(0);
      }

      const ev = { deltaY: 20, deltaMode: 0, preventDefault, stopPropagation, timeStamp: 5 } as unknown as WheelEvent;
      (capture as any).handleWheel(ev);
      expect((capture as any).queue.size).toBe(1);
      expect(stopPropagation).toHaveBeenCalledTimes(5);

      capture.flushNow();

      expect(posted).toHaveLength(1);
      const msg = posted[0] as { buffer: ArrayBuffer };
      const words = decodeFirstEventWords(msg.buffer);

      expect(words[0]).toBe(1); // count
      expect(words[2]).toBe(InputEventType.MouseWheel);
      expect(words[4]).toBe(-1); // DOM deltaY > 0 => wheel down => PS/2 negative
      expect(words[5]).toBe(0);
    });
  });

  it("preserves fractional wheel deltas across events (no per-event truncation)", () => {
    withStubbedDocument(() => {
      const canvas = {
        tabIndex: 0,
        addEventListener: () => {},
        removeEventListener: () => {},
        focus: () => {},
      } as unknown as HTMLCanvasElement;

      const posted: any[] = [];
      const ioWorker = {
        postMessage: (msg: unknown) => posted.push(msg),
      };

      const capture = new InputCapture(canvas, ioWorker, { enableGamepad: false, recycleBuffers: false });

      (capture as any).hasFocus = true;

      const preventDefault = vi.fn();
      const stopPropagation = vi.fn();
      // 150px + 50px = 200px => 2 wheel "clicks" (100px/click). Old behavior would floor each event and
      // lose the final 0.5 click.
      (capture as any).handleWheel({ deltaY: 150, deltaMode: 0, preventDefault, stopPropagation, timeStamp: 1 } as unknown as WheelEvent);
      (capture as any).handleWheel({ deltaY: 50, deltaMode: 0, preventDefault, stopPropagation, timeStamp: 2 } as unknown as WheelEvent);
      expect(stopPropagation).toHaveBeenCalledTimes(2);

      capture.flushNow();

      expect(posted).toHaveLength(1);
      const msg = posted[0] as { buffer: ArrayBuffer };
      const words = decodeFirstEventWords(msg.buffer);

      expect(words[0]).toBe(1); // count
      expect(words[2]).toBe(InputEventType.MouseWheel);
      expect(words[4]).toBe(-2);
      expect(words[5]).toBe(0);
    });
  });
});
