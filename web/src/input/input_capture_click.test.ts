import { describe, expect, it, vi } from "vitest";

import { InputCapture } from "./input_capture";
import { makeCanvasStub, withStubbedDocument } from "./test_utils";

describe("InputCapture click handling", () => {
  it("swallows click events on the canvas so app-level listeners do not observe them", () => {
    withStubbedDocument(() => {
      const focus = vi.fn();
      const canvas = makeCanvasStub({ focus });
      const ioWorker = { postMessage: () => {} };
      const capture = new InputCapture(canvas, ioWorker, { enableGamepad: false });

      const preventDefault = vi.fn();
      const stopPropagation = vi.fn();
      (capture as any).handleClick({ preventDefault, stopPropagation } as unknown as MouseEvent);
      expect(preventDefault).toHaveBeenCalledTimes(1);
      expect(stopPropagation).toHaveBeenCalledTimes(1);
      expect(focus).toHaveBeenCalledTimes(1);
    });
  });
});
