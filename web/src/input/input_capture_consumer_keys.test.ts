import { describe, expect, it, vi } from "vitest";

import { InputEventType } from "./event_queue";
import { InputCapture } from "./input_capture";
import { decodeInputBatchEvents, withStubbedDocument } from "./test_utils";

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
      const events = decodeInputBatchEvents(msg.buffer);
      expect(events).toHaveLength(2);

      expect(events[0]!.type).toBe(InputEventType.HidUsage16);
      expect(events[0]!.a).toBe(0x0000_000c | (1 << 16));
      expect(events[0]!.b).toBe(0x00e9);

      expect(events[1]!.type).toBe(InputEventType.HidUsage16);
      expect(events[1]!.a).toBe(0x0000_000c);
      expect(events[1]!.b).toBe(0x00e9);
    });
  });
});
