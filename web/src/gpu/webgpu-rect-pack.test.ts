import { describe, expect, it } from "vitest";
import { clampRect, packRgba8RectToAlignedBuffer, type PackedRect, type Rect } from "./webgpu-rect-pack";

function alignUp(value: number, alignment: number): number {
  return Math.ceil(value / alignment) * alignment;
}

function makeSyntheticRgbaImage(width: number, height: number, strideBytes: number): Uint8Array {
  const out = new Uint8Array(strideBytes * height);
  for (let y = 0; y < height; y += 1) {
    for (let x = 0; x < width; x += 1) {
      const off = y * strideBytes + x * 4;
      out[off + 0] = x & 0xff;
      out[off + 1] = y & 0xff;
      out[off + 2] = (x + y) & 0xff;
      out[off + 3] = 0xff;
    }
    // Mark stride padding with a non-zero value so tests can detect accidental copies.
    out.fill(0xee, y * strideBytes + width * 4, (y + 1) * strideBytes);
  }
  return out;
}

function expectedPackedRect(rect: Rect): { bytesPerRow: number; bytes: Uint8Array } {
  const rowBytes = rect.w * 4;
  const bytesPerRow = alignUp(rowBytes, 256);
  const out = new Uint8Array(bytesPerRow * rect.h);
  for (let row = 0; row < rect.h; row += 1) {
    for (let col = 0; col < rect.w; col += 1) {
      const x = rect.x + col;
      const y = rect.y + row;
      const dst = row * bytesPerRow + col * 4;
      out[dst + 0] = x & 0xff;
      out[dst + 1] = y & 0xff;
      out[dst + 2] = (x + y) & 0xff;
      out[dst + 3] = 0xff;
    }
  }
  return { bytesPerRow, bytes: out };
}

describe("clampRect", () => {
  it("returns null for empty or non-intersecting rects", () => {
    expect(clampRect({ x: 0, y: 0, w: 0, h: 1 }, 4, 4)).toBeNull();
    expect(clampRect({ x: 0, y: 0, w: 1, h: 0 }, 4, 4)).toBeNull();
    expect(clampRect({ x: -10, y: 0, w: 5, h: 5 }, 4, 4)).toBeNull();
    expect(clampRect({ x: 10, y: 0, w: 5, h: 5 }, 4, 4)).toBeNull();
  });

  it("clamps rects partially out-of-bounds", () => {
    expect(clampRect({ x: 2, y: 1, w: 10, h: 10 }, 4, 3)).toEqual({ x: 2, y: 1, w: 2, h: 2 });
    expect(clampRect({ x: -2, y: -1, w: 4, h: 3 }, 4, 3)).toEqual({ x: 0, y: 0, w: 2, h: 2 });
  });
});

describe("packRgba8RectToAlignedBuffer", () => {
  it("packs rows using srcStrideBytes, ignoring stride padding", () => {
    const srcWidth = 4;
    const srcHeight = 3;
    const strideBytes = 24; // >= width*4, with padding bytes.
    const src = makeSyntheticRgbaImage(srcWidth, srcHeight, strideBytes);

    const rect: Rect = { x: 1, y: 1, w: 2, h: 2 };
    const out: PackedRect = { x: 0, y: 0, w: 0, h: 0, bytesPerRow: 0, byteLength: 0 };
    const buf = packRgba8RectToAlignedBuffer(src, strideBytes, srcWidth, srcHeight, rect, null, out);
    expect(buf).not.toBeNull();
    if (!buf) return;

    const expected = expectedPackedRect(rect);
    expect(out.bytesPerRow).toBe(expected.bytesPerRow);
    expect(Array.from(buf.subarray(0, out.byteLength))).toEqual(Array.from(expected.bytes));
  });

  it("handles height=1 (single-row rects)", () => {
    const srcWidth = 5;
    const srcHeight = 4;
    const strideBytes = 32;
    const src = makeSyntheticRgbaImage(srcWidth, srcHeight, strideBytes);

    const rect: Rect = { x: 0, y: 2, w: 3, h: 1 };
    const out: PackedRect = { x: 0, y: 0, w: 0, h: 0, bytesPerRow: 0, byteLength: 0 };
    const buf = packRgba8RectToAlignedBuffer(src, strideBytes, srcWidth, srcHeight, rect, null, out);
    expect(buf).not.toBeNull();
    if (!buf) return;

    const expected = expectedPackedRect(rect);
    expect(out.bytesPerRow).toBe(expected.bytesPerRow);
    expect(Array.from(buf.subarray(0, out.byteLength))).toEqual(Array.from(expected.bytes));
  });

  it("clamps the input rect before packing", () => {
    const srcWidth = 4;
    const srcHeight = 3;
    const strideBytes = 24;
    const src = makeSyntheticRgbaImage(srcWidth, srcHeight, strideBytes);

    const rect: Rect = { x: 3, y: 2, w: 10, h: 10 };
    const out: PackedRect = { x: 0, y: 0, w: 0, h: 0, bytesPerRow: 0, byteLength: 0 };
    const buf = packRgba8RectToAlignedBuffer(src, strideBytes, srcWidth, srcHeight, rect, null, out);
    expect(buf).not.toBeNull();
    if (!buf) return;

    expect({ x: out.x, y: out.y, w: out.w, h: out.h }).toEqual({ x: 3, y: 2, w: 1, h: 1 });

    const expected = expectedPackedRect({ x: out.x, y: out.y, w: out.w, h: out.h });
    expect(Array.from(buf.subarray(0, out.byteLength))).toEqual(Array.from(expected.bytes));
  });

  it("reuses the staging buffer when it is large enough", () => {
    const srcWidth = 8;
    const srcHeight = 8;
    const strideBytes = 64;
    const src = makeSyntheticRgbaImage(srcWidth, srcHeight, strideBytes);

    const out: PackedRect = { x: 0, y: 0, w: 0, h: 0, bytesPerRow: 0, byteLength: 0 };

    const rect0: Rect = { x: 0, y: 0, w: 4, h: 4 };
    const buf0 = packRgba8RectToAlignedBuffer(src, strideBytes, srcWidth, srcHeight, rect0, null, out);
    expect(buf0).not.toBeNull();
    if (!buf0) return;

    const rect1: Rect = { x: 1, y: 1, w: 2, h: 2 };
    const buf1 = packRgba8RectToAlignedBuffer(src, strideBytes, srcWidth, srcHeight, rect1, buf0, out);
    expect(buf1).toBe(buf0);
  });
});
