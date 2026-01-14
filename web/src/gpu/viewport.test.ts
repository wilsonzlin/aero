import { describe, expect, it } from "vitest";

import { computeViewport } from "./viewport";

describe("computeViewport", () => {
  it("returns an empty viewport for any non-positive canvas/src dimension", () => {
    const empty = { x: 0, y: 0, w: 0, h: 0 };

    const cases: Array<[number, number, number, number]> = [
      [0, 100, 10, 10],
      [100, 0, 10, 10],
      [-1, 100, 10, 10],
      [100, -1, 10, 10],
      [100, 100, 0, 10],
      [100, 100, 10, 0],
      [100, 100, -10, 10],
      [100, 100, 10, -10],
    ];

    for (const [canvasW, canvasH, srcW, srcH] of cases) {
      expect(computeViewport(canvasW, canvasH, srcW, srcH, "fit")).toEqual(empty);
      expect(computeViewport(canvasW, canvasH, srcW, srcH, "stretch")).toEqual(empty);
      expect(computeViewport(canvasW, canvasH, srcW, srcH, "integer")).toEqual(empty);
    }
  });

  it("mode=stretch fills the canvas", () => {
    expect(computeViewport(123, 45, 10, 10, "stretch")).toEqual({ x: 0, y: 0, w: 123, h: 45 });
  });

  it("mode=fit letterboxes/pillarboxes while preserving aspect ratio", () => {
    // Letterbox: canvas is taller than source.
    expect(computeViewport(100, 100, 200, 100, "fit")).toEqual({ x: 0, y: 25, w: 100, h: 50 });

    // Pillarbox: canvas is wider than source.
    // Note: x uses floor() rounding for the odd leftover pixel.
    expect(computeViewport(100, 50, 100, 200, "fit")).toEqual({ x: 37, y: 0, w: 25, h: 50 });
  });

  it("mode=integer uses an integer scale when possible", () => {
    // scaleFit = 2.5 -> integer scale 2
    expect(computeViewport(400, 300, 160, 120, "integer")).toEqual({ x: 40, y: 30, w: 320, h: 240 });

    // scaleFit ≈ 1.666 -> integer scale 1 (more letterboxing than fit mode).
    expect(computeViewport(300, 200, 160, 120, "integer")).toEqual({ x: 70, y: 40, w: 160, h: 120 });
  });

  it("mode=integer falls back to fit when integer scale would be < 1", () => {
    const fit = computeViewport(100, 100, 200, 100, "fit");
    const integer = computeViewport(100, 100, 200, 100, "integer");
    expect(integer).toEqual(fit);
  });

  it("centers using floor() rounding for odd pixel differences", () => {
    // diffX = 4 - 3 = 1 => x = 0 (floor(0.5)).
    expect(computeViewport(4, 3, 2, 2, "fit")).toEqual({ x: 0, y: 0, w: 3, h: 3 });

    // diffY = 4 - 3 = 1 => y = 0 (floor(0.5)).
    expect(computeViewport(3, 4, 2, 2, "fit")).toEqual({ x: 0, y: 0, w: 3, h: 3 });

    // diffX = 7 - 4 = 3 => x = 1 (floor(1.5)).
    expect(computeViewport(7, 4, 2, 2, "fit")).toEqual({ x: 1, y: 0, w: 4, h: 4 });

    // diffY = 7 - 4 = 3 => y = 1 (floor(1.5)).
    expect(computeViewport(4, 7, 2, 2, "fit")).toEqual({ x: 0, y: 1, w: 4, h: 4 });
  });
});
