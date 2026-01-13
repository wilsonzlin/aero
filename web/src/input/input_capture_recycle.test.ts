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

describe("InputCapture buffer recycling", () => {
  it("reuses ArrayBuffers returned by the worker when recycleBuffers=true", () => {
    withStubbedDocument(() => {
      const canvas = {
        tabIndex: 0,
        addEventListener: () => {},
        removeEventListener: () => {},
        focus: () => {},
      } as unknown as HTMLCanvasElement;

      const posted: ArrayBuffer[] = [];
      const postedByteLengths: number[] = [];
      let recycledFromFirstFlush: ArrayBuffer | null = null;
      let postCount = 0;
      let capture: InputCapture;

      const ioWorker = {
        postMessage: (msg: any, transfer?: any[]) => {
          postedByteLengths.push(msg.buffer.byteLength);
          posted.push(msg.buffer);
          // Ensure the capture side is requesting recycling via the documented wire format.
          expect(msg.recycle).toBe(true);
          expect(transfer).toContain(msg.buffer);

          // Simulate the worker transferring the ArrayBuffer back for reuse. Real workers would only
          // do this when `recycle: true` was requested.
          if (msg.recycle === true) {
            postCount++;
            // NOTE: In real worker transfer semantics, the ArrayBuffer object identity is not
            // preserved across threads. The sender's buffer is detached and the receiver sees a new
            // ArrayBuffer instance. Use `structuredClone(..., { transfer })` to mimic that behavior
            // (including detaching the sender-side buffer).
            const workerSide = structuredClone(msg.buffer, { transfer: [msg.buffer] });
            const recycled = structuredClone(workerSide, { transfer: [workerSide] });
            if (postCount === 1) {
              recycledFromFirstFlush = recycled;
            }
            (capture as any).handleWorkerMessage({
              data: { type: "in:input-batch-recycle", buffer: recycled },
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
      expect(postedByteLengths).toHaveLength(2);
      expect(recycledFromFirstFlush).not.toBeNull();
      // When the worker returns a buffer, the next flush should reuse it (no fresh allocation).
      expect(posted[1]).toBe(recycledFromFirstFlush);
      expect(postedByteLengths[1]).toBe(postedByteLengths[0]);
      expect(postedByteLengths[1]).toBeGreaterThan(0);
      // Returned buffers should be a separate instance than what we originally posted (detached).
      expect(posted[0]).not.toBe(recycledFromFirstFlush);
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
      const postedByteLengths: number[] = [];
      let recycledFromFirstFlush: ArrayBuffer | null = null;
      let postCount = 0;
      let capture: InputCapture;

      const ioWorker = {
        postMessage: (msg: any, transfer?: any[]) => {
          postedByteLengths.push(msg.buffer.byteLength);
          posted.push(msg.buffer);
          expect(msg.recycle).toBe(true);
          expect(transfer).toContain(msg.buffer);
          if (msg.recycle === true) {
            postCount++;
            const workerSide = structuredClone(msg.buffer, { transfer: [msg.buffer] });
            const recycled = structuredClone(workerSide, { transfer: [workerSide] });
            if (postCount === 1) {
              recycledFromFirstFlush = recycled;
            }
            (capture as any).handleWorkerMessage({
              data: { type: "in:input-batch-recycle", buffer: recycled },
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
      expect(postedByteLengths).toHaveLength(2);
      expect(recycledFromFirstFlush).not.toBeNull();

      // Sanity: the grown queue should have a larger backing buffer than the default 128-event queue.
      // Default: (2 + 128*4) * 4 = 2056 bytes; grown: (2 + 256*4) * 4 = 4104 bytes.
      expect(postedByteLengths[0]).toBeGreaterThan(2056);

      // The second flush should still be able to reuse the recycled buffer of that larger size.
      expect(posted[1]).toBe(recycledFromFirstFlush);
      expect(postedByteLengths[1]).toBe(postedByteLengths[0]);
      expect(posted[0]).not.toBe(recycledFromFirstFlush);
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
        postMessage: (msg: any, transfer?: any[]) => {
          posted.push(msg.buffer);
          expect(msg.recycle).not.toBe(true);
          expect(transfer).toContain(msg.buffer);
          // Detach the sender-side buffer to match real transfer semantics.
          const workerSide = structuredClone(msg.buffer, { transfer: [msg.buffer] });
          // Even if the worker tries to recycle, InputCapture should ignore it when disabled.
          (capture as any).handleWorkerMessage({
            data: { type: "in:input-batch-recycle", buffer: structuredClone(workerSide, { transfer: [workerSide] }) },
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
  it("caps distinct recycled buffer sizes so buckets do not grow unbounded", () => {
    withStubbedDocument(() => {
      const canvas = {
        tabIndex: 0,
        addEventListener: () => {},
        removeEventListener: () => {},
        focus: () => {},
      } as unknown as HTMLCanvasElement;
      const ioWorker = { postMessage: () => {} };
      const capture = new InputCapture(canvas, ioWorker, { enableGamepad: false });

      const handleWorkerMessage = (capture as any).handleWorkerMessage as (ev: { data: unknown }) => void;
      for (let i = 0; i < 100; i++) {
        handleWorkerMessage({
          data: { type: "in:input-batch-recycle", buffer: new ArrayBuffer(1024 + i) },
        });
      }

      const buckets = (capture as any).recycledBuffersBySize as Map<number, ArrayBuffer[]>;
      expect(buckets.size).toBeLessThanOrEqual(8);
    });
  });

  it("caps the number of buffers stored per bucket", () => {
    withStubbedDocument(() => {
      const canvas = {
        tabIndex: 0,
        addEventListener: () => {},
        removeEventListener: () => {},
        focus: () => {},
      } as unknown as HTMLCanvasElement;
      const ioWorker = { postMessage: () => {} };
      const capture = new InputCapture(canvas, ioWorker, { enableGamepad: false });

      const handleWorkerMessage = (capture as any).handleWorkerMessage as (ev: { data: unknown }) => void;
      const size = 2048;
      for (let i = 0; i < 50; i++) {
        handleWorkerMessage({
          data: { type: "in:input-batch-recycle", buffer: new ArrayBuffer(size) },
        });
      }

      const buckets = (capture as any).recycledBuffersBySize as Map<number, ArrayBuffer[]>;
      const bucket = buckets.get(size);
      expect(bucket).toBeDefined();
      expect(bucket).toHaveLength(4);
    });
  });

  it("ignores invalid recycle messages", () => {
    withStubbedDocument(() => {
      const canvas = {
        tabIndex: 0,
        addEventListener: () => {},
        removeEventListener: () => {},
        focus: () => {},
      } as unknown as HTMLCanvasElement;
      const ioWorker = { postMessage: () => {} };
      const capture = new InputCapture(canvas, ioWorker, { enableGamepad: false });

      const handleWorkerMessage = (capture as any).handleWorkerMessage as (ev: { data: unknown }) => void;
      handleWorkerMessage({ data: null });
      handleWorkerMessage({ data: { type: "not-a-recycle", buffer: new ArrayBuffer(16) } });
      handleWorkerMessage({ data: { type: "in:input-batch-recycle" } });
      handleWorkerMessage({ data: { type: "in:input-batch-recycle", buffer: new Uint8Array(16) } });
      // Oversized buffers should be ignored to avoid storing unexpectedly large allocations.
      handleWorkerMessage({
        data: { type: "in:input-batch-recycle", buffer: new ArrayBuffer(5 * 1024 * 1024) },
      });
      // Detached / empty buffers should not be stored.
      handleWorkerMessage({ data: { type: "in:input-batch-recycle", buffer: new ArrayBuffer(0) } });

      const buckets = (capture as any).recycledBuffersBySize as Map<number, ArrayBuffer[]>;
      expect(buckets.size).toBe(0);
    });
  });
});
