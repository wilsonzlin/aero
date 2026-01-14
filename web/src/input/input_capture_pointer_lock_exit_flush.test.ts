import { describe, expect, it, vi } from "vitest";

import { InputEventType } from "./event_queue";
import { InputCapture } from "./input_capture";
import { decodeInputBatchEvents, makeCanvasStub, withStubbedDocument } from "./test_utils";

type InputCapturePointerLockExitHarness = {
  pointerLock: { locked: boolean };
  hasFocus: boolean;
  handleKeyDown: (ev: KeyboardEvent) => void;
  handleMouseDown: (ev: MouseEvent) => void;
  pressedCodes: Set<string>;
  mouseButtons: number;
  handlePointerLockChange: (locked: boolean) => void;
  queue: { size: number };
};

describe("InputCapture pointer-lock exit safety flush", () => {
  it("flushes an immediate all-released snapshot when pointer lock exits while the canvas is not focused", () => {
    withStubbedDocument((doc) => {
      const canvas = makeCanvasStub();

      const posted: any[] = [];
      const ioWorker = {
        postMessage: (msg: unknown) => posted.push(msg),
      };

      const capture = new InputCapture(canvas, ioWorker, { enableGamepad: false, recycleBuffers: false });
      const h = capture as unknown as InputCapturePointerLockExitHarness;

      // Force pointer lock active and canvas focus already lost.
      doc.pointerLockElement = canvas;
      h.pointerLock.locked = true;
      h.hasFocus = false;

      // Hold a key + mouse button while pointer locked.
      h.handleKeyDown({
        code: "KeyA",
        repeat: false,
        timeStamp: 0,
        altKey: false,
        ctrlKey: false,
        shiftKey: false,
        metaKey: false,
        preventDefault: vi.fn(),
        stopPropagation: vi.fn(),
      } as unknown as KeyboardEvent);

      h.handleMouseDown({
        button: 0,
        target: canvas,
        timeStamp: 1,
        preventDefault: vi.fn(),
        stopPropagation: vi.fn(),
      } as unknown as MouseEvent);

      expect(h.pressedCodes.has("KeyA")).toBe(true);
      expect(h.mouseButtons).toBe(1);

      // Pointer lock exits and the canvas is still not focused. This must flush an "all released"
      // state immediately so the guest can't be left with latched inputs.
      doc.pointerLockElement = null;
      h.pointerLock.locked = false;
      h.handlePointerLockChange(false);

      expect(posted).toHaveLength(1);

      const msg = posted[0] as { buffer: ArrayBuffer };
      const events = decodeInputBatchEvents(msg.buffer);

      const KEY_A_USAGE = 0x04;
      expect(
        events.some((e) => {
          if (e.type !== InputEventType.KeyHidUsage) return false;
          const packed = e.a >>> 0;
          const usage = packed & 0xff;
          const pressed = (packed >>> 8) & 1;
          return usage === KEY_A_USAGE && pressed === 0;
        })
      ).toBe(true);

      expect(events.some((e) => e.type === InputEventType.MouseButtons && (e.a | 0) === 0)).toBe(true);

      // Pressed-state tracking should be cleared so future capture sessions start cleanly.
      expect(h.pressedCodes.size).toBe(0);
      expect(h.mouseButtons).toBe(0);
      expect(h.queue.size).toBe(0);
    });
  });
});
