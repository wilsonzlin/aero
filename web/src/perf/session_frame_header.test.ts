import { describe, expect, it } from "vitest";

import { PerfSession } from "./session";
import { PERF_FRAME_HEADER_FRAME_ID_INDEX } from "./shared.js";

describe("PerfSession", () => {
  it("updates the shared perf frame header on RAF ticks", () => {
    const g = globalThis as unknown as {
      window?: unknown;
      requestAnimationFrame?: unknown;
      cancelAnimationFrame?: unknown;
    } & Record<string, unknown>;

    const originalWindow = g.window;
    const originalRaf = g.requestAnimationFrame;
    const originalCancel = g.cancelAnimationFrame;

    let nextRafId = 1;
    let rafQueue: Array<{ id: number; cb: FrameRequestCallback }> = [];

    g.window = {
      setInterval: () => 1,
      clearInterval: () => {},
    };

    g.requestAnimationFrame = ((cb: FrameRequestCallback) => {
      const id = nextRafId++;
      rafQueue.push({ id, cb });
      return id;
    }) as unknown as typeof requestAnimationFrame;

    g.cancelAnimationFrame = ((id: number) => {
      rafQueue = rafQueue.filter((item) => item.id !== id);
    }) as unknown as typeof cancelAnimationFrame;

    try {
      const session = new PerfSession();
      session.setHudActive(true);

      const header = new Int32Array(session.getChannel().frameHeader);
      expect(Atomics.load(header, PERF_FRAME_HEADER_FRAME_ID_INDEX)).toBe(0);

      let now = performance.now() + 1;
      for (let expectedFrameId = 1; expectedFrameId <= 3; expectedFrameId += 1) {
        const next = rafQueue.shift();
        expect(next).toBeDefined();
        now += 16;
        next!.cb(now);
        expect(Atomics.load(header, PERF_FRAME_HEADER_FRAME_ID_INDEX) >>> 0).toBe(expectedFrameId);
      }

      session.setHudActive(false);
      expect(rafQueue.length).toBe(0);
    } finally {
      if (originalWindow === undefined) {
        delete g.window;
      } else {
        g.window = originalWindow;
      }

      if (originalRaf === undefined) {
        delete g.requestAnimationFrame;
      } else {
        g.requestAnimationFrame = originalRaf;
      }

      if (originalCancel === undefined) {
        delete g.cancelAnimationFrame;
      } else {
        g.cancelAnimationFrame = originalCancel;
      }
    }
  });
});

