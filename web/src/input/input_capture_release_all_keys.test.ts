import { describe, expect, it, vi } from "vitest";

import { InputEventType } from "./event_queue";
import { InputCapture } from "./input_capture";
import { decodeInputBatchEvents, decodePackedBytes, makeCanvasStub, withStubbedDocument } from "./test_utils";

describe("InputCapture.releaseAllKeys", () => {
  it("emits the full PrintScreen break scancode sequence on releaseAllKeys", () => {
    withStubbedDocument(() => {
      const canvas = makeCanvasStub();

      const posted: any[] = [];
      const ioWorker = { postMessage: (msg: unknown) => posted.push(msg) };
      const capture = new InputCapture(canvas, ioWorker, { enableGamepad: false, recycleBuffers: false });

      (capture as any).hasFocus = true;

      (capture as any).handleKeyDown({
        code: "PrintScreen",
        repeat: false,
        timeStamp: 0,
        altKey: false,
        ctrlKey: false,
        shiftKey: false,
        metaKey: false,
        preventDefault: vi.fn(),
        stopPropagation: vi.fn(),
      } as unknown as KeyboardEvent);

      expect((capture as any).pressedCodes.has("PrintScreen")).toBe(true);
      expect((capture as any).queue.size).toBe(2); // HID usage + scancode make

      (capture as any).releaseAllKeys();
      expect((capture as any).pressedCodes.size).toBe(0);
      expect((capture as any).queue.size).toBe(5);

      capture.flushNow();
      expect(posted).toHaveLength(1);

      const msg = posted[0] as { buffer: ArrayBuffer };
      const events = decodeInputBatchEvents(msg.buffer);
      expect(events).toHaveLength(5);

      expect(events.map((e) => e.type)).toEqual([
        InputEventType.KeyHidUsage,
        InputEventType.KeyScancode,
        InputEventType.KeyScancode,
        InputEventType.KeyScancode,
        InputEventType.KeyHidUsage,
      ]);

      // Keydown scancode make: E0 12 E0 7C.
      const makePacked = events[1]!.a;
      const makeLen = events[1]!.b;
      expect(decodePackedBytes(makePacked, makeLen)).toEqual([0xe0, 0x12, 0xe0, 0x7c]);

      // releaseAllKeys scancode break: E0 F0 7C E0 F0 12 (split across 4+2 bytes).
      const brk1Packed = events[2]!.a;
      const brk1Len = events[2]!.b;
      const brk2Packed = events[3]!.a;
      const brk2Len = events[3]!.b;
      expect(decodePackedBytes(brk1Packed, brk1Len)).toEqual([0xe0, 0xf0, 0x7c, 0xe0]);
      expect(decodePackedBytes(brk2Packed, brk2Len)).toEqual([0xf0, 0x12]);

      // HID usage: PrintScreen is 0x46.
      expect(events[0]!.a >>> 0).toBe(0x46 | (1 << 8));
      expect(events[4]!.a >>> 0).toBe(0x46);
    });
  });

  it("does not emit a PS/2 break scancode sequence for Pause (make-only) on releaseAllKeys", () => {
    withStubbedDocument(() => {
      const canvas = makeCanvasStub();

      const posted: any[] = [];
      const ioWorker = { postMessage: (msg: unknown) => posted.push(msg) };
      const capture = new InputCapture(canvas, ioWorker, { enableGamepad: false, recycleBuffers: false });

      (capture as any).hasFocus = true;

      (capture as any).handleKeyDown({
        code: "Pause",
        repeat: false,
        timeStamp: 0,
        altKey: false,
        ctrlKey: false,
        shiftKey: false,
        metaKey: false,
        preventDefault: vi.fn(),
        stopPropagation: vi.fn(),
      } as unknown as KeyboardEvent);

      // Pause make is an 8-byte scancode sequence split into two events + HID usage.
      expect((capture as any).queue.size).toBe(3);

      (capture as any).releaseAllKeys();
      // releaseAllKeys should add only a HID usage release (Pause has no PS/2 break sequence).
      expect((capture as any).queue.size).toBe(4);

      capture.flushNow();
      expect(posted).toHaveLength(1);

      const msg = posted[0] as { buffer: ArrayBuffer };
      const events = decodeInputBatchEvents(msg.buffer);
      expect(events).toHaveLength(4);

      expect(events.map((e) => e.type)).toEqual([
        InputEventType.KeyHidUsage,
        InputEventType.KeyScancode,
        InputEventType.KeyScancode,
        InputEventType.KeyHidUsage,
      ]);

      // Keydown scancode make: E1 14 77 E1 F0 14 F0 77 (split across 4+4 bytes).
      const make1Packed = events[1]!.a;
      const make1Len = events[1]!.b;
      const make2Packed = events[2]!.a;
      const make2Len = events[2]!.b;
      expect(decodePackedBytes(make1Packed, make1Len)).toEqual([0xe1, 0x14, 0x77, 0xe1]);
      expect(decodePackedBytes(make2Packed, make2Len)).toEqual([0xf0, 0x14, 0xf0, 0x77]);

      // HID usage: Pause is 0x48.
      expect(events[0]!.a >>> 0).toBe(0x48 | (1 << 8));
      expect(events[3]!.a >>> 0).toBe(0x48);
    });
  });
});
