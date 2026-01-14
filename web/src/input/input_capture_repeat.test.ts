import { describe, expect, it, vi } from "vitest";

import { InputEventType } from "./event_queue";
import { InputCapture } from "./input_capture";
import { decodeInputBatchEvents, makeCanvasStub, withStubbedDocument } from "./test_utils";

describe("InputCapture key repeat behavior", () => {
  it("emits repeated PS/2 make scancodes but does not emit repeated HID usage events", () => {
    withStubbedDocument(() => {
      const canvas = makeCanvasStub();
      const posted: any[] = [];
      const ioWorker = { postMessage: (msg: unknown) => posted.push(msg) };
      const capture = new InputCapture(canvas, ioWorker, { enableGamepad: false, recycleBuffers: false });
      const h = capture as unknown as {
        hasFocus: boolean;
        handleKeyDown: (ev: KeyboardEvent) => void;
        queue: { size: number };
      };

      h.hasFocus = true;

      const preventDefault = vi.fn();
      const stopPropagation = vi.fn();

      // Arrow keys are prevented by default (avoid scroll), and they commonly generate key repeat.
      h.handleKeyDown({
        code: "ArrowUp",
        repeat: false,
        timeStamp: 0,
        altKey: false,
        ctrlKey: false,
        shiftKey: false,
        metaKey: false,
        preventDefault,
        stopPropagation,
      } as unknown as KeyboardEvent);

      expect(preventDefault).toHaveBeenCalledTimes(1);
      expect(stopPropagation).toHaveBeenCalledTimes(1);
      // Initial press emits HID usage + PS/2 scancode.
      expect(h.queue.size).toBe(2);

      h.handleKeyDown({
        code: "ArrowUp",
        repeat: true,
        timeStamp: 1,
        altKey: false,
        ctrlKey: false,
        shiftKey: false,
        metaKey: false,
        preventDefault,
        stopPropagation,
      } as unknown as KeyboardEvent);

      // Repeat press should still be swallowed, but only emit a PS/2 make scancode (typematic).
      expect(preventDefault).toHaveBeenCalledTimes(2);
      expect(stopPropagation).toHaveBeenCalledTimes(2);
      expect(h.queue.size).toBe(3);

      capture.flushNow();
      expect(posted).toHaveLength(1);
      const msg = posted[0] as { buffer: ArrayBuffer };
      const events = decodeInputBatchEvents(msg.buffer);
      expect(events).toHaveLength(3);
      expect(events.map((e) => e.type)).toEqual([
        InputEventType.KeyHidUsage,
        InputEventType.KeyScancode,
        InputEventType.KeyScancode,
      ]);
    });
  });
});
