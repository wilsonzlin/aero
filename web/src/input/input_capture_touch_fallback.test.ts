import { describe, expect, it, vi } from "vitest";

import { InputEventType } from "./event_queue";
import { InputCapture } from "./input_capture";
import { decodeInputBatchEvents, makeCanvasStub, withStubbedDocument } from "./test_utils";

function touch(identifier: number, clientX: number, clientY: number): Touch {
  return { identifier, clientX, clientY } as unknown as Touch;
}

describe("InputCapture touch fallback", () => {
  it("converts touchmove delta into relative MouseMove events using fractional accumulation", () => {
    withStubbedDocument(() => {
      const canvas = makeCanvasStub({ focus: vi.fn() });

      const posted: any[] = [];
      const ioWorker = {
        postMessage: (msg: unknown) => posted.push(msg),
      };

      const capture = new InputCapture(canvas, ioWorker, {
        enableGamepad: false,
        recycleBuffers: false,
        enableTouchFallback: true,
        // Use a fractional sensitivity to ensure we reuse the existing remainder logic.
        touchSensitivity: 0.5,
        touchTapToClick: false,
      });

      // Touch begins at (0,0).
      (capture as any).handleTouchStart({
        timeStamp: 0,
        touches: [touch(1, 0, 0)],
        changedTouches: [touch(1, 0, 0)],
        preventDefault: vi.fn(),
        stopPropagation: vi.fn(),
      } as unknown as TouchEvent);

      // Move by +1px: with sensitivity 0.5 this should not emit yet.
      (capture as any).handleTouchMove({
        timeStamp: 1,
        touches: [touch(1, 1, 0)],
        changedTouches: [touch(1, 1, 0)],
        preventDefault: vi.fn(),
        stopPropagation: vi.fn(),
      } as unknown as TouchEvent);
      expect((capture as any).queue.size).toBe(0);

      // Move by another +1px: total 1px, should emit one MouseMove.
      (capture as any).handleTouchMove({
        timeStamp: 2,
        touches: [touch(1, 2, 0)],
        changedTouches: [touch(1, 2, 0)],
        preventDefault: vi.fn(),
        stopPropagation: vi.fn(),
      } as unknown as TouchEvent);
      expect((capture as any).queue.size).toBe(1);

      capture.flushNow();

      expect(posted).toHaveLength(1);
      const msg = posted[0] as { buffer: ArrayBuffer };

      const events = decodeInputBatchEvents(msg.buffer);
      expect(events).toHaveLength(1);
      expect(events[0]!.type).toBe(InputEventType.MouseMove);
      expect(events[0]!.a).toBe(1); // dx
      expect(events[0]!.b).toBe(0); // dy
    });
  });

  it("emulates left click via touchTapToClick (touchstart→down, touchend→up)", () => {
    withStubbedDocument(() => {
      const canvas = makeCanvasStub({ focus: vi.fn() });

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

      (capture as any).handleTouchStart({
        timeStamp: 0,
        touches: [touch(1, 10, 10)],
        changedTouches: [touch(1, 10, 10)],
        preventDefault: vi.fn(),
        stopPropagation: vi.fn(),
      } as unknown as TouchEvent);
      expect((capture as any).queue.size).toBe(0);

      (capture as any).handleTouchEnd({
        timeStamp: 1,
        // No remaining touches (finger lifted).
        touches: [],
        changedTouches: [touch(1, 10, 10)],
        preventDefault: vi.fn(),
        stopPropagation: vi.fn(),
      } as unknown as TouchEvent);
      expect((capture as any).queue.size).toBe(2);

      capture.flushNow();

      expect(posted).toHaveLength(1);
      const msg = posted[0] as { buffer: ArrayBuffer };

      const events = decodeInputBatchEvents(msg.buffer);
      expect(events).toHaveLength(2);
      // First: left down.
      expect(events[0]!.type).toBe(InputEventType.MouseButtons);
      expect(events[0]!.a).toBe(1);
      // Second: left up.
      expect(events[1]!.type).toBe(InputEventType.MouseButtons);
      expect(events[1]!.a).toBe(0);
    });
  });

  it("cancels pending tap-to-click on blur so a later touchend cannot click", () => {
    withStubbedDocument(() => {
      const canvas = makeCanvasStub({ focus: vi.fn() });

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

      (capture as any).handleTouchStart({
        timeStamp: 0,
        touches: [touch(1, 0, 0)],
        changedTouches: [touch(1, 0, 0)],
        preventDefault: vi.fn(),
        stopPropagation: vi.fn(),
      } as unknown as TouchEvent);

      // Blur should cancel the touch session and flush immediately (no-op since queue is empty).
      (capture as any).handleBlur();

      // A subsequent touchend (common when the browser delivers the end event after blur) must not
      // produce a click.
      (capture as any).handleTouchEnd({
        timeStamp: 1,
        touches: [],
        changedTouches: [touch(1, 0, 0)],
        preventDefault: vi.fn(),
        stopPropagation: vi.fn(),
      } as unknown as TouchEvent);

      capture.flushNow();

      expect((capture as any).mouseButtons).toBe(0);
      expect(posted).toHaveLength(0);
    });
  });
});
