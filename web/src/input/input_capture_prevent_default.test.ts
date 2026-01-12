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
  it("prevents default for BrowserBack even though it is not mapped to guest input", () => {
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
      const event = {
        code: "BrowserBack",
        repeat: false,
        timeStamp: 0,
        altKey: false,
        ctrlKey: false,
        shiftKey: false,
        metaKey: false,
        preventDefault,
        stopPropagation: vi.fn(),
      } as unknown as KeyboardEvent;

      (capture as any).handleKeyDown(event);
      expect(preventDefault).toHaveBeenCalledTimes(1);
    });
  });

  it("prevents default for non-guest mouse buttons while capture is active (e.g. browser back/forward buttons)", () => {
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
      const ev = { button: 3, preventDefault, timeStamp: 0 } as unknown as MouseEvent;
      (capture as any).handleMouseDown(ev);
      expect(preventDefault).toHaveBeenCalledTimes(1);
    });
  });
});

