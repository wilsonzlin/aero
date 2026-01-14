import { describe, expect, it, vi } from "vitest";

import { InputEventType } from "./event_queue";
import { InputCapture } from "./input_capture";
import { withStubbedDocument } from "./test_utils";

describe("InputCapture key repeat behavior", () => {
  it("emits repeated PS/2 make scancodes but does not emit repeated HID usage events", () => {
    withStubbedDocument(() => {
      const canvas = {
        tabIndex: 0,
        addEventListener: () => {},
        removeEventListener: () => {},
        focus: () => {},
      } as unknown as HTMLCanvasElement;
      const posted: any[] = [];
      const ioWorker = { postMessage: (msg: unknown) => posted.push(msg) };
      const capture = new InputCapture(canvas, ioWorker, { enableGamepad: false, recycleBuffers: false });

      (capture as any).hasFocus = true;

      const preventDefault = vi.fn();
      const stopPropagation = vi.fn();

      // Arrow keys are prevented by default (avoid scroll), and they commonly generate key repeat.
      (capture as any).handleKeyDown({
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
      expect((capture as any).queue.size).toBe(2);

      (capture as any).handleKeyDown({
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
      expect((capture as any).queue.size).toBe(3);

      capture.flushNow();
      expect(posted).toHaveLength(1);
      const msg = posted[0] as { buffer: ArrayBuffer };
      const words = new Int32Array(msg.buffer);
      expect(words[0] >>> 0).toBe(3);

      const base = 2;
      expect(words[base + 0]).toBe(InputEventType.KeyHidUsage);
      expect(words[base + 4]).toBe(InputEventType.KeyScancode);
      expect(words[base + 8]).toBe(InputEventType.KeyScancode);
    });
  });
});
