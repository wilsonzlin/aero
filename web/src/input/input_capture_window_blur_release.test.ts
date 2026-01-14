import { describe, expect, it, vi } from "vitest";

import { InputEventType } from "./event_queue";
import { InputCapture } from "./input_capture";
import { decodeInputBatchEvents, decodePackedBytes, makeCanvasStub, withStubbedDocument } from "./test_utils";

type InputCaptureFocusHarness = {
  hasFocus: boolean;
  pressedCodes: Set<string>;
  mouseButtons: number;
  handleKeyDown(event: KeyboardEvent): void;
  handleMouseDown(event: MouseEvent): void;
  handleWindowBlur(): void;
  handleVisibilityChange(): void;
};

function expectAllReleasedBatch(posted: any[]): void {
  expect(posted).toHaveLength(1);
  const msg = posted[0] as { buffer: ArrayBuffer };
  const events = decodeInputBatchEvents(msg.buffer);

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
      const canvas = makeCanvasStub();

      const posted: any[] = [];
      const ioWorker = { postMessage: (msg: unknown) => posted.push(msg) };
      const capture = new InputCapture(canvas, ioWorker, { enableGamepad: false, recycleBuffers: false });

      (capture as unknown as InputCaptureFocusHarness).hasFocus = true;

      // Press and hold KeyA + mouse left button.
      (capture as unknown as InputCaptureFocusHarness).handleKeyDown({
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
      (capture as unknown as InputCaptureFocusHarness).handleMouseDown({
        button: 0,
        timeStamp: 1,
        target: canvas,
        preventDefault: vi.fn(),
        stopPropagation: vi.fn(),
      } as unknown as MouseEvent);

      expect((capture as unknown as InputCaptureFocusHarness).pressedCodes.size).toBe(1);
      expect((capture as unknown as InputCaptureFocusHarness).pressedCodes.has("KeyA")).toBe(true);
      expect((capture as unknown as InputCaptureFocusHarness).mouseButtons).toBe(1);
      expect(posted).toHaveLength(0);

      // Losing window focus should immediately flush a batch containing releases.
      (capture as unknown as InputCaptureFocusHarness).handleWindowBlur();

      expectAllReleasedBatch(posted);
      expect((capture as unknown as InputCaptureFocusHarness).pressedCodes.size).toBe(0);
      expect((capture as unknown as InputCaptureFocusHarness).mouseButtons).toBe(0);
    });
  });

  it("flushes an all-released snapshot when the page becomes hidden", () => {
    withStubbedDocument((doc) => {
      const canvas = makeCanvasStub();

      const posted: any[] = [];
      const ioWorker = { postMessage: (msg: unknown) => posted.push(msg) };
      const capture = new InputCapture(canvas, ioWorker, { enableGamepad: false, recycleBuffers: false });

      (capture as unknown as InputCaptureFocusHarness).hasFocus = true;

      (capture as unknown as InputCaptureFocusHarness).handleKeyDown({
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
      (capture as unknown as InputCaptureFocusHarness).handleMouseDown({
        button: 0,
        timeStamp: 1,
        target: canvas,
        preventDefault: vi.fn(),
        stopPropagation: vi.fn(),
      } as unknown as MouseEvent);

      doc.visibilityState = "hidden";
      expect(posted).toHaveLength(0);

      (capture as unknown as InputCaptureFocusHarness).handleVisibilityChange();

      expectAllReleasedBatch(posted);
      expect((capture as unknown as InputCaptureFocusHarness).pressedCodes.size).toBe(0);
      expect((capture as unknown as InputCaptureFocusHarness).mouseButtons).toBe(0);
    });
  });
});
