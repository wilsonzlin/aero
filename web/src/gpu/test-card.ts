/**
 * Deterministic test card for validating:
 * - sRGB vs linear output (gamma)
 * - alpha handling (opaque vs premultiplied)
 * - UV origin / Y flip correctness (corner markers)
 *
 * The generated buffer is **linear RGBA8** (matching the presentation policy default).
 */

function clamp01(x: number): number {
  return Math.min(1, Math.max(0, x));
}

export function srgbEncodeChannel(linear01: number): number {
  const v = clamp01(linear01);
  const srgb = v <= 0.0031308 ? v * 12.92 : 1.055 * Math.pow(v, 1 / 2.4) - 0.055;
  return Math.min(255, Math.max(0, Math.round(srgb * 255)));
}

export function createGpuColorTestCardRgba8Linear(width: number, height: number): Uint8Array {
  const data = new Uint8Array(width * height * 4);

  const half = Math.floor(width / 2);

  for (let y = 0; y < height; y++) {
    const a = height === 1 ? 1 : y / (height - 1); // alpha gradient top->bottom
    for (let x = 0; x < width; x++) {
      let r = 0;
      let g = 0;
      let b = 0;
      let alpha = 1;

      if (x < half) {
        // Left: grayscale ramp (linear) with full alpha.
        const t = half <= 1 ? 0 : x / (half - 1);
        r = g = b = t;
        alpha = 1;
      } else {
        // Right: constant magenta in linear space with varying alpha.
        r = 1;
        g = 0;
        b = 1;
        alpha = a;
      }

      const i = (y * width + x) * 4;
      data[i + 0] = Math.round(clamp01(r) * 255);
      data[i + 1] = Math.round(clamp01(g) * 255);
      data[i + 2] = Math.round(clamp01(b) * 255);
      data[i + 3] = Math.round(clamp01(alpha) * 255);
    }
  }

  // Orientation markers (UV origin top-left):
  // top-left: red, top-right: green, bottom-left: blue, bottom-right: white.
  const set = (x: number, y: number, r: number, g: number, b: number, a: number) => {
    const i = (y * width + x) * 4;
    data[i + 0] = r;
    data[i + 1] = g;
    data[i + 2] = b;
    data[i + 3] = a;
  };
  if (width > 0 && height > 0) {
    set(0, 0, 255, 0, 0, 255);
    set(width - 1, 0, 0, 255, 0, 255);
    set(0, height - 1, 0, 0, 255, 255);
    set(width - 1, height - 1, 255, 255, 255, 255);
  }

  return data;
}

