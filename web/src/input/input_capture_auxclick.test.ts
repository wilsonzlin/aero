import { describe, expect, it, vi } from "vitest";

import { InputCapture } from "./input_capture";
import { makeCanvasStub, withStubbedDocument } from "./test_utils";

type InputCaptureAuxclickHarness = {
  hasFocus: boolean;
  pointerLock: { locked: boolean };
  handleAuxClick: (ev: MouseEvent) => void;
};

describe("InputCapture auxclick handling", () => {
  it("swallows auxclick events on the canvas while capture is active", () => {
    withStubbedDocument(() => {
      const canvas = makeCanvasStub();
      const ioWorker = { postMessage: () => {} };
      const capture = new InputCapture(canvas, ioWorker, { enableGamepad: false });
      const h = capture as unknown as InputCaptureAuxclickHarness;

      h.hasFocus = true;

      const preventDefault = vi.fn();
      const stopPropagation = vi.fn();
      h.handleAuxClick({
        button: 1,
        target: canvas,
        preventDefault,
        stopPropagation,
      } as unknown as MouseEvent);

      expect(preventDefault).toHaveBeenCalledTimes(1);
      expect(stopPropagation).toHaveBeenCalledTimes(1);
    });
  });

  it("does not swallow auxclick events outside the canvas when pointer lock is inactive", () => {
    withStubbedDocument(() => {
      const canvas = makeCanvasStub();
      const ioWorker = { postMessage: () => {} };
      const capture = new InputCapture(canvas, ioWorker, { enableGamepad: false });
      const h = capture as unknown as InputCaptureAuxclickHarness;

      h.hasFocus = true;

      const preventDefault = vi.fn();
      const stopPropagation = vi.fn();
      h.handleAuxClick({
        button: 1,
        target: {},
        preventDefault,
        stopPropagation,
      } as unknown as MouseEvent);

      expect(preventDefault).toHaveBeenCalledTimes(0);
      expect(stopPropagation).toHaveBeenCalledTimes(0);
    });
  });

  it("swallows auxclick events while pointer lock is active even if the target is outside the canvas", () => {
    withStubbedDocument(() => {
      const canvas = makeCanvasStub();
      const ioWorker = { postMessage: () => {} };
      const capture = new InputCapture(canvas, ioWorker, { enableGamepad: false });
      const h = capture as unknown as InputCaptureAuxclickHarness;

      h.pointerLock.locked = true;

      const preventDefault = vi.fn();
      const stopPropagation = vi.fn();
      h.handleAuxClick({
        button: 1,
        target: {},
        preventDefault,
        stopPropagation,
      } as unknown as MouseEvent);

      expect(preventDefault).toHaveBeenCalledTimes(1);
      expect(stopPropagation).toHaveBeenCalledTimes(1);
    });
  });
});
