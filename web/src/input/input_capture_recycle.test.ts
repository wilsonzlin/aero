import { describe, expect, it } from "vitest";

import { InputCapture } from "./input_capture";
import { withStubbedDocument } from "./test_utils";

function withStubbedWindow<T>(run: (win: any) => T): T {
  const original = (globalThis as any).window;
  const win = {
    addEventListener: () => {},
    removeEventListener: () => {},
    setInterval: () => 1,
    clearInterval: () => {},
  };
  (globalThis as any).window = win;
  try {
    return run(win);
  } finally {
    (globalThis as any).window = original;
  }
}

function makeCanvasStub(): HTMLCanvasElement {
  return {
    tabIndex: 0,
    addEventListener: () => {},
    removeEventListener: () => {},
    focus: () => {},
  } as unknown as HTMLCanvasElement;
}

function transferToWorker(buffer: ArrayBuffer): ArrayBuffer {
  return structuredClone(buffer, { transfer: [buffer] });
}

function transferFromWorker(buffer: ArrayBuffer): ArrayBuffer {
  return structuredClone(buffer, { transfer: [buffer] });
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
  it("listens for worker recycle messages via start()'s message handler wiring", () => {
    withStubbedDocument((doc) =>
      withStubbedWindow(() => {
        const canvas = makeCanvasStub();

        doc.activeElement = canvas;

        let onMessage: ((ev: MessageEvent<unknown>) => void) | null = null;

        const posted: ArrayBuffer[] = [];
        const postedByteLengths: number[] = [];
        let recycledFromFirstFlush: ArrayBuffer | null = null;
        let postCount = 0;

        const ioWorker = {
          addEventListener: (type: "message", listener: (ev: MessageEvent<unknown>) => void) => {
            expect(type).toBe("message");
            onMessage = listener;
          },
          removeEventListener: () => {},
          postMessage: (msg: any, transfer?: any[]) => {
            postedByteLengths.push(msg.buffer.byteLength);
            posted.push(msg.buffer);
            expect(msg.recycle).toBe(true);
            expect(transfer).toContain(msg.buffer);
            expect(onMessage).not.toBeNull();

            // Mimic transfer -> worker -> transfer-back, which yields a *new* ArrayBuffer instance.
            const workerSide = transferToWorker(msg.buffer);
            const recycled = transferFromWorker(workerSide);

            postCount++;
            if (postCount === 1) {
              recycledFromFirstFlush = recycled;
            }

            onMessage?.({ data: { type: "in:input-batch-recycle", buffer: recycled } } as unknown as MessageEvent<unknown>);
          },
        };

        const capture = new InputCapture(canvas, ioWorker as any, { enableGamepad: false, recycleBuffers: true });
        capture.start();

        (capture as any).handleKeyDown(keyDownEvent("KeyA", 0));
        capture.flushNow();

        (capture as any).handleKeyDown(keyDownEvent("KeyB", 1));
        capture.flushNow();

        expect(posted).toHaveLength(2);
        expect(postedByteLengths).toHaveLength(2);
        expect(recycledFromFirstFlush).not.toBeNull();
        expect(posted[1]).toBe(recycledFromFirstFlush);
        expect(postedByteLengths[1]).toBe(postedByteLengths[0]);
      }),
    );
  });

  it("removes the worker message listener on stop()", () => {
    withStubbedDocument((doc) =>
      withStubbedWindow(() => {
        const canvas = makeCanvasStub();

        doc.activeElement = canvas;

        let added: ((ev: MessageEvent<unknown>) => void) | null = null;
        let removed: ((ev: MessageEvent<unknown>) => void) | null = null;

        const ioWorker = {
          addEventListener: (type: "message", listener: (ev: MessageEvent<unknown>) => void) => {
            if (type === "message") {
              added = listener;
            }
          },
          removeEventListener: (type: "message", listener: (ev: MessageEvent<unknown>) => void) => {
            if (type === "message") {
              removed = listener;
            }
          },
          postMessage: () => {},
        };

        const capture = new InputCapture(canvas, ioWorker as any, { enableGamepad: false, recycleBuffers: true });
        capture.start();
        expect(added).not.toBeNull();

        capture.stop();
        expect(removed).toBe(added);
      }),
    );
  });

  it("does not request buffer recycling on stop() flush (even when recycleBuffers=true)", () => {
    withStubbedDocument((doc) =>
      withStubbedWindow(() => {
        const canvas = makeCanvasStub();

        doc.activeElement = canvas;

        const posted: any[] = [];
        const ioWorker = {
          addEventListener: () => {},
          removeEventListener: () => {},
          postMessage: (msg: any, transfer?: any[]) => {
            posted.push({ msg, transfer });
          },
        };

        const capture = new InputCapture(canvas, ioWorker as any, { enableGamepad: false, recycleBuffers: true });
        capture.start();

        (capture as any).handleKeyDown(keyDownEvent("KeyA", 0));

        capture.stop();

        expect(posted).toHaveLength(1);
        const { msg, transfer } = posted[0]!;
        expect(msg.type).toBe("in:input-batch");
        expect(msg.recycle).toBeUndefined();
        expect(transfer).toContain(msg.buffer);
      }),
    );
  });

  it("reuses recycled buffers even when the recycle response arrives after flush (one-flush delay)", () => {
    withStubbedDocument(() => {
      const canvas = makeCanvasStub();

      const posted: ArrayBuffer[] = [];
      const workerSideCopies: ArrayBuffer[] = [];
      let capture: InputCapture;

      const ioWorker = {
        postMessage: (msg: any, transfer?: any[]) => {
          posted.push(msg.buffer);
          expect(msg.recycle).toBe(true);
          expect(transfer).toContain(msg.buffer);
          // Detach the sender-side buffer, but do not immediately return it.
          const workerSide = transferToWorker(msg.buffer);
          workerSideCopies.push(workerSide);
        },
      };

      capture = new InputCapture(canvas, ioWorker, { enableGamepad: false, recycleBuffers: true });
      (capture as any).hasFocus = true;

      // Flush #1: no recycled buffers exist yet, so the queue must allocate a new buffer after
      // transfer. The worker receives the transferred buffer (workerSideCopies[0]).
      (capture as any).handleKeyDown(keyDownEvent("KeyA", 0));
      capture.flushNow();
      expect(posted).toHaveLength(1);
      expect(workerSideCopies).toHaveLength(1);

      // Deliver the recycle response *after* the flush (mimics real worker scheduling).
      const workerSide0 = workerSideCopies[0]!;
      const recycled0 = transferFromWorker(workerSide0);
      (capture as any).handleWorkerMessage({
        data: { type: "in:input-batch-recycle", buffer: recycled0 },
      } as unknown as MessageEvent<unknown>);

      // Flush #2: still uses the buffer allocated after flush #1, but should swap in `recycled0`
      // for subsequent batches.
      (capture as any).handleKeyDown(keyDownEvent("KeyB", 1));
      capture.flushNow();
      expect(posted).toHaveLength(2);
      expect(posted[1]).not.toBe(recycled0);

      // Flush #3: should now send the recycled buffer from flush #1.
      (capture as any).handleKeyDown(keyDownEvent("KeyC", 2));
      capture.flushNow();
      expect(posted).toHaveLength(3);
      expect(posted[2]).toBe(recycled0);
    });
  });

  it("does not reuse recycled buffers of the wrong size after the internal queue grows", () => {
    withStubbedDocument(() => {
      const canvas = makeCanvasStub();

      const postedByteLengths: number[] = [];
      const workerSideCopies: ArrayBuffer[] = [];
      let capture: InputCapture;

      const ioWorker = {
        postMessage: (msg: any, transfer?: any[]) => {
          postedByteLengths.push(msg.buffer.byteLength);
          expect(msg.recycle).toBe(true);
          expect(transfer).toContain(msg.buffer);
          const workerSide = transferToWorker(msg.buffer);
          workerSideCopies.push(workerSide);
        },
      };

      capture = new InputCapture(canvas, ioWorker, { enableGamepad: false, recycleBuffers: true });
      (capture as any).hasFocus = true;

      // Flush #1: default (smaller) buffer size.
      (capture as any).handleKeyDown(keyDownEvent("KeyA", 0));
      capture.flushNow();
      expect(postedByteLengths).toHaveLength(1);
      expect(workerSideCopies).toHaveLength(1);
      const smallSize = postedByteLengths[0]!;

      // Recycle the small buffer after flush.
      const workerSide0 = workerSideCopies[0]!;
      const recycled0 = transferFromWorker(workerSide0);
      (capture as any).handleWorkerMessage({
        data: { type: "in:input-batch-recycle", buffer: recycled0 },
      } as unknown as MessageEvent<unknown>);

      const buckets = (capture as any).recycledBuffersBySize as Map<number, ArrayBuffer[]>;
      expect(buckets.get(smallSize)).toHaveLength(1);

      // Push enough events to force `InputEventQueue.grow()` (larger buffer size), then flush.
      for (let i = 0; i < 65; i++) {
        (capture as any).handleKeyDown(keyDownEvent("KeyA", 1 + i));
      }
      capture.flushNow();
      expect(postedByteLengths).toHaveLength(2);
      const largeSize = postedByteLengths[1]!;
      expect(largeSize).toBeGreaterThan(smallSize);

      // Growing / allocating a larger buffer must not consume the smaller recycled buffer.
      expect(buckets.get(smallSize)).toHaveLength(1);
      expect(buckets.get(smallSize)?.[0]).toBe(recycled0);

      // The next flush should use the larger backing buffer size, not the smaller recycled one.
      (capture as any).handleKeyDown(keyDownEvent("KeyB", 999));
      capture.flushNow();
      expect(postedByteLengths).toHaveLength(3);
      expect(postedByteLengths[2]).toBe(largeSize);
    });
  });

  it("reuses ArrayBuffers returned by the worker when recycleBuffers=true", () => {
    withStubbedDocument(() => {
      const canvas = makeCanvasStub();

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
            const workerSide = transferToWorker(msg.buffer);
            const recycled = transferFromWorker(workerSide);
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
      const recycled = recycledFromFirstFlush!;
      // When the worker returns a buffer, the next flush should reuse it (no fresh allocation).
      expect(posted[1]).toBe(recycled);
      expect(postedByteLengths[1]).toBe(postedByteLengths[0]);
      expect(postedByteLengths[1]).toBeGreaterThan(0);
      // Returned buffers should be a separate instance than what we originally posted (detached).
      expect(posted[0]).not.toBe(recycled);
    });
  });

  it("reuses recycled buffers even after the internal queue grows (larger byteLength buckets)", () => {
    withStubbedDocument(() => {
      const canvas = makeCanvasStub();

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
            const workerSide = transferToWorker(msg.buffer);
            const recycled = transferFromWorker(workerSide);
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
      const recycled = recycledFromFirstFlush!;

      // Sanity: the grown queue should have a larger backing buffer than the default 128-event queue.
      // Default: (2 + 128*4) * 4 = 2056 bytes; grown: (2 + 256*4) * 4 = 4104 bytes.
      expect(postedByteLengths[0]).toBeGreaterThan(2056);

      // The second flush should still be able to reuse the recycled buffer of that larger size.
      expect(posted[1]).toBe(recycled);
      expect(postedByteLengths[1]).toBe(postedByteLengths[0]);
      expect(posted[0]).not.toBe(recycled);
    });
  });

  it("does not cache buffers when recycleBuffers=false", () => {
    withStubbedDocument(() => {
      const canvas = makeCanvasStub();

      const posted: ArrayBuffer[] = [];
      let capture: InputCapture;

      const ioWorker = {
        postMessage: (msg: any, transfer?: any[]) => {
          posted.push(msg.buffer);
          expect(msg.recycle).not.toBe(true);
          expect(transfer).toContain(msg.buffer);
          // Detach the sender-side buffer to match real transfer semantics.
          const workerSide = transferToWorker(msg.buffer);
          // Even if the worker tries to recycle, InputCapture should ignore it when disabled.
          (capture as any).handleWorkerMessage({
            data: { type: "in:input-batch-recycle", buffer: transferFromWorker(workerSide) },
          } as unknown as MessageEvent<unknown>);
        },
      };

      capture = new InputCapture(canvas, ioWorker, { enableGamepad: false, recycleBuffers: false });
      (capture as any).hasFocus = true;

      (capture as any).handleKeyDown(keyDownEvent("KeyA", 0));
      capture.flushNow();
      expect(((capture as any).recycledBuffersBySize as Map<number, ArrayBuffer[]>).size).toBe(0);

      (capture as any).handleKeyDown(keyDownEvent("KeyB", 1));
      capture.flushNow();

      expect(posted).toHaveLength(2);
      expect(posted[1]).not.toBe(posted[0]);
      expect(((capture as any).recycledBuffersBySize as Map<number, ArrayBuffer[]>).size).toBe(0);
    });
  });
  it("caps distinct recycled buffer sizes so buckets do not grow unbounded", () => {
    withStubbedDocument(() => {
      const canvas = makeCanvasStub();
      const ioWorker = { postMessage: () => {} };
      const capture = new InputCapture(canvas, ioWorker, { enableGamepad: false, recycleBuffers: true });

      const handleWorkerMessage = (capture as any).handleWorkerMessage as (ev: { data: unknown }) => void;
      for (let i = 0; i < 100; i++) {
        handleWorkerMessage({
          data: { type: "in:input-batch-recycle", buffer: new ArrayBuffer(1024 + i) },
        });
      }

      const buckets = (capture as any).recycledBuffersBySize as Map<number, ArrayBuffer[]>;
      // The exact constant is internal, but we want to ensure the map is bounded (not one bucket per
      // observed byteLength forever).
      expect(buckets.size).toBe(8);
      // The newest buckets should remain present after eviction.
      expect(buckets.has(1024 + 99)).toBe(true);
      // The oldest buckets should have been evicted.
      expect(buckets.has(1024)).toBe(false);
      expect(buckets.has(1024 + 91)).toBe(false);
    });
  });

  it("caps the number of buffers stored per bucket", () => {
    withStubbedDocument(() => {
      const canvas = makeCanvasStub();
      const ioWorker = { postMessage: () => {} };
      const capture = new InputCapture(canvas, ioWorker, { enableGamepad: false, recycleBuffers: true });

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
      const canvas = makeCanvasStub();
      const ioWorker = { postMessage: () => {} };
      const capture = new InputCapture(canvas, ioWorker, { enableGamepad: false, recycleBuffers: true });

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
