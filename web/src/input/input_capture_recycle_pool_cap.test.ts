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

describe("InputCapture recycled buffer pool", () => {
  it("caps the number of retained recycled buffers per size bucket", () => {
    withStubbedDocument(() => {
      const canvas = {
        tabIndex: 0,
        addEventListener: () => {},
        removeEventListener: () => {},
        focus: () => {},
      } as unknown as HTMLCanvasElement;
      const ioWorker = { postMessage: () => {} };
      const capture = new InputCapture(canvas, ioWorker, { enableGamepad: false, recycleBuffers: true });

      const byteLength = 1024;
      // Keep in sync with `MAX_RECYCLED_BUFFERS_PER_BUCKET` in `input_capture.ts`.
      const cap = 4;
      const n = cap + 8;

      for (let i = 0; i < n; i++) {
        (capture as any).handleWorkerMessage({
          data: { type: "in:input-batch-recycle", buffer: new ArrayBuffer(byteLength) },
        } as unknown as MessageEvent<unknown>);
      }

      const bucket = (capture as any).recycledBuffersBySize.get(byteLength);
      expect(bucket).toHaveLength(cap);
    });
  });
});
