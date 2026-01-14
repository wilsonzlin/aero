import { describe, expect, it, vi } from "vitest";

import { InputEventType } from "./event_queue";
import { InputCapture } from "./input_capture";
import { decodeInputBatchEvents, makeCanvasStub, withStubbedDocument } from "./test_utils";

type InputCaptureMouseupOutsideCanvasHarness = {
  hasFocus: boolean;
  pointerLock: { isLocked: boolean };
  handleMouseDown: (ev: MouseEvent) => void;
  handleMouseUp: (ev: MouseEvent) => void;
  mouseButtons: number;
};

describe("InputCapture mouseup outside canvas handling", () => {
  it("releases tracked mouse buttons even if mouseup target is not the canvas (prevents stuck buttons)", () => {
    withStubbedDocument(() => {
      const canvas = makeCanvasStub();

      const posted: any[] = [];
      const ioWorker = {
        postMessage: (msg: unknown) => posted.push(msg),
      };

      const capture = new InputCapture(canvas, ioWorker, { enableGamepad: false, recycleBuffers: false });
      const h = capture as unknown as InputCaptureMouseupOutsideCanvasHarness;

      // Simulate the canvas being focused (capture active) and pointer lock not active.
      h.hasFocus = true;
      expect(h.pointerLock.isLocked).toBe(false);

      // Press left button on the canvas.
      h.handleMouseDown({
        button: 0,
        target: canvas,
        preventDefault: vi.fn(),
        stopPropagation: vi.fn(),
        timeStamp: 0,
      } as unknown as MouseEvent);

      expect(h.mouseButtons).toBe(1);

      // Release left button outside the canvas (common drag-out scenario).
      h.handleMouseUp({
        button: 0,
        // Not the canvas.
        target: {},
        preventDefault: vi.fn(),
        stopPropagation: vi.fn(),
        timeStamp: 1,
      } as unknown as MouseEvent);

      expect(h.mouseButtons).toBe(0);

      capture.flushNow();

      expect(posted).toHaveLength(1);
      const msg = posted[0] as { buffer: ArrayBuffer };
      const events = decodeInputBatchEvents(msg.buffer);

      // Expect both the press and release MouseButtons snapshots to have been flushed.
      expect(events).toHaveLength(2);
      expect(events[0]!.type).toBe(InputEventType.MouseButtons);
      expect(events[0]!.a).toBe(1);
      expect(events[1]!.type).toBe(InputEventType.MouseButtons);
      expect(events[1]!.a).toBe(0);
    });
  });

  it("ignores mouseup events outside the canvas when that button is not tracked as pressed", () => {
    withStubbedDocument(() => {
      const canvas = makeCanvasStub();

      const posted: any[] = [];
      const ioWorker = {
        postMessage: (msg: unknown) => posted.push(msg),
      };

      const capture = new InputCapture(canvas, ioWorker, { enableGamepad: false, recycleBuffers: false });
      const h = capture as unknown as InputCaptureMouseupOutsideCanvasHarness;
      h.hasFocus = true;

      const preventDefault = vi.fn();
      const stopPropagation = vi.fn();
      h.handleMouseUp({
        button: 0,
        target: {},
        preventDefault,
        stopPropagation,
        timeStamp: 0,
      } as unknown as MouseEvent);

      // Not captured: should not interfere with unrelated page UI.
      expect(preventDefault).not.toHaveBeenCalled();
      expect(stopPropagation).not.toHaveBeenCalled();

      capture.flushNow();
      expect(posted).toHaveLength(0);
    });
  });
});
