import { describe, expect, it } from "vitest";

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

function keyDownEvent(code: string, timeStamp: number): KeyboardEvent {
  return {
    code,
    repeat: false,
    timeStamp,
    altKey: false,
    ctrlKey: false,
    shiftKey: false,
    metaKey: false,
    preventDefault: () => {},
    stopPropagation: () => {},
  } as unknown as KeyboardEvent;
}

describe("InputCapture input batch buffer recycling", () => {
  it("reuses ArrayBuffers returned by the worker when recycleBuffers=true", () => {
    withStubbedDocument(() => {
      const canvas = {
        tabIndex: 0,
        addEventListener: () => {},
        removeEventListener: () => {},
        focus: () => {},
      } as unknown as HTMLCanvasElement;

      const posted: ArrayBuffer[] = [];
      let capture: InputCapture;

      const ioWorker = {
        postMessage: (msg: any) => {
          posted.push(msg.buffer);
          // Ensure the capture side is requesting recycling via the documented wire format.
          expect(msg.recycle).toBe(true);

          // Simulate the worker transferring the ArrayBuffer back for reuse. Real workers would only
          // do this when `recycle: true` was requested.
          if (msg.recycle === true) {
            (capture as any).handleWorkerMessage({
              data: { type: "in:input-batch-recycle", buffer: msg.buffer },
            } as unknown as MessageEvent<unknown>);
          }
        },
      };

      capture = new InputCapture(canvas, ioWorker, { enableGamepad: false, recycleBuffers: true });
      (capture as any).hasFocus = true;

      (capture as any).handleKeyDown(keyDownEvent("KeyA", 0));
      capture.flushNow();

      (capture as any).handleKeyDown(keyDownEvent("KeyB", 1));
      capture.flushNow();

      expect(posted).toHaveLength(2);
      // When the worker returns a buffer, the next flush should reuse it (no fresh allocation).
      expect(posted[1]).toBe(posted[0]);
      expect(posted[1]?.byteLength).toBe(posted[0]?.byteLength);
    });
  });

  it("reuses recycled buffers even after the internal queue grows (larger byteLength buckets)", () => {
    withStubbedDocument(() => {
      const canvas = {
        tabIndex: 0,
        addEventListener: () => {},
        removeEventListener: () => {},
        focus: () => {},
      } as unknown as HTMLCanvasElement;

      const posted: ArrayBuffer[] = [];
      let capture: InputCapture;

      const ioWorker = {
        postMessage: (msg: any) => {
          posted.push(msg.buffer);
          expect(msg.recycle).toBe(true);
          if (msg.recycle === true) {
            (capture as any).handleWorkerMessage({
              data: { type: "in:input-batch-recycle", buffer: msg.buffer },
            } as unknown as MessageEvent<unknown>);
          }
        },
      };

      capture = new InputCapture(canvas, ioWorker, { enableGamepad: false, recycleBuffers: true });
      (capture as any).hasFocus = true;

      // Push enough events to trigger `InputEventQueue.grow()` from 128 -> 256 capacity.
      for (let i = 0; i < 65; i++) {
        (capture as any).handleKeyDown(keyDownEvent("KeyA", i));
      }
      capture.flushNow();

      (capture as any).handleKeyDown(keyDownEvent("KeyB", 999));
      capture.flushNow();

      expect(posted).toHaveLength(2);

      // Sanity: the grown queue should have a larger backing buffer than the default 128-event queue.
      // Default: (2 + 128*4) * 4 = 2056 bytes; grown: (2 + 256*4) * 4 = 4104 bytes.
      expect(posted[0].byteLength).toBeGreaterThan(2056);

      // The second flush should still be able to reuse the recycled buffer of that larger size.
      expect(posted[1]).toBe(posted[0]);
      expect(posted[1].byteLength).toBe(posted[0].byteLength);
    });
  });

  it("does not cache buffers when recycleBuffers=false", () => {
    withStubbedDocument(() => {
      const canvas = {
        tabIndex: 0,
        addEventListener: () => {},
        removeEventListener: () => {},
        focus: () => {},
      } as unknown as HTMLCanvasElement;

      const posted: ArrayBuffer[] = [];
      let capture: InputCapture;

      const ioWorker = {
        postMessage: (msg: any) => {
          posted.push(msg.buffer);
          expect(msg.recycle).not.toBe(true);
          // Even if the worker tries to recycle, InputCapture should ignore it when disabled.
          (capture as any).handleWorkerMessage({
            data: { type: "in:input-batch-recycle", buffer: msg.buffer },
          } as unknown as MessageEvent<unknown>);
        },
      };

      capture = new InputCapture(canvas, ioWorker, { enableGamepad: false, recycleBuffers: false });
      (capture as any).hasFocus = true;

      (capture as any).handleKeyDown(keyDownEvent("KeyA", 0));
      capture.flushNow();

      (capture as any).handleKeyDown(keyDownEvent("KeyB", 1));
      capture.flushNow();

      expect(posted).toHaveLength(2);
      expect(posted[1]).not.toBe(posted[0]);
    });
  });
});
