import { describe, expect, it, vi } from "vitest";

import { InputEventType } from "./event_queue";
import { InputCapture } from "./input_capture";
import { decodeInputBatchEvents, makeCanvasStub, withStubbedDocument } from "./test_utils";

describe("InputCapture pointer-lock exit safety flush", () => {
  it("flushes an immediate all-released snapshot when pointer lock exits while the canvas is not focused", () => {
    withStubbedDocument((doc) => {
      const canvas = makeCanvasStub();

      const posted: any[] = [];
      const ioWorker = {
        postMessage: (msg: unknown) => posted.push(msg),
      };

      const capture = new InputCapture(canvas, ioWorker, { enableGamepad: false, recycleBuffers: false });

      // Force pointer lock active and canvas focus already lost.
      doc.pointerLockElement = canvas;
      (capture as any).pointerLock.locked = true;
      (capture as any).hasFocus = false;

      // Hold a key + mouse button while pointer locked.
      (capture as any).handleKeyDown({
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

      (capture as any).handleMouseDown({
        button: 0,
        target: canvas,
        timeStamp: 1,
        preventDefault: vi.fn(),
        stopPropagation: vi.fn(),
      } as unknown as MouseEvent);

      expect((capture as any).pressedCodes.has("KeyA")).toBe(true);
      expect((capture as any).mouseButtons).toBe(1);

      // Pointer lock exits and the canvas is still not focused. This must flush an "all released"
      // state immediately so the guest can't be left with latched inputs.
      doc.pointerLockElement = null;
      (capture as any).pointerLock.locked = false;
      (capture as any).handlePointerLockChange(false);

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
      expect((capture as any).pressedCodes.size).toBe(0);
      expect((capture as any).mouseButtons).toBe(0);
      expect((capture as any).queue.size).toBe(0);
    });
  });
});
