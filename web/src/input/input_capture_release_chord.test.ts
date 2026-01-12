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

  it("does not let a missing chord keyup cause a later key press to become stuck", () => {
    withStubbedDocument((doc) => {
      const canvas = {
        tabIndex: 0,
        addEventListener: () => {},
        removeEventListener: () => {},
        focus: () => {},
      } as unknown as HTMLCanvasElement;

      doc.pointerLockElement = canvas;

      const ioWorker = { postMessage: () => {} };
      const capture = new InputCapture(canvas, ioWorker, {
        enableGamepad: false,
        recycleBuffers: false,
        releasePointerLockChord: { code: "Escape" },
      });

      (capture as any).hasFocus = true;
      (capture as any).pointerLock.locked = true;

      // Trigger the host-only chord. Intentionally do NOT deliver keyup (some browsers may swallow it).
      (capture as any).handleKeyDown({
        code: "Escape",
        repeat: false,
        timeStamp: 0,
        altKey: false,
        ctrlKey: false,
        shiftKey: false,
        metaKey: false,
        preventDefault: vi.fn(),
        stopPropagation: vi.fn(),
      } as unknown as KeyboardEvent);

      // Pointer lock is now assumed to be released; subsequent Escape presses should be delivered to the guest normally.
      (capture as any).pointerLock.locked = false;

      const downPreventDefault = vi.fn();
      const downStopPropagation = vi.fn();
      (capture as any).handleKeyDown({
        code: "Escape",
        repeat: false,
        timeStamp: 1,
        altKey: false,
        ctrlKey: false,
        shiftKey: false,
        metaKey: false,
        preventDefault: downPreventDefault,
        stopPropagation: downStopPropagation,
      } as unknown as KeyboardEvent);

      // Expect a HID + PS/2 event pair for keydown.
      expect((capture as any).queue.size).toBe(2);

      const upPreventDefault = vi.fn();
      const upStopPropagation = vi.fn();
      (capture as any).handleKeyUp({
        code: "Escape",
        repeat: false,
        timeStamp: 2,
        altKey: false,
        ctrlKey: false,
        shiftKey: false,
        metaKey: false,
        preventDefault: upPreventDefault,
        stopPropagation: upStopPropagation,
      } as unknown as KeyboardEvent);

      // If stale suppression isn't cleared, this keyup would be swallowed and the key would stick.
      expect((capture as any).queue.size).toBe(4);
      expect(downPreventDefault).toHaveBeenCalled();
      expect(downStopPropagation).toHaveBeenCalled();
      expect(upPreventDefault).toHaveBeenCalled();
      expect(upStopPropagation).toHaveBeenCalled();
    });
  });

  it("swallows repeated keydown events for a chord key while waiting for the corresponding keyup", () => {
    withStubbedDocument((doc) => {
      const canvas = {
        tabIndex: 0,
        addEventListener: () => {},
        removeEventListener: () => {},
        focus: () => {},
      } as unknown as HTMLCanvasElement;

      doc.pointerLockElement = canvas;

      const ioWorker = { postMessage: () => {} };
      const capture = new InputCapture(canvas, ioWorker, {
        enableGamepad: false,
        recycleBuffers: false,
        releasePointerLockChord: { code: "Escape" },
      });

      (capture as any).hasFocus = true;
      (capture as any).pointerLock.locked = true;

      // Trigger the chord; this should swallow and mark Escape for keyup suppression.
      (capture as any).handleKeyDown({
        code: "Escape",
        repeat: false,
        timeStamp: 0,
        altKey: false,
        ctrlKey: false,
        shiftKey: false,
        metaKey: false,
        preventDefault: vi.fn(),
        stopPropagation: vi.fn(),
      } as unknown as KeyboardEvent);
      expect((capture as any).suppressedKeyUps.has("Escape")).toBe(true);

      // Simulate pointer lock exit and browser key repeat still firing while Escape is held.
      (capture as any).pointerLock.locked = false;
      (capture as any).handleKeyDown({
        code: "Escape",
        repeat: true,
        timeStamp: 1,
        altKey: false,
        ctrlKey: false,
        shiftKey: false,
        metaKey: false,
        preventDefault: vi.fn(),
        stopPropagation: vi.fn(),
      } as unknown as KeyboardEvent);

      // Repeat events must not be forwarded to the guest; the queue should stay empty.
      expect((capture as any).queue.size).toBe(0);

      // Keyup is swallowed to complete the host-only chord.
      (capture as any).handleKeyUp({
        code: "Escape",
        repeat: false,
        timeStamp: 2,
        altKey: false,
        ctrlKey: false,
        shiftKey: false,
        metaKey: false,
        preventDefault: vi.fn(),
        stopPropagation: vi.fn(),
      } as unknown as KeyboardEvent);

      expect((capture as any).suppressedKeyUps.has("Escape")).toBe(false);
    });
  });
});
