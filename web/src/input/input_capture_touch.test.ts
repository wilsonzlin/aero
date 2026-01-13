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

describe("InputCapture touch (PointerEvent) fallback", () => {
  it("translates touch drag into relative MouseMove with correct sign conventions", () => {
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

      (capture as any).handlePointerDown({
        pointerId: 1,
        pointerType: "touch",
        clientX: 100,
        clientY: 100,
        timeStamp: 0,
        preventDefault: vi.fn(),
        stopPropagation: vi.fn(),
      } satisfies Partial<PointerEvent>);

      (capture as any).handlePointerMove({
        pointerId: 1,
        pointerType: "touch",
        clientX: 110,
        clientY: 120,
        timeStamp: 1,
        preventDefault: vi.fn(),
        stopPropagation: vi.fn(),
      } satisfies Partial<PointerEvent>);

      // End the gesture to avoid a later tap being detected; movement is large enough to not count as a tap.
      (capture as any).handlePointerUp({
        pointerId: 1,
        pointerType: "touch",
        clientX: 110,
        clientY: 120,
        timeStamp: 2,
        preventDefault: vi.fn(),
        stopPropagation: vi.fn(),
      } satisfies Partial<PointerEvent>);

      capture.flushNow();

      expect(posted).toHaveLength(1);
      const msg = posted[0] as { buffer: ArrayBuffer };
      const words = new Int32Array(msg.buffer);

      expect(words[0]).toBe(1); // count
      expect(words[2]).toBe(InputEventType.MouseMove);
      expect(words[4]).toBe(10);
      // DOM Y increases downward; PS/2 positive Y is up.
      expect(words[5]).toBe(-20);
    });
  });

  it("translates a tap into a left click (MouseButtons down+up)", () => {
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

      (capture as any).handlePointerDown({
        pointerId: 1,
        pointerType: "touch",
        clientX: 50,
        clientY: 60,
        timeStamp: 100,
        preventDefault: vi.fn(),
        stopPropagation: vi.fn(),
      } satisfies Partial<PointerEvent>);

      (capture as any).handlePointerUp({
        pointerId: 1,
        pointerType: "touch",
        clientX: 50,
        clientY: 60,
        timeStamp: 150,
        preventDefault: vi.fn(),
        stopPropagation: vi.fn(),
      } satisfies Partial<PointerEvent>);

      capture.flushNow();

      expect(posted).toHaveLength(1);
      const msg = posted[0] as { buffer: ArrayBuffer };
      const words = new Int32Array(msg.buffer);
      expect(words[0]).toBe(2);

      // Event 0: left down.
      expect(words[2]).toBe(InputEventType.MouseButtons);
      expect(words[4]).toBe(1);

      // Event 1: left up.
      expect(words[6]).toBe(InputEventType.MouseButtons);
      expect(words[8]).toBe(0);
    });
  });
});

