import { describe, expect, it, vi } from "vitest";

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

describe("InputCapture preventDefault policy", () => {
  it("prevents default for browser navigation keys even though they are not mapped to guest input", () => {
    withStubbedDocument(() => {
      const canvas = {
        tabIndex: 0,
        addEventListener: () => {},
        removeEventListener: () => {},
        focus: () => {},
      } as unknown as HTMLCanvasElement;
      const ioWorker = { postMessage: () => {} };
      const capture = new InputCapture(canvas, ioWorker, { enableGamepad: false });

      // Simulate the canvas being focused.
      (capture as any).hasFocus = true;

      for (const code of ["BrowserBack", "BrowserSearch"]) {
        const preventDefault = vi.fn();
        const stopPropagation = vi.fn();
        const event = {
          code,
          repeat: false,
          timeStamp: 0,
          altKey: false,
          ctrlKey: false,
          shiftKey: false,
          metaKey: false,
          preventDefault,
          stopPropagation,
        } as unknown as KeyboardEvent;

        (capture as any).handleKeyDown(event);
        expect(preventDefault).toHaveBeenCalledTimes(1);
        expect(stopPropagation).toHaveBeenCalledTimes(1);

        (capture as any).handleKeyUp(event);
        expect(preventDefault).toHaveBeenCalledTimes(2);
        expect(stopPropagation).toHaveBeenCalledTimes(2);
      }
    });
  });

  it("prevents default for extra mouse buttons while capture is active (e.g. browser back/forward buttons)", () => {
    withStubbedDocument(() => {
      const canvas = {
        tabIndex: 0,
        addEventListener: () => {},
        removeEventListener: () => {},
        focus: () => {},
      } as unknown as HTMLCanvasElement;
      const ioWorker = { postMessage: () => {} };
      const capture = new InputCapture(canvas, ioWorker, { enableGamepad: false });

      // Simulate the canvas being focused.
      (capture as any).hasFocus = true;

      const preventDefault = vi.fn();
      const stopPropagation = vi.fn();
      const ev = { button: 3, preventDefault, stopPropagation, timeStamp: 0, target: canvas } as unknown as MouseEvent;
      (capture as any).handleMouseDown(ev);
      expect(preventDefault).toHaveBeenCalledTimes(1);
      expect(stopPropagation).toHaveBeenCalledTimes(1);

      (capture as any).handleMouseUp(ev);
      expect(preventDefault).toHaveBeenCalledTimes(2);
      expect(stopPropagation).toHaveBeenCalledTimes(2);
    });
  });

  it("stops propagation for mousemove events while pointer lock is active", () => {
    withStubbedDocument(() => {
      const canvas = {
        tabIndex: 0,
        addEventListener: () => {},
        removeEventListener: () => {},
        focus: () => {},
      } as unknown as HTMLCanvasElement;
      const ioWorker = { postMessage: () => {} };
      const capture = new InputCapture(canvas, ioWorker, { enableGamepad: false });

      // Simulate pointer lock (the mousemove listener is attached to `document`).
      (capture as any).pointerLock.locked = true;

      const preventDefault = vi.fn();
      const stopPropagation = vi.fn();
      const ev = {
        movementX: 3,
        movementY: -2,
        preventDefault,
        stopPropagation,
        timeStamp: 0,
      } as unknown as MouseEvent;
      (capture as any).handleMouseMove(ev);
      expect(preventDefault).toHaveBeenCalledTimes(1);
      expect(stopPropagation).toHaveBeenCalledTimes(1);
    });
  });

  it("releases captured mouse buttons even if mouseup occurs outside the canvas (drag-out)", () => {
    withStubbedDocument(() => {
      const canvas = {
        tabIndex: 0,
        addEventListener: () => {},
        removeEventListener: () => {},
        focus: () => {},
      } as unknown as HTMLCanvasElement;
      const ioWorker = { postMessage: () => {} };
      const capture = new InputCapture(canvas, ioWorker, { enableGamepad: false, recycleBuffers: false });

      // Simulate the canvas being focused (capture active).
      (capture as any).hasFocus = true;

      const downPreventDefault = vi.fn();
      const downStopPropagation = vi.fn();
      (capture as any).handleMouseDown({
        button: 0,
        preventDefault: downPreventDefault,
        stopPropagation: downStopPropagation,
        timeStamp: 0,
        target: canvas,
      } as unknown as MouseEvent);

      expect(downPreventDefault).toHaveBeenCalledTimes(1);
      expect(downStopPropagation).toHaveBeenCalledTimes(1);
      expect((capture as any).mouseButtons).toBe(1);
      expect((capture as any).queue.size).toBe(1);

      const upPreventDefault = vi.fn();
      const upStopPropagation = vi.fn();
      (capture as any).handleMouseUp({
        button: 0,
        preventDefault: upPreventDefault,
        stopPropagation: upStopPropagation,
        timeStamp: 1,
        // Not the canvas: common when the user drags outside the VM and releases.
        target: {},
      } as unknown as MouseEvent);

      expect(upPreventDefault).toHaveBeenCalledTimes(1);
      expect(upStopPropagation).toHaveBeenCalledTimes(1);
      expect((capture as any).mouseButtons).toBe(0);
      expect((capture as any).queue.size).toBe(2);
    });
  });

  it("does not swallow mouseup events outside the canvas when the VM was not tracking that button", () => {
    withStubbedDocument(() => {
      const canvas = {
        tabIndex: 0,
        addEventListener: () => {},
        removeEventListener: () => {},
        focus: () => {},
      } as unknown as HTMLCanvasElement;
      const ioWorker = { postMessage: () => {} };
      const capture = new InputCapture(canvas, ioWorker, { enableGamepad: false, recycleBuffers: false });

      (capture as any).hasFocus = true;

      const upPreventDefault = vi.fn();
      const upStopPropagation = vi.fn();
      (capture as any).handleMouseUp({
        button: 0,
        preventDefault: upPreventDefault,
        stopPropagation: upStopPropagation,
        timeStamp: 0,
        target: {},
      } as unknown as MouseEvent);

      expect(upPreventDefault).toHaveBeenCalledTimes(0);
      expect(upStopPropagation).toHaveBeenCalledTimes(0);
      expect((capture as any).queue.size).toBe(0);
    });
  });
});
