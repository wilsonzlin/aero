import { describe, expect, it } from "vitest";
import { createHash } from "node:crypto";

import { createGpuColorTestCardRgba8Linear, srgbEncodeChannel } from "./test-card";

function getPixelRgba(rgba: Uint8Array, width: number, x: number, y: number): [number, number, number, number] {
  const i = (y * width + x) * 4;
  return [rgba[i + 0]!, rgba[i + 1]!, rgba[i + 2]!, rgba[i + 3]!];
}

function sha256Hex(bytes: Uint8Array): string {
  return createHash("sha256").update(bytes).digest("hex");
}

describe("gpu/test-card", () => {
  describe("srgbEncodeChannel", () => {
    it("clamps out-of-range inputs to [0, 255]", () => {
      expect(srgbEncodeChannel(-1)).toBe(0);
      expect(srgbEncodeChannel(2)).toBe(255);
    });

    it("encodes key points deterministically", () => {
      expect(srgbEncodeChannel(0)).toBe(0);
      expect(srgbEncodeChannel(1)).toBe(255);

      // Compute the expected value without calling the function under test.
      const linear = 0.5;
      const v = Math.min(1, Math.max(0, linear));
      const srgb = v <= 0.0031308 ? v * 12.92 : 1.055 * Math.pow(v, 1 / 2.4) - 0.055;
      const expected = Math.min(255, Math.max(0, Math.round(srgb * 255)));

      expect(srgbEncodeChannel(linear)).toBe(expected);
    });

    it("uses the linear segment for very dark values", () => {
      // Compute the expected value without calling the function under test.
      const linear = 0.002;
      const v = Math.min(1, Math.max(0, linear));
      const srgb = v <= 0.0031308 ? v * 12.92 : 1.055 * Math.pow(v, 1 / 2.4) - 0.055;
      const expected = Math.min(255, Math.max(0, Math.round(srgb * 255)));

      expect(srgbEncodeChannel(linear)).toBe(expected);
      // Sanity: this should be in the "low end" range (single-digit / teen values).
      expect(expected).toBeLessThan(20);
    });
  });

  describe("createGpuColorTestCardRgba8Linear", () => {
    it("generates a stable linear RGBA8 test card (gamma + alpha + orientation markers)", () => {
      const width = 8;
      const height = 4;
      const rgba = createGpuColorTestCardRgba8Linear(width, height);

      expect(rgba.length).toBe(width * height * 4);
      // Full-buffer hash to ensure the generator remains deterministic even if individual spot
      // checks above/below miss something.
      expect(sha256Hex(rgba)).toBe("0a8c8fdfa78c3d894b85409177f8a7870f8cac9c9964476de0f5239e4c531381");

      // Left half: grayscale ramp, full alpha.
      expect(getPixelRgba(rgba, width, 0, 1)).toEqual([0, 0, 0, 255]);
      expect(getPixelRgba(rgba, width, 1, 1)).toEqual([85, 85, 85, 255]);
      expect(getPixelRgba(rgba, width, 2, 1)).toEqual([170, 170, 170, 255]);
      expect(getPixelRgba(rgba, width, 3, 1)).toEqual([255, 255, 255, 255]);

      // Right half: magenta with top->bottom alpha gradient (height=4 => y=0..3 => a=0,1/3,2/3,1).
      expect(getPixelRgba(rgba, width, 4, 0)).toEqual([255, 0, 255, 0]);
      expect(getPixelRgba(rgba, width, 4, 1)).toEqual([255, 0, 255, 85]);
      expect(getPixelRgba(rgba, width, 4, 2)).toEqual([255, 0, 255, 170]);
      expect(getPixelRgba(rgba, width, 4, 3)).toEqual([255, 0, 255, 255]);

      // Corner orientation markers (UV origin top-left).
      expect(getPixelRgba(rgba, width, 0, 0)).toEqual([255, 0, 0, 255]); // red
      expect(getPixelRgba(rgba, width, width - 1, 0)).toEqual([0, 255, 0, 255]); // green
      expect(getPixelRgba(rgba, width, 0, height - 1)).toEqual([0, 0, 255, 255]); // blue
      expect(getPixelRgba(rgba, width, width - 1, height - 1)).toEqual([255, 255, 255, 255]); // white
    });

    it("handles height=1 without throwing (degenerate alpha gradient)", () => {
      const width = 3;
      const height = 1;

      expect(() => createGpuColorTestCardRgba8Linear(width, height)).not.toThrow();

      const rgba = createGpuColorTestCardRgba8Linear(width, height);
      expect(rgba.length).toBe(width * height * 4);

      // With height=1, the "top" and "bottom" corners overlap. The marker writes are still
      // deterministic (later writes win):
      // - (0,0) ends up as bottom-left marker (blue)
      // - (w-1,0) ends up as bottom-right marker (white)
      expect(getPixelRgba(rgba, width, 0, 0)).toEqual([0, 0, 255, 255]);
      expect(getPixelRgba(rgba, width, width - 1, 0)).toEqual([255, 255, 255, 255]);

      // Middle pixel is not a corner marker. For height=1 we define a=1, so the right half stays fully opaque.
      expect(getPixelRgba(rgba, width, 1, 0)).toEqual([255, 0, 255, 255]);
    });

    it("handles width=1 without throwing (degenerate left/right split)", () => {
      const width = 1;
      const height = 3;

      expect(() => createGpuColorTestCardRgba8Linear(width, height)).not.toThrow();

      const rgba = createGpuColorTestCardRgba8Linear(width, height);
      expect(rgba.length).toBe(width * height * 4);

      // With width=1, the "left" and "right" corner markers overlap in X. Later writes win:
      // - (0,0) ends up as top-right marker (green)
      // - (0,h-1) ends up as bottom-right marker (white)
      expect(getPixelRgba(rgba, width, 0, 0)).toEqual([0, 255, 0, 255]);
      expect(getPixelRgba(rgba, width, 0, height - 1)).toEqual([255, 255, 255, 255]);

      // Non-marker pixels still follow the card definition (magenta with vertical alpha gradient).
      expect(getPixelRgba(rgba, width, 0, 1)).toEqual([255, 0, 255, 128]);
    });

    it("handles width=2 (half <= 1 grayscale path) without throwing", () => {
      const width = 2;
      const height = 3;

      expect(() => createGpuColorTestCardRgba8Linear(width, height)).not.toThrow();
      const rgba = createGpuColorTestCardRgba8Linear(width, height);

      // Left half has only one column when width=2 (half=floor(2/2)=1). The ramp degenerates to t=0.
      expect(getPixelRgba(rgba, width, 0, 1)).toEqual([0, 0, 0, 255]);

      // Right half remains magenta with vertical alpha gradient.
      expect(getPixelRgba(rgba, width, 1, 1)).toEqual([255, 0, 255, 128]);
    });

    it("handles zero-sized dimensions without throwing", () => {
      expect(() => createGpuColorTestCardRgba8Linear(0, 0)).not.toThrow();
      expect(createGpuColorTestCardRgba8Linear(0, 0)).toHaveLength(0);

      expect(() => createGpuColorTestCardRgba8Linear(0, 5)).not.toThrow();
      expect(createGpuColorTestCardRgba8Linear(0, 5)).toHaveLength(0);

      expect(() => createGpuColorTestCardRgba8Linear(5, 0)).not.toThrow();
      expect(createGpuColorTestCardRgba8Linear(5, 0)).toHaveLength(0);
    });
  });
});
