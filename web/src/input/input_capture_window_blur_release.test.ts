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

type DecodedEvent = Readonly<{
  type: number;
  timestampUs: number;
  a: number;
  b: number;
}>;

function decodeEvents(buffer: ArrayBuffer): DecodedEvent[] {
  const words = new Int32Array(buffer);
  const count = words[0] >>> 0;
  const base = 2;
  const out: DecodedEvent[] = [];
  for (let i = 0; i < count; i++) {
    const o = base + i * 4;
    out.push({
      type: words[o]! >>> 0,
      timestampUs: words[o + 1]! >>> 0,
      a: words[o + 2]! | 0,
      b: words[o + 3]! | 0,
    });
  }
  return out;
}

function expectAllReleasedBatch(posted: any[]): void {
  expect(posted).toHaveLength(1);
  const msg = posted[0] as { buffer: ArrayBuffer };
  const events = decodeEvents(msg.buffer);

  // KeyA break scancode: F0 1C.
  expect(
    events.some((ev) => {
      if (ev.type !== InputEventType.KeyScancode) return false;
      if (ev.b !== 2) return false;
      return decodePackedBytes(ev.a, ev.b).every((b, i) => b === [0xf0, 0x1c][i]);
    })
  ).toBe(true);

  // KeyA HID usage: 0x04, released (pressed bit unset).
  expect(
    events.some((ev) => ev.type === InputEventType.KeyHidUsage && (ev.a >>> 0) === 0x04)
  ).toBe(true);

  // Mouse buttons released (0).
  expect(
    events.some((ev) => ev.type === InputEventType.MouseButtons && (ev.a | 0) === 0)
  ).toBe(true);
}

describe("InputCapture focus loss", () => {
  it("flushes an all-released snapshot on window blur", () => {
    withStubbedDocument(() => {
      const canvas = {
        tabIndex: 0,
        addEventListener: () => {},
        removeEventListener: () => {},
        focus: () => {},
      } as unknown as HTMLCanvasElement;

      const posted: any[] = [];
      const ioWorker = { postMessage: (msg: unknown) => posted.push(msg) };
      const capture = new InputCapture(canvas, ioWorker as any, { enableGamepad: false, recycleBuffers: false });

      (capture as any).hasFocus = true;

      // Press and hold KeyA + mouse left button.
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
        timeStamp: 1,
        target: canvas,
        preventDefault: vi.fn(),
        stopPropagation: vi.fn(),
      } as unknown as MouseEvent);

      expect((capture as any).pressedCodes.size).toBe(1);
      expect((capture as any).pressedCodes.has("KeyA")).toBe(true);
      expect((capture as any).mouseButtons).toBe(1);
      expect(posted).toHaveLength(0);

      // Losing window focus should immediately flush a batch containing releases.
      (capture as any).handleWindowBlur();

      expectAllReleasedBatch(posted);
      expect((capture as any).pressedCodes.size).toBe(0);
      expect((capture as any).mouseButtons).toBe(0);
    });
  });

  it("flushes an all-released snapshot when the page becomes hidden", () => {
    withStubbedDocument((doc) => {
      const canvas = {
        tabIndex: 0,
        addEventListener: () => {},
        removeEventListener: () => {},
        focus: () => {},
      } as unknown as HTMLCanvasElement;

      const posted: any[] = [];
      const ioWorker = { postMessage: (msg: unknown) => posted.push(msg) };
      const capture = new InputCapture(canvas, ioWorker as any, { enableGamepad: false, recycleBuffers: false });

      (capture as any).hasFocus = true;

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
        timeStamp: 1,
        target: canvas,
        preventDefault: vi.fn(),
        stopPropagation: vi.fn(),
      } as unknown as MouseEvent);

      doc.visibilityState = "hidden";
      expect(posted).toHaveLength(0);

      (capture as any).handleVisibilityChange();

      expectAllReleasedBatch(posted);
      expect((capture as any).pressedCodes.size).toBe(0);
      expect((capture as any).mouseButtons).toBe(0);
    });
  });
});

