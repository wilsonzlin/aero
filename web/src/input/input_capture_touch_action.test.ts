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

describe("InputCapture touch-action policy", () => {
  it("sets canvas.style.touchAction='none' while touch fallback capture is active and restores on stop()", () => {
    withStubbedDocument(() =>
      withStubbedWindow(() => {
        const canvas = {
          tabIndex: 0,
          addEventListener: () => {},
          removeEventListener: () => {},
          focus: () => {},
          style: { touchAction: "pan-x" },
        } as unknown as HTMLCanvasElement;

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
      }),
    );
  });
});

