import { describe, expect, it, vi } from "vitest";

import { InputEventType } from "./event_queue";
import { InputCapture } from "./input_capture";
import { withStubbedDocument } from "./test_utils";

describe("InputCapture mouseup outside canvas handling", () => {
  it("releases tracked mouse buttons even if mouseup target is not the canvas (prevents stuck buttons)", () => {
    withStubbedDocument(() => {
      const canvas = {
        tabIndex: 0,
        addEventListener: () => {},
        removeEventListener: () => {},
        focus: () => {},
      } as unknown as HTMLCanvasElement;

      const posted: any[] = [];
      const ioWorker = {
        postMessage: (msg: unknown) => posted.push(msg),
      };

      const capture = new InputCapture(canvas, ioWorker, { enableGamepad: false, recycleBuffers: false });

      // Simulate the canvas being focused (capture active) and pointer lock not active.
      (capture as any).hasFocus = true;
      expect((capture as any).pointerLock.isLocked).toBe(false);

      // Press left button on the canvas.
      (capture as any).handleMouseDown({
        button: 0,
        target: canvas,
        preventDefault: vi.fn(),
        stopPropagation: vi.fn(),
        timeStamp: 0,
      } as unknown as MouseEvent);

      expect((capture as any).mouseButtons).toBe(1);

      // Release left button outside the canvas (common drag-out scenario).
      (capture as any).handleMouseUp({
        button: 0,
        // Not the canvas.
        target: {},
        preventDefault: vi.fn(),
        stopPropagation: vi.fn(),
        timeStamp: 1,
      } as unknown as MouseEvent);

      expect((capture as any).mouseButtons).toBe(0);

      capture.flushNow();

      expect(posted).toHaveLength(1);
      const msg = posted[0] as { buffer: ArrayBuffer };
      const words = new Int32Array(msg.buffer);

      // Expect both the press and release MouseButtons snapshots to have been flushed.
      expect(words[0]).toBe(2); // count
      expect(words[2]).toBe(InputEventType.MouseButtons);
      expect(words[4]).toBe(1);
      expect(words[6]).toBe(InputEventType.MouseButtons);
      expect(words[8]).toBe(0);
    });
  });

  it("ignores mouseup events outside the canvas when that button is not tracked as pressed", () => {
    withStubbedDocument(() => {
      const canvas = {
        tabIndex: 0,
        addEventListener: () => {},
        removeEventListener: () => {},
        focus: () => {},
      } as unknown as HTMLCanvasElement;

      const posted: any[] = [];
      const ioWorker = {
        postMessage: (msg: unknown) => posted.push(msg),
      };

      const capture = new InputCapture(canvas, ioWorker, { enableGamepad: false, recycleBuffers: false });
      (capture as any).hasFocus = true;

      const preventDefault = vi.fn();
      const stopPropagation = vi.fn();
      (capture as any).handleMouseUp({
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
