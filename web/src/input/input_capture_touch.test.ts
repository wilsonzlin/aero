import { describe, expect, it, vi } from "vitest";

import { InputEventType } from "./event_queue";
import { InputCapture } from "./input_capture";
import { decodeInputBatchEvents, withStubbedDocument } from "./test_utils";

describe("InputCapture touch (PointerEvent) fallback", () => {
  it("translates touch drag into relative MouseMove with correct sign conventions", () => {
    withStubbedDocument(() => {
      const canvas = {
        tabIndex: 0,
        addEventListener: () => {},
        removeEventListener: () => {},
        focus: () => {},
      } as unknown as HTMLCanvasElement;

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

      (capture as any).handlePointerDown({
        pointerId: 1,
        pointerType: "touch",
        clientX: 100,
        clientY: 100,
        timeStamp: 0,
        preventDefault: vi.fn(),
        stopPropagation: vi.fn(),
      } satisfies Partial<PointerEvent>);

      (capture as any).handlePointerMove({
        pointerId: 1,
        pointerType: "touch",
        clientX: 110,
        clientY: 120,
        timeStamp: 1,
        preventDefault: vi.fn(),
        stopPropagation: vi.fn(),
      } satisfies Partial<PointerEvent>);

      // End the gesture to avoid a later tap being detected; movement is large enough to not count as a tap.
      (capture as any).handlePointerUp({
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
      const canvas = {
        tabIndex: 0,
        addEventListener: () => {},
        removeEventListener: () => {},
        focus: () => {},
      } as unknown as HTMLCanvasElement;

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

      (capture as any).handlePointerDown({
        pointerId: 1,
        pointerType: "touch",
        clientX: 50,
        clientY: 60,
        timeStamp: 100,
        preventDefault: vi.fn(),
        stopPropagation: vi.fn(),
      } satisfies Partial<PointerEvent>);

      (capture as any).handlePointerUp({
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
});
