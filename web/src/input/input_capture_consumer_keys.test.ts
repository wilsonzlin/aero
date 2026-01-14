import { describe, expect, it, vi } from "vitest";

import { InputEventType } from "./event_queue";
import { InputCapture } from "./input_capture";
import { withStubbedDocument } from "./test_utils";

describe("InputCapture consumer/media keys", () => {
  it("emits HidUsage16 Consumer Control events for AudioVolumeUp", () => {
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

      (capture as any).handleKeyDown({
        code: "AudioVolumeUp",
        repeat: false,
        timeStamp: 0,
        altKey: false,
        ctrlKey: false,
        shiftKey: false,
        metaKey: false,
        preventDefault: vi.fn(),
        stopPropagation: vi.fn(),
      } as unknown as KeyboardEvent);

      (capture as any).handleKeyUp({
        code: "AudioVolumeUp",
        repeat: false,
        timeStamp: 1,
        altKey: false,
        ctrlKey: false,
        shiftKey: false,
        metaKey: false,
        preventDefault: vi.fn(),
        stopPropagation: vi.fn(),
      } as unknown as KeyboardEvent);

      capture.flushNow();

      expect(posted).toHaveLength(1);
      const msg = posted[0] as { buffer: ArrayBuffer };
      const words = new Int32Array(msg.buffer);
      expect(words[0] >>> 0).toBe(2);

      const base = 2;
      expect(words[base + 0] >>> 0).toBe(InputEventType.HidUsage16);
      expect(words[base + 2] >>> 0).toBe(0x0000_000c | (1 << 16));
      expect(words[base + 3] >>> 0).toBe(0x00e9);

      expect(words[base + 4] >>> 0).toBe(InputEventType.HidUsage16);
      expect(words[base + 6] >>> 0).toBe(0x0000_000c);
      expect(words[base + 7] >>> 0).toBe(0x00e9);
    });
  });
});
