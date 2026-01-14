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

describe("InputCapture buffer recycling", () => {
  it("reuses ArrayBuffer instances when the worker recycles input batches", () => {
    withStubbedDocument(() => {
      const canvas = {
        tabIndex: 0,
        addEventListener: () => {},
        removeEventListener: () => {},
        focus: () => {},
      } as unknown as HTMLCanvasElement;

      const posted: Array<{ msg: any; transfer: Transferable[] }> = [];
      const ioWorker = {
        postMessage: (msg: unknown, transfer: Transferable[]) => posted.push({ msg, transfer }),
      };

      const capture = new InputCapture(canvas, ioWorker, { enableGamepad: false, recycleBuffers: true });

      // Ensure capture is in an active state (required for gamepad polling, though disabled here).
      (capture as any).hasFocus = true;

      // Flush #1: posts buffer A. The queue allocates the next buffer *before* we process a recycle
      // message, so the next flush cannot immediately reuse A.
      (capture as any).queue.pushMouseButtons(1, 1);
      capture.flushNow();
      expect(posted).toHaveLength(1);
      const msg1 = posted[0].msg as { buffer: ArrayBuffer; recycle?: true };
      const bufA = msg1.buffer;
      expect(msg1.recycle).toBe(true);

      // Simulate the worker transferring buffer A back for reuse.
      (capture as any).handleWorkerMessage({ data: { type: "in:input-batch-recycle", buffer: bufA } });

      // Flush #2: should use buffer B (allocated during flush #1), not A.
      (capture as any).queue.pushMouseButtons(2, 2);
      capture.flushNow();
      expect(posted).toHaveLength(2);
      const msg2 = posted[1].msg as { buffer: ArrayBuffer; recycle?: true };
      const bufB = msg2.buffer;
      expect(msg2.recycle).toBe(true);
      expect(bufB).not.toBe(bufA);

      // Flush #3: after flush #2, the queue allocates a new buffer and should reuse A from the
      // recycle bucket.
      (capture as any).queue.pushMouseButtons(3, 3);
      capture.flushNow();
      expect(posted).toHaveLength(3);
      const msg3 = posted[2].msg as { buffer: ArrayBuffer; recycle?: true };
      expect(msg3.recycle).toBe(true);
      expect(msg3.buffer).toBe(bufA);
    });
  });

  it("does not request recycling or reuse buffers when recycleBuffers is disabled", () => {
    withStubbedDocument(() => {
      const canvas = {
        tabIndex: 0,
        addEventListener: () => {},
        removeEventListener: () => {},
        focus: () => {},
      } as unknown as HTMLCanvasElement;

      const posted: Array<{ msg: any; transfer: Transferable[] }> = [];
      const ioWorker = {
        postMessage: (msg: unknown, transfer: Transferable[]) => posted.push({ msg, transfer }),
      };

      const capture = new InputCapture(canvas, ioWorker, { enableGamepad: false, recycleBuffers: false });
      (capture as any).hasFocus = true;

      // Flush #1: should not set `recycle: true`.
      (capture as any).queue.pushMouseButtons(1, 1);
      capture.flushNow();
      expect(posted).toHaveLength(1);
      const msg1 = posted[0].msg as { buffer: ArrayBuffer; recycle?: true };
      expect(msg1.recycle).toBeUndefined();
      const bufA = msg1.buffer;

      // Recycle messages are ignored when recycling is disabled.
      (capture as any).handleWorkerMessage({ data: { type: "in:input-batch-recycle", buffer: bufA } });
      expect(((capture as any).recycledBuffersBySize as Map<number, ArrayBuffer[]>).size).toBe(0);

      // Flush #2: must not reuse A.
      (capture as any).queue.pushMouseButtons(2, 2);
      capture.flushNow();
      expect(posted).toHaveLength(2);
      const msg2 = posted[1].msg as { buffer: ArrayBuffer; recycle?: true };
      expect(msg2.recycle).toBeUndefined();
      expect(msg2.buffer).not.toBe(bufA);

      // Flush #3: also must not reuse A even though we sent a recycle message.
      (capture as any).queue.pushMouseButtons(3, 3);
      capture.flushNow();
      expect(posted).toHaveLength(3);
      const msg3 = posted[2].msg as { buffer: ArrayBuffer; recycle?: true };
      expect(msg3.recycle).toBeUndefined();
      expect(msg3.buffer).not.toBe(bufA);
    });
  });
});

