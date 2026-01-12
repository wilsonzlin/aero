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
    exitPointerLock: vi.fn(),
  };
  (globalThis as any).document = doc;
  try {
    return run(doc);
  } finally {
    (globalThis as any).document = original;
  }
}

describe("InputCapture releasePointerLockChord", () => {
  it("swallows both keydown and keyup for the release chord so the guest does not see a stray break", () => {
    withStubbedDocument((doc) => {
      const canvas = {
        tabIndex: 0,
        addEventListener: () => {},
        removeEventListener: () => {},
        focus: () => {},
      } as unknown as HTMLCanvasElement;

      // Simulate that pointer lock is active for the canvas so PointerLock.exit() will invoke document.exitPointerLock().
      doc.pointerLockElement = canvas;

      const posted: any[] = [];
      const ioWorker = { postMessage: (msg: unknown) => posted.push(msg) };
      const capture = new InputCapture(canvas, ioWorker, {
        enableGamepad: false,
        recycleBuffers: false,
        releasePointerLockChord: { code: "Escape" },
      });

      // Simulate the VM actively capturing keyboard input while pointer locked.
      (capture as any).hasFocus = true;
      (capture as any).pointerLock.locked = true;

      const downPreventDefault = vi.fn();
      const downStopPropagation = vi.fn();
      (capture as any).handleKeyDown({
        code: "Escape",
        repeat: false,
        timeStamp: 0,
        altKey: false,
        ctrlKey: false,
        shiftKey: false,
        metaKey: false,
        preventDefault: downPreventDefault,
        stopPropagation: downStopPropagation,
      } as unknown as KeyboardEvent);

      expect(downPreventDefault).toHaveBeenCalledTimes(1);
      expect(downStopPropagation).toHaveBeenCalledTimes(1);
      expect(doc.exitPointerLock).toHaveBeenCalledTimes(1);
      expect((capture as any).queue.size).toBe(0);

      const upPreventDefault = vi.fn();
      const upStopPropagation = vi.fn();
      (capture as any).handleKeyUp({
        code: "Escape",
        repeat: false,
        timeStamp: 1,
        altKey: false,
        ctrlKey: false,
        shiftKey: false,
        metaKey: false,
        preventDefault: upPreventDefault,
        stopPropagation: upStopPropagation,
      } as unknown as KeyboardEvent);

      expect(upPreventDefault).toHaveBeenCalledTimes(1);
      expect(upStopPropagation).toHaveBeenCalledTimes(1);
      expect((capture as any).queue.size).toBe(0);

      capture.flushNow();
      expect(posted).toHaveLength(0);
    });
  });
});

