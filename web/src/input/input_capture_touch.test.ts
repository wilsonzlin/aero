import { describe, expect, it, vi } from "vitest";

import { InputEventType } from "./event_queue";
import { InputCapture } from "./input_capture";
import { decodeInputBatchEvents, makeCanvasStub, withStubbedDocument } from "./test_utils";

type InputCaptureTouchHarness = {
  handlePointerDown: (ev: Partial<PointerEvent>) => void;
  handlePointerMove: (ev: Partial<PointerEvent>) => void;
  handlePointerUp: (ev: Partial<PointerEvent>) => void;
  mouseButtons: number;
};

describe("InputCapture touch (PointerEvent) fallback", () => {
  it("translates touch drag into relative MouseMove with correct sign conventions", () => {
    withStubbedDocument(() => {
      const canvas = makeCanvasStub();

      const posted: any[] = [];
      const ioWorker = {
        postMessage: (msg: unknown) => posted.push(msg),
      };

      const capture = new InputCapture(canvas, ioWorker, {
        enableGamepad: false,
        recycleBuffers: false,
        enableTouchFallback: true,
        // Keep this test focused on movement (no button emulation).
        touchTapToClick: false,
      });

      const h = capture as unknown as InputCaptureTouchHarness;

      h.handlePointerDown({
        pointerId: 1,
        pointerType: "touch",
        clientX: 100,
        clientY: 100,
        timeStamp: 0,
        preventDefault: vi.fn(),
        stopPropagation: vi.fn(),
      } satisfies Partial<PointerEvent>);

      h.handlePointerMove({
        pointerId: 1,
        pointerType: "touch",
        clientX: 110,
        clientY: 120,
        timeStamp: 1,
        preventDefault: vi.fn(),
        stopPropagation: vi.fn(),
      } satisfies Partial<PointerEvent>);

      // End the gesture to avoid a later tap being detected; movement is large enough to not count as a tap.
      h.handlePointerUp({
        pointerId: 1,
        pointerType: "touch",
        clientX: 110,
        clientY: 120,
        timeStamp: 2,
        preventDefault: vi.fn(),
        stopPropagation: vi.fn(),
      } satisfies Partial<PointerEvent>);

      capture.flushNow();

      expect(posted).toHaveLength(1);
      const msg = posted[0] as { buffer: ArrayBuffer };

      const events = decodeInputBatchEvents(msg.buffer);
      expect(events).toHaveLength(1);
      expect(events[0]!.type).toBe(InputEventType.MouseMove);
      expect(events[0]!.a).toBe(10);
      // DOM Y increases downward; PS/2 positive Y is up.
      expect(events[0]!.b).toBe(-20);
    });
  });

  it("translates a tap into a left click (MouseButtons down+up)", () => {
    withStubbedDocument(() => {
      const canvas = makeCanvasStub();

      const posted: any[] = [];
      const ioWorker = {
        postMessage: (msg: unknown) => posted.push(msg),
      };

      const capture = new InputCapture(canvas, ioWorker, {
        enableGamepad: false,
        recycleBuffers: false,
        enableTouchFallback: true,
        touchTapToClick: true,
      });

      const h = capture as unknown as InputCaptureTouchHarness;

      h.handlePointerDown({
        pointerId: 1,
        pointerType: "touch",
        clientX: 50,
        clientY: 60,
        timeStamp: 100,
        preventDefault: vi.fn(),
        stopPropagation: vi.fn(),
      } satisfies Partial<PointerEvent>);

      h.handlePointerUp({
        pointerId: 1,
        pointerType: "touch",
        clientX: 50,
        clientY: 60,
        timeStamp: 150,
        preventDefault: vi.fn(),
        stopPropagation: vi.fn(),
      } satisfies Partial<PointerEvent>);

      capture.flushNow();

      expect(posted).toHaveLength(1);
      const msg = posted[0] as { buffer: ArrayBuffer };
      const events = decodeInputBatchEvents(msg.buffer);
      expect(events).toHaveLength(2);

      // Event 0: left down.
      expect(events[0]!.type).toBe(InputEventType.MouseButtons);
      expect(events[0]!.a).toBe(1);

      // Event 1: left up.
      expect(events[1]!.type).toBe(InputEventType.MouseButtons);
      expect(events[1]!.a).toBe(0);
    });
  });

  it("does not emulate a tap when pointer coordinates are non-finite", () => {
    withStubbedDocument(() => {
      const canvas = makeCanvasStub();

      const posted: any[] = [];
      const ioWorker = {
        postMessage: (msg: unknown) => posted.push(msg),
      };

      const capture = new InputCapture(canvas, ioWorker, {
        enableGamepad: false,
        recycleBuffers: false,
        enableTouchFallback: true,
        touchTapToClick: true,
      });

      const h = capture as unknown as InputCaptureTouchHarness;

      h.handlePointerDown({
        pointerId: 1,
        pointerType: "touch",
        clientX: Number.NaN,
        clientY: 0,
        timeStamp: 0,
        preventDefault: vi.fn(),
        stopPropagation: vi.fn(),
      } satisfies Partial<PointerEvent>);

      h.handlePointerUp({
        pointerId: 1,
        pointerType: "touch",
        clientX: Number.NaN,
        clientY: 0,
        timeStamp: 1,
        preventDefault: vi.fn(),
        stopPropagation: vi.fn(),
      } satisfies Partial<PointerEvent>);

      capture.flushNow();

      expect(posted).toHaveLength(0);
      expect(h.mouseButtons).toBe(0);
    });
  });

  it("does not track touch pointers with non-finite pointerId", () => {
    withStubbedDocument(() => {
      const canvas = makeCanvasStub();

      const posted: any[] = [];
      const ioWorker = {
        postMessage: (msg: unknown) => posted.push(msg),
      };

      const capture = new InputCapture(canvas, ioWorker, {
        enableGamepad: false,
        recycleBuffers: false,
        enableTouchFallback: true,
        touchTapToClick: true,
      });

      const h = capture as unknown as InputCaptureTouchHarness;

      h.handlePointerDown({
        pointerId: Number.NaN,
        pointerType: "touch",
        clientX: 0,
        clientY: 0,
        timeStamp: 0,
        preventDefault: vi.fn(),
        stopPropagation: vi.fn(),
      } satisfies Partial<PointerEvent>);

      h.handlePointerUp({
        pointerId: Number.NaN,
        pointerType: "touch",
        clientX: 0,
        clientY: 0,
        timeStamp: 1,
        preventDefault: vi.fn(),
        stopPropagation: vi.fn(),
      } satisfies Partial<PointerEvent>);

      capture.flushNow();

      expect(posted).toHaveLength(0);
      expect(h.mouseButtons).toBe(0);
    });
  });
});
