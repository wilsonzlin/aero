import { describe, expect, it } from "vitest";

import { InputCapture } from "./input_capture";
import { makeCanvasStub, withStubbedDom } from "./test_utils";

describe("InputCapture touch-action policy", () => {
  it("sets canvas.style.touchAction='none' while touch fallback capture is active and restores on stop()", () => {
    withStubbedDom(() => {
      const canvas = makeCanvasStub({ style: { touchAction: "pan-x" } });

      const ioWorker = { postMessage: () => {} };
      const capture = new InputCapture(canvas, ioWorker as any, {
        enableGamepad: false,
        enableTouchFallback: true,
        recycleBuffers: false,
      });

      capture.start();
      expect((canvas as any).style.touchAction).toBe("none");

      capture.stop();
      expect((canvas as any).style.touchAction).toBe("pan-x");
    });
  });
});
