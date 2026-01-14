import { describe, expect, it, vi } from "vitest";

import { InputCapture } from "./input_capture";

function withStubbedDom<T>(run: (ctx: { window: any; document: any }) => T): T {
  const originalWindow = (globalThis as any).window;
  const originalDocument = (globalThis as any).document;

  const addCounts = new Map<string, number>();
  const removeCounts = new Map<string, number>();

  const doc: any = {
    pointerLockElement: null,
    visibilityState: "visible",
    activeElement: null,
    hasFocus: () => true,
    addEventListener: vi.fn((type: string) => {
      addCounts.set(type, (addCounts.get(type) ?? 0) + 1);
    }),
    removeEventListener: vi.fn((type: string) => {
      removeCounts.set(type, (removeCounts.get(type) ?? 0) + 1);
    }),
    exitPointerLock: vi.fn(() => {}),
  };
  doc.addCounts = addCounts;
  doc.removeCounts = removeCounts;

  const win: any = {
    addEventListener: vi.fn(() => {}),
    removeEventListener: vi.fn(() => {}),
    setInterval: vi.fn(() => 1),
    clearInterval: vi.fn(() => {}),
  };

  (globalThis as any).document = doc;
  (globalThis as any).window = win;

  try {
    return run({ window: win, document: doc });
  } finally {
    (globalThis as any).window = originalWindow;
    (globalThis as any).document = originalDocument;
  }
}

describe("InputCapture restart behavior", () => {
  it("reattaches pointer lock listeners and refreshes pointerLocked across stop/start cycles", () => {
    withStubbedDom(({ document: doc }) => {
      const canvas = {
        tabIndex: 0,
        addEventListener: () => {},
        removeEventListener: () => {},
        focus: () => {},
      } as unknown as HTMLCanvasElement;

      const ioWorker = { postMessage: vi.fn() };
      const capture = new InputCapture(canvas, ioWorker, { enableGamepad: false, recycleBuffers: false });

      expect(doc.addCounts.get("pointerlockchange") ?? 0).toBe(1);

      capture.start();
      // Pointer lock was already attached by the constructor; start() should not double-attach.
      expect(doc.addCounts.get("pointerlockchange") ?? 0).toBe(1);

      capture.stop();
      expect(doc.removeCounts.get("pointerlockchange") ?? 0).toBe(1);

      // Simulate a pointer lock state change while capture is stopped (and listeners are detached).
      doc.pointerLockElement = canvas;
      expect(capture.pointerLocked).toBe(false);

      capture.start();

      // Pointer lock listeners should be reattached and lock state refreshed.
      expect(doc.addCounts.get("pointerlockchange") ?? 0).toBe(2);
      expect(capture.pointerLocked).toBe(true);

      capture.stop();
    });
  });
});
