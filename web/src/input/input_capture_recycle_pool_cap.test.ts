import { describe, expect, it } from "vitest";

import { InputCapture } from "./input_capture";
import { makeCanvasStub, withStubbedDocument } from "./test_utils";

describe("InputCapture recycled buffer pool", () => {
  it("caps the number of retained recycled buffers per size bucket", () => {
    withStubbedDocument(() => {
      const canvas = makeCanvasStub();
      const ioWorker = { postMessage: () => {} };
      const capture = new InputCapture(canvas, ioWorker, { enableGamepad: false, recycleBuffers: true });

      const byteLength = 1024;
      // Keep in sync with `MAX_RECYCLED_BUFFERS_PER_BUCKET` in `input_capture.ts`.
      const cap = 4;
      const n = cap + 8;
      const h = capture as unknown as {
        handleWorkerMessage: (ev: MessageEvent<unknown>) => void;
        recycledBuffersBySize: Map<number, ArrayBuffer[]>;
      };

      for (let i = 0; i < n; i++) {
        h.handleWorkerMessage({
          data: { type: "in:input-batch-recycle", buffer: new ArrayBuffer(byteLength) },
        } as unknown as MessageEvent<unknown>);
      }

      const bucket = h.recycledBuffersBySize.get(byteLength);
      expect(bucket).toHaveLength(cap);
    });
  });
});
