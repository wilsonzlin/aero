import { describe, expect, it, vi } from "vitest";

import { InputEventType } from "./event_queue";
import { InputCapture } from "./input_capture";

function withStubbedDocument<T>(run: (doc: any) => T): T {
  const original = (globalThis as any).document;
  const doc = {
    pointerLockElement: null,
    visibilityState: "visible",
    hasFocus: () => true,
    addEventListener: () => {},
    removeEventListener: () => {},
    exitPointerLock: () => {},
  };
  (globalThis as any).document = doc;
  try {
    return run(doc);
  } finally {
    (globalThis as any).document = original;
  }
}

function decodePackedBytes(packed: number, len: number): number[] {
  const out: number[] = [];
  const p = packed >>> 0;
  for (let i = 0; i < len; i++) {
    out.push((p >>> (i * 8)) & 0xff);
  }
  return out;
}

describe("InputCapture.releaseAllKeys", () => {
  it("emits the full PrintScreen break scancode sequence on releaseAllKeys", () => {
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
      const words = new Int32Array(msg.buffer);
      expect(words[0] >>> 0).toBe(5);

      const base = 2;
      const types = [0, 1, 2, 3, 4].map((i) => words[base + i * 4] >>> 0);
      expect(types).toEqual([
        InputEventType.KeyHidUsage,
        InputEventType.KeyScancode,
        InputEventType.KeyScancode,
        InputEventType.KeyScancode,
        InputEventType.KeyHidUsage,
      ]);

      // Keydown scancode make: E0 12 E0 7C.
      const makePacked = words[base + 1 * 4 + 2] >>> 0;
      const makeLen = words[base + 1 * 4 + 3] >>> 0;
      expect(decodePackedBytes(makePacked, makeLen)).toEqual([0xe0, 0x12, 0xe0, 0x7c]);

      // releaseAllKeys scancode break: E0 F0 7C E0 F0 12 (split across 4+2 bytes).
      const brk1Packed = words[base + 2 * 4 + 2] >>> 0;
      const brk1Len = words[base + 2 * 4 + 3] >>> 0;
      const brk2Packed = words[base + 3 * 4 + 2] >>> 0;
      const brk2Len = words[base + 3 * 4 + 3] >>> 0;
      expect(decodePackedBytes(brk1Packed, brk1Len)).toEqual([0xe0, 0xf0, 0x7c, 0xe0]);
      expect(decodePackedBytes(brk2Packed, brk2Len)).toEqual([0xf0, 0x12]);

      // HID usage: PrintScreen is 0x46.
      const hidDownPacked = words[base + 0 * 4 + 2] >>> 0;
      expect(hidDownPacked).toBe(0x46 | (1 << 8));
      const hidUpPacked = words[base + 4 * 4 + 2] >>> 0;
      expect(hidUpPacked).toBe(0x46);
    });
  });

  it("does not emit a PS/2 break scancode sequence for Pause (make-only) on releaseAllKeys", () => {
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
      const words = new Int32Array(msg.buffer);
      expect(words[0] >>> 0).toBe(4);

      const base = 2;
      const types = [0, 1, 2, 3].map((i) => words[base + i * 4] >>> 0);
      expect(types).toEqual([
        InputEventType.KeyHidUsage,
        InputEventType.KeyScancode,
        InputEventType.KeyScancode,
        InputEventType.KeyHidUsage,
      ]);

      // Keydown scancode make: E1 14 77 E1 F0 14 F0 77 (split across 4+4 bytes).
      const make1Packed = words[base + 1 * 4 + 2] >>> 0;
      const make1Len = words[base + 1 * 4 + 3] >>> 0;
      const make2Packed = words[base + 2 * 4 + 2] >>> 0;
      const make2Len = words[base + 2 * 4 + 3] >>> 0;
      expect(decodePackedBytes(make1Packed, make1Len)).toEqual([0xe1, 0x14, 0x77, 0xe1]);
      expect(decodePackedBytes(make2Packed, make2Len)).toEqual([0xf0, 0x14, 0xf0, 0x77]);

      // HID usage: Pause is 0x48.
      const hidDownPacked = words[base + 0 * 4 + 2] >>> 0;
      expect(hidDownPacked).toBe(0x48 | (1 << 8));
      const hidUpPacked = words[base + 3 * 4 + 2] >>> 0;
      expect(hidUpPacked).toBe(0x48);
    });
  });
});

