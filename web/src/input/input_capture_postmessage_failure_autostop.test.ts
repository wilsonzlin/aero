import { describe, expect, it, vi } from "vitest";

import { InputCapture } from "./input_capture";
import { withStubbedDocument, withStubbedWindow } from "./test_utils";

describe("InputCapture postMessage failures", () => {
  it("auto-stops capture when an input batch cannot be delivered", () => {
    withStubbedWindow((win) =>
      withStubbedDocument((doc) => {
        const clearInterval = vi.fn();
        win.setInterval = vi.fn(() => 123);
        win.clearInterval = clearInterval;
        win.addEventListener = vi.fn();
        win.removeEventListener = vi.fn();

        doc.addEventListener = vi.fn();
        doc.removeEventListener = vi.fn();
        doc.exitPointerLock = vi.fn(() => {});

        const canvas = {
          tabIndex: 0,
          addEventListener: vi.fn(),
          removeEventListener: vi.fn(),
          focus: vi.fn(),
        } as unknown as HTMLCanvasElement;
        doc.activeElement = canvas;

        const ioWorker = {
          postMessage: vi.fn(() => {
            throw new Error("postMessage failed");
          }),
        };

        const capture = new InputCapture(canvas, ioWorker, { enableGamepad: false, recycleBuffers: false });
        capture.start();

        // Inject a keydown event so there is definitely something to flush.
        const preventDefault = vi.fn();
        const stopPropagation = vi.fn();
        const event = {
          code: "KeyA",
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
        expect((capture as any).queue.size).toBeGreaterThan(0);

        // The flush should fail and trigger an automatic stop (clearing the timer).
        capture.flushNow();
        expect(clearInterval).toHaveBeenCalledTimes(1);

        // Subsequent explicit stop should be a no-op.
        capture.stop();
        expect(clearInterval).toHaveBeenCalledTimes(1);
      }),
    );
  });
});

