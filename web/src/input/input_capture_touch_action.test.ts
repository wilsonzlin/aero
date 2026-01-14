import { describe, expect, it } from "vitest";

import { InputCapture } from "./input_capture";
import type { InputBatchTarget } from "./event_queue";
import { makeCanvasStub, withStubbedDom } from "./test_utils";

describe("InputCapture touch-action policy", () => {
  it("sets canvas.style.touchAction='none' while touch fallback capture is active and restores on stop()", () => {
    withStubbedDom(() => {
      const canvas = makeCanvasStub({ style: { touchAction: "pan-x" } });

      const ioWorker: InputBatchTarget = { postMessage: () => {} };
      const capture = new InputCapture(canvas, ioWorker, {
        enableGamepad: false,
        enableTouchFallback: true,
        recycleBuffers: false,
      });

      capture.start();
      expect(canvas.style.touchAction).toBe("none");

      capture.stop();
      expect(canvas.style.touchAction).toBe("pan-x");
    });
  });
});
